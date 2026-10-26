// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI glue for one SeeCut-owned complete image editor. Blocking host operations
//! stay on a serial worker; the UI receives lifecycle facts, never control secrets.

use crate::ui::{App, PhotoWorkspace, SeeCut};
use seecut_photo_host::{CloseOutcome, InstanceStatus, LaunchOptions, NativeHost};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

mod agent_read;
mod owned_metadata;
pub(crate) use agent_read::{Cancellation, PendingState, ReadBinding, ReadOrigin};
mod target;
pub(crate) use owned_metadata::{
    OwnedMetadata, OwnedOrigin, OwnedSource, PendingOwnedMetadata, PhotoWorkerClient,
};
pub(crate) use target::PhotoTarget;
type SourceHook = (uuid::Uuid, std::rc::Rc<dyn Fn()>);
type ReadSource = std::rc::Rc<dyn Fn() -> Option<OwnedSource>>;
thread_local! {
    static READ_CANCEL: RefCell<Option<Cancellation>> = const { RefCell::new(None) };
    static READ_SOURCE: RefCell<Option<ReadSource>> = const { RefCell::new(None) };
    static TARGET: RefCell<target::TargetState> = RefCell::new(target::TargetState::default());
    static SOURCE_FENCE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(Some(0)) };
    static SOURCE_HOOK: RefCell<Option<SourceHook>> = const { RefCell::new(None) };
}
pub(crate) fn set_read_source(probe: ReadSource) {
    READ_SOURCE.with(|slot| *slot.borrow_mut() = Some(probe));
}
pub(crate) fn current_read_source() -> Option<OwnedSource> {
    let probe = READ_SOURCE.with(|slot| slot.borrow().clone())?;
    probe()
}
pub(crate) fn cancel_photo_reads() {
    READ_CANCEL.with(|slot| {
        if let Some(cancel) = slot.borrow_mut().take() {
            cancel.cancel();
        }
    });
}
pub(crate) fn register_read_cancel(cancel: Cancellation) {
    cancel_photo_reads();
    READ_CANCEL.with(|slot| *slot.borrow_mut() = Some(cancel));
}
pub(crate) fn is_photo_target() -> bool {
    TARGET.with(|t| t.borrow().is_photo())
}
pub(crate) fn current_photo_target() -> Option<PhotoTarget> {
    let source = read_client().and_then(|c| c.managed_source());
    TARGET.with(|t| t.borrow().current(source))
}
pub(crate) fn set_source_hook(id: uuid::Uuid, hook: std::rc::Rc<dyn Fn()>) {
    SOURCE_HOOK.with(|s| *s.borrow_mut() = Some((id, hook)));
}
pub(crate) fn current_ui_id() -> Option<uuid::Uuid> {
    SOURCE_HOOK.with(|s| s.borrow().as_ref().map(|(id, _)| *id))
}
pub(crate) fn remove_source_hook(id: uuid::Uuid) {
    if current_ui_id() == Some(id) {
        invalidate_photo_target();
    }

    SOURCE_HOOK.with(|s| {
        if s.borrow().as_ref().is_some_and(|(old, _)| *old == id) {
            s.borrow_mut().take();
        }
    });
}
pub(crate) fn source_fence() -> Option<u64> {
    SOURCE_FENCE.with(|f| f.get())
}
fn source_changed() {
    clear_reader_display();
    cancel_photo_reads();
    // Original admission/publication gates revoke without Studio, Bridge, Cloud,
    // or controller borrows, including reentrant native-dialog callbacks.
    crate::cloud::app_page_navigation_changed();
    crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::DocumentChanged);
    crate::clip_export_fence::invalidate(
        concat_host::export_publication::PublishRevocation::DocumentChanged,
    );
    SOURCE_FENCE.with(|f| f.set(f.get().and_then(|value| value.checked_add(1))));
    let hook = SOURCE_HOOK.with(|s| s.borrow().as_ref().map(|(_, h)| h.clone()));
    if let Some(hook) = hook {
        hook();
    }
}
pub(crate) fn select_parent_target() {
    let was_photo = is_photo_target();
    TARGET.with(|t| {
        t.borrow_mut().parent();
    });
    if was_photo {
        source_changed();
    }
}
pub(crate) fn invalidate_photo_target() {
    TARGET.with(|t| t.borrow_mut().invalidate());
    source_changed();
}

