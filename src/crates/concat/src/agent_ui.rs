// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native assistant host wiring. No connection, grant or model request is automatic.

use crate::ui::{App, Assistant};
use slint::ComponentHandle;
use std::rc::Rc;

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
pub(crate) use desktop::Binding;

/// Bind once after Shell::install and keep the returned guard until app.run returns.
pub(crate) fn bind(app: &App) -> Rc<Binding> {
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    {
        desktop::bind(app)
    }
    #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
    {
        let ui = app.global::<Assistant>();
        ui.set_enabled(false);
        ui.set_open(false);
        ui.set_can_select_files(false);
        ui.set_connection("此平台暂不支持本地助手".into());
        ui.on_close_requested({
            let app = app.as_weak();
            move || {
                if let Some(app) = app.upgrade() {
                    let ui = app.global::<Assistant>();
                    ui.set_open(false);
                    ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
                }
            }
        });
        Rc::new(Binding)
    }
}

#[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
pub(crate) struct Binding;

#[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
impl Binding {
    /// No local process exists on unsupported platforms.
    pub(crate) fn shutdown(&self) {}
}

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
mod desktop {
    use super::*;
    use crate::agent_controller::{
        ConnectionConfig, Controller, DocumentIdentity, Phase, TaskOutcome,
    };
    use crate::agent_process::LaunchSpec;
    use crate::editor_mcp::{
        ASSISTANT_CLIENT_ID, AssistantPermission, AssistantPermissionError, AssistantSnapshot,
    };
    use crate::host::{Shell, spawn_detached};
    use crate::ui::AssistantMessage;
    use serde_json::json;
    use slint::{Model, ModelRc, Timer, TimerMode, VecModel};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::time::Duration;

    const MAX_ROWS: usize = 32;
    const MAX_HISTORY_BYTES: usize = 256 * 1024;

    // Never Debug/serialize this: comparison includes real lease tokens.
    #[derive(Clone, PartialEq, Eq)]
    struct Lease {
        document: Option<DocumentIdentity>,
        read: Option<String>,
        write: Option<String>,
    }
    impl Lease {
        fn from_snapshot(snapshot: &AssistantSnapshot) -> Self {
            Self {
                document: snapshot.document.clone(),
                read: snapshot.read_token.clone(),
                write: snapshot.write_token.clone(),
            }
        }
    }
    #[derive(Clone, Copy)]
    enum FileKind {
        Runtime,
        Entrypoint,
        McpClient,
    }
    enum WorkerResult {
        File {
            epoch: u64,
            kind: FileKind,
            path: Option<Result<PathBuf, &'static str>>,
        },
        Prepared {
            epoch: u64,
            config: Result<Box<ConnectionConfig>, &'static str>,
        },
    }

    // The bounded channel holds at most four results. Drop stale configurations
    // (including their keys) even after close invalidated their epoch/flags.
    fn discard_closed_worker_results<T>(receiver: &Receiver<T>) {
        for _ in 0..4 {
            let Ok(result) = receiver.try_recv() else {
                break;
            };
            drop(result);
        }
    }
    #[derive(Clone, PartialEq)]
    struct FrozenApproval {
        id: String,
        turn_id: String,
        object_id: Option<String>,
        delta_x: Option<f64>,
        delta_y: Option<f64>,
    }
    struct State {
        controller: Controller,
        lease: Option<Lease>,
        runtime: Option<PathBuf>,
        entrypoint: Option<PathBuf>,
        mcp_client: Option<PathBuf>,
        selecting: bool,
        preparing: bool,
        epoch: u64,
        notice: String,
        provider_label: String,
        assistant_row: Option<usize>,
        approval: Option<FrozenApproval>,
    }
    /// Owns the controller, stable transcript model and nonblocking UI timer.
    pub(crate) struct Binding {
        timer: Timer,
        state: RefCell<State>,
        messages: Rc<VecModel<AssistantMessage>>,
        sender: SyncSender<WorkerResult>,
        receiver: Receiver<WorkerResult>,
    }
    impl Drop for Binding {
        fn drop(&mut self) {
            self.timer.stop();
            self.state.get_mut().controller.shutdown_now();
        }
    }
    impl Binding {
        /// Start bounded process shutdown before the app's exit-only reaping hook.
        pub(crate) fn shutdown(&self) {
            self.state.borrow_mut().controller.shutdown_now();
        }

