// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI glue for one SeeCut-owned complete image editor. Blocking host operations
//! stay on a serial worker; the UI receives lifecycle facts, never control secrets.

use crate::ui::{App, PhotoWorkspace, SeeCut};
use seecut_photo_host::{CloseOutcome, InstanceStatus, LaunchOptions, NativeHost};
use slint::ComponentHandle;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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
    Open,
    Close,
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

/// Hold the returned timer for the lifetime of the application event loop.
pub(crate) fn bind(app: &App) -> slint::Timer {
    let timer = slint::Timer::default();
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
    let (commands, receiver) = mpsc::channel();
    let (updates, incoming) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("seecut-photo-workspace".into())
        .spawn(move || {
            worker(
                manifest.expect("checked development manifest"),
                receiver,
                updates,
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
    Snapshot::idle().publish(app);
    app.global::<PhotoWorkspace>().on_open_editor({
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
            // Set the ownership fence before enqueueing: duplicate clicks and Agent
            // calls in this event-loop turn cannot observe the previous document.
            Snapshot {
                phase: OPENING,
                instance: String::new(),
                detail: "正在打开图像编辑器…",
            }
            .publish(&app);
            if commands.send(Command::Open).is_err() {
                Snapshot {
                    phase: FAILED,
                    instance: String::new(),
                    detail: "图像编辑服务已停止，请重新打开 SeeCut。",
                }
                .publish(&app);
            }
        }
    });
    app.global::<PhotoWorkspace>().on_close_editor({
        let weak = app.as_weak();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<PhotoWorkspace>();
            if !matches!(ui.get_phase(), RUNNING | NEEDS_USER) {
                return;
            }
            Snapshot {
                phase: CLOSING,
                instance: instance_id(&app),
                detail: "正在请求关闭，请留意编辑窗口中的保存提示。",
            }
            .publish(&app);
            if commands.send(Command::Close).is_err() {
                Snapshot {
                    phase: MANUAL_RECOVERY,
                    instance: instance_id(&app),
                    detail: "暂时无法连接编辑窗口，请在该窗口中保存并关闭。",
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
            // Drain in order so a completion cannot be replaced by an older update.
            while let Ok(snapshot) = incoming.try_recv() {
                snapshot.publish(&app);
            }
        },
    );
    timer
}

fn worker(manifest: PathBuf, commands: Receiver<Command>, updates: Sender<Snapshot>) {
    let mut host = NativeHost::new();
    let mut snapshot = Snapshot::idle();
    loop {
        let previous = snapshot.clone();
        let mut requested = false;
        match commands.recv_timeout(Duration::from_millis(500)) {
            Ok(Command::Open) => {
                requested = true;
                // NativeHost also enforces single-process ownership.
                if !snapshot.managed() {
                    let options = concat_host::AppDirs::locate().map(|dirs| {
                        let mut options =
                            LaunchOptions::new(manifest.clone(), dirs.data.join("photo-workspace"));
                        options.request_timeout = Duration::from_secs(10);
                        options
                    });
                    match options {
                        Ok(options) => match host.start(options) {
                            Ok(info) => {
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
            Ok(Command::Close) => {
                requested = true;
                if let Ok(Some(info)) = host.current() {
                    if info.status == InstanceStatus::Exited {
                        snapshot = Snapshot::idle();
                    } else if matches!(snapshot.phase, RUNNING | NEEDS_USER) {
                        snapshot = match host.close(info.id, Duration::from_secs(1)) {
                            Ok(CloseOutcome::Exited) => Snapshot::idle(),
                            Ok(CloseOutcome::NeedsUser { .. }) => Snapshot {
                                phase: NEEDS_USER,
                                instance: info.id.to_string(),
                                detail: "编辑窗口尚未关闭。请在该窗口中完成保存或取消关闭。",
                            },
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
        if (requested || snapshot != previous) && updates.send(snapshot.clone()).is_err() {
            break;
        }
    }
}