thread_local! { static READ_CLIENT: RefCell<Option<PhotoWorkerClient>> = const { RefCell::new(None) }; }

/// Current bind only. Clone on the UI thread before passing to another worker.
/// This client prepares parent history metadata only; it grants no Agent access.
pub(crate) fn read_client() -> Option<PhotoWorkerClient> {
    READ_CLIENT.with(|slot| slot.borrow().as_ref().filter(|c| c.available()).cloned())
}

struct ConnectionGuard(Arc<AtomicBool>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
use std::time::Duration;

const UNAVAILABLE: i32 = 0;
const IDLE: i32 = 1;
const OPENING: i32 = 2;
const RUNNING: i32 = 3;
const CLOSING: i32 = 4;
const NEEDS_USER: i32 = 5;
const FAILED: i32 = 6;
const MANUAL_RECOVERY: i32 = 7;

enum Command {
    Open(u64),
    Close(u64),
    PrepareOwnedMetadata(owned_metadata::OwnedJob),
    AgentState(agent_read::StateJob),
}

#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    phase: i32,
    instance: String,
    detail: &'static str,
}

impl Snapshot {
    fn idle() -> Self {
        Self {
            phase: IDLE,
            instance: String::new(),
            detail: "可新建图像或在编辑窗口中打开文件。",
        }
    }
    fn managed(&self) -> bool {
        matches!(
            self.phase,
            OPENING | RUNNING | CLOSING | NEEDS_USER | MANUAL_RECOVERY
        )
    }
    fn publish(&self, app: &App) {
        let ui = app.global::<PhotoWorkspace>();
        ui.set_phase(self.phase);
        ui.set_managed(self.managed());
        ui.set_instance_id(self.instance.clone().into());
        ui.set_detail(self.detail.into());
    }
}

/// Conservative ownership fence while a child is starting, alive or uncertain.
/// This does not claim that the child is the foreground window or a Canvas document.
pub(crate) fn has_instance(app: &App) -> bool {
    app.global::<PhotoWorkspace>().get_managed()
}

/// A stable host instance ID, never an upstream document/session ID.
pub(crate) fn instance_id(app: &App) -> String {
    app.global::<PhotoWorkspace>().get_instance_id().to_string()
}

/// All parent exit paths must call this before hiding the window or quitting.
/// The user completes the editor's own save prompt, then closes SeeCut again.
pub(crate) fn defer_close(app: &App) -> bool {
    if !has_instance(app) {
        return false;
    }
    let ui = app.global::<PhotoWorkspace>();
    if ui.get_phase() == RUNNING {
        ui.invoke_close_editor();
    }
    app.global::<SeeCut>()
        .set_notice("请先在图像编辑窗口完成保存并关闭，再退出 SeeCut。".into());
    true
}

