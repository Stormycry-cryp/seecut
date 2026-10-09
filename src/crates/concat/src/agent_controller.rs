// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal UI-independent controller for the documented HOST-IPC-V1 protocol.
//! Only the trusted host constructs configuration; no credential discovery or automatic retry.
use super::agent_process::{self, BridgeError, Event, Handle, LaunchSpec};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TEXT_LIMIT: usize = 64 * 1024;
const MAX_TOOL_PROGRESS: usize = 12;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub use super::agent_identity::DocumentIdentity;
/// Credentials deliberately have no Debug implementation and are never written to disk/logs.
pub struct ConnectionConfig {
    pub launch: LaunchSpec,
    pub document: DocumentIdentity,
    pub client_id: String,
    pub mcp_client_path: PathBuf,
    pub mcp_environment: BTreeMap<String, String>,
    pub session_directory: PathBuf,
    pub provider: Value,
    pub read_token: String,
    pub write_token: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Disconnected,
    Initializing,
    OpeningSession,
    Ready,
    Running,
    AwaitingApproval,
    Stopping,
    Closing,
    OutcomeUnknown,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    Completed,
    SubmittedPendingVerification,
    Failed,
    Stopped,
    OutcomeUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiError {
    Configuration,
    Connection,
    Protocol,
    Busy,
    AuthorizationRequired,
    DocumentChanged,
    VerifyRequired,
    ResponseTimeout,
    ModelVerifyRequired,
    AccountExpired,
    ServiceUnavailable,
    SessionRecoveryRequired,
}
impl UiError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Configuration => "请检查 Agent 运行时和模型配置",
            Self::Connection => "Agent 连接已断开，请重新连接",
            Self::Protocol => "Agent 协议不兼容，请检查运行时版本",
            Self::Busy => "请求正在处理，请稍后操作",
            Self::AuthorizationRequired => "请在 App 中检查当前工程的授权",
            Self::DocumentChanged => "工程已改变，请重新连接当前工程",
            Self::VerifyRequired => "操作结果待核实，已暂停后续请求",
            Self::ResponseTimeout => "Agent 未及时回应，请重新连接",
            Self::ModelVerifyRequired => "推理或积分结果待核实，请核实原操作",
            Self::AccountExpired => "登录已过期，请重新登录",
            Self::ServiceUnavailable => "Agent 服务暂不可用，请稍后重试",
            Self::SessionRecoveryRequired => "会话需要恢复，请重新连接后发送新问题",
        }
    }
}
pub struct Approval {
    pub id: String,
    pub turn_id: String,
    pub object_id: Option<String>,
    pub delta_x: Option<f64>,
    pub delta_y: Option<f64>,
}
pub struct View<'a> {
    pub phase: Phase,
    pub text: &'a str,
    pub progress: &'a str,
    pub approval: Option<&'a Approval>,
    pub outcome: Option<TaskOutcome>,
    pub error: Option<UiError>,
    pub can_send: bool,
    pub can_stop: bool,
    pub can_approve: bool,
    pub can_connect: bool,
    pub can_reconcile: bool,
    pub account_expired: bool,
    pub budget_label: &'a str,
    pub billing_revision: u64,
}
#[derive(Clone, Copy)]
enum ToolAction {
    Context,
    Project,
    Preview,
    Capabilities,
    Move,
}
impl ToolAction {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "context" => Some(Self::Context),
            "project" => Some(Self::Project),
            "preview" => Some(Self::Preview),
            "capabilities" => Some(Self::Capabilities),
            "move_selected_image" => Some(Self::Move),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Context => "读取当前画布",
            Self::Project => "读取工程信息",
            Self::Preview => "查看画布预览",
            Self::Capabilities => "检查可用操作",
            Self::Move => "发送移动请求",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolProgressState {
    Running,
    Completed,
    Failed,
    Cancelled,
}
struct ToolProgress {
    id: String,
    action: ToolAction,
    state: ToolProgressState,
}
impl ToolProgress {
    fn label(&self) -> String {
        if matches!(self.action, ToolAction::Move) && self.state == ToolProgressState::Completed {
            return "移动请求已返回".into();
        }
        format!(
            "{}{}",
            self.action.label(),
            match self.state {
                ToolProgressState::Running => "…",
                ToolProgressState::Completed => " · 已返回",
                ToolProgressState::Failed => " · 失败",
                ToolProgressState::Cancelled => " · 已取消",
            }
        )
    }
}
enum Request {
    Initialize,
    Create,
    Send { input_id: String },
    Stop,
    Approve { approval_id: String },
    Close,
    Reconcile,
    RecoveryStatus,
}
struct Pending {
    id: String,
    request: Request,
    since: Instant,
}

pub struct Controller {
    process: Option<Handle>,
    document: Option<DocumentIdentity>,
    session_id: String,
    phase: Phase,
    pending: VecDeque<Pending>,
    counter: u64,
    turn_id: Option<String>,
    last_result_turn: Option<String>,
    last_seq: u64,
    text: String,
    tool_progress: Vec<ToolProgress>,
    progress: String,
    approval: Option<Approval>,
    outcome: Option<TaskOutcome>,
    error: Option<UiError>,
    unknown_latched: bool,
    managed_mode: bool,
    model_unknown: bool,
    recovery_pending: bool,
    last_model_state: Option<Value>,
    account_expired: bool,
    budget_label: String,
    billing_fingerprint: Value,
    billing_revision: u64,
    ignore_runtime_events: bool,
    secrets: Vec<String>,
    write_in_flight: bool,
    write_identity: Option<(String, String)>,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            process: None,
            document: None,
            session_id: String::new(),
            phase: Phase::Disconnected,
            pending: VecDeque::new(),
            counter: 0,
            turn_id: None,
            last_result_turn: None,
            last_seq: 0,
            text: String::new(),
            tool_progress: Vec::new(),
            progress: String::new(),
            approval: None,
            outcome: None,
            error: None,
            unknown_latched: false,
            managed_mode: false,
            model_unknown: false,
            recovery_pending: false,
            last_model_state: None,
            account_expired: false,
            budget_label: String::new(),
            billing_fingerprint: Value::Null,
            billing_revision: 0,
            ignore_runtime_events: false,
            secrets: Vec::new(),
            write_in_flight: false,
            write_identity: None,
        }
    }
}
impl Controller {
    /// An explicit host action only. Ready requires successful initialize AND session.create replies.
    pub fn connect(&mut self, config: ConnectionConfig) -> Result<(), UiError> {
        if self.unknown_latched {
            return Err(UiError::VerifyRequired);
        }
        if self.process.is_some() {
            return Err(UiError::Busy);
        }
        let valid_id = |id: &str| max64(id);
        if config.document.instance_id.is_empty()
            || config.document.project_id.is_empty()
            || !valid_id(&config.document.document_session_id)
            || !valid_id(&config.client_id)
            || !config.provider.is_object()
            || config.read_token.is_empty()
            || !config.mcp_client_path.is_absolute()
            || !config.session_directory.is_absolute()
            || !config
                .mcp_environment
                .get("TMPDIR")
                .is_some_and(|p| PathBuf::from(p).is_absolute())
            || config.mcp_environment.iter().any(|(key, value)| {
                !matches!(key.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR")
                    || !PathBuf::from(value).is_absolute()
            })
        {
            return Err(UiError::Configuration);
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| UiError::Configuration)?
            .as_nanos();
        self.session_id = format!(
            "native-{}-{epoch}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        self.secrets = vec![config.read_token.clone()];
        if let Some(token) = &config.write_token {
            self.secrets.push(token.clone());
        }
        if let Some(key) = config.provider.get("apiKey").and_then(Value::as_str) {
            self.secrets.push(key.to_owned());
        }
        if let Some(token) = config.provider.get("sessionToken").and_then(Value::as_str) {
            self.secrets.push(token.to_owned());
        }
        self.managed_mode = config.provider["protocol"].as_str() == Some("seecut-managed");
        // A new managed runtime must supply its persisted recovery state before Ready.
        self.model_unknown = self.managed_mode && self.model_unknown;
        self.account_expired = false;
        self.recovery_pending = false;
        self.last_model_state = None;
        self.budget_label.clear();
        self.billing_fingerprint = Value::Null;
        self.secrets.retain(|value| !value.is_empty());
        let params = json!({"protocolVersion":1,"appProtocolVersion":1,"instanceId":config.document.instance_id,"clientId":config.client_id,"clientPath":config.mcp_client_path,"mcpEnvironment":config.mcp_environment,"sessionDirectory":config.session_directory,"provider":config.provider,"readToken":config.read_token,"writeToken":config.write_token});
        // The runtime distinguishes absent writeToken from null.
        let mut params = params;
        if params["writeToken"].is_null() {
            params.as_object_mut().unwrap().remove("writeToken");
        }
        let handle = agent_process::start(config.launch).map_err(bridge_error)?;
        self.process = Some(handle);
        self.document = Some(config.document);
        self.pending.clear();
        self.turn_id = None;
        self.last_result_turn = None;
        self.last_seq = 0;
        self.write_in_flight = false;
        self.write_identity = None;
        self.text.clear();
        self.tool_progress.clear();
        self.progress.clear();
        self.approval = None;
        self.outcome = None;
        self.error = None;
        self.ignore_runtime_events = false;
        self.phase = Phase::Initializing;
        if let Err(error) = self.enqueue("initialize", params, Request::Initialize) {
            self.fail(error);
            return Err(error);
        }
        Ok(())
    }
    fn enqueue(&mut self, method: &str, params: Value, request: Request) -> Result<(), UiError> {
        if self.pending.len() >= 4 {
            return Err(UiError::Busy);
        }
        self.counter = self.counter.checked_add(1).ok_or(UiError::Protocol)?;
        let id = format!("request-{}", self.counter);
        self.process
            .as_ref()
            .ok_or(UiError::Connection)?
            .try_send(&id, method, params)
            .map_err(bridge_error)?;
        self.pending.push_back(Pending {
            id,
            request,
            since: Instant::now(),
        });
        Ok(())
    }
    pub fn view(&self) -> View<'_> {
        View {
            can_connect: self.process.is_none() && !self.unknown_latched,
            can_reconcile: self.managed_mode
                && self.model_unknown
                && !self.account_expired
                && self.process.is_some()
                && self.turn_id.is_none()
                && self.pending.is_empty()
                && !self.ignore_runtime_events,
            account_expired: self.account_expired,
            budget_label: &self.budget_label,
            billing_revision: self.billing_revision,
            phase: self.phase,
            text: &self.text,
            progress: &self.progress,
            approval: self.approval.as_ref(),
            outcome: self.outcome,
            error: self.error,
            can_send: self.phase == Phase::Ready && self.pending.is_empty() && !self.blocked(),
            can_stop: self.turn_id.is_some()
                && !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::Stop))
                && matches!(
                    self.phase,
                    Phase::Running | Phase::AwaitingApproval | Phase::OutcomeUnknown
                ),
            can_approve: self.phase == Phase::AwaitingApproval
                && self.approval.is_some()
                && !self.blocked()
                && !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::Stop | Request::Approve { .. })),
        }
    }
    pub fn send_text(&mut self, text: &str) -> Result<(), UiError> {
        if let Some(error) = self.blocking_error() {
            return Err(error);
        }
        if !self.view().can_send {
            return Err(UiError::Busy);
        }
        if text.trim().is_empty() || text.len() > 32_000 {
            return Err(UiError::Configuration);
        }
        let input_id = format!("input-{}-{}", self.session_id, self.counter + 1);
        self.enqueue("send",json!({"sessionId":self.session_id,"inputId":input_id,"mode":"follow_up","content":[{"type":"text","text":text}]}),Request::Send{input_id})?;
        self.text.clear();
        self.tool_progress.clear();
        self.progress.clear();
        self.outcome = None;
        self.error = None;
        self.last_result_turn = None;
        Ok(())
    }
    /// Enqueue isn't confirmation. Phase becomes Stopping only on the real {stopping:true} reply.
    pub fn stop(&mut self) -> Result<(), UiError> {
        if !self.view().can_stop {
            return Err(UiError::Busy);
        }
        self.enqueue(
            "stop",
            json!({"sessionId":self.session_id,"turnId":self.turn_id}),
            Request::Stop,
        )
    }
    pub fn approve(&mut self, id: &str, allowed: bool) -> Result<(), UiError> {
        if let Some(error) = self.blocking_error() {
            return Err(error);
        }
        if !self.view().can_approve {
            return Err(UiError::Busy);
        }
        if !self
            .approval
            .as_ref()
            .is_some_and(|a| a.id == id && self.turn_id.as_deref() == Some(a.turn_id.as_str()))
        {
            return Err(UiError::AuthorizationRequired);
        }
        self.enqueue(
            "approve",
            json!({"sessionId":self.session_id,"approvalId":id,"allowed":allowed}),
            Request::Approve {
                approval_id: id.to_owned(),
            },
        )
    }
    pub fn reconcile(&mut self) -> Result<(), UiError> {
        if !self.view().can_reconcile {
            return Err(self.blocking_error().unwrap_or(UiError::Busy));
        }
        self.enqueue(
            "reconcile",
            json!({"sessionId":self.session_id}),
            Request::Reconcile,
        )
    }
    fn apply_model_state(&mut self, value: &Value) -> bool {
        if !self.managed_mode {
            return false;
        }
        let Some(state) = checked_model_state(value, &self.session_id) else {
            return false;
        };
        if let Some(previous) = &self.last_model_state {
            let previous_revision = previous["revision"].as_u64().unwrap();
            if state.revision < previous_revision {
                return true;
            }
            if state.revision == previous_revision {
                return previous == value;
            }
        }
        self.last_model_state = Some(value.clone());
        let was_unknown = self.model_unknown;
        self.model_unknown = state.unknown;
        // Login expiry is sticky for this runtime; only a fresh login can clear it.
        self.account_expired |= state.account_expired;
        self.budget_label = state.budget_label;
        if self.billing_fingerprint != state.billing {
            self.billing_fingerprint = state.billing;
            self.billing_revision = self.billing_revision.saturating_add(1);
        }
        if let Some(error) = self.blocking_error() {
            self.error = Some(error);
            if self.turn_id.is_none()
                && !matches!(
                    self.phase,
                    Phase::Initializing | Phase::OpeningSession | Phase::Closing
                )
            {
                self.phase = Phase::OutcomeUnknown;
            }
        } else {
            if matches!(self.error, Some(UiError::ModelVerifyRequired)) {
                self.error = None;
            }
            if was_unknown && self.phase == Phase::OutcomeUnknown && self.turn_id.is_none() {
                self.phase = Phase::Ready;
                self.outcome = None;
            }
        }
        true
    }
    fn blocked(&self) -> bool {
        self.unknown_latched || self.model_unknown || self.account_expired || self.recovery_pending
    }
    fn blocking_error(&self) -> Option<UiError> {
        if self.unknown_latched {
            Some(UiError::VerifyRequired)
        } else if self.account_expired {
            Some(UiError::AccountExpired)
        } else if self.model_unknown {
            Some(UiError::ModelVerifyRequired)
        } else if self.recovery_pending {
            Some(UiError::SessionRecoveryRequired)
        } else {
            None
        }
    }
    fn mark_interrupted_model(&mut self) {
        if self.managed_mode
            && (self.turn_id.is_some()
                || self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::Send { .. })))
        {
            self.model_unknown = true;
        }
    }
    /// Stop the old runtime and close its session. Reconnection requires another explicit connect.
    pub fn disconnect(&mut self) {
        self.mark_interrupted_model();
        self.ignore_runtime_events = true;
        self.approval = None;
        self.turn_id = None;
        self.pending.clear();
        if self.process.is_some() {
            self.phase = Phase::Closing;
            let params = json!({"sessionId":self.session_id});
            if self
                .enqueue("session.close", params, Request::Close)
                .is_err()
                && let Some(process) = &self.process
            {
                process.request_shutdown();
            }
        } else {
            self.phase = Phase::Disconnected;
        }
    }
    /// App exit may use this immediately, without waiting for a session.close response.
    /// The actual process-exit wait is agent_process::shutdown_all_at_exit, never this UI call.
    pub fn shutdown_now(&mut self) {
        self.mark_interrupted_model();
        self.ignore_runtime_events = true;
        self.approval = None;
        self.pending.clear();
        self.turn_id = None;
        if let Some(process) = &self.process {
            process.request_shutdown();
            self.phase = Phase::Closing;
        } else {
            self.phase = if self.unknown_latched {
                Phase::OutcomeUnknown
            } else {
                Phase::Disconnected
            };
        }
    }
    /// Called by a host timer with the actual currently active document identity; no model polling.
    pub fn tick(&mut self, current: Option<&DocumentIdentity>) {
        if self.process.is_some()
            && self.document.as_ref() != current
            && !self.ignore_runtime_events
        {
            self.disconnect();
            self.error = Some(UiError::DocumentChanged);
            self.text.clear();
            self.outcome = None;
        }
        for _ in 0..64 {
            let event = self.process.as_ref().and_then(Handle::try_recv);
            let Some(event) = event else {
                break;
            };
            self.handle_event(event);
        }
        if self
            .pending
            .iter()
            .any(|pending| pending.since.elapsed() >= RESPONSE_TIMEOUT)
        {
            self.fail(UiError::ResponseTimeout);
        }
    }
    fn fail(&mut self, error: UiError) {
        self.mark_interrupted_model();
        if self.write_in_flight {
            self.unknown();
        }
        self.error = Some(self.blocking_error().unwrap_or(error));
        self.phase = if self.blocked() {
            Phase::OutcomeUnknown
        } else {
            Phase::Failed
        };
        self.approval = None;
        self.pending.clear();
        self.ignore_runtime_events = true;
        if let Some(process) = &self.process {
            process.request_shutdown();
        }
    }
    fn text(&mut self, text: &str, replace: bool) {
        let mut safe = text.to_owned();
        for secret in &self.secrets {
            safe = safe.replace(secret, "[已隐藏]");
        }
        if replace {
            self.text.clear();
        }
        self.text.push_str(&safe);
        if self.text.len() > TEXT_LIMIT {
            let mut cut = self.text.len() - TEXT_LIMIT;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
    }
    fn unknown(&mut self) {
        self.unknown_latched = true;
        self.phase = Phase::OutcomeUnknown;
        self.outcome = Some(TaskOutcome::OutcomeUnknown);
        self.error = Some(UiError::VerifyRequired);
        self.approval = None;
    }
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Response(value) => self.response(value),
            Event::Runtime(value) => {
                if self.ignore_runtime_events {
                    self.observe_closing_write(&value["event"]);
                } else {
                    self.runtime_event(&value["event"]);
                }
            }
            Event::Result(value) if !self.ignore_runtime_events => self.result(&value),
            Event::Result(value)
                if value["sessionId"].as_str() == Some(self.session_id.as_str())
                    && value["toolState"]["outcomeUnknown"].as_bool() == Some(true) =>
            {
                self.unknown();
            }
            Event::Managed(value) if !self.ignore_runtime_events => {
                if value["sessionId"].as_str() != Some(self.session_id.as_str())
                    || !self.apply_model_state(&value["modelState"])
                {
                    self.fail(UiError::Protocol);
                }
            }
            // Transport retains bounded diagnostics for its contract checks. The
            // UI shows fixed wording and never exposes child-provided details.
            Event::RuntimeDiagnostic { code: _code } => self.fail(UiError::Protocol),
            Event::ProtocolFailure(error) => self.fail(bridge_error(error)),
            // Process status cannot establish whether an edit completed; only
            // the controller's observed operation state can establish that.
            Event::Exit {
                code: _code,
                requested: _requested,
            } => {
                self.mark_interrupted_model();
                if self.write_in_flight {
                    self.unknown();
                }
                let was_closing = self.phase == Phase::Closing;
                self.process.take();
                self.pending.clear();
                self.approval = None;
                self.turn_id = None;
                self.secrets.clear();
                if self.blocked() {
                    self.phase = Phase::OutcomeUnknown;
                    self.error = self.blocking_error();
                } else if was_closing {
                    self.phase = Phase::Disconnected;
                } else if self.phase != Phase::Failed {
                    self.phase = Phase::Failed;
                    self.error = Some(UiError::Connection);
                }
            }
            _ => {}
        }
    }
    /// Revocation still blocks the old session immediately. A late refusal can
    /// settle only the exact in-flight write; it cannot reopen a turn, clear a
    /// model/account fence, or undo an already established edit UNKNOWN.
    fn observe_closing_write(&mut self, event: &Value) {
        if event["sessionId"].as_str() != Some(self.session_id.as_str()) {
            return;
        }
        let Some(seq) = event["seq"].as_u64().filter(|seq| *seq > self.last_seq) else {
            return;
        };
        // Keep the trusted session's sequence monotonic even for ignored UI
        // events. A stale refusal must not supersede a newer uncertainty frame.
        self.last_seq = seq;
        if matches!(
            event["type"].as_str(),
            Some("tool.outcome_unknown" | "recovery.required")
        ) {
            self.unknown();
            return;
        }
        let Some((turn, invocation)) = self.write_identity.as_ref() else {
            return;
        };
        let data = &event["data"];
        if !self.write_in_flight
            || event["turnId"].as_str() != Some(turn.as_str())
            || event["type"].as_str() != Some("tool.failed")
            || data["invocationId"].as_str() != Some(invocation.as_str())
            || data["message"]["toolName"].as_str() != Some("move_selected_image")
            || data["message"]["isError"].as_bool() != Some(true)
            || data["metadata"]["appStatus"].as_str() != Some("not_sent")
        {
            return;
        }
        self.write_in_flight = false;
        self.write_identity = None;
    }
    fn response(&mut self, value: Value) {
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            self.fail(UiError::Protocol);
            return;
        };
        let Some(index) = self.pending.iter().position(|p| p.id == id) else {
            return;
        };
        let pending = self.pending.remove(index).unwrap();
        if value.get("error").is_some() {
            let code = value["error"]["code"].as_str().unwrap_or("");
            // recovery_required also covers interrupted inputs and managed budgets.
            // Query the original session before classifying an edit as UNKNOWN.
            if code == "recovery_required" {
                self.recovery_pending = true;
                self.error = self.blocking_error();
                if !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::RecoveryStatus))
                    && let Err(error) = self.enqueue(
                        "status",
                        json!({"sessionId":self.session_id}),
                        Request::RecoveryStatus,
                    )
                {
                    self.fail(error);
                }
                return;
            }
            let error = remote_error(code);
            if error == UiError::Protocol {
                self.fail(error);
                return;
            }
            if error == UiError::ModelVerifyRequired || error == UiError::AccountExpired {
                self.model_unknown |= error == UiError::ModelVerifyRequired;
                self.account_expired |= error == UiError::AccountExpired;
                self.error = self.blocking_error();
                if matches!(pending.request, Request::Initialize | Request::Create) {
                    self.fail(error);
                } else if self.turn_id.is_none() {
                    self.phase = Phase::OutcomeUnknown;
                }
                return;
            }
            if error == UiError::VerifyRequired {
                self.unknown();
                return;
            }
            match pending.request {
                Request::Send { .. } if !self.unknown_latched => {
                    self.phase = Phase::Ready;
                    self.error = Some(error);
                }
                Request::Approve { .. } | Request::Stop | Request::Reconcile => {
                    self.error = Some(error);
                }
                Request::Close => {
                    if self.error.is_none() {
                        self.error = Some(error);
                    }
                    if let Some(process) = &self.process {
                        process.request_shutdown();
                    }
                }
                _ => self.fail(error),
            }
            return;
        }
        let result = &value["result"];
        match pending.request {
            Request::Initialize => {
                if self.phase != Phase::Initializing {
                    return;
                }
                if result["protocolVersion"].as_u64() != Some(1)
                    || result["instanceId"].as_str()
                        != self.document.as_ref().map(|d| d.instance_id.as_str())
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                let document = self.document.as_ref().unwrap();
                let params = json!({"sessionId":self.session_id,"projectId":document.project_id,"documentSessionId":document.document_session_id});
                match self.enqueue("session.create", params, Request::Create) {
                    Ok(()) => self.phase = Phase::OpeningSession,
                    Err(error) => self.fail(error),
                }
            }
            Request::Create => {
                if self.phase != Phase::OpeningSession {
                    return;
                }
                if result["sessionId"].as_str() != Some(self.session_id.as_str())
                    || result["status"].as_str() != Some("idle")
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.managed_mode && !self.apply_model_state(&result["modelState"]) {
                    self.fail(UiError::Protocol);
                    return;
                }
                if result["toolState"]["outcomeUnknown"].as_bool() == Some(true) {
                    self.unknown();
                } else {
                    self.phase = if self.blocked() {
                        Phase::OutcomeUnknown
                    } else {
                        Phase::Ready
                    };
                }
            }
            Request::Send { input_id } => {
                if result["inputId"].as_str() != Some(input_id.as_str())
                    || !matches!(result["status"].as_str(), Some("pending" | "applied"))
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.last_result_turn.is_none()
                    && !self.unknown_latched
                    && !self.ignore_runtime_events
                    && self.phase != Phase::AwaitingApproval
                {
                    self.phase = Phase::Running;
                }
            }
            Request::Stop => {
                if result["stopping"].as_bool() != Some(true) {
                    self.fail(UiError::Protocol);
                } else if self.turn_id.is_some()
                    && !self.unknown_latched
                    && !self.ignore_runtime_events
                {
                    self.phase = Phase::Stopping;
                    self.approval = None;
                }
            }
            Request::Approve { approval_id } => {
                if result["resolved"].as_bool() != Some(true)
                    || result["appAuthorizationChanged"].as_bool() != Some(false)
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.approval.as_ref().is_some_and(|a| a.id == approval_id) {
                    self.approval = None;
                    if self.turn_id.is_some() && !self.unknown_latched {
                        self.phase = Phase::Running;
                    }
                }
            }
            Request::RecoveryStatus => {
                if result["sessionId"].as_str() != Some(self.session_id.as_str())
                    || !result["toolState"]["outcomeUnknown"].is_boolean()
                    || (self.managed_mode && !self.apply_model_state(&result["modelState"]))
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if result["toolState"]["outcomeUnknown"] == true {
                    self.unknown();
                } else if self.model_unknown || self.account_expired {
                    self.recovery_pending = false;
                    self.error = self.blocking_error();
                } else if result["status"].as_str() == Some("idle")
                    && result.get("activeTurnId").is_none_or(Value::is_null)
                {
                    // A budget-close fence may already have settled while status was in flight.
                    self.recovery_pending = false;
                    self.error = self.blocking_error();
                } else {
                    // Interrupted pending input requires an explicit fresh session.
                    // Do not resume or resubmit it, and do not poison the edit latch.
                    self.error = Some(UiError::SessionRecoveryRequired);
                }
                if self.turn_id.is_none() {
                    self.phase = if self.blocked() {
                        Phase::OutcomeUnknown
                    } else {
                        Phase::Ready
                    };
                }
            }
            Request::Reconcile => {
                if !self.apply_model_state(&result["modelState"]) {
                    self.fail(UiError::Protocol);
                }
            }
            Request::Close => {
                if result["closed"].as_bool() != Some(true) {
                    self.fail(UiError::Protocol);
                }
                if let Some(process) = &self.process {
                    process.request_shutdown();
                }
            }
        }
    }
    fn runtime_event(&mut self, event: &Value) {
        if event["sessionId"].as_str() != Some(self.session_id.as_str()) {
            self.fail(UiError::Protocol);
            return;
        }
        let Some(seq) = event["seq"].as_u64() else {
            self.fail(UiError::Protocol);
            return;
        };
        if seq <= self.last_seq {
            return;
        }
        self.last_seq = seq;
        let kind = event["type"].as_str().unwrap_or("");
        let data = &event["data"];
        if kind == "recovery.required" || kind == "tool.outcome_unknown" {
            // Generic COTO recovery remains an edit fence; managed state has its own frame.
            self.unknown();
            return;
        }
        let turn = event["turnId"].as_str();
        if kind == "turn.started" {
            if self.unknown_latched {
                return;
            }
            if !self
                .pending
                .iter()
                .any(|p| matches!(p.request, Request::Send { .. }))
                && self.phase != Phase::Running
            {
                self.fail(UiError::Protocol);
                return;
            }
            let Some(turn) = turn else {
                self.fail(UiError::Protocol);
                return;
            };
            self.turn_id = Some(turn.to_owned());
            self.phase = Phase::Running;
        }
        if turn.is_some() && turn != self.turn_id.as_deref() {
            return;
        }
        // Display only events scoped to the live turn. Security/recovery branches
        // above retain their original handling; model text never creates a step.
        if turn.is_some()
            && turn == self.turn_id.as_deref()
            && !self.unknown_latched
            && matches!(
                self.phase,
                Phase::Running | Phase::AwaitingApproval | Phase::Stopping
            )
        {
            self.track_tool_progress(kind, data);
        }
        match kind {
            "tool.started" => {
                if data["call"]["name"].as_str() == Some("move_selected_image") {
                    self.write_in_flight = true;
                    self.write_identity = turn
                        .zip(
                            data["invocationId"]
                                .as_str()
                                .filter(|id| !id.is_empty() && id.len() <= 128),
                        )
                        .map(|(turn, invocation)| (turn.to_owned(), invocation.to_owned()));
                }
            }
            "tool.completed" | "tool.failed" => {
                if data["message"]["toolName"].as_str() == Some("move_selected_image") {
                    self.write_in_flight = false;
                    self.write_identity = None;
                    if data["metadata"]["postVerification"].as_str() == Some("pending") {
                        self.outcome = Some(TaskOutcome::SubmittedPendingVerification);
                    }
                }
            }
            "tool.cancelled" => {
                if self.write_in_flight && data["executed"].as_bool() == Some(true) {
                    self.unknown();
                } else {
                    self.write_in_flight = false;
                }
            }
            "text.delta" => {
                if let Some(text) = data["text"].as_str() {
                    self.text(text, false);
                }
            }
            "approval.required" => {
                if self.blocked() {
                    return;
                }
                let (Some(id), Some(turn), Some(tool)) =
                    (data["approvalId"].as_str(), turn, data["tool"].as_str())
                else {
                    self.fail(UiError::Protocol);
                    return;
                };
                if id.is_empty() || id.len() > 100 || tool != "move_selected_image" {
                    self.fail(UiError::Protocol);
                    return;
                }
                let args = &data["arguments"];
                self.approval = Some(Approval {
                    id: id.to_owned(),
                    turn_id: turn.to_owned(),
                    object_id: args["objectId"]
                        .as_str()
                        .filter(|v| {
                            v.strip_prefix("layer:").is_some_and(|digits| {
                                digits
                                    .parse::<u64>()
                                    .ok()
                                    .is_some_and(|id| id > 0 && id.to_string() == digits)
                            })
                        })
                        .map(str::to_owned),
                    delta_x: args["deltaX"].as_f64(),
                    delta_y: args["deltaY"].as_f64(),
                });
                self.phase = Phase::AwaitingApproval;
            }
            "approval.resolved" => {
                if self
                    .approval
                    .as_ref()
                    .is_some_and(|a| Some(a.id.as_str()) == data["approvalId"].as_str())
                {
                    self.approval = None;
                    if !self.unknown_latched {
                        self.phase = Phase::Running;
                    }
                }
            }
            _ => {}
        }
    }
    fn track_tool_progress(&mut self, kind: &str, data: &Value) {
        let state = match kind {
            "tool.started" | "tool.progress" => ToolProgressState::Running,
            "tool.completed" => ToolProgressState::Completed,
            "tool.failed" => ToolProgressState::Failed,
            "tool.cancelled" => ToolProgressState::Cancelled,
            _ => return,
        };
        let Some(id) = data["invocationId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128)
        else {
            return;
        };
        if let Some(step) = self.tool_progress.iter_mut().find(|step| step.id == id) {
            // A repeated start/progress or a delayed terminal cannot reopen or
            // replace a settled invocation. Never read progress's free text.
            if step.state != ToolProgressState::Running {
                return;
            }
            step.state = state;
        } else {
            if kind == "tool.progress" || self.tool_progress.len() == MAX_TOOL_PROGRESS {
                return;
            }
            let name = if kind == "tool.started" {
                data["call"]["name"].as_str()
            } else {
                data["message"]["toolName"].as_str()
            };
            let Some(action) = name.and_then(ToolAction::from_name) else {
                return;
            };
            self.tool_progress.push(ToolProgress {
                id: id.to_owned(),
                action,
                state,
            });
        }
        self.progress = self
            .tool_progress
            .iter()
            .map(ToolProgress::label)
            .collect::<Vec<_>>()
            .join("\n");
    }
    fn result(&mut self, value: &Value) {
        if value["sessionId"].as_str() != Some(self.session_id.as_str()) {
            self.fail(UiError::Protocol);
            return;
        }
        let result = &value["result"];
        let Some(turn) = result["turnId"].as_str() else {
            self.fail(UiError::Protocol);
            return;
        };
        if self.last_result_turn.as_deref() == Some(turn) {
            return;
        }
        if self.turn_id.as_deref() != Some(turn) {
            self.fail(UiError::Protocol);
            return;
        }
        self.last_result_turn = Some(turn.to_owned());
        self.turn_id = None;
        self.approval = None;
        if let Some(text) = result["text"].as_str() {
            // Failed or stopped streams may have no final message. Keep the
            // already observed reply while the separate state reports failure.
            if !text.is_empty()
                || !matches!(result["status"].as_str(), Some("failed" | "interrupted"))
            {
                self.text(text, true);
            }
        }
        if self.managed_mode && !self.apply_model_state(&value["modelState"]) {
            self.fail(UiError::Protocol);
            return;
        }
        let tools = &value["toolState"];
        if tools["outcomeUnknown"].as_bool() == Some(true) || self.unknown_latched {
            self.unknown();
            return;
        }
        let prefix = format!("{turn}_");
        let current: Vec<&Value> = tools["observed"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        item["invocationId"]
                            .as_str()
                            .is_some_and(|id| id.starts_with(&prefix))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.outcome = Some(
            if current
                .iter()
                .any(|tool| tool["postVerification"].as_str() == Some("pending"))
            {
                TaskOutcome::SubmittedPendingVerification
            } else if current
                .iter()
                .any(|tool| tool["state"].as_str() == Some("failed"))
            {
                TaskOutcome::Failed
            } else {
                match result["status"].as_str() {
                    Some("completed") => TaskOutcome::Completed,
                    Some("interrupted") => TaskOutcome::Stopped,
                    _ => TaskOutcome::Failed,
                }
            },
        );
        self.phase = if self.blocked() {
            Phase::OutcomeUnknown
        } else {
            Phase::Ready
        };
        if self.blocked() {
            self.outcome = Some(TaskOutcome::OutcomeUnknown);
            self.error = self.blocking_error();
        }
    }
}
struct CheckedModelState {
    revision: u64,
    unknown: bool,
    account_expired: bool,
    budget_label: String,
    billing: Value,
}
/// Check every field consumed by the host. Display only fixed labels and numeric amounts.
fn checked_model_state(value: &Value, session_id: &str) -> Option<CheckedModelState> {
    if serde_json::to_vec(value).ok()?.len() > 64 * 1024 {
        return None;
    }
    let revision = value
        .get("revision")?
        .as_u64()
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)?;
    let unknown = value.get("outcomeUnknown")?.as_bool()?;
    let account_expired = value.get("accountExpired")?.as_bool()?;
    let operations = value.get("operations")?.as_array()?;
    if operations.len() > 64 {
        return None;
    }
    let id = |v: &Value| {
        v.as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        })
    };
    let credit = |v: &Value| v.as_u64().filter(|n| *n <= 1_000_000_000);
    let mut billing = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut operation_ids = std::collections::BTreeSet::new();
    for op in operations {
        if !op.is_object()
            || !op.as_object()?.contains_key("operationId")
            || !op.as_object()?.contains_key("billingState")
            || !id(&op["clientRequestId"])
            || !ids.insert(op["clientRequestId"].as_str()?)
            || !(op["operationId"].is_null() || id(&op["operationId"]))
            || !id(&op["turnId"])
            || !matches!(
                op["state"].as_str(),
                Some(
                    "prepared"
                        | "quoted"
                        | "creating"
                        | "accepted"
                        | "settled"
                        | "unknown"
                        | "not_submitted"
                        | "queued"
                        | "dispatching"
                        | "running"
                        | "succeeded"
                        | "failed"
                        | "cancelled"
                        | "outcome_unknown"
                )
            )
            || !(op["billingState"].is_null()
                || matches!(
                    op["billingState"].as_str(),
                    Some("held" | "settlement_pending" | "captured" | "released")
                ))
            || op["outcomeUnknown"].as_bool().is_none()
            || op["doneDelivered"].as_bool().is_none()
        {
            return None;
        }
        if !op["operationId"].is_null() && !operation_ids.insert(op["operationId"].as_str()?) {
            return None;
        }
        let reserved = credit(&op["reservedCredits"])?;
        let captured = credit(&op["capturedCredits"])?;
        let released = credit(&op["releasedCredits"])?;
        if captured + released > reserved || (op["outcomeUnknown"] == true && !unknown) {
            return None;
        }
        if op["billingState"].is_null() && (reserved != 0 || captured != 0 || released != 0) {
            return None;
        }
        if matches!(op["billingState"].as_str(), Some("captured" | "released"))
            && (!matches!(
                op["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) || captured + released != reserved
                || reserved == 0
                || (op["billingState"] == "captured" && captured == 0)
                || (op["billingState"] == "released" && captured != 0))
        {
            return None;
        }
        billing.push(json!([op["clientRequestId"], reserved, captured, released]));
    }
    let budget = value.get("budget")?;
    let mut budget_label = String::new();
    if !budget.is_null() {
        if !id(&budget["budget_id"])
            || budget["local_session_id"].as_str() != Some(session_id)
            || !id(&budget["turn_id"])
            || !matches!(
                budget["state"].as_str(),
                Some("active" | "closed" | "expired")
            )
        {
            return None;
        }
        let max = credit(&budget["max_credits"])?;
        let held = credit(&budget["held_credits"])?;
        let spent = credit(&budget["spent_credits"])?;
        let count = budget["operation_count"].as_u64()?;
        let steps = budget["max_steps"].as_u64()?;
        if max == 0 || held + spent > max || steps == 0 || steps > 8 || count > steps {
            return None;
        }
        budget_label = format!("本轮已用 {spent} 积分 · 预留 {held} · 上限 {max}");
        billing.push(json!([budget["budget_id"], held, spent]));
    }
    Some(CheckedModelState {
        revision,
        unknown,
        account_expired,
        budget_label,
        billing: json!(billing),
    })
}
fn max64(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64
}
fn bridge_error(error: BridgeError) -> UiError {
    match error {
        BridgeError::QueueBusy | BridgeError::InputBackpressure => UiError::Busy,
        BridgeError::InvalidLaunch => UiError::Configuration,
        #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
        BridgeError::UnsupportedPlatform => UiError::Configuration,
        BridgeError::InvalidFrame | BridgeError::ProtocolFailed => UiError::Protocol,
        _ => UiError::Connection,
    }
}
fn remote_error(code: &str) -> UiError {
    match code {
        "turn_settling" | "managed_busy" | "session_busy" => UiError::Busy,
        "managed_unknown" | "managed_transport" | "managed_timeout" => UiError::ModelVerifyRequired,
        "managed_protocol" => UiError::Protocol,
        "managed_account_expired" => UiError::AccountExpired,
        "managed_unavailable" | "managed_http" => UiError::ServiceUnavailable,
        "tool_denied" | "notAuthorized" | "approval_expired" => UiError::AuthorizationRequired,
        "project_conflict" | "session_scope" | "instance_conflict" | "managed_scope"
        | "account_scope" => UiError::DocumentChanged,
        "outcome_unknown" => UiError::VerifyRequired,
        "recovery_required" => UiError::SessionRecoveryRequired,
        "provider_configuration" | "invalid_initialize" | "invalid_binding" => {
            UiError::Configuration
        }
        _ => UiError::Connection,
    }
}

#[cfg(all(test, unix, not(any(target_os = "ios", target_os = "android"))))]
mod tests {
    use super::*;
    use std::{fs, thread};
    const FIXTURE: &str = r#"import sys,json,time
scenario='finish';session='';seq=0;managed=False
revision=0
def modelstate(unknown=False):
    global revision
    revision+=1
    return {'revision':revision,'outcomeUnknown':unknown,'accountExpired':False,'budget':None,'operations':[]}
def reply(f,result=None,error=None):
    frame={'version':1,'type':'response','id':f['id']}
    frame['error' if error else 'result']=error if error else result
    print(json.dumps(frame),flush=True)
def event(kind,data={}):
    global seq
    seq+=1
    print(json.dumps({'version':1,'type':'event','event':{'schemaVersion':1,'seq':seq,'eventId':str(seq),'timestamp':'2026-10-05','sessionId':session,'turnId':'turn-1','type':kind,'data':data}}),flush=True)
def final(status='completed',unknown=False,pending=False,failed=False):
    observed=[{'invocationId':'older_move','state':'failed','tool':'move_selected_image'}]
    if pending or failed: observed.append({'invocationId':'turn-1_move','state':'failed' if failed else 'completed','tool':'move_selected_image','appStatus':'notAuthorized' if failed else 'ok','postVerification':'pending' if pending else 'verified'})
    print(json.dumps({'version':1,'type':'result','sessionId':session,'result':{'turnId':'turn-1','status':status,'text':'模型结果 inference-secret read-secret write-secret'},'toolState':{'outcomeUnknown':unknown,'unresolved':[],'recoveryOperations':[],'observed':observed}}),flush=True)
for line in sys.stdin:
    f=json.loads(line);p=f['params'];m=f['method']
    if m=='initialize':
        assert p['protocolVersion']==1 and p['appProtocolVersion']==1
        managed=p['provider']['protocol']=='seecut-managed'
        assert p['readToken']=='read-secret'
        assert p['provider']['sessionToken' if managed else 'apiKey']=='inference-secret'
        assert p['mcpEnvironment']['TMPDIR']==p['sessionDirectory']
        scenario=p['provider']['model'];time.sleep(.08)
        if scenario=='init-fail': reply(f,error={'code':'provider_configuration','message':'raw inference-secret'});continue
        reply(f,{'protocolVersion':1,'instanceId':p['instanceId']})
    elif m=='session.create':
        assert p['projectId']=='project-1' and p['documentSessionId']=='doc-1'
        session=p['sessionId'];time.sleep(.08)
        if managed: print(json.dumps({'version':1,'type':'managed.state','sessionId':session,'modelState':modelstate(True)}),flush=True)
        reply(f,{'sessionId':session if scenario!='bad-session' else 'other','status':'idle','toolState':{'outcomeUnknown':False,'unresolved':[],'recoveryOperations':[],'observed':[]},'modelState':modelstate(True) if managed else None})
    elif m=='reconcile':
        assert managed and p=={'sessionId':session}
        reply(f,{'modelState':modelstate(False)})
    elif m=='send':
        assert p['sessionId']==session and p['mode']=='follow_up' and p['content'][0]['type']=='text'
        if scenario=='settling-once':
            scenario='finish';reply(f,error={'code':'turn_settling','message':'上一轮正在结束，请稍后发送'});continue
        if scenario=='early-result':
            event('turn.started');event('text.delta',{'text':'处理中'});final();reply(f,{'inputId':p['inputId'],'status':'pending'});continue
        reply(f,{'inputId':p['inputId'],'status':'pending'});event('turn.started')
        event('text.delta',{'text':'处理中 read-secret'})
        if scenario in ('approval','stop'):
            event('approval.required',{'approvalId':'approval-1','tool':'move_selected_image','arguments':{'objectId':'layer:1','deltaX':-20,'deltaY':0}})
        elif scenario=='unknown':
            event('tool.outcome_unknown',{'invocationId':'turn-1_move','tool':'move_selected_image'});final('interrupted',True)
        elif scenario=='exit-during-write':
            event('tool.started',{'invocationId':'turn-1_move','call':{'name':'move_selected_image'}});sys.exit(0)
        else: final(pending=scenario=='pending')
    elif m=='approve':
        assert p['approvalId']=='approval-1' and p['sessionId']==session and isinstance(p['allowed'],bool)
        event('approval.resolved',{'approvalId':'approval-1','allowed':p['allowed']})
        reply(f,{'resolved':True,'appAuthorizationChanged':False});final(failed=not p['allowed'])
    elif m=='stop':
        assert p['turnId']=='turn-1' and p['sessionId']==session
        reply(f,{'stopping':True});time.sleep(.15);final('interrupted')
    elif m=='session.close': reply(f,{'closed':True})
"#;
    struct Fixture {
        controller: Controller,
        identity: DocumentIdentity,
        directory: PathBuf,
    }
    impl Fixture {
        fn new(scenario: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let directory = std::env::temp_dir().join(format!(
                "agent-controller-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let entry = directory.join("fixture.py");
            fs::write(&entry, FIXTURE).unwrap();
            let identity = DocumentIdentity {
                instance_id: "12345678-1234-1234-1234-123456789abc".into(),
                project_id: "project-1".into(),
                document_session_id: "doc-1".into(),
            };
            let mut controller = Controller::default();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let result = controller.connect(ConnectionConfig{launch:LaunchSpec{executable:fs::canonicalize("/usr/bin/python3").unwrap(),entrypoint:entry.clone(),environment:BTreeMap::new()},document:identity.clone(),client_id:"client-1".into(),mcp_client_path:entry.clone(),mcp_environment:BTreeMap::from([("TMPDIR".into(),directory.to_string_lossy().into_owned())]),session_directory:directory.clone(),provider:if scenario == "managed" {json!({"protocol":"seecut-managed","model":scenario,"sessionToken":"inference-secret"})} else {json!({"protocol":"fixture","model":scenario,"apiKey":"inference-secret"})},read_token:"read-secret".into(),write_token:Some("write-secret".into())});
                match result {
                    Ok(()) => break,
                    // Parallel fixtures can contend on the nonblocking registration/input
                    // locks. Busy means initialize was not accepted; never retry a send.
                    Err(UiError::Busy) => {
                        controller.shutdown_now();
                        while controller.process.is_some() {
                            controller.tick(Some(&identity));
                            assert!(Instant::now() < deadline, "busy fixture cleanup timed out");
                            thread::yield_now();
                        }
                        controller = Controller::default();
                        assert!(
                            Instant::now() < deadline,
                            "fixture connection remained busy"
                        );
                        thread::yield_now();
                    }
                    Err(error) => panic!("fixture connection failed: {error:?}"),
                }
            }
            Self {
                controller,
                identity,
                directory,
            }
        }
        fn until(&mut self, condition: impl Fn(&Controller) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                self.controller.tick(Some(&self.identity));
                if condition(&self.controller) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!(
                "controller fixture timed out, phase {:?}",
                self.controller.view().phase
            );
        }
        fn ready(&mut self) {
            self.until(|controller| controller.view().phase == Phase::Ready);
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.controller.disconnect();
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.controller.process.is_some() && Instant::now() < deadline {
                self.controller.tick(Some(&self.identity));
                thread::sleep(Duration::from_millis(10));
            }
            assert!(self.controller.process.is_none());
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }
    #[test]
    fn managed_ipc_initialization_and_explicit_original_reconciliation() {
        let mut f = Fixture::new("managed");
        f.until(|c| c.view().can_reconcile);
        assert!(!f.controller.view().can_send);
        f.controller
            .text("inference-secret read-secret write-secret", true);
        assert_eq!(f.controller.view().text, "[已隐藏] [已隐藏] [已隐藏]");
        f.controller.reconcile().unwrap();
        f.ready();
        assert!(f.controller.view().can_send);
        assert!(!f.controller.unknown_latched);
    }
    fn managed_state(unknown: bool) -> Value {
        static REV: AtomicU64 = AtomicU64::new(1);
        json!({"revision":REV.fetch_add(1,Ordering::Relaxed),"outcomeUnknown": unknown, "accountExpired": false, "budget": null, "operations": []})
    }
    fn managed_controller() -> Controller {
        Controller {
            managed_mode: true,
            session_id: "managed-test".into(),
            phase: Phase::Ready,
            ..Controller::default()
        }
    }
    #[test]
    fn delayed_model_snapshots_cannot_undo_newer_unknown_or_budget_state() {
        let mut c = managed_controller();
        let old = managed_state(false);
        let new = managed_state(true);
        assert!(c.apply_model_state(&new));
        assert!(c.apply_model_state(&old));
        assert!(c.model_unknown);
        assert!(!c.view().can_send);
        let mut conflict = new.clone();
        conflict["outcomeUnknown"] = json!(false);
        assert!(!c.apply_model_state(&conflict));
        let clear = managed_state(false);
        assert!(c.apply_model_state(&clear));
        assert!(c.view().can_send);
    }
    #[test]
    fn ambiguous_recovery_error_reads_original_status_without_mcp_latch() {
        let mut f = Fixture::new("managed");
        f.until(|c| c.view().can_reconcile);
        f.controller.pending.push_back(Pending {
            id: "ambiguous".into(),
            request: Request::Send {
                input_id: "input".into(),
            },
            since: Instant::now(),
        });
        f.controller
            .response(json!({"id":"ambiguous","error":{"code":"recovery_required"}}));
        assert!(!f.controller.unknown_latched);
        let id = f
            .controller
            .pending
            .iter()
            .find(|p| matches!(p.request, Request::RecoveryStatus))
            .unwrap()
            .id
            .clone();
        f.controller.response(json!({"id":id,"result":{"sessionId":f.controller.session_id,"toolState":{"outcomeUnknown":false},"modelState":managed_state(true)}}));
        assert!(!f.controller.recovery_pending);
        assert!(!f.controller.unknown_latched);
        assert!(f.controller.view().can_reconcile);
    }
    #[test]
    fn recovery_status_idle_clears_temporary_gate_but_interrupted_requires_new_session() {
        for status in ["idle", "interrupted"] {
            let mut c = managed_controller();
            c.recovery_pending = true;
            c.pending.push_back(Pending {
                id: "status".into(),
                request: Request::RecoveryStatus,
                since: Instant::now(),
            });
            c.response(json!({"id":"status","result":{"sessionId":"managed-test","status":status,"activeTurnId":null,
                "toolState":{"outcomeUnknown":false},"modelState":managed_state(false)}}));
            assert_eq!(c.view().can_send, status == "idle");
            assert_eq!(c.recovery_pending, status != "idle");
            assert!(!c.unknown_latched);
        }
    }
    #[test]
    fn opening_session_older_create_reply_does_not_clear_newer_model_unknown() {
        let mut c = managed_controller();
        c.phase = Phase::OpeningSession;
        let old = managed_state(false);
        let new = managed_state(true);
        c.pending.push_back(Pending {
            id: "create".into(),
            request: Request::Create,
            since: Instant::now(),
        });
        c.handle_event(Event::Managed(
            json!({"sessionId":"managed-test","modelState":new}),
        ));
        assert_eq!(c.view().phase, Phase::OpeningSession);
        assert!(!c.view().can_send);
        c.response(
            json!({"id":"create","result":{"sessionId":"managed-test","status":"idle",
            "toolState":{"outcomeUnknown":false},"modelState":old}}),
        );
        assert!(c.model_unknown);
        assert!(!c.view().can_send);
    }
    #[test]
    fn late_unknown_terminal_status_and_reconcile_use_latest_clear_state() {
        for source in ["terminal", "status", "reconcile"] {
            let mut c = managed_controller();
            let old = managed_state(true);
            let new = managed_state(false);
            assert!(c.apply_model_state(&new));
            let billing = c.view().billing_revision;
            if source == "terminal" {
                c.turn_id = Some("turn-1".into());
                c.phase = Phase::Running;
                c.result(&json!({"sessionId":"managed-test","result":{"turnId":"turn-1","text":"complete","status":"completed"},
                    "toolState":{"outcomeUnknown":false,"observed":[]},"modelState":old}));
            } else {
                let request = if source == "status" {
                    c.recovery_pending = true;
                    Request::RecoveryStatus
                } else {
                    Request::Reconcile
                };
                c.pending.push_back(Pending {
                    id: "late".into(),
                    request,
                    since: Instant::now(),
                });
                c.response(json!({"id":"late","result":{"sessionId":"managed-test","status":"idle","activeTurnId":null,
                    "toolState":{"outcomeUnknown":false},"modelState":old}}));
            }
            assert!(!c.model_unknown, "{source}");
            assert!(c.view().can_send, "{source}");
            assert_eq!(c.view().billing_revision, billing);
        }
    }
    #[test]
    fn managed_reconciliation_cannot_clear_mcp_unknown() {
        let mut c = managed_controller();
        assert!(c.apply_model_state(&managed_state(true)));
        assert!(!c.view().can_send);
        assert_eq!(
            c.send_text("new question"),
            Err(UiError::ModelVerifyRequired)
        );
        c.unknown();
        assert!(c.apply_model_state(&managed_state(false)));
        assert!(!c.model_unknown);
        assert!(c.unknown_latched);
        assert!(!c.view().can_send);
        assert_eq!(c.view().error, Some(UiError::VerifyRequired));
        assert_eq!(c.view().phase, Phase::OutcomeUnknown);
    }
    #[test]
    fn authoritative_model_state_clears_only_model_fence_and_expiry_stays_sticky() {
        let mut c = managed_controller();
        assert!(c.apply_model_state(&managed_state(true)));
        assert_eq!(c.view().phase, Phase::OutcomeUnknown);
        assert!(c.apply_model_state(&managed_state(false)));
        assert!(c.view().can_send);
        let mut expired = managed_state(false);
        expired["accountExpired"] = json!(true);
        assert!(c.apply_model_state(&expired));
        assert!(c.apply_model_state(&managed_state(false)));
        assert!(c.view().account_expired);
        assert_eq!(c.send_text("new question"), Err(UiError::AccountExpired));
    }
    #[test]
    fn managed_budget_is_bounded_and_only_financial_changes_increment_revision() {
        let mut c = managed_controller();
        let mut state = managed_state(false);
        state["budget"] = json!({"budget_id":"budget-1","local_session_id":"managed-test","turn_id":"turn-1",
            "state":"active","max_credits":100,"held_credits":20,"spent_credits":5,"max_steps":8,"operation_count":1});
        assert!(c.apply_model_state(&state));
        let revision = c.view().billing_revision;
        assert_eq!(
            c.view().budget_label,
            "本轮已用 5 积分 · 预留 20 · 上限 100"
        );
        state["outcomeUnknown"] = json!(true);
        state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().billing_revision, revision);
        state["budget"]["held_credits"] = json!(0);
        state["budget"]["spent_credits"] = json!(12);
        state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().billing_revision, revision + 1);
        for (key, value) in [
            ("max_steps", json!(9)),
            ("held_credits", json!(101)),
            ("local_session_id", json!("other")),
            ("spent_credits", json!(-1)),
        ] {
            let mut invalid = state.clone();
            invalid["budget"][key] = value;
            assert!(!c.apply_model_state(&invalid), "{key}");
        }
    }
    #[test]
    fn malformed_operations_cannot_unlock_or_supply_display_text() {
        let mut c = managed_controller();
        assert!(c.apply_model_state(&managed_state(true)));
        let mut state = managed_state(false);
        state["operations"] = json!([{"clientRequestId":"op-1","operationId":null,"turnId":"turn-1","state":"unknown",
            "billingState":null,"reservedCredits":0,"capturedCredits":0,"releasedCredits":0,"outcomeUnknown":true,"doneDelivered":false}]);
        assert!(!c.apply_model_state(&state));
        assert!(c.model_unknown);
        state["outcomeUnknown"] = json!(true);
        state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
        assert!(c.apply_model_state(&state));
        state["operations"][0]["state"] = json!("token-secret");
        assert!(!c.apply_model_state(&state));
        assert!(!c.view().text.contains("token-secret"));
    }
    #[test]
    fn managed_operation_billing_accepts_pending_settlement_and_rejects_invalid_amounts() {
        let mut state = managed_state(true);
        state["operations"] = json!([{"clientRequestId":"op-1","operationId":"operation-1","turnId":"older-turn",
            "state":"succeeded","billingState":"settlement_pending","reservedCredits":10,"capturedCredits":0,
            "releasedCredits":0,"outcomeUnknown":true,"doneDelivered":false}]);
        assert!(checked_model_state(&state, "managed-test").is_some());
        state["operations"][0]["billingState"] = json!("captured");
        state["operations"][0]["capturedCredits"] = json!(3);
        state["operations"][0]["releasedCredits"] = json!(7);
        assert!(checked_model_state(&state, "managed-test").is_some());
        for (key, value) in [
            ("state", json!("running")),
            ("capturedCredits", json!(0)),
            ("releasedCredits", json!(8)),
            ("billingState", json!(null)),
        ] {
            let mut invalid = state.clone();
            invalid["operations"][0][key] = value;
            assert!(
                checked_model_state(&invalid, "managed-test").is_none(),
                "{key}"
            );
        }
        let mut missing = state.clone();
        missing["operations"][0]
            .as_object_mut()
            .unwrap()
            .remove("operationId");
        assert!(checked_model_state(&missing, "managed-test").is_none());
        let mut duplicate = state.clone();
        let mut second = duplicate["operations"][0].clone();
        second["clientRequestId"] = json!("op-2");
        duplicate["operations"].as_array_mut().unwrap().push(second);
        assert!(checked_model_state(&duplicate, "managed-test").is_none());
    }
    #[test]
    fn managed_unknown_during_approval_retains_stop_but_blocks_approval() {
        let mut c = managed_controller();
        c.turn_id = Some("turn-1".into());
        c.phase = Phase::AwaitingApproval;
        c.approval = Some(Approval {
            id: "approval-1".into(),
            turn_id: "turn-1".into(),
            object_id: None,
            delta_x: None,
            delta_y: None,
        });
        assert!(c.apply_model_state(&managed_state(true)));
        assert!(c.view().can_stop);
        assert!(!c.view().can_approve);
        assert_eq!(
            c.approve("approval-1", true),
            Err(UiError::ModelVerifyRequired)
        );
        assert!(c.apply_model_state(&managed_state(false)));
        assert!(c.view().can_approve);
    }
    #[test]
    fn runtime_disconnect_marks_model_unknown_without_inventing_mcp_unknown() {
        let mut c = managed_controller();
        c.turn_id = Some("turn-1".into());
        c.handle_event(Event::Exit {
            code: Some(0),
            requested: false,
        });
        assert!(c.model_unknown);
        assert!(!c.unknown_latched);
        assert_eq!(c.view().error, Some(UiError::ModelVerifyRequired));
    }
    #[test]
    fn managed_model_terminal_state_does_not_replay_old_turn() {
        let mut c = managed_controller();
        c.turn_id = Some("turn-1".into());
        c.phase = Phase::Running;
        c.result(&json!({"sessionId":"managed-test","result":{"turnId":"turn-1","text":"partial","status":"interrupted"},
            "toolState":{"outcomeUnknown":false,"observed":[]},"modelState":managed_state(true)}));
        assert_eq!(c.view().outcome, Some(TaskOutcome::OutcomeUnknown));
        assert!(c.turn_id.is_none());
        assert!(c.apply_model_state(&managed_state(false)));
        assert!(c.view().can_send);
        assert!(c.pending.is_empty());
        assert_eq!(c.view().text, "partial");
    }
    fn display_controller() -> Controller {
        Controller {
            session_id: "display-session".into(),
            turn_id: Some("display-turn".into()),
            phase: Phase::Running,
            ..Controller::default()
        }
    }
    fn display_event(controller: &mut Controller, seq: u64, kind: &str, data: Value) {
        controller.runtime_event(&json!({"sessionId":"display-session", "turnId":"display-turn", "seq":seq, "type":kind, "data":data}));
    }
    #[test]
    fn tool_progress_uses_fixed_labels_and_settled_invocations_do_not_reopen() {
        let mut c = display_controller();
        display_event(
            &mut c,
            1,
            "tool.started",
            json!({"invocationId":"one", "call":{"name":"context", "arguments":{"private":"do-not-display"}}}),
        );
        display_event(
            &mut c,
            2,
            "tool.progress",
            json!({"invocationId":"one", "progress":{"message":"secret-path /private/token"}}),
        );
        display_event(
            &mut c,
            3,
            "tool.started",
            json!({"invocationId":"one", "call":{"name":"context"}}),
        );
        assert_eq!(c.view().progress, "读取当前画布…");
        display_event(
            &mut c,
            4,
            "tool.completed",
            json!({"invocationId":"one", "message":{"toolName":"context", "content":"private-result"}}),
        );
        display_event(
            &mut c,
            5,
            "tool.progress",
            json!({"invocationId":"one", "progress":"private"}),
        );
        display_event(
            &mut c,
            6,
            "tool.failed",
            json!({"invocationId":"one", "message":{"toolName":"context"}}),
        );
        assert_eq!(c.view().progress, "读取当前画布 · 已返回");
        assert_eq!(c.tool_progress.len(), 1);
        display_event(
            &mut c,
            7,
            "text.delta",
            json!({"text":"tool.started fake model text"}),
        );
        assert_eq!(c.view().progress, "读取当前画布 · 已返回");
        assert_eq!(c.view().text, "tool.started fake model text");
    }
    #[test]
    fn tool_progress_ignores_unscoped_old_duplicate_and_post_terminal_events() {
        let mut c = display_controller();
        let data = json!({"invocationId":"one", "call":{"name":"preview"}});
        c.runtime_event(
            &json!({"sessionId":"display-session", "seq":1, "type":"tool.started", "data":data}),
        );
        c.runtime_event(&json!({"sessionId":"display-session", "turnId":"old", "seq":2, "type":"tool.started", "data":data}));
        assert!(c.view().progress.is_empty());
        display_event(&mut c, 3, "tool.started", data);
        display_event(
            &mut c,
            3,
            "tool.failed",
            json!({"invocationId":"one", "message":{"toolName":"preview"}}),
        );
        assert_eq!(c.view().progress, "查看画布预览…");
        c.result(&json!({"sessionId":"display-session", "result":{"turnId":"display-turn", "status":"completed", "text":"final"}, "toolState":{"observed":[]}}));
        let retained = c.view().progress.to_owned();
        display_event(
            &mut c,
            4,
            "tool.failed",
            json!({"invocationId":"one", "message":{"toolName":"preview"}}),
        );
        assert_eq!(c.view().progress, retained);
        c.runtime_event(&json!({"sessionId":"other-session", "turnId":"display-turn", "seq":5, "type":"tool.started", "data":{"invocationId":"other", "call":{"name":"project"}}}));
        assert_eq!(c.view().progress, retained);
        assert_eq!(c.view().error, Some(UiError::Protocol));
    }
    #[test]
    fn tool_progress_caps_steps_ignores_unknown_names_and_retains_failure() {
        let mut c = display_controller();
        display_event(
            &mut c,
            1,
            "tool.started",
            json!({"invocationId":"unknown", "call":{"name":"/private/secret"}}),
        );
        display_event(
            &mut c,
            2,
            "tool.progress",
            json!({"invocationId":"unknown", "progress":"arbitrary"}),
        );
        assert!(c.view().progress.is_empty());
        display_event(
            &mut c,
            3,
            "tool.failed",
            json!({"invocationId":"denied", "message":{"toolName":"move_selected_image"}}),
        );
        assert_eq!(c.view().progress, "发送移动请求 · 失败");
        for index in 0..MAX_TOOL_PROGRESS + 8 {
            display_event(
                &mut c,
                index as u64 + 4,
                "tool.started",
                json!({"invocationId":format!("step-{index}"), "call":{"name":"capabilities"}}),
            );
        }
        assert_eq!(c.tool_progress.len(), MAX_TOOL_PROGRESS);
        assert_eq!(c.view().progress.lines().count(), MAX_TOOL_PROGRESS);
        assert!(c.view().progress.len() < 1024);
        display_event(
            &mut c,
            100,
            "tool.cancelled",
            json!({"invocationId":"step-0", "executed":false, "message":{"toolName":"capabilities"}}),
        );
        assert!(c.view().progress.contains("检查可用操作 · 已取消"));
    }
    #[test]
    fn pending_move_receipt_keeps_text_streaming_until_final_result() {
        let mut c = display_controller();
        display_event(
            &mut c,
            1,
            "tool.started",
            json!({"invocationId":"display-turn_move", "call":{"name":"move_selected_image"}}),
        );
        display_event(
            &mut c,
            2,
            "tool.completed",
            json!({"invocationId":"display-turn_move", "message":{"toolName":"move_selected_image"}, "metadata":{"postVerification":"pending"}}),
        );
        assert_eq!(c.view().phase, Phase::Running);
        assert_eq!(
            c.view().outcome,
            Some(TaskOutcome::SubmittedPendingVerification)
        );
        assert_eq!(c.view().progress, "移动请求已返回");
        display_event(&mut c, 3, "text.delta", json!({"text":"仍需核对画布"}));
        assert_eq!(c.view().text, "仍需核对画布");
        assert_eq!(
            c.view().outcome,
            Some(TaskOutcome::SubmittedPendingVerification)
        );
        c.result(&json!({"sessionId":"display-session", "result":{"turnId":"display-turn", "status":"completed", "text":"最终回复，待验证"}, "toolState":{"observed":[{"invocationId":"display-turn_move", "postVerification":"pending", "state":"completed"}]}}));
        assert_eq!(c.view().text, "最终回复，待验证");
        assert_eq!(c.view().phase, Phase::Ready);
        assert_eq!(
            c.view().outcome,
            Some(TaskOutcome::SubmittedPendingVerification)
        );
        assert_eq!(c.view().progress, "移动请求已返回");
        assert!(!c.write_in_flight);
    }
    #[test]
    fn next_accepted_question_resets_only_current_progress() {
        let mut fixture = Fixture::new("finish");
        fixture.ready();
        fixture.controller.tool_progress.push(ToolProgress {
            id: "old".into(),
            action: ToolAction::Context,
            state: ToolProgressState::Completed,
        });
        fixture.controller.progress = "读取当前画布 · 已返回".into();
        fixture.controller.send_text("new question").unwrap();
        assert!(fixture.controller.tool_progress.is_empty());
        assert!(fixture.controller.view().progress.is_empty());
        fixture.until(|c| c.view().outcome.is_some());
    }
    #[test]
    fn connection_waits_for_both_real_responses_and_never_replays() {
        let mut fixture = Fixture::new("finish");
        assert_eq!(fixture.controller.view().phase, Phase::Initializing);
        assert!(!fixture.controller.view().can_send);
        fixture.until(|controller| controller.view().phase == Phase::OpeningSession);
        assert!(!fixture.controller.view().can_send);
        fixture.ready();
        fixture.controller.send_text("读取当前工程").unwrap();
        assert!(!fixture.controller.view().can_send);
        assert_eq!(fixture.controller.send_text("重复点击"), Err(UiError::Busy));
        fixture.until(|controller| controller.view().outcome == Some(TaskOutcome::Completed));
        assert!(fixture.controller.view().can_send);
        assert!(!fixture.controller.view().text.contains("secret"));
    }
    #[test]
    fn settling_response_keeps_connection_and_allows_explicit_retry() {
        let mut fixture = Fixture::new("settling-once");
        fixture.ready();
        fixture.controller.send_text("新问题").unwrap();
        fixture.until(|controller| controller.view().error == Some(UiError::Busy));
        assert_eq!(fixture.controller.view().phase, Phase::Ready);
        assert!(fixture.controller.view().can_send);
        assert!(!fixture.controller.view().can_connect);
        assert_eq!(
            fixture.controller.view().error.unwrap().message(),
            "请求正在处理，请稍后操作"
        );
        fixture.controller.send_text("用户明确重试").unwrap();
        fixture.until(|controller| controller.view().outcome == Some(TaskOutcome::Completed));
        assert!(fixture.controller.view().can_send);
        assert_eq!(fixture.controller.view().error, None);
    }
    #[test]
    fn early_result_before_send_receipt_stays_completed() {
        let mut fixture = Fixture::new("early-result");
        fixture.ready();
        fixture.controller.send_text("测试").unwrap();
        fixture.until(|controller| controller.view().can_send);
        assert_eq!(
            fixture.controller.view().outcome,
            Some(TaskOutcome::Completed)
        );
    }
    #[test]
    fn failed_handshake_does_not_expose_provider_error_or_credentials() {
        let mut fixture = Fixture::new("init-fail");
        fixture.until(|controller| controller.view().phase == Phase::Failed);
        assert_eq!(
            fixture.controller.view().error,
            Some(UiError::Configuration)
        );
        assert_eq!(fixture.controller.view().text, "");
        assert!(!fixture.controller.view().can_send);
    }
    #[test]
    fn wrong_session_reply_is_rejected() {
        let mut fixture = Fixture::new("bad-session");
        fixture.until(|controller| controller.view().phase == Phase::Failed);
        assert_eq!(fixture.controller.view().error, Some(UiError::Protocol));
    }
    fn account_expired_closing_write() -> Controller {
        let mut c = display_controller();
        c.managed_mode = true;
        display_event(
            &mut c,
            1,
            "tool.started",
            json!({"invocationId":"closing-write", "call":{"name":"move_selected_image"}}),
        );
        let mut expired = managed_state(false);
        expired["accountExpired"] = json!(true);
        assert!(c.apply_model_state(&expired));
        c.disconnect();
        c
    }
    fn closing_not_sent() -> Value {
        json!({"version":1,"type":"event","event":{"sessionId":"display-session","turnId":"display-turn","seq":2,
            "type":"tool.failed","data":{"invocationId":"closing-write","message":{"toolName":"move_selected_image","isError":true},
            "metadata":{"appStatus":"not_sent","reasonCode":"managed_account_expired"}}}})
    }
    #[test]
    fn closing_not_sent_receipt_preserves_account_fence_without_false_edit_unknown() {
        let mut c = account_expired_closing_write();
        c.handle_event(Event::Runtime(closing_not_sent()));
        assert!(!c.write_in_flight);
        assert!(c.account_expired);
        assert!(c.model_unknown);
        assert!(c.ignore_runtime_events);
        assert!(c.approval.is_none());
        assert!(c.pending.is_empty());
        c.handle_event(Event::Exit {
            code: Some(0),
            requested: true,
        });
        assert!(!c.unknown_latched);
        assert_eq!(c.view().error, Some(UiError::AccountExpired));
        assert!(c.view().can_connect);
        assert_eq!(
            c.approve("old-approval", true),
            Err(UiError::AccountExpired)
        );
        // A later start or approval never reopens the old turn.
        c.handle_event(Event::Runtime(json!({"event":{"sessionId":"display-session","turnId":"display-turn","seq":3,
            "type":"approval.required","data":{"approvalId":"stale","tool":"move_selected_image","arguments":{}}}})));
        assert!(c.approval.is_none());
        assert!(c.pending.is_empty());
    }
    #[test]
    fn closing_not_sent_rejects_mismatches_and_preserves_real_unknown() {
        for key in [
            "session",
            "turn",
            "invocation",
            "sequence",
            "tool",
            "status",
            "error",
            "kind",
            "missing",
        ] {
            let mut c = account_expired_closing_write();
            let mut frame = closing_not_sent();
            match key {
                "session" => frame["event"]["sessionId"] = json!("other"),
                "turn" => frame["event"]["turnId"] = json!("other"),
                "invocation" => frame["event"]["data"]["invocationId"] = json!("other"),
                "sequence" => frame["event"]["seq"] = json!(1),
                "tool" => frame["event"]["data"]["message"]["toolName"] = json!("context"),
                "status" => {
                    frame["event"]["data"]["metadata"]["appStatus"] = json!("outcomeUnknown")
                }
                "error" => frame["event"]["data"]["message"]["isError"] = json!(false),
                "kind" => frame["event"]["type"] = json!("tool.completed"),
                "missing" => frame["event"]["data"]["metadata"] = Value::Null,
                _ => unreachable!(),
            }
            c.handle_event(Event::Runtime(frame));
            assert!(c.write_in_flight, "{key}");
            c.handle_event(Event::Exit {
                code: Some(0),
                requested: true,
            });
            assert!(c.unknown_latched, "{key}");
            assert!(!c.view().can_connect, "{key}");
        }
        let mut c = account_expired_closing_write();
        c.unknown();
        c.handle_event(Event::Runtime(closing_not_sent()));
        c.handle_event(Event::Exit {
            code: Some(0),
            requested: true,
        });
        assert!(c.unknown_latched);
        assert!(!c.view().can_connect);
    }
    #[test]
    fn closing_unknown_and_sequence_evidence_cannot_be_overridden_by_not_sent() {
        for kind in ["tool.outcome_unknown", "recovery.required"] {
            for refusal_seq in [2, 4] {
                let mut c = account_expired_closing_write();
                c.handle_event(Event::Runtime(
                    json!({"event":{"sessionId":"display-session", "turnId":"display-turn", "seq":3,
                    "type":kind,"data":{"invocationId":"closing-write"}}}),
                ));
                let mut refusal = closing_not_sent();
                refusal["event"]["seq"] = json!(refusal_seq);
                c.handle_event(Event::Runtime(refusal));
                c.handle_event(Event::Exit {
                    code: Some(0),
                    requested: true,
                });
                assert!(c.unknown_latched, "{kind}/{refusal_seq}");
                assert!(!c.view().can_connect);
                assert!(c.model_unknown && c.account_expired);
            }
        }
        let mut c = account_expired_closing_write();
        c.handle_event(Event::Runtime(json!({"event":{"sessionId":"display-session","seq":3,"type":"text.delta","data":{"text":"ignored"}}})));
        c.handle_event(Event::Runtime(closing_not_sent()));
        assert_eq!(c.last_seq, 3);
        assert!(c.write_in_flight);
        assert!(c.text.is_empty());
        let mut c = account_expired_closing_write();
        c.handle_event(Event::Result(json!({"sessionId":"display-session","result":{"turnId":"display-turn"},"toolState":{"outcomeUnknown":true}})));
        c.handle_event(Event::Runtime(closing_not_sent()));
        assert!(c.unknown_latched);
        let mut c = account_expired_closing_write();
        c.handle_event(Event::Runtime(closing_not_sent()));
        c.handle_event(Event::Runtime(closing_not_sent()));
        assert_eq!(c.last_seq, 2);
        assert!(!c.write_in_flight);
        assert!(c.pending.is_empty() && c.approval.is_none());
    }
    #[test]
    fn approval_receipt_requires_exact_false_boolean() {
        for changed in [
            json!(false),
            json!(true),
            json!("[已隐藏]"),
            json!(null),
            json!({}),
        ] {
            let mut controller = display_controller();
            controller.phase = Phase::AwaitingApproval;
            controller.approval = Some(Approval {
                id: "approval-wire".into(),
                turn_id: "display-turn".into(),
                object_id: Some("layer:1".into()),
                delta_x: Some(12.0),
                delta_y: Some(0.0),
            });
            controller.pending.push_back(Pending {
                id: "reply-wire".into(),
                request: Request::Approve {
                    approval_id: "approval-wire".into(),
                },
                since: Instant::now(),
            });
            controller.response(json!({"id":"reply-wire", "result": {
                "resolved":true, "appAuthorizationChanged":changed,
            }}));
            if changed == json!(false) {
                assert_eq!(controller.view().phase, Phase::Running);
                assert_eq!(controller.view().error, None);
            } else {
                assert_eq!(controller.view().phase, Phase::Failed);
                assert_eq!(controller.view().error, Some(UiError::Protocol));
                assert!(controller.ignore_runtime_events);
            }
            assert!(controller.view().approval.is_none());
            assert!(controller.view().outcome.is_none());
        }
    }
    #[test]
    fn approval_is_current_only_and_false_does_not_claim_edit_success() {
        let mut fixture = Fixture::new("approval");
        fixture.ready();
        fixture.controller.send_text("向左移").unwrap();
        fixture.until(|controller| controller.view().phase == Phase::AwaitingApproval);
        assert_eq!(
            fixture.controller.approve("wrong", true),
            Err(UiError::AuthorizationRequired)
        );
        fixture.controller.approve("approval-1", false).unwrap();
        assert_eq!(
            fixture.controller.approve("approval-1", true),
            Err(UiError::Busy)
        );
        fixture.until(|controller| controller.view().outcome.is_some());
        assert_eq!(fixture.controller.view().outcome, Some(TaskOutcome::Failed));
        assert!(fixture.controller.view().approval.is_none());
    }
    #[test]
    fn stop_phase_changes_only_after_ack_and_completion_is_actual() {
        let mut fixture = Fixture::new("stop");
        fixture.ready();
        fixture.controller.send_text("向左移").unwrap();
        fixture.until(|controller| controller.view().phase == Phase::AwaitingApproval);
        fixture.controller.stop().unwrap();
        assert_eq!(fixture.controller.view().phase, Phase::AwaitingApproval);
        assert!(!fixture.controller.view().can_stop);
        fixture.until(|controller| controller.view().phase == Phase::Stopping);
        assert!(fixture.controller.view().outcome.is_none());
        fixture.until(|controller| controller.view().outcome == Some(TaskOutcome::Stopped));
    }
    #[test]
    fn document_change_closes_process_requires_explicit_reconnect() {
        let mut fixture = Fixture::new("finish");
        fixture.ready();
        fixture.identity.project_id = "project-2".into();
        fixture.controller.tick(Some(&fixture.identity));
        assert_eq!(fixture.controller.view().phase, Phase::Closing);
        assert_eq!(
            fixture.controller.view().error,
            Some(UiError::DocumentChanged)
        );
        fixture.until(|controller| controller.view().phase == Phase::Disconnected);
        assert!(!fixture.controller.view().can_send);
    }
    #[test]
    fn submitted_pending_verification_is_distinct_from_unknown() {
        let mut fixture = Fixture::new("pending");
        fixture.ready();
        fixture.controller.send_text("移动").unwrap();
        fixture.until(|controller| controller.view().outcome.is_some());
        assert_eq!(
            fixture.controller.view().outcome,
            Some(TaskOutcome::SubmittedPendingVerification)
        );
        assert_eq!(fixture.controller.view().phase, Phase::Ready);
    }
    #[test]
    fn unknown_blocks_later_send_and_stays_latched_after_disconnect() {
        let mut fixture = Fixture::new("unknown");
        fixture.ready();
        fixture.controller.send_text("移动").unwrap();
        fixture.until(|controller| controller.view().phase == Phase::OutcomeUnknown);
        assert_eq!(
            fixture.controller.send_text("再试"),
            Err(UiError::VerifyRequired)
        );
        fixture.controller.disconnect();
        fixture.until(|controller| controller.process.is_none());
        assert_eq!(fixture.controller.view().phase, Phase::OutcomeUnknown);
    }
    #[test]
    fn runtime_exit_during_dispatched_write_is_unknown() {
        let mut fixture = Fixture::new("exit-during-write");
        fixture.ready();
        fixture.controller.send_text("移动").unwrap();
        fixture.until(|controller| controller.process.is_none());
        assert_eq!(fixture.controller.view().phase, Phase::OutcomeUnknown);
        assert_eq!(
            fixture.controller.view().error,
            Some(UiError::VerifyRequired)
        );
    }
    #[test]
    fn failed_phase_does_not_mean_process_already_reaped() {
        let mut fixture = Fixture::new("finish");
        fixture.ready();
        fixture.controller.fail(UiError::Connection);
        assert_eq!(fixture.controller.view().phase, Phase::Failed);
        assert!(!fixture.controller.view().can_connect);
        fixture.until(|controller| controller.process.is_none());
        assert!(fixture.controller.view().can_connect);
    }
    #[test]
    fn shutdown_now_is_nonblocking_and_does_not_wait_for_close_reply() {
        let mut fixture = Fixture::new("finish");
        fixture.ready();
        let started = Instant::now();
        fixture.controller.shutdown_now();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(fixture.controller.view().phase, Phase::Closing);
        assert!(fixture.controller.pending.is_empty());
        assert!(!fixture.controller.view().can_connect);
        fixture.until(|controller| controller.process.is_none());
        assert!(fixture.controller.view().can_connect);
    }
    #[test]
    fn transcript_is_bounded_on_utf8_boundaries() {
        let mut controller = Controller {
            secrets: vec!["secret".into()],
            ..Controller::default()
        };
        controller.text(&("界".repeat(30_000) + "secret"), false);
        assert!(controller.view().text.len() <= TEXT_LIMIT);
        assert!(!controller.view().text.contains("secret"));
        assert!(controller.view().text.ends_with("[已隐藏]"));
    }

    #[test]
    fn empty_failed_or_stopped_result_retains_observed_reply() {
        for status in ["failed", "interrupted"] {
            let mut controller = Controller {
                session_id: "session".into(),
                turn_id: Some("turn".into()),
                text: "已收到的部分回复".into(),
                ..Controller::default()
            };
            controller.result(&json!({"sessionId":"session", "result":{
                "turnId":"turn", "status":status, "text":""
            }, "toolState":{"outcomeUnknown":false,"observed":[]}}));
            assert_eq!(controller.view().text, "已收到的部分回复");
            assert!(controller.turn_id.is_none());
            assert!(!controller.view().can_stop);
        }
    }
}
