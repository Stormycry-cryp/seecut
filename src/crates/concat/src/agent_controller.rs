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
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, Eq)]
pub struct DocumentIdentity {
    pub instance_id: String,
    pub project_id: String,
    pub document_session_id: String,
}
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
}
impl UiError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Configuration => "请检查助手运行时和模型配置",
            Self::Connection => "助手连接已断开，请重新连接",
            Self::Protocol => "助手协议不兼容，请检查运行时版本",
            Self::Busy => "请求正在处理，请稍后操作",
            Self::AuthorizationRequired => "请在 App 中检查当前工程的授权",
            Self::DocumentChanged => "工程已改变，请重新连接当前工程",
            Self::VerifyRequired => "操作结果待核实，已暂停后续请求",
            Self::ResponseTimeout => "助手未及时回应，请重新连接",
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
    pub approval: Option<&'a Approval>,
    pub outcome: Option<TaskOutcome>,
    pub error: Option<UiError>,
    pub can_send: bool,
    pub can_stop: bool,
    pub can_approve: bool,
    pub can_connect: bool,
}
enum Request {
    Initialize,
    Create,
    Send { input_id: String },
    Stop,
    Approve { approval_id: String },
    Close,
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
    approval: Option<Approval>,
    outcome: Option<TaskOutcome>,
    error: Option<UiError>,
    unknown_latched: bool,
    ignore_runtime_events: bool,
    secrets: Vec<String>,
    write_in_flight: bool,
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
            approval: None,
            outcome: None,
            error: None,
            unknown_latched: false,
            ignore_runtime_events: false,
            secrets: Vec::new(),
            write_in_flight: false,
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
        self.text.clear();
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
            phase: self.phase,
            text: &self.text,
            approval: self.approval.as_ref(),
            outcome: self.outcome,
            error: self.error,
            can_send: self.phase == Phase::Ready
                && self.pending.is_empty()
                && !self.unknown_latched,
            can_stop: self.turn_id.is_some()
                && !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::Stop))
                && matches!(self.phase, Phase::Running | Phase::AwaitingApproval),
            can_approve: self.phase == Phase::AwaitingApproval
                && self.approval.is_some()
                && !self.unknown_latched
                && !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::Stop | Request::Approve { .. })),
        }
    }
    pub fn send_text(&mut self, text: &str) -> Result<(), UiError> {
        if self.unknown_latched {
            return Err(UiError::VerifyRequired);
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
        if self.unknown_latched {
            return Err(UiError::VerifyRequired);
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
    /// Stop the old runtime and close its session. Reconnection requires another explicit connect.
    pub fn disconnect(&mut self) {
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
            {
                if let Some(process) = &self.process {
                    process.request_shutdown();
                }
            }
        } else {
            self.phase = Phase::Disconnected;
        }
    }
    /// App exit may use this immediately, without waiting for a session.close response.
    /// The actual process-exit wait is agent_process::shutdown_all_at_exit, never this UI call.
    pub fn shutdown_now(&mut self) {
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
        if self.write_in_flight {
            self.unknown();
        }
        self.error = Some(if self.unknown_latched {
            UiError::VerifyRequired
        } else {
            error
        });
        self.phase = if self.unknown_latched {
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
            Event::RuntimeEvent(value) if !self.ignore_runtime_events => {
                self.runtime_event(&value["event"])
            }
            Event::Result(value) if !self.ignore_runtime_events => self.result(&value),
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
                if self.write_in_flight {
                    self.unknown();
                }
                let was_closing = self.phase == Phase::Closing;
                self.process.take();
                self.pending.clear();
                self.approval = None;
                self.turn_id = None;
                self.secrets.clear();
                if self.unknown_latched {
                    self.phase = Phase::OutcomeUnknown;
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
            let error = remote_error(code);
            if error == UiError::VerifyRequired {
                self.unknown();
                return;
            }
            match pending.request {
                Request::Send { .. } if !self.unknown_latched => {
                    self.phase = Phase::Ready;
                    self.error = Some(error);
                }
                Request::Approve { .. } | Request::Stop => {
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
                if result["toolState"]["outcomeUnknown"].as_bool() == Some(true) {
                    self.unknown();
                } else {
                    self.phase = Phase::Ready;
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
        match kind {
            "tool.started" => {
                if data["call"]["name"].as_str() == Some("move_selected_image") {
                    self.write_in_flight = true;
                }
            }
            "tool.completed" | "tool.failed" => {
                if data["message"]["toolName"].as_str() == Some("move_selected_image") {
                    self.write_in_flight = false;
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
                if self.unknown_latched {
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
            self.text(text, true);
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
        self.phase = Phase::Ready;
    }
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
        "tool_denied" | "notAuthorized" | "approval_expired" => UiError::AuthorizationRequired,
        "project_conflict" | "session_scope" | "instance_conflict" => UiError::DocumentChanged,
        "recovery_required" | "outcome_unknown" => UiError::VerifyRequired,
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
scenario='finish';session='';seq=0
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
        assert p['readToken']=='read-secret' and p['provider']['apiKey']=='inference-secret'
        assert p['mcpEnvironment']['TMPDIR']==p['sessionDirectory']
        scenario=p['provider']['model'];time.sleep(.08)
        if scenario=='init-fail': reply(f,error={'code':'provider_configuration','message':'raw inference-secret'});continue
        reply(f,{'protocolVersion':1,'instanceId':p['instanceId']})
    elif m=='session.create':
        assert p['projectId']=='project-1' and p['documentSessionId']=='doc-1'
        session=p['sessionId'];time.sleep(.08)
        reply(f,{'sessionId':session if scenario!='bad-session' else 'other','status':'idle','toolState':{'outcomeUnknown':False,'unresolved':[],'recoveryOperations':[],'observed':[]}})
    elif m=='send':
        assert p['sessionId']==session and p['mode']=='follow_up' and p['content'][0]['type']=='text'
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
            controller.connect(ConnectionConfig{launch:LaunchSpec{executable:fs::canonicalize("/usr/bin/python3").unwrap(),entrypoint:entry.clone(),environment:BTreeMap::new()},document:identity.clone(),client_id:"client-1".into(),mcp_client_path:entry,mcp_environment:BTreeMap::from([("TMPDIR".into(),directory.to_string_lossy().into_owned())]),session_directory:directory.clone(),provider:json!({"protocol":"fixture","model":scenario,"apiKey":"inference-secret"}),read_token:"read-secret".into(),write_token:Some("write-secret".into())}).unwrap();
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
        let mut controller = Controller::default();
        controller.secrets = vec!["secret".into()];
        controller.text(&("界".repeat(30_000) + "secret"), false);
        assert!(controller.view().text.len() <= TEXT_LIMIT);
        assert!(!controller.view().text.contains("secret"));
        assert!(controller.view().text.ends_with("[已隐藏]"));
    }
}