/// Clear local credential projection without borrowing Studio or Bridge.
/// The grant itself remains owned and revoked by the existing Bridge registry.
pub(crate) fn clear_reader_display() {
    crate::host::Shell::with(|_, app| clear_reader_fields(&app));
}
fn clear_reader_fields(app: &App) {
    let ui = app.global::<PhotoWorkspace>();
    ui.set_reader_token("".into());
    ui.set_reader_instance("".into());
    ui.set_reader_can_grant(false);
    ui.set_reader_can_revoke(false);
    ui.set_reader_status("请先选择图像来源，并关联已保存的图像文档。".into());
}

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
fn refresh_reader(app: &App) {
    clear_reader_fields(app);
    if !app.global::<PhotoWorkspace>().get_reader_open() {
        return;
    }
    crate::host::Shell::with(|shell, _| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return;
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return;
        };
        let view = bridge.photo_reader_view(&studio, app);
        let ui = app.global::<PhotoWorkspace>();
        ui.set_reader_client(view.client.into());
        ui.set_reader_instance(view.instance.into());
        ui.set_reader_token(view.token.into());
        ui.set_reader_can_grant(view.can_grant);
        ui.set_reader_can_revoke(view.can_revoke);
        ui.set_reader_status(view.status.into());
    });
}

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
fn bind_reader(app: &App) {
    app.global::<PhotoWorkspace>().on_reader_toggle(|| {
        crate::host::Shell::with(|_, app| {
            let ui = app.global::<PhotoWorkspace>();
            ui.set_reader_open(!ui.get_reader_open());
            refresh_reader(&app);
        });
    });
    app.global::<PhotoWorkspace>()
        .on_reader_client_edited(|text| {
            crate::host::Shell::with(|shell, app| {
                clear_reader_fields(&app);
                if !app.global::<PhotoWorkspace>().get_reader_open() {
                    return;
                }
                {
                    let Ok(studio) = shell.studio.try_borrow() else {
                        return;
                    };
                    let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
                        return;
                    };
                    bridge.client_draft = text.to_string();
                }
                refresh_reader(&app);
            });
        });
    app.global::<PhotoWorkspace>().on_reader_grant(|| {
        crate::host::Shell::with(|shell, app| {
            clear_reader_fields(&app);
            if !app.global::<PhotoWorkspace>().get_reader_open() {
                return;
            }
            {
                let Ok(studio) = shell.studio.try_borrow() else {
                    return;
                };
                let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
                    return;
                };
                let client = app.global::<PhotoWorkspace>().get_reader_client();
                bridge.grant_photo_reader(&studio, &app, client.as_str());
            }
            refresh_reader(&app);
        });
    });
    app.global::<PhotoWorkspace>().on_reader_revoke(|| {
        crate::host::Shell::with(|shell, app| {
            clear_reader_fields(&app);
            if !app.global::<PhotoWorkspace>().get_reader_open() {
                return;
            }
            {
                let Ok(studio) = shell.studio.try_borrow() else {
                    return;
                };
                let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
                    return;
                };
                let client = app.global::<PhotoWorkspace>().get_reader_client();
                bridge.revoke_photo_reader(&studio, &app, client.as_str());
            }
            refresh_reader(&app);
        });
    });
}