        fn snapshot(app: &App) -> Option<AssistantSnapshot> {
            let mut result = None;
            Shell::with(|shell, _| {
                let studio = shell.studio.borrow();
                // Bridge and studio borrows end before any setter or callback runs.
                result = Some(
                    studio
                        .editor_mcp
                        .borrow_mut()
                        .assistant_snapshot(&studio, app),
                );
            });
            result
        }
        fn refresh(&self, app: &App) {
            if !app.global::<Assistant>().get_open() {
                discard_closed_worker_results(&self.receiver);
                // close already disconnected. Reap responses/process exit
                // without reading Studio, publishing UI, or clearing unknown.
                self.state.borrow_mut().controller.tick(None);
                return;
            }
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            if self.sync_lease(&snapshot) {
                app.global::<Assistant>().set_draft("".into());
            }
            {
                let mut state = self.state.borrow_mut();
                state.controller.tick(snapshot.document.as_ref());
            }
            for _ in 0..4 {
                let Ok(result) = self.receiver.try_recv() else {
                    break;
                };
                let mut state = self.state.borrow_mut();
                match result {
                    WorkerResult::File { epoch, kind, path } if state.epoch == epoch => {
                        state.selecting = false;
                        if let Some(path) = path {
                            match path {
                                Ok(path)
                                    if state.controller.view().can_connect && !state.preparing =>
                                {
                                    match kind {
                                        FileKind::Runtime => state.runtime = Some(path),
                                        FileKind::Entrypoint => state.entrypoint = Some(path),
                                        FileKind::McpClient => state.mcp_client = Some(path),
                                    }
                                    state.notice.clear();
                                }
                                Err(message) => state.notice = message.into(),
                                _ => {}
                            }
                        }
                    }
                    WorkerResult::Prepared { epoch, config } if state.epoch == epoch => {
                        state.preparing = false;
                        match config {
                            Ok(config)
                                if app.global::<Assistant>().get_open()
                                    && snapshot.document.as_ref() == Some(&config.document)
                                    && snapshot.read_token.as_deref()
                                        == Some(config.read_token.as_str())
                                    && snapshot.write_token == config.write_token =>
                            {
                                state.notice.clear();
                                if let Err(error) = state.controller.connect(*config) {
                                    state.notice = error.message().into();
                                }
                            }
                            Err(message) => state.notice = message.into(),
                            _ => state.notice = "工程或权限已变化，请重新连接".into(),
                        }
                    }
                    _ => {}
                }
            }
            self.publish(app, &snapshot);
        }
        fn sync_lease(&self, snapshot: &AssistantSnapshot) -> bool {
            let next = Lease::from_snapshot(snapshot);
            let mut state = self.state.borrow_mut();
            let mut clear_draft = false;
            if let Some(previous) = &state.lease
                && previous != &next
            {
                let document_changed = previous.document != next.document;
                state.controller.disconnect();
                state.preparing = false;
                state.selecting = false;
                state.epoch = state.epoch.wrapping_add(1);
                state.approval = None;
                state.notice = if document_changed {
                    "工程已变化，请重新连接"
                } else {
                    "权限已变化，请重新连接"
                }
                .into();
                if document_changed {
                    clear_draft = true;
                    state.assistant_row = None;
                    while self.messages.row_count() != 0 {
                        self.messages.remove(0);
                    }
                }
            }
            state.lease = Some(next);
            clear_draft
        }
        fn publish(&self, app: &App, snapshot: &AssistantSnapshot) {
            let mut state = self.state.borrow_mut();
            let view = state.controller.view();
            let phase = view.phase;
            let unknown =
                view.outcome == Some(TaskOutcome::OutcomeUnknown) || phase == Phase::OutcomeUnknown;
            let busy = state.preparing
                || matches!(
                    phase,
                    Phase::Initializing
                        | Phase::OpeningSession
                        | Phase::Running
                        | Phase::AwaitingApproval
                        | Phase::Stopping
                        | Phase::Closing
                )
                || (phase == Phase::Ready && !view.can_send);
            let outcome = outcome_label(view.outcome);
            let status = if unknown {
                "操作结果待核实"
            } else if !state.notice.is_empty() {
                &state.notice
            } else if let Some(error) = view.error {
                error.message()
            } else if !outcome.is_empty() {
                outcome
            } else {
                match phase {
                    Phase::Running => "正在处理",
                    Phase::AwaitingApproval => "等待你确认移动操作",
                    Phase::Stopping => "正在停止，等待结果",
                    Phase::Ready if busy => "已提交，等待助手回应",
                    Phase::Ready => "可以发送问题",
                    _ => "",
                }
            };
            let ui = app.global::<Assistant>();
            ui.set_current_project(snapshot.project_name.as_str().into());
            ui.set_provider_label(state.provider_label.as_str().into());
            ui.set_connection(
                match phase {
                    Phase::Disconnected => "尚未连接",
                    Phase::Initializing | Phase::OpeningSession => "正在初始化助手",
                    Phase::Closing => "正在断开助手",
                    Phase::Failed => "连接失败，请检查配置后显式重连",
                    Phase::OutcomeUnknown => "助手已暂停",
                    _ => "助手已连接",
                }
                .into(),
            );
            ui.set_status(status.into());
            ui.set_details("".into());
            ui.set_busy(busy);
            ui.set_unknown(unknown);
            ui.set_can_send(view.can_send && !state.preparing && snapshot.read_token.is_some());
            ui.set_can_stop(view.can_stop);
            let connected = matches!(
                phase,
                Phase::Ready | Phase::Running | Phase::AwaitingApproval | Phase::Stopping
            );
            ui.set_connected(connected);
            ui.set_connecting(
                state.preparing
                    || matches!(
                        phase,
                        Phase::Initializing | Phase::OpeningSession | Phase::Closing
                    ),
            );
            ui.set_can_disconnect(connected && !busy);
            ui.set_connect_label(if state.preparing || matches!(phase, Phase::Initializing | Phase::OpeningSession) { "正在连接…" } else if phase == Phase::Closing { "正在断开…" } else if connected { "断开" } else { "连接" }.into());
            let reason = if unknown {
                "操作结果待核实，连接已锁定"
            } else if snapshot.document.is_none() {
                "请先打开工程"
            } else if snapshot.read_token.is_none() {
                "请先允许读取与预览"
            } else if state.selecting {
                "请先完成文件选择"
            } else if state.runtime.is_none() {
                "请选择运行环境文件"
            } else if state.entrypoint.is_none() {
                "请选择助手入口文件"
            } else if state.mcp_client.is_none() {
                "请选择 MCP 客户端文件"
            } else if busy {
                "请求正在处理，请稍后操作"
            } else {
                ""
            };
            ui.set_can_connect(view.can_connect && reason.is_empty());
            ui.set_connect_disabled_reason(reason.into());
            ui.set_send_disabled_reason(
                if unknown {
                    "核实操作结果前已暂停后续请求"
                } else if busy {
                    "请求正在处理，可等待或停止"
                } else if snapshot.read_token.is_none() {
                    "请先允许读取与预览并连接助手"
                } else if !view.can_send {
                    "请先连接助手"
                } else {
                    ""
                }
                .into(),
            );
            ui.set_read_granted(snapshot.read_token.is_some());
            ui.set_write_granted(snapshot.write_token.is_some());
            ui.set_read_permission_status(snapshot.read_status.as_str().into());
            ui.set_write_permission_status(snapshot.write_status.as_str().into());
            ui.set_can_grant_read(snapshot.can_grant_read && !busy);
            ui.set_can_revoke_read(snapshot.can_revoke_read);
            ui.set_can_grant_write(snapshot.can_grant_write && !busy);
            ui.set_can_renew_write(snapshot.can_renew_write && !busy);
            ui.set_can_revoke_write(snapshot.can_revoke_write);
            ui.set_runtime_filename(filename(state.runtime.as_deref()).into());
            ui.set_entrypoint_filename(filename(state.entrypoint.as_deref()).into());
            ui.set_mcp_client_filename(filename(state.mcp_client.as_deref()).into());
            ui.set_can_select_files(view.can_connect && !busy && !state.selecting && !unknown);
            let approval = view.approval;
            let description = approval
                .map(|a| match (&a.object_id, a.delta_x, a.delta_y) {
                    (Some(object), Some(dx), Some(dy)) if dx.is_finite() && dy.is_finite() => {
                        format!("对象：{object}\n水平移动：{dx} px\n垂直移动：{dy} px")
                    }
                    _ => "移动参数不完整，无法允许此操作".into(),
                })
                .unwrap_or_default();
            let valid_approval = approval.is_some_and(|a| {
                a.object_id.is_some()
                    && a.delta_x.is_some_and(f64::is_finite)
                    && a.delta_y.is_some_and(f64::is_finite)
            });
            let frozen_approval = approval.map(|a| FrozenApproval {
                id: a.id.clone(),
                turn_id: a.turn_id.clone(),
                object_id: a.object_id.clone(),
                delta_x: a.delta_x,
                delta_y: a.delta_y,
            });
            ui.set_approval_visible(approval.is_some());
            ui.set_approval_title("移动画布对象".into());
            ui.set_approval_description(description.into());
            ui.set_can_allow(view.can_approve && valid_approval);
            ui.set_can_reject(view.can_approve);
            let next_row = update_streaming_reply(
                &self.messages,
                state.assistant_row,
                phase,
                view.outcome,
                view.text,
            );
            state.assistant_row = next_row;
            state.approval = frozen_approval;
            self.trim(&mut state);
        }
        fn trim(&self, state: &mut State) {
            let bytes = || {
                self.messages
                    .iter()
                    .map(|row| row.role.len() + row.text.len() + row.state.len())
                    .sum::<usize>()
            };
            while self.messages.row_count() > MAX_ROWS || bytes() > MAX_HISTORY_BYTES {
                self.messages.remove(0);
                state.assistant_row = state.assistant_row.and_then(|index| index.checked_sub(1));
            }
        }
        fn send(&self, app: &App, text: &str) {
            self.refresh(app);
            if !app.global::<Assistant>().get_open() {
                return;
            }
            let mut state = self.state.borrow_mut();
            if state.preparing || state.selecting {
                return;
            }
            match state.controller.send_text(text) {
                Ok(()) => {
                    let mut visible = text.to_owned();
                    if let Some(lease) = &state.lease {
                        for token in [lease.read.as_ref(), lease.write.as_ref()]
                            .into_iter()
                            .flatten()
                        {
                            visible = visible.replace(token, "[已隐藏]");
                        }
                    }
                    self.messages.push(AssistantMessage {
                        role: "你".into(),
                        text: visible.into(),
                        state: "".into(),
                    });
                    self.messages.push(AssistantMessage {
                        role: "助手".into(),
                        text: "".into(),
                        state: "".into(),
                    });
                    state.assistant_row = Some(self.messages.row_count() - 1);
                    state.notice.clear();
                    self.trim(&mut state);
                    // Enqueue acceptance immediately locks submission, before any timer tick.
                    let ui = app.global::<Assistant>();
                    ui.set_draft("".into());
                    ui.set_busy(true);
                    ui.set_can_send(false);
                }
                Err(error) => state.notice = error.message().into(),
            }
            drop(state);
            self.refresh(app);
        }
        fn close(&self, app: &App) {
            {
                let mut state = self.state.borrow_mut();
                state.epoch = state.epoch.wrapping_add(1);
                state.selecting = false;
                state.preparing = false;
                state.approval = None;
                state.controller.disconnect();
            }
            let ui = app.global::<Assistant>();
            ui.set_open(false);
            ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
            // App may call close while Studio::publish still holds its borrow.
            // Subsequent closed ticks only discard stale worker results and
            // pump shutdown; closing itself never borrows Studio.
        }
        fn disconnect(&self, app: &App) {
            {
                let mut state = self.state.borrow_mut();
                if !app.global::<Assistant>().get_can_disconnect() {
                    return;
                }
                state.controller.disconnect();
                state.epoch = state.epoch.wrapping_add(1);
                state.notice.clear();
            }
            let ui = app.global::<Assistant>();
            ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
            self.refresh(app);
        }
        fn permission(&self, app: &App, action: AssistantPermission) {
            self.refresh(app);
            let ui = app.global::<Assistant>();
            let allowed = match action {
                AssistantPermission::GrantRead => ui.get_can_grant_read() && !ui.get_busy(),
                AssistantPermission::RevokeRead => ui.get_can_revoke_read(),
                AssistantPermission::GrantWrite => ui.get_can_grant_write() && !ui.get_busy(),
                AssistantPermission::RenewWrite => ui.get_can_renew_write() && !ui.get_busy(),
                AssistantPermission::RevokeWrite => ui.get_can_revoke_write(),
            };
            if !ui.get_open() {
                return;
            }
            if !allowed {
                self.state.borrow_mut().notice =
                    AssistantPermissionError::NotAllowed.message().into();
                self.refresh(app);
                return;
            }
            let mut result = Err(AssistantPermissionError::NotAllowed);
            Shell::with(|shell, _| {
                let studio = shell.studio.borrow();
                result = studio
                    .editor_mcp
                    .borrow_mut()
                    .assistant_permission(&studio, app, action);
            });
            self.state.borrow_mut().notice = result
                .err()
                .map(|error| error.message())
                .unwrap_or_default()
                .into();
            self.refresh(app);
        }
        fn approve(&self, app: &App, allowed: bool) {
            // Freeze the target the user actually saw before draining new IPC.
            let frozen = self.state.borrow().approval.clone();
            self.refresh(app);
            let ui = app.global::<Assistant>();
            if !ui.get_open()
                || !(if allowed {
                    ui.get_can_allow()
                } else {
                    ui.get_can_reject()
                })
            {
                return;
            }
            let mut state = self.state.borrow_mut();
            if frozen != state.approval {
                state.notice = "待确认操作已变化，请检查当前对象和位移".into();
            } else if let Some(approval) = frozen
                && let Err(error) = state.controller.approve(&approval.id, allowed)
            {
                state.notice = error.message().into();
            }
            drop(state);
            self.refresh(app);
        }
        fn choose(&self, app: &App, kind: FileKind) {
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || !app.global::<Assistant>().get_can_select_files()
            {
                return;
            }
            let epoch = {
                let mut state = self.state.borrow_mut();
                state.selecting = true;
                state.epoch = state.epoch.wrapping_add(1);
                state.epoch
            };
            let sender = self.sender.clone();
            spawn_detached(move || {
                let title = match kind {
                    FileKind::Runtime => "选择 Node 运行环境文件",
                    FileKind::Entrypoint => "选择助手入口文件",
                    FileKind::McpClient => "选择 MCP 客户端文件",
                };
                // The shared async wrapper skips its callback on desktop cancel.
                // This worker always reports completion, including cancellation.
                let paths = crate::platform::pick_files(title, None);
                let path = match paths.as_deref().unwrap_or_default() {
                    [] => None,
                    [path] => Some(ordinary_file(path).map(|()| path.clone())),
                    _ => Some(Err("请只选择一个普通文件")),
                };
                let _ = sender.send(WorkerResult::File { epoch, kind, path });
            });
            self.refresh(app);
        }
        fn connect(&self, app: &App, base: &str, model: &str, key: &str, images: bool) {
            self.refresh(app);
            let ui = app.global::<Assistant>();
            if !ui.get_open() || !ui.get_can_connect() {
                return;
            }
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            let validated = provider_url(base).and_then(|base| {
                if model.trim().is_empty()
                    || model.len() > 256
                    || key.trim().is_empty()
                    || key.len() > 16 * 1024
                {
                    Err("请填写有效的模型名称和本次会话密钥")
                } else {
                    Ok(base)
                }
            });
            let base = match validated {
                Ok(base) => base,
                Err(message) => {
                    self.state.borrow_mut().notice = message.into();
                    self.refresh(app);
                    return;
                }
            };
            let Some(document) = snapshot.document else {
                return;
            };
            let Some(read_token) = snapshot.read_token else {
                return;
            };
            let mut data = None;
            Shell::with(|shell, _| data = Some(shell.studio.borrow().host.dirs.data.clone()));
            let Some(data) = data else { return };
            let (epoch, runtime, entrypoint, mcp_client) = {
                let mut state = self.state.borrow_mut();
                let (Some(runtime), Some(entrypoint), Some(client)) =
                    (&state.runtime, &state.entrypoint, &state.mcp_client)
                else {
                    return;
                };
                let files = (runtime.clone(), entrypoint.clone(), client.clone());
                state.epoch = state.epoch.wrapping_add(1);
                state.preparing = true;
                state.notice.clear();
                state.provider_label = model.trim().to_owned();
                (state.epoch, files.0, files.1, files.2)
            };
            // Only same-App necessary environment, never ambient credential discovery.
            let environment = app_environment();
            let provider = json!({"protocol":"openai-chat","baseURL":base,"model":model.trim(),"apiKey":key,"images":images});
            let sender = self.sender.clone();
            spawn_detached(move || {
                let config = (|| {
                    ordinary_file(&runtime)?;
                    ordinary_file(&entrypoint)?;
                    ordinary_file(&mcp_client)?;
                    let environment = environment?;
                    let mcp_environment = environment
                        .iter()
                        .filter(|(name, _)| matches!(name.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR"))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect();
                    let session_directory = session_directory(&data)?;
                    Ok(ConnectionConfig {
                        launch: LaunchSpec {
                            executable: runtime,
                            entrypoint,
                            environment,
                        },
                        document,
                        client_id: ASSISTANT_CLIENT_ID.into(),
                        mcp_client_path: mcp_client,
                        mcp_environment,
                        session_directory,
                        provider,
                        read_token,
                        write_token: snapshot.write_token,
                    })
                })();
                let _ = sender.send(WorkerResult::Prepared {
                    epoch,
                    config: config.map(Box::new),
                });
            });
            self.refresh(app);
        }
    }

    pub(super) fn bind(app: &App) -> Rc<Binding> {
        let (sender, receiver) = sync_channel(4);
        let binding = Rc::new(Binding {
            timer: Timer::default(),
            state: RefCell::new(State {
                controller: Controller::default(),
                lease: None,
                runtime: None,
                entrypoint: None,
                mcp_client: None,
                selecting: false,
                preparing: false,
                epoch: 0,
                notice: String::new(),
                provider_label: String::new(),
                assistant_row: None,
                approval: None,
            }),
            messages: Rc::new(VecModel::default()),
            sender,
            receiver,
        });
        let ui = app.global::<Assistant>();
        ui.set_enabled(true);
        ui.set_messages(ModelRc::from(binding.messages.clone()));
        macro_rules! callback {
            ($name:ident, $method:ident $(, $argument:expr)*) => {
                ui.$name({
                    let binding = Rc::downgrade(&binding);
                    let app = app.as_weak();
                    move || if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) { binding.$method(&app $(, $argument)*); }
                });
            };
        }
        callback!(on_close_requested, close);
        callback!(on_disconnect, disconnect);
        callback!(on_grant_read, permission, AssistantPermission::GrantRead);
        callback!(on_revoke_read, permission, AssistantPermission::RevokeRead);
        callback!(on_grant_write, permission, AssistantPermission::GrantWrite);
        callback!(on_renew_write, permission, AssistantPermission::RenewWrite);
        callback!(
            on_revoke_write,
            permission,
            AssistantPermission::RevokeWrite
        );
        callback!(on_allow, approve, true);
        callback!(on_reject, approve, false);
        callback!(on_choose_runtime, choose, FileKind::Runtime);
        callback!(on_choose_entrypoint, choose, FileKind::Entrypoint);
        callback!(on_choose_mcp_client, choose, FileKind::McpClient);
        ui.on_send({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |text| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.send(&app, &text);
                }
            }
        });
        ui.on_connect({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |base, model, key, images| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.connect(&app, &base, &model, &key, images);
                }
            }
        });
        ui.on_stop({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move || {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.refresh(&app);
                    if app.global::<Assistant>().get_open() {
                        let mut state = binding.state.borrow_mut();
                        if let Err(error) = state.controller.stop() {
                            state.notice = error.message().into();
                        }
                        drop(state);
                        binding.refresh(&app);
                    }
                }
            }
        });
        binding.refresh(app);
        binding
            .timer
            .start(TimerMode::Repeated, Duration::from_millis(40), {
                let binding = Rc::downgrade(&binding);
                let app = app.as_weak();
                move || {
                    if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                        binding.refresh(&app);
                    }
                }
            });
        binding
    }

    fn outcome_label(outcome: Option<TaskOutcome>) -> &'static str {
        match outcome {
            Some(TaskOutcome::Completed) => "已完成",
            Some(TaskOutcome::SubmittedPendingVerification) => "已提交，待验证",
            Some(TaskOutcome::Failed) => "处理失败",
            Some(TaskOutcome::Stopped) => "已停止",
            Some(TaskOutcome::OutcomeUnknown) => "操作结果待核实",
            None => "",
        }
    }

    // Once a reply is terminal, the host detaches its streaming row. Failed or
    // disconnected transport cannot replace retained text with a reset buffer.
    // Closing is not final: process exit can still establish an unknown result.
    fn reply_row_update(
        phase: Phase,
        outcome: Option<TaskOutcome>,
        text: &str,
    ) -> (Option<&str>, &'static str, bool) {
        if outcome.is_some() {
            return (Some(text), outcome_label(outcome), true);
        }
        match phase {
            Phase::Failed => (None, "连接失败", true),
            Phase::Disconnected => (None, "已断开", true),
            Phase::OutcomeUnknown => (Some(text), "操作结果待核实", true),
            _ => (Some(text), "", false),
        }
    }

    fn update_streaming_reply(
        messages: &VecModel<AssistantMessage>,
        index: Option<usize>,
        phase: Phase,
        outcome: Option<TaskOutcome>,
        text: &str,
    ) -> Option<usize> {
        let index = index?;
        let mut row = messages.row_data(index)?;
        let (reply_text, reply_state, terminal) = reply_row_update(phase, outcome, text);
        if reply_text.is_some_and(|text| row.text.as_str() != text)
            || row.state.as_str() != reply_state
        {
            if let Some(text) = reply_text {
                row.text = text.into();
            }
            row.state = reply_state.into();
            messages.set_row_data(index, row);
        }
        if terminal { None } else { Some(index) }
    }
    fn filename(path: Option<&Path>) -> String {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    fn provider_url(raw: &str) -> Result<String, &'static str> {
        let raw = raw.trim();
        let url = url::Url::parse(raw).map_err(|_| "服务地址格式无效")?;
        let has_userinfo = raw.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        });
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || has_userinfo
        {
            return Err("服务地址需为 HTTP(S)，且不能包含账号、密码或片段");
        }
        Ok(url.into())
    }
    fn ordinary_file(path: &Path) -> Result<(), &'static str> {
        if !path.is_absolute() {
            return Err("请选择绝对路径的普通文件");
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "所选文件无法读取")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("请选择普通文件，不能使用软链接或目录");
        }
        Ok(())
    }
    fn app_environment() -> Result<BTreeMap<String, String>, &'static str> {
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        let temporary = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let temporary = temporary.to_str().ok_or("App 临时目录编码无效")?;
        values.insert("TMPDIR".into(), temporary.into());
        for name in ["XDG_RUNTIME_DIR", "LANG", "LC_ALL", "TZ"] {
            if let Some(value) = std::env::var_os(name) {
                values.insert(
                    name.into(),
                    value.into_string().map_err(|_| "App 环境值编码无效")?,
                );
            }
        }
        for (name, value) in &values {
            if value.contains('\0') || value.len() > 4096 {
                return Err("App 环境值无效");
            }
            if matches!(name.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR")
                && (!Path::new(value).is_absolute() || !Path::new(value).is_dir())
            {
                return Err("App 临时目录或运行目录不可用");
            }
        }
        Ok(values)
    }
    fn session_directory(data: &Path) -> Result<PathBuf, &'static str> {
        if !data.is_absolute() {
            return Err("App 数据目录必须是绝对路径");
        }
        let mut path = PathBuf::new();
        for component in data.components() {
            if matches!(component, Component::ParentDir | Component::CurDir) {
                return Err("App 数据目录路径无效");
            }
            path.push(component);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Each parent was checked before creating the next component.
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&path)
                        .map_err(|_| "无法创建 App 数据目录")?;
                    std::fs::symlink_metadata(&path).map_err(|_| "App 数据目录不可用")?
                }
                Err(_) => return Err("App 数据目录不可用"),
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("App 数据目录不能经过软链接");
            }
        }
        let directory = data.join("assistant-sessions");
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .map_err(|_| "无法创建助手会话目录")?;
            }
            Err(_) => return Err("助手会话目录不可用"),
        }
        let metadata = std::fs::symlink_metadata(&directory).map_err(|_| "助手会话目录不可用")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err("助手会话目录必须由当前用户拥有、权限为 0700，且不能是软链接");
        }
        Ok(directory)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicU64, Ordering};

        #[test]
        fn closed_ticks_discard_queued_and_late_credential_results() {
            use std::cell::Cell;
            struct CredentialResult {
                secret: String,
                dropped: Rc<Cell<usize>>,
            }
            impl Drop for CredentialResult {
                fn drop(&mut self) {
                    assert!(!self.secret.is_empty());
                    self.dropped.set(self.dropped.get() + 1);
                }
            }
            let dropped = Rc::new(Cell::new(0));
            let (sender, receiver) = sync_channel(4);
            for _ in 0..4 {
                assert!(
                    sender
                        .send(CredentialResult {
                            secret: "fixture-key".into(),
                            dropped: dropped.clone(),
                        })
                        .is_ok()
                );
            }
            discard_closed_worker_results(&receiver);
            assert_eq!(dropped.get(), 4);
            assert!(receiver.try_recv().is_err());
            // A cancelled preparation can report after close cleared its flags.
            assert!(
                sender
                    .send(CredentialResult {
                        secret: "late-fixture-key".into(),
                        dropped: dropped.clone(),
                    })
                    .is_ok()
            );
            discard_closed_worker_results(&receiver);
            assert_eq!(dropped.get(), 5);
            assert!(receiver.try_recv().is_err());
            discard_closed_worker_results(&receiver);
            assert_eq!(dropped.get(), 5);
        }

        #[test]
        fn terminal_replies_detach_and_transport_failure_preserves_text() {
            for outcome in [
                TaskOutcome::Completed,
                TaskOutcome::SubmittedPendingVerification,
                TaskOutcome::Failed,
                TaskOutcome::Stopped,
                TaskOutcome::OutcomeUnknown,
            ] {
                let (text, state, terminal) =
                    reply_row_update(Phase::Ready, Some(outcome), "最终回复");
                assert_eq!(text, Some("最终回复"));
                assert!(!state.is_empty() && terminal);
            }
            for phase in [Phase::Failed, Phase::Disconnected] {
                let (text, state, terminal) = reply_row_update(phase, None, "");
                assert!(
                    text.is_none(),
                    "transport state must not erase the retained row"
                );
                assert!(!state.is_empty() && terminal);
            }
            assert_eq!(
                reply_row_update(Phase::Closing, None, "处理中"),
                (Some("处理中"), "", false)
            );
            assert_eq!(
                reply_row_update(
                    Phase::Closing,
                    Some(TaskOutcome::OutcomeUnknown),
                    "保留回复"
                ),
                (Some("保留回复"), "操作结果待核实", true)
            );
        }

        #[test]
        fn completed_reply_survives_new_connection_buffer_reset() {
            let messages = VecModel::from(vec![AssistantMessage {
                role: "助手".into(),
                text: "".into(),
                state: "".into(),
            }]);
            let mut index =
                update_streaming_reply(&messages, Some(0), Phase::Running, None, "流式回复");
            assert_eq!(messages.row_data(0).unwrap().text.as_str(), "流式回复");
            index = update_streaming_reply(
                &messages,
                index,
                Phase::Ready,
                Some(TaskOutcome::Completed),
                "最终回复",
            );
            assert!(index.is_none());
            // Controller.connect resets text/outcome before initialize/create.
            index = update_streaming_reply(&messages, index, Phase::Initializing, None, "");
            index = update_streaming_reply(&messages, index, Phase::Ready, None, "");
            assert!(index.is_none());
            let retained = messages.row_data(0).unwrap();
            assert_eq!(retained.text.as_str(), "最终回复");
            assert_eq!(retained.state.as_str(), "已完成");
        }

        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                // macOS's ambient /var temp path is a symlink; fixture root itself
                // uses the actual directory so tests can isolate our own symlinks.
                let temporary = std::fs::canonicalize(std::env::temp_dir()).unwrap();
                let root = temporary.join(format!(
                    "seecut-assistant-host-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&root)
                    .unwrap();
                Self(root)
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn session_storage_creates_missing_parents_and_rejects_symlinks() {
            let fixture = Fixture::new();
            let existing = fixture.0.join("existing");
            std::fs::create_dir(&existing).unwrap();
            std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
            let sessions = session_directory(&existing).unwrap();
            assert_eq!(
                std::fs::metadata(&sessions).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(session_directory(&existing).unwrap(), sessions);

            let missing = fixture.0.join("fresh/nested-data");
            assert!(session_directory(&missing).is_ok());
            assert_eq!(
                std::fs::metadata(&missing).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let linked = fixture.0.join("linked");
            symlink(&existing, &linked).unwrap();
            assert!(session_directory(&linked.join("must-not-create")).is_err());
            assert!(!existing.join("must-not-create").exists());

            std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(session_directory(&existing).is_err());
            let leaf_data = fixture.0.join("leaf-data");
            std::fs::create_dir(&leaf_data).unwrap();
            symlink(&sessions, leaf_data.join("assistant-sessions")).unwrap();
            assert!(session_directory(&leaf_data).is_err());
        }

        #[test]
        fn provider_requires_http_without_userinfo_or_fragment() {
            assert!(provider_url("https://example.invalid/v1").is_ok());
            for invalid in [
                "file:///tmp/x",
                "https://user@example.invalid/v1",
                "https://user:password@example.invalid",
                "https://@example.invalid",
                "https://example.invalid/#fragment",
            ] {
                assert!(provider_url(invalid).is_err());
            }
        }
    }
}