/// Hold the returned timer for the lifetime of the application event loop.
pub(crate) fn bind(app: &App) -> slint::Timer {
    let timer = slint::Timer::default();
    READ_CLIENT.with(|slot| {
        slot.borrow_mut().take();
    });
    TARGET.with(|t| *t.borrow_mut() = target::TargetState::default());
    app.global::<SeeCut>()
        .on_photo_source_will_change(select_parent_target);
    // Development is opt-in. Packaged Helpers resolution awaits verified packaging.
    let manifest = std::env::var_os("SEECUT_PHOTO_BUILD_MANIFEST")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if !cfg!(all(
        unix,
        not(any(target_os = "ios", target_os = "android"))
    )) || manifest.is_none()
    {
        app.global::<PhotoWorkspace>().set_phase(UNAVAILABLE);
        return timer;
    }
    let (commands, receiver) = mpsc::sync_channel(8);
    let connected = Arc::new(AtomicBool::new(true));
    let client = PhotoWorkerClient::new(commands.clone(), connected.clone());
    let owned = client.owned.clone();
    let worker_owned = owned.clone();
    let connection_guard = ConnectionGuard(connected);
    let (updates, incoming) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("seecut-photo-workspace".into())
        .spawn(move || {
            worker(
                manifest.expect("checked development manifest"),
                receiver,
                updates,
                connection_guard,
                worker_owned,
            )
        });
    if spawned.is_err() {
        Snapshot {
            phase: UNAVAILABLE,
            instance: String::new(),
            detail: "图像编辑服务无法启动，请重新打开 SeeCut 后重试。",
        }
        .publish(app);
        return timer;
    }
    READ_CLIENT.with(|slot| *slot.borrow_mut() = Some(client));
    Snapshot::idle().publish(app);
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    bind_reader(app);
    app.global::<PhotoWorkspace>().on_return_to_photo({
        let weak = app.as_weak();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let Some(source) = read_client().and_then(|c| c.managed_source()) else {
                return;
            };
            let previous = current_photo_target();
            TARGET.with(|t| {
                t.borrow_mut()
                    .photo(source.bind_id, source.epoch, Some(source.instance));
            });
            if previous != current_photo_target() {
                source_changed();
            }
            app.global::<PhotoWorkspace>()
                .invoke_owned_metadata_requested();
        }
    });
    app.global::<PhotoWorkspace>().on_open_editor({
        let owned = owned.clone();
        let weak = app.as_weak();
        let commands = commands.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<PhotoWorkspace>();
            if !matches!(ui.get_phase(), IDLE | FAILED) {
                return;
            }
            let Some(epoch) = owned.invalidate() else {
                return;
            };
            TARGET.with(|t| {
                t.borrow_mut().photo(owned.bind_id, epoch, None);
            });
            source_changed();
            // Set the ownership fence before enqueueing: duplicate clicks and Agent
            // calls in this event-loop turn cannot observe the previous document.
            Snapshot {
                phase: OPENING,
                instance: String::new(),
                detail: "正在打开图像编辑器…",
            }
            .publish(&app);
            if commands.try_send(Command::Open(epoch)).is_err() {
                Snapshot {
                    phase: FAILED,
                    instance: String::new(),
                    detail: "图像编辑请求未能排队，请稍后重试或重新打开 SeeCut。",
                }
                .publish(&app);
            }
        }
    });
    app.global::<PhotoWorkspace>().on_close_editor({
        let owned = owned.clone();
        let weak = app.as_weak();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<PhotoWorkspace>();
            if !matches!(ui.get_phase(), RUNNING | NEEDS_USER) {
                return;
            }
            let Some(epoch) = owned.invalidate() else {
                return;
            };
            source_changed();
            Snapshot {
                phase: CLOSING,
                instance: instance_id(&app),
                detail: "正在请求关闭，请留意编辑窗口中的保存提示。",
            }
            .publish(&app);
            if commands.try_send(Command::Close(epoch)).is_err() {
                Snapshot {
                    phase: MANUAL_RECOVERY,
                    instance: instance_id(&app),
                    detail: "关闭请求未能排队，请在编辑窗口中保存并关闭。",
                }
                .publish(&app);
            }
        }
    });
    let weak = app.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
            refresh_reader(&app);
            // Drain in order so a completion cannot be replaced by an older update.
            while let Ok(snapshot) = incoming.try_recv() {
                if snapshot.phase == RUNNING && owned.current().is_none() {
                    continue;
                }
                snapshot.publish(&app);
                if snapshot.phase == RUNNING
                    && let Some(source) = owned.current()
                {
                    let selected = TARGET.with(|t| t.borrow_mut().ready(source));
                    if selected {
                        app.global::<PhotoWorkspace>()
                            .invoke_owned_metadata_requested();
                    }
                }
            }
        },
    );
    timer
}

fn worker(
    manifest: PathBuf,
    commands: Receiver<Command>,
    updates: Sender<Snapshot>,
    _connection: ConnectionGuard,
    owned: Arc<owned_metadata::OwnedControl>,
) {
    let mut host = NativeHost::new();
    let mut snapshot = Snapshot::idle();
    let mut metadata = owned_metadata::OwnedStore::default();
    loop {
        let previous = snapshot.clone();
        let mut requested = false;
        match commands.recv_timeout(Duration::from_millis(500)) {
            Ok(Command::Open(epoch)) => {
                requested = true;
                // NativeHost also enforces single-process ownership.
                if !snapshot.managed() {
                    metadata.clear();
                    let options = concat_host::AppDirs::locate().map(|dirs| {
                        let mut options =
                            LaunchOptions::new(manifest.clone(), dirs.data.join("photo-workspace"));
                        options.request_timeout = Duration::from_secs(10);
                        options
                    });
                    match options {
                        Ok(options) => match host.start(options) {
                            Ok(info) => {
                                owned.ready(epoch, info.id);
                                snapshot = Snapshot {
                                    phase: RUNNING,
                                    instance: info.id.to_string(),
                                    detail: "编辑窗口已打开。请在该窗口中编辑、保存和导出。",
                                }
                            }
                            Err(_) => {
                                snapshot = Snapshot {
                                    phase: FAILED,
                                    instance: String::new(),
                                    detail: "图像编辑器未能打开，请重试；若仍失败，请检查组件是否完整。",
                                }
                            }
                        },
                        Err(_) => {
                            snapshot = Snapshot {
                                phase: FAILED,
                                instance: String::new(),
                                detail: "无法准备图像编辑器的本地数据，请检查目录权限后重试。",
                            }
                        }
                    }
                }
            }
            Ok(Command::Close(epoch)) => {
                requested = true;
                metadata.clear();
                if let Ok(Some(info)) = host.current() {
                    if info.status == InstanceStatus::Exited {
                        snapshot = Snapshot::idle();
                    } else if matches!(snapshot.phase, RUNNING | NEEDS_USER) {
                        snapshot = match host.close(info.id, Duration::from_secs(1)) {
                            Ok(CloseOutcome::Exited) => Snapshot::idle(),
                            Ok(CloseOutcome::NeedsUser { .. }) => {
                                // A live owned child can retain historical file identity
                                // while its dialog is open. This confers no read/edit grant.
                                if let Ok(Some(current)) = host.current()
                                    && current.id == info.id
                                    && current.ready
                                    && current.status == InstanceStatus::Ready
                                {
                                    owned.ready(epoch, current.id);
                                }
                                Snapshot {
                                    phase: NEEDS_USER,
                                    instance: info.id.to_string(),
                                    detail: "编辑窗口尚未关闭。请在该窗口中完成保存或取消关闭。",
                                }
                            }
                            Err(_) => Snapshot {
                                phase: MANUAL_RECOVERY,
                                instance: info.id.to_string(),
                                detail: "关闭结果尚未确认，请在编辑窗口中保存并关闭。",
                            },
                        };
                    }
                } else {
                    snapshot.phase = MANUAL_RECOVERY;
                    snapshot.detail = "无法确认编辑窗口状态，请在该窗口中保存并关闭。";
                }
            }
            Ok(Command::AgentState(job)) => {
                job.run(&mut host, &owned);
            }
            Ok(Command::PrepareOwnedMetadata(job)) => {
                metadata.run(&mut host, &owned, job);
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        match host.current() {
            Ok(Some(info)) if info.status == InstanceStatus::Exited => {
                // A failed attempt that already exited keeps its retry explanation.
                if snapshot.managed() {
                    snapshot = Snapshot::idle();
                }
            }
            Ok(Some(info)) => {
                snapshot.instance = info.id.to_string();
                if info.status == InstanceStatus::StartupFailed {
                    snapshot.phase = MANUAL_RECOVERY;
                    snapshot.detail =
                        "编辑窗口已启动，但连接未完成。请在该窗口中保存并关闭后重试。";
                }
            }
            Err(_) => {
                snapshot.phase = MANUAL_RECOVERY;
                snapshot.detail = "无法确认编辑窗口状态，请在该窗口中保存并关闭。";
            }
            Ok(None) => {}
        }
        if !matches!(snapshot.phase, RUNNING | NEEDS_USER) && owned.current().is_some() {
            owned.invalidate();
        }
        metadata.prune(&owned);
        if (requested || snapshot != previous) && updates.send(snapshot.clone()).is_err() {
            break;
        }
    }
}
