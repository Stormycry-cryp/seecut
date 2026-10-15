// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal UI-independent controller for the documented HOST-IPC-V1 protocol.
//! Only the trusted host constructs configuration; no credential discovery or automatic retry.
use super::agent_process::{self, BridgeError, Event, Handle, LaunchSpec};
use serde_json::{Value, json};
use sha2::Digest;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
    pub local_session_id: String,
    pub restore: bool,
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
    OperationVerified,
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
    ModelSelectionUnavailable,
    InvalidInput,
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
            Self::ModelSelectionUnavailable => "模型或档位已变更，请重新选择",
            Self::InvalidInput => "当前模型无法处理这些内容，请调整文字或附件",
        }
    }
}
pub struct Approval {
    pub tool: String,
    pub arguments: Value,
    pub id: String,
    pub turn_id: String,
    pub object_id: Option<String>,
    pub delta_x: Option<f64>,
    pub delta_y: Option<f64>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct EditorRecovery {
    pub operation_id: String,
    pub client_sequence: u64,
    pub project_id: String,
    pub document_session_id: String,
    pub retry_registration: bool,
}
impl EditorRecovery {
    pub fn query_arguments(&self) -> Value {
        json!({"projectId":self.project_id,"documentSessionId":self.document_session_id,"operationId":self.operation_id,"clientSequence":self.client_sequence})
    }
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
    Library,
    Insert,
    Transform,
    Save,
    Export,
    Operation,
    RegistrationRetry,
}
impl ToolAction {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "context" => Some(Self::Context),
            "project" => Some(Self::Project),
            "preview" => Some(Self::Preview),
            "capabilities" => Some(Self::Capabilities),
            "move_selected_image" => Some(Self::Move),
            "library_images" => Some(Self::Library),
            "insert_library_image" => Some(Self::Insert),
            "transform_canvas_layer" => Some(Self::Transform),
            "save_canvas_project" => Some(Self::Save),
            "export_canvas_png" => Some(Self::Export),
            "canvas_operation" => Some(Self::Operation),
            "retry_canvas_registration" => Some(Self::RegistrationRetry),
            _ => None,
        }
    }
    fn is_write(self) -> bool {
        matches!(
            self,
            Self::Move
                | Self::Insert
                | Self::Transform
                | Self::Save
                | Self::Export
                | Self::RegistrationRetry
        )
    }
    fn label(self) -> &'static str {
        match self {
            Self::Context => "读取当前页面",
            Self::Project => "读取工程信息",
            Self::Preview => "查看画布预览",
            Self::Capabilities => "检查可用操作",
            Self::Move => "发送移动请求",
            Self::Library => "查找库内图片",
            Self::Insert => "放入画布",
            Self::Transform => "调整图层",
            Self::Save => "保存画布",
            Self::Export => "导出并入库",
            Self::Operation => "核对操作结果",
            Self::RegistrationRetry => "登记原 PNG",
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
    EditorReconcile { operation_id: String },
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
    restore: bool,
    recover_existing: bool,
    model_selection_supported: bool,
    session_opened: bool,
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
    cost_turn_id: Option<String>,
    cost_account_fingerprint: Option<Vec<u8>>,
    cost_waiting_for_turn: bool,
    billing_fingerprint: Value,
    billing_revision: u64,
    ignore_runtime_events: bool,
    secrets: Vec<String>,
    write_in_flight: bool,
    write_identity: Option<(String, String, String)>,
    write_operation: Option<(String, u64)>,
    editor_client: String,
    editor_recovery: Vec<EditorRecovery>,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            process: None,
            document: None,
            session_id: String::new(),
            restore: false,
            recover_existing: false,
            model_selection_supported: false,
            session_opened: false,
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
            budget_label: "0".into(),
            cost_turn_id: None,
            cost_account_fingerprint: None,
            cost_waiting_for_turn: false,
            billing_fingerprint: Value::Null,
            billing_revision: 0,
            ignore_runtime_events: false,
            secrets: Vec::new(),
            write_in_flight: false,
            write_identity: None,
            write_operation: None,
            editor_client: String::new(),
            editor_recovery: Vec::new(),
        }
    }
}
impl Controller {
    /// An explicit host action only. Opening or restoring never sends saved input.
    pub fn connect(&mut self, config: ConnectionConfig) -> Result<(), UiError> {
        let recovering = self.unknown_latched;
        if recovering
            && !(self.can_restore_original()
                && config.restore
                && config.local_session_id == self.session_id
                && config.client_id == self.editor_client
                && self.document.as_ref() == Some(&config.document)
                && self.managed_mode == (config.provider["protocol"] == "seecut-managed"))
        {
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
            || !valid_session_id(&config.local_session_id)
            || (!self.session_id.is_empty() && self.session_id != config.local_session_id)
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
        let account_fingerprint = config.provider["sessionToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(|token| sha2::Sha256::digest(token.as_bytes()).to_vec());
        if recovering && self.managed_mode && account_fingerprint != self.cost_account_fingerprint {
            return Err(UiError::VerifyRequired);
        }
        let preserve_cost = config.restore
            && self.session_id == config.local_session_id
            && self.document.as_ref() == Some(&config.document)
            && self.managed_mode
            && config.provider["protocol"] == "seecut-managed"
            && account_fingerprint.is_some()
            && self.cost_account_fingerprint == account_fingerprint;
        self.cost_account_fingerprint = account_fingerprint;
        self.cost_waiting_for_turn = false;
        if !preserve_cost {
            self.cost_turn_id = None;
            self.budget_label = "0".into();
        }
        self.session_id = config.local_session_id;
        self.restore = config.restore;
        self.recover_existing = false;
        self.model_selection_supported = false;
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
        self.editor_client = config.client_id.clone();
        if !recovering {
            self.editor_recovery.clear();
            self.write_operation = None;
        }
        self.managed_mode = config.provider["protocol"].as_str() == Some("seecut-managed");
        // A new managed runtime must supply its persisted recovery state before Ready.
        self.model_unknown = self.managed_mode && self.model_unknown;
        self.account_expired = false;
        self.recovery_pending = false;
        self.last_model_state = None;
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
        if !recovering {
            self.write_in_flight = false;
            self.write_identity = None;
        }
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
    /// True only after the runtime confirms this session exists. Hosts persist
    /// this separately from connection state and never infer it from enqueueing.
    pub fn session_opened(&self) -> bool {
        self.session_opened
    }
    /// Includes closing and failed-but-not-yet-reaped children, so a host can
    /// enforce one runtime owner for a shared account operation journal.
    pub fn process_present(&self) -> bool {
        self.process.is_some()
    }
    /// Negotiated for this process only; a reconnect must advertise it again.
    pub fn supports_model_selection(&self) -> bool {
        self.process.is_some() && self.model_selection_supported
    }
    fn can_restore_original(&self) -> bool {
        self.session_opened
            && !self.session_id.is_empty()
            && !self.editor_client.is_empty()
            && self.document.is_some()
    }
    pub fn view(&self) -> View<'_> {
        View {
            can_connect: self.process.is_none()
                && (!self.unknown_latched || self.can_restore_original()),
            can_reconcile: ((self.managed_mode && self.model_unknown)
                || (!self.editor_recovery.is_empty() && self.unknown_latched))
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
        self.send_text_with_selection(text, None)
    }
    /// Selection is consumed by this new input only. It is never installed as
    /// mutable controller state or attached to stop, reconciliation or recovery.
    pub fn send_text_with_selection(
        &mut self,
        text: &str,
        selection: Option<Value>,
    ) -> Result<(), UiError> {
        self.send_content_with_selection(vec![json!({"type":"text","text":text})], selection)
    }
    pub fn send_content_with_selection(
        &mut self,
        content: Vec<Value>,
        selection: Option<Value>,
    ) -> Result<(), UiError> {
        if let Some(error) = self.blocking_error() {
            return Err(error);
        }
        if !self.view().can_send {
            return Err(UiError::Busy);
        }
        let text_bytes: usize = content
            .iter()
            .filter_map(|part| part["text"].as_str())
            .map(str::len)
            .sum();
        if content.is_empty()
            || content.len() > 8
            || text_bytes > 32_000
            || content.iter().any(|part| match part["type"].as_str() {
                Some("text") => part["text"]
                    .as_str()
                    .is_none_or(|text| text.trim().is_empty()),
                Some("image") => {
                    part["data"]
                        .as_str()
                        .is_none_or(|data| data.len() > 1_398_104)
                        || !matches!(
                            part["mimeType"].as_str(),
                            Some("image/jpeg" | "image/png" | "image/webp" | "image/gif")
                        )
                }
                _ => true,
            })
            || content.iter().filter(|p| p["type"] == "image").count() > 2
        {
            return Err(UiError::Configuration);
        }
        if let Some(selection) = selection.as_ref() {
            if !self.supports_model_selection() {
                return Err(UiError::Protocol);
            }
            if !valid_model_selection(selection) {
                return Err(UiError::Configuration);
            }
        }
        // IPC counters restart with the process; persisted sessions must never
        // mistake a new explicit question for an already accepted input.
        let input_id = format!("input-{}", uuid::Uuid::new_v4());
        let mut params = json!({"sessionId":self.session_id,"inputId":input_id,"mode":"follow_up","content":content});
        if let Some(selection) = selection {
            params["modelSelection"] = selection;
        }
        self.enqueue("send", params, Request::Send { input_id })?;
        self.text.clear();
        self.tool_progress.clear();
        self.progress.clear();
        self.outcome = None;
        self.error = None;
        self.last_result_turn = None;
        self.cost_turn_id = None;
        self.cost_waiting_for_turn = true;
        self.budget_label = "0".into();
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
    pub fn editor_recovery(&self) -> Option<&EditorRecovery> {
        if self.view().can_reconcile && !self.model_unknown {
            self.editor_recovery.first()
        } else {
            None
        }
    }
    pub fn reconcile_editor(&mut self, operation: &EditorRecovery) -> Result<(), UiError> {
        if self.editor_recovery() != Some(operation) {
            return Err(UiError::Busy);
        }
        self.enqueue(
            "editor.reconcile",
            if operation.retry_registration {
                json!({"sessionId":self.session_id,"operationId":operation.operation_id,"retryRegistration":true})
            } else {
                json!({"sessionId":self.session_id,"operationId":operation.operation_id})
            },
            Request::EditorReconcile {
                operation_id: operation.operation_id.clone(),
            },
        )
    }
    fn update_editor_recovery(&mut self, tools: &Value) -> bool {
        let Some(items) = tools.get("recoveryOperations").and_then(Value::as_array) else {
            self.editor_recovery.clear();
            return true;
        };
        if items.len() > 128 {
            return false;
        }
        let mut next = Vec::new();
        for item in items {
            if item["sessionId"].as_str() != Some(self.session_id.as_str())
                || !matches!(
                    item["toolName"].as_str(),
                    Some(
                        "insert_library_image"
                            | "transform_canvas_layer"
                            | "save_canvas_project"
                            | "export_canvas_png"
                    )
                )
            {
                continue;
            }
            let binding = &item["binding"];
            let parameters = &item["parameters"];
            if binding["instanceId"].as_str()
                != self.document.as_ref().map(|d| d.instance_id.as_str())
                || binding["clientId"].as_str() != Some(self.editor_client.as_str())
            {
                continue;
            }
            let (Some(project), Some(session), Some(sequence), Some(operation)) = (
                binding["projectId"].as_str(),
                binding["documentSessionId"].as_str(),
                parameters["clientSequence"].as_u64(),
                parameters["operationId"].as_str(),
            ) else {
                return false;
            };
            if project.is_empty()
                || project.len() > 1024
                || session.is_empty()
                || session.len() > 64
                || sequence == 0
                || sequence > 9_007_199_254_740_991
                || parameters["projectId"].as_str() != Some(project)
                || parameters["documentSessionId"].as_str() != Some(session)
                || item["operationId"].as_str() != Some(operation)
                || operation
                    != format!(
                        "scm2:1:{}:{}:{}:{}:{}",
                        self.editor_client.len(),
                        self.editor_client,
                        session.len(),
                        session,
                        sequence
                    )
            {
                return false;
            }
            let entry = EditorRecovery {
                operation_id: operation.into(),
                client_sequence: sequence,
                project_id: project.into(),
                document_session_id: session.into(),
                retry_registration: item["toolName"] == "export_canvas_png"
                    && item["result"]["status"] == "partial"
                    && item["result"]["data"]["operationId"].as_str() == Some(operation)
                    && item["result"]["data"]["clientSequence"].as_u64() == Some(sequence)
                    && item["result"]["data"]["registrationPending"] == true,
            };
            if next.contains(&entry) {
                return false;
            }
            next.push(entry);
        }
        self.editor_recovery = next;
        true
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
                if previous != value {
                    return false;
                }
                self.refresh_cost(&state);
                return true;
            }
        }
        self.last_model_state = Some(value.clone());
        let was_unknown = self.model_unknown;
        self.model_unknown = state.unknown;
        // Login expiry is sticky for this runtime; only a fresh login can clear it.
        self.account_expired |= state.account_expired;
        self.refresh_cost(&state);
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
    fn refresh_cost(&mut self, state: &CheckedModelState) {
        if self.cost_waiting_for_turn {
            return;
        }
        if self.cost_turn_id.is_none() {
            self.cost_turn_id = self.turn_id.clone().or(state.budget_turn.clone());
        }
        if let Some(turn) = self.cost_turn_id.as_deref() {
            if state.budget_turn.as_deref() == Some(turn) {
                // The server's task total replaces the display; events never add fees.
                if let Some(spent) = state.budget_spent {
                    self.budget_label = spent.to_string();
                }
            } else if let Some(captured) = state.captured_by_turn.get(turn) {
                // A bounded operation list may omit older steps. Preserve the known total.
                let previous = self.budget_label.parse::<u64>().unwrap_or(0);
                self.budget_label = (*captured).max(previous).to_string();
            }
        }
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
        let Some((turn, invocation, tool)) = self.write_identity.as_ref() else {
            return;
        };
        let data = &event["data"];
        if !self.write_in_flight
            || event["turnId"].as_str() != Some(turn.as_str())
            || event["type"].as_str() != Some("tool.failed")
            || data["invocationId"].as_str() != Some(invocation.as_str())
            || data["message"]["toolName"].as_str() != Some(tool.as_str())
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
                Request::Approve { .. }
                | Request::Stop
                | Request::Reconcile
                | Request::EditorReconcile { .. } => {
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
                    || (self.restore
                        && result["capabilities"]["session_restore_rebind"].as_bool() != Some(true))
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                let document = self.document.as_ref().unwrap();
                self.model_selection_supported =
                    result["capabilities"]["turn_model_selection"].as_bool() == Some(true);
                let mut params = json!({"sessionId":self.session_id,"projectId":document.project_id,"documentSessionId":document.document_session_id});
                self.recover_existing = !self.restore
                    && result["capabilities"]["session_create_or_restore"].as_bool() == Some(true);
                let method = if self.restore {
                    params["rebind"] = json!(true);
                    if self.unknown_latched && !self.editor_recovery.is_empty() {
                        // A durable reconciliation may have completed before
                        // its IPC response was lost. Request only the exact
                        // receipts still retained by this host.
                        params["recoveryQueries"] = json!(
                            self.editor_recovery
                                .iter()
                                .map(EditorRecovery::query_arguments)
                                .collect::<Vec<_>>()
                        );
                    }
                    if self.unknown_latched
                        && self.editor_recovery.is_empty()
                        && let Some((turn, invocation, tool)) = &self.write_identity
                    {
                        params["recoveryInvocations"] = json!([{
                            "turnId":turn,"invocationId":invocation,"toolName":tool
                        }]);
                    }
                    "session.restore"
                } else {
                    if self.recover_existing {
                        // The first create may have committed before its receipt
                        // was lost. Only an advertised, explicit recovery may
                        // open that same record; the host never retries a send.
                        params["recoverExisting"] = json!(true);
                        params["rebind"] = json!(true);
                    }
                    "session.create"
                };
                match self.enqueue(method, params, Request::Create) {
                    Ok(()) => self.phase = Phase::OpeningSession,
                    Err(error) => self.fail(error),
                }
            }
            Request::Create => {
                if self.phase != Phase::OpeningSession {
                    return;
                }
                let status = result["status"].as_str();
                let restoring = self.restore || self.recover_existing;
                if result["sessionId"].as_str() != Some(self.session_id.as_str())
                    || if restoring {
                        !matches!(status, Some("idle" | "interrupted"))
                            || !result["recoveryRequired"].is_boolean()
                            || result["pendingInputCount"].as_u64().is_none_or(|n| n > 8)
                    } else {
                        status != Some("idle")
                    }
                    || !result["toolState"]["outcomeUnknown"].is_boolean()
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.managed_mode && !self.apply_model_state(&result["modelState"]) {
                    self.fail(UiError::Protocol);
                    return;
                }
                let original_recovery = self.editor_recovery.clone();
                if !self.update_editor_recovery(&result["toolState"]) {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.unknown_latched
                    && self.restore
                    && !original_recovery.iter().all(|original| {
                        self.editor_recovery
                            .iter()
                            .any(|next| next.query_arguments() == original.query_arguments())
                    })
                {
                    self.editor_recovery = original_recovery;
                    self.fail(UiError::Protocol);
                    return;
                }
                self.session_opened = true;
                if restoring {
                    self.recovery_pending = result["recoveryRequired"] == true
                        || result["pendingInputCount"].as_u64().is_some_and(|n| n > 0);
                }
                if result["toolState"]["outcomeUnknown"].as_bool() == Some(true) {
                    self.unknown();
                } else {
                    self.phase = if self.blocked() {
                        Phase::OutcomeUnknown
                    } else {
                        Phase::Ready
                    };
                    self.error = self.blocking_error();
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
            Request::EditorReconcile { operation_id } => {
                let session = &result["session"];
                if session["sessionId"].as_str() != Some(self.session_id.as_str())
                    || !result["verified"].is_boolean()
                    || result["receipt"]["data"]["operationId"].as_str()
                        != Some(operation_id.as_str())
                    || !session["toolState"]["outcomeUnknown"].is_boolean()
                    || !self.update_editor_recovery(&session["toolState"])
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.managed_mode && !self.apply_model_state(&session["modelState"]) {
                    self.fail(UiError::Protocol);
                    return;
                }
                if result["verified"] == true && session["toolState"]["outcomeUnknown"] == false {
                    self.unknown_latched = false;
                    self.write_in_flight = false;
                    self.write_identity = None;
                    self.write_operation = None;
                    self.recovery_pending =
                        session["pendingInputCount"].as_u64().is_none_or(|n| n > 0);
                    self.outcome = Some(if result["success"] == true {
                        TaskOutcome::OperationVerified
                    } else {
                        TaskOutcome::Failed
                    });
                    self.phase = if self.blocked() {
                        Phase::OutcomeUnknown
                    } else {
                        Phase::Ready
                    };
                    self.error = self.blocking_error();
                } else {
                    self.unknown();
                }
            }
            Request::RecoveryStatus => {
                if !self.update_editor_recovery(&result["toolState"]) {
                    self.fail(UiError::Protocol);
                    return;
                }
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
            if self.cost_waiting_for_turn || self.cost_turn_id.as_deref() != Some(turn) {
                self.cost_turn_id = Some(turn.to_owned());
                self.cost_waiting_for_turn = false;
                self.budget_label = "0".into();
            }
            if let Some(state) = self
                .last_model_state
                .as_ref()
                .and_then(|value| checked_model_state(value, &self.session_id))
            {
                self.refresh_cost(&state);
            }
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
                // A registration retry acts on the outstanding original export.
                // Its fresh COTO invocation must not erase that business identity.
                if data["call"]["name"] == "retry_canvas_registration"
                    && self.write_in_flight
                    && self.write_operation.as_ref().is_some_and(|(id, sequence)| {
                        data["call"]["arguments"]["operationId"].as_str() == Some(id.as_str())
                            && data["call"]["arguments"]["clientSequence"].as_u64()
                                == Some(*sequence)
                    })
                {
                    return;
                }
                if data["call"]["name"]
                    .as_str()
                    .and_then(ToolAction::from_name)
                    .is_some_and(ToolAction::is_write)
                {
                    self.write_in_flight = true;
                    self.write_operation = None;
                    self.write_identity = turn
                        .zip(
                            data["invocationId"]
                                .as_str()
                                .filter(|id| !id.is_empty() && id.len() <= 128),
                        )
                        .map(|(turn, invocation)| {
                            (
                                turn.to_owned(),
                                invocation.to_owned(),
                                data["call"]["name"].as_str().unwrap_or_default().to_owned(),
                            )
                        });
                }
            }
            "tool.completed" | "tool.failed" => {
                let owns_write = self.write_identity.as_ref().is_some_and(
                    |(original_turn, invocation, tool)| {
                        turn == Some(original_turn.as_str())
                            && data["invocationId"].as_str() == Some(invocation.as_str())
                            && data["message"]["toolName"].as_str() == Some(tool.as_str())
                    },
                );
                if owns_write {
                    if data["metadata"]["postVerification"].as_str() == Some("pending") {
                        self.write_operation = data["metadata"]["operationId"]
                            .as_str()
                            .filter(|id| !id.is_empty() && id.len() <= 192)
                            .zip(
                                data["metadata"]["clientSequence"]
                                    .as_u64()
                                    .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991),
                            )
                            .map(|(id, sequence)| (id.to_owned(), sequence));
                        self.outcome = Some(TaskOutcome::SubmittedPendingVerification);
                    } else {
                        self.write_in_flight = false;
                        self.write_identity = None;
                    }
                } else if matches!(
                    data["message"]["toolName"].as_str(),
                    Some("canvas_operation" | "retry_canvas_registration")
                ) && data["metadata"]["postVerification"].as_str() == Some("verified")
                    && self
                        .write_identity
                        .as_ref()
                        .is_some_and(|(_, invocation, tool)| {
                            data["metadata"]["resolvesInvocationId"].as_str()
                                == Some(invocation.as_str())
                                && data["metadata"]["resolvesToolName"].as_str()
                                    == Some(tool.as_str())
                        })
                    && self.write_operation.as_ref().is_some_and(|(id, sequence)| {
                        data["metadata"]["operationId"].as_str() == Some(id.as_str())
                            && data["metadata"]["clientSequence"].as_u64() == Some(*sequence)
                    })
                {
                    self.write_in_flight = false;
                    self.write_identity = None;
                    self.write_operation = None;
                }
            }

            "tool.cancelled" => {
                if self
                    .write_identity
                    .as_ref()
                    .is_some_and(|(original_turn, invocation, _)| {
                        turn == Some(original_turn.as_str())
                            && data["invocationId"].as_str() == Some(invocation.as_str())
                    })
                {
                    if data["executed"].as_bool() == Some(true) {
                        self.unknown();
                    } else {
                        self.write_in_flight = false;
                        self.write_identity = None;
                        self.write_operation = None;
                    }
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
                if id.is_empty()
                    || id.len() > 100
                    || !matches!(
                        tool,
                        "move_selected_image"
                            | "library_images"
                            | "insert_library_image"
                            | "transform_canvas_layer"
                            | "save_canvas_project"
                            | "export_canvas_png"
                            | "canvas_operation"
                            | "retry_canvas_registration"
                    )
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                let args = &data["arguments"];
                self.approval = Some(Approval {
                    tool: tool.to_owned(),
                    arguments: args.clone(),
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
        if !self.update_editor_recovery(tools) {
            self.fail(UiError::Protocol);
            return;
        }
        if tools["outcomeUnknown"].as_bool() == Some(true) || self.unknown_latched {
            self.unknown();
            return;
        }
        // Preserve the existing Move contract: a returned commit with a
        // pending post-read is no longer an in-flight native write. Canvas
        // jobs retain their original owner until their receipt is verified.
        if self
            .write_identity
            .as_ref()
            .is_some_and(|(_, _, tool)| tool == "move_selected_image")
        {
            self.write_in_flight = false;
            self.write_identity = None;
            self.write_operation = None;
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
    budget_turn: Option<String>,
    budget_spent: Option<u64>,
    captured_by_turn: std::collections::BTreeMap<String, u64>,
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
    let mut captured_by_turn = std::collections::BTreeMap::<String, u64>::new();
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
        if matches!(op["billingState"].as_str(), Some("captured" | "released")) {
            let total = captured_by_turn
                .entry(op["turnId"].as_str()?.to_owned())
                .or_default();
            *total = total
                .checked_add(captured)
                .filter(|value| *value <= 1_000_000_000)?;
        }
        billing.push(json!([op["clientRequestId"], reserved, captured, released]));
    }
    let budget = value.get("budget")?;
    let mut budget_turn = None;
    let mut budget_spent = None;
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
        budget_turn = budget["turn_id"].as_str().map(str::to_owned);
        budget_spent = Some(spent);
        billing.push(json!([budget["budget_id"], held, spent]));
    }
    Some(CheckedModelState {
        revision,
        unknown,
        account_expired,
        budget_turn,
        budget_spent,
        captured_by_turn,
        billing: json!(billing),
    })
}
fn max64(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64
}
fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}
fn valid_model_selection(value: &Value) -> bool {
    let Some(selection) = value.as_object() else {
        return false;
    };
    if selection
        .keys()
        .any(|key| !matches!(key.as_str(), "modelId" | "effortId" | "policyRevision"))
        || !selection
            .get("modelId")
            .and_then(Value::as_str)
            .is_some_and(valid_session_id)
    {
        return false;
    }
    let effort = selection.get("effortId").filter(|value| !value.is_null());
    let revision = selection
        .get("policyRevision")
        .filter(|value| !value.is_null());
    match (effort, revision) {
        (None, None) => true,
        (Some(effort), Some(revision)) => {
            effort.as_str().is_some_and(valid_session_id)
                && revision.as_str().is_some_and(valid_session_id)
        }
        _ => false,
    }
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
        "invalid_input" | "input_limit" | "media_limit" => UiError::InvalidInput,
        "turn_settling" | "managed_busy" | "session_busy" => UiError::Busy,
        "managed_unknown" | "managed_transport" | "managed_timeout" => UiError::ModelVerifyRequired,
        "managed_protocol" | "idempotency_conflict" => UiError::Protocol,
        "managed_account_expired" => UiError::AccountExpired,
        "managed_unavailable" | "managed_http" => UiError::ServiceUnavailable,
        "managed_policy_unavailable" => UiError::ModelSelectionUnavailable,
        "tool_denied" | "notAuthorized" | "approval_expired" => UiError::AuthorizationRequired,
        "project_conflict" | "session_scope" | "instance_conflict" | "managed_scope"
        | "account_scope" => UiError::DocumentChanged,
        "outcome_unknown" => UiError::VerifyRequired,
        "recovery_required" => UiError::SessionRecoveryRequired,
        "provider_configuration"
        | "invalid_initialize"
        | "invalid_binding"
        | "invalid_model_selection" => UiError::Configuration,
        _ => UiError::Connection,
    }
}

#[cfg(all(test, unix, not(any(target_os = "ios", target_os = "android"))))]
mod tests {
    use super::*;
    #[test]
    fn rejected_content_is_not_reported_as_a_disconnected_agent() {
        for code in ["invalid_input", "input_limit", "media_limit"] {
            assert_eq!(remote_error(code), UiError::InvalidInput);
            assert!(!remote_error(code).message().contains("连接"));
        }
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{fs, thread};
    const FIXTURE: &str = r#"import sys,json,time,pathlib,os
scenario='finish';session='';seq=0;managed=False
revision=0
original_path=pathlib.Path(__file__).with_name('original-operation.json')
def original_store(record):
    temporary=original_path.with_suffix('.tmp')
    with temporary.open('w') as stream:
        json.dump(record,stream);stream.flush();os.fsync(stream.fileno())
    os.replace(temporary,original_path)
    directory=os.open(str(original_path.parent),os.O_RDONLY)
    try: os.fsync(directory)
    finally: os.close(directory)
def original_query(record):
    return {key:record['parameters'][key] for key in ('projectId','documentSessionId','operationId','clientSequence')}
def recovery_state(record):
    return {'outcomeUnknown':True,'unresolved':[],'recoveryOperations':[record],'observed':[]}
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
    with pathlib.Path(__file__).with_name('methods.txt').open('a') as trace: trace.write(m+'\n')
    if m in ('send','stop','reconcile','editor.reconcile'):
        with pathlib.Path(__file__).with_name('requests.jsonl').open('a') as trace: trace.write(json.dumps({'method':m,'params':p})+'\n')
    if m=='initialize':
        assert p['protocolVersion']==1 and p['appProtocolVersion']==1
        managed=p['provider']['protocol']=='seecut-managed'
        assert p['readToken']=='read-secret'
        assert p['provider']['sessionToken' if managed else 'apiKey']=='inference-secret'
        assert p['mcpEnvironment']['TMPDIR']==p['sessionDirectory']
        scenario=p['provider']['model'];time.sleep(.08)
        if scenario=='init-fail': reply(f,error={'code':'provider_configuration','message':'raw inference-secret'});continue
        capabilities={} if scenario=='old-runtime' else {'session_restore_rebind':True,'session_create_or_restore':True,'turn_model_selection':True}
        if scenario=='selection-absent': capabilities.pop('turn_model_selection')
        if scenario=='selection-false': capabilities['turn_model_selection']=False
        if scenario=='selection-string': capabilities['turn_model_selection']='true'
        reply(f,{'protocolVersion':1,'instanceId':p['instanceId'],'capabilities':capabilities})
    elif m in ('session.create','session.restore'):
        assert p['projectId']=='project-1' and p['documentSessionId']=='doc-1'
        if m=='session.restore': assert p['rebind'] is True and 'recoverExisting' not in p
        elif scenario=='old-runtime': assert 'rebind' not in p and 'recoverExisting' not in p
        else: assert p['rebind'] is True and p['recoverExisting'] is True
        session=p['sessionId'];time.sleep(.08)
        if m=='session.restore' and scenario in ('exit-insert','exit-export','first-terminal-lost','lost-reconcile-reply'):
            record=json.loads(original_path.read_text())
            if scenario=='first-terminal-lost': assert p['recoveryInvocations']==[{'turnId':'turn-1','invocationId':'turn-1_canvas','toolName':record['toolName']}]
            elif record['status']=='verified': assert p['recoveryQueries']==[original_query(record)]
            else: assert 'recoveryQueries' not in p
            assert record['sessionId']==session
            reply(f,{'sessionId':session,'status':'interrupted','recoveryRequired':True,'pendingInputCount':0,'toolState':recovery_state(record)});continue
        if managed: print(json.dumps({'version':1,'type':'managed.state','sessionId':session,'modelState':modelstate(True)}),flush=True)
        status='interrupted' if scenario in ('restore-interrupted','restore-pending','create-existing-clean','create-existing-pending','create-existing-unknown') else 'idle'
        pending=scenario in ('restore-pending','create-existing-pending')
        reply(f,{'sessionId':session if scenario!='bad-session' else 'other','status':status,'recoveryRequired':pending,'pendingInputCount':1 if pending else 0,'toolState':{'outcomeUnknown':scenario in ('restore-unknown','create-existing-unknown'),'unresolved':[],'recoveryOperations':[],'observed':[]},'modelState':modelstate(True) if managed else None})
    elif m=='reconcile':
        assert managed and p=={'sessionId':session}
        reply(f,{'modelState':modelstate(False)})
    elif m=='editor.reconcile':
        record=json.loads(original_path.read_text())
        assert p=={'sessionId':session,'operationId':record['operationId']}
        record['queries']+=1;record['status']='verified';original_store(record)
        if scenario=='lost-reconcile-reply' and record['queries']==1: sys.exit(0)
        reply(f,{'verified':True,'success':True,'receipt':record['result'],'session':{'sessionId':session,'pendingInputCount':0,'toolState':{'outcomeUnknown':False,'unresolved':[],'recoveryOperations':[],'observed':[]}}})
    elif m=='send':
        assert p['sessionId']==session and p['mode']=='follow_up' and p['content'][0]['type']=='text'
        if scenario=='selection-policy-rejected':
            reply(f,error={'code':'managed_policy_unavailable','message':'raw service details'});continue
        if scenario=='settling-once':
            scenario='finish';reply(f,error={'code':'turn_settling','message':'上一轮正在结束，请稍后发送'});continue
        if scenario=='early-result':
            event('turn.started');event('text.delta',{'text':'处理中'});final();reply(f,{'inputId':p['inputId'],'status':'pending'});continue
        reply(f,{'inputId':p['inputId'],'status':'pending'});event('turn.started')
        if scenario in ('exit-insert','exit-export','first-terminal-lost','lost-reconcile-reply'):
            name='insert_library_image' if scenario=='exit-insert' else 'export_canvas_png'
            operation='scm2:1:8:client-1:5:doc-1:1'
            record={'sessionId':session,'invocationId':'turn-1_canvas','toolName':name,'operationId':operation,'parameters':{'projectId':'project-1','documentSessionId':'doc-1','operationId':operation,'clientSequence':1},'binding':{'instanceId':'12345678-1234-1234-1234-123456789abc','clientId':'client-1','projectId':'project-1','documentSessionId':'doc-1'},'status':'pending','writes':1,'queries':0,'result':{'status':'ok','data':{'operationId':operation,'clientSequence':1}}}
            if scenario=='first-terminal-lost': record['status']='verified'
            original_store(record)
            event('tool.started',{'invocationId':'turn-1_canvas','call':{'name':name}})
            if scenario!='lost-reconcile-reply': sys.exit(0)
            event('tool.completed',{'invocationId':'turn-1_canvas','message':{'toolName':name},'metadata':{'postVerification':'pending','operationId':operation,'clientSequence':1}})
            print(json.dumps({'version':1,'type':'result','sessionId':session,'result':{'turnId':'turn-1','status':'interrupted','text':''},'toolState':recovery_state(record)}),flush=True);continue
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
        assert p=={'turnId':'turn-1','sessionId':session}
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
            Self::with_session(scenario, "fixture-session", false)
        }
        fn with_session(scenario: &str, session_id: &str, restore: bool) -> Self {
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
                let result = controller.connect(ConnectionConfig{launch:LaunchSpec{executable:fs::canonicalize("/usr/bin/python3").unwrap(),entrypoint:entry.clone(),environment:BTreeMap::new()},document:identity.clone(),client_id:"client-1".into(),mcp_client_path:entry.clone(),mcp_environment:BTreeMap::from([("TMPDIR".into(),directory.to_string_lossy().into_owned())]),session_directory:directory.clone(),local_session_id:session_id.into(),restore,provider:if scenario == "managed" {json!({"protocol":"seecut-managed","model":scenario,"sessionToken":"inference-secret"})} else {json!({"protocol":"fixture","model":scenario,"apiKey":"inference-secret"})},read_token:"read-secret".into(),write_token:Some("write-secret".into())});
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
        fn original_restore_config(&self, scenario: &str) -> ConnectionConfig {
            let entry = self.directory.join("fixture.py");
            ConnectionConfig {
                launch: LaunchSpec {
                    executable: fs::canonicalize("/usr/bin/python3").unwrap(),
                    entrypoint: entry.clone(),
                    environment: BTreeMap::new(),
                },
                document: self.identity.clone(),
                client_id: "client-1".into(),
                mcp_client_path: entry,
                mcp_environment: BTreeMap::from([(
                    "TMPDIR".into(),
                    self.directory.to_string_lossy().into_owned(),
                )]),
                session_directory: self.directory.clone(),
                local_session_id: "fixture-session".into(),
                restore: true,
                provider: json!({"protocol":"fixture","model":scenario,"apiKey":"inference-secret"}),
                read_token: "read-secret".into(),
                write_token: None,
            }
        }
        fn requests(&self) -> Vec<Value> {
            match fs::read_to_string(self.directory.join("requests.jsonl")) {
                Ok(trace) => trace
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => panic!("fixture request trace failed: {error}"),
            }
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
    fn mcp_canvas_query_and_cancellation_clear_only_exact_original_write() {
        let mut controller = Controller {
            session_id: "session".into(),
            turn_id: Some("turn".into()),
            phase: Phase::Running,
            ..Default::default()
        };
        let mut sequence = 0;
        let mut event = |controller: &mut Controller, kind: &str, data: Value| {
            sequence += 1;
            let turn = controller.turn_id.clone();
            controller.runtime_event(&json!({"sessionId":"session","turnId":turn,"seq":sequence,"type":kind,"data":data}));
        };
        event(
            &mut controller,
            "tool.started",
            json!({"invocationId":"turn_export","call":{"name":"export_canvas_png"}}),
        );
        event(
            &mut controller,
            "tool.completed",
            json!({"invocationId":"turn_export","message":{"toolName":"export_canvas_png"},"metadata":{"postVerification":"pending","operationId":"original","clientSequence":1}}),
        );
        assert!(controller.write_in_flight);
        event(
            &mut controller,
            "tool.cancelled",
            json!({"invocationId":"turn_read","executed":false}),
        );
        assert!(controller.write_in_flight);
        event(
            &mut controller,
            "tool.completed",
            json!({"invocationId":"turn_query","message":{"toolName":"canvas_operation"},"metadata":{"postVerification":"verified","operationId":"original","clientSequence":2,"resolvesInvocationId":"turn_export","resolvesToolName":"export_canvas_png"}}),
        );
        assert!(controller.write_in_flight);
        event(
            &mut controller,
            "tool.completed",
            json!({"invocationId":"turn_query_2","message":{"toolName":"canvas_operation"},"metadata":{"postVerification":"verified","operationId":"original","clientSequence":1,"resolvesInvocationId":"turn_other","resolvesToolName":"export_canvas_png"}}),
        );
        assert!(controller.write_in_flight);
        controller.turn_id = Some("later-turn".into());
        event(
            &mut controller,
            "tool.started",
            json!({"invocationId":"later_retry","call":{"name":"retry_canvas_registration","arguments":{"operationId":"original","clientSequence":1}}}),
        );
        assert_eq!(controller.write_identity.as_ref().unwrap().1, "turn_export");
        event(
            &mut controller,
            "tool.completed",
            json!({"invocationId":"turn_query_3","message":{"toolName":"canvas_operation"},"metadata":{"postVerification":"verified","operationId":"original","clientSequence":1,"resolvesInvocationId":"turn_export","resolvesToolName":"export_canvas_png"}}),
        );
        assert!(!controller.write_in_flight);
        assert!(controller.write_identity.is_none());
    }

    #[test]
    fn model_selection_is_frozen_per_new_input_and_never_inherited() {
        let mut f = Fixture::new("finish");
        assert!(!f.controller.supports_model_selection());
        f.ready();
        assert!(f.controller.supports_model_selection());
        let first =
            json!({"modelId":"agent.fast","effortId":"balanced","policyRevision":"policy-1"});
        let mut selected = first.clone();
        f.controller
            .send_text_with_selection("第一轮", Some(selected.clone()))
            .unwrap();
        selected["modelId"] = json!("agent.deep");
        selected["effortId"] = json!("deep");
        assert_eq!(
            f.controller
                .send_text_with_selection("队列中不能覆盖", Some(selected.clone())),
            Err(UiError::Busy)
        );
        f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
        f.controller
            .send_text_with_selection("第二轮", Some(selected.clone()))
            .unwrap();
        f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
        f.controller.send_text("第三轮沿用初始化配置").unwrap();
        f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
        let requests = f.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request["method"] == "send"));
        assert_eq!(requests[0]["params"]["modelSelection"], first);
        assert_eq!(requests[1]["params"]["modelSelection"], selected);
        assert!(requests[2]["params"].get("modelSelection").is_none());
        assert_ne!(
            requests[0]["params"]["inputId"],
            requests[1]["params"]["inputId"]
        );
        assert_ne!(
            requests[1]["params"]["inputId"],
            requests[2]["params"]["inputId"]
        );
        f.controller.disconnect();
        f.until(|c| !c.process_present());
        assert!(!f.controller.supports_model_selection());
    }
    #[test]
    fn model_selection_requires_an_exact_capability_without_legacy_fallback() {
        for scenario in [
            "old-runtime",
            "selection-absent",
            "selection-false",
            "selection-string",
        ] {
            let mut f = Fixture::new(scenario);
            f.ready();
            assert!(!f.controller.supports_model_selection(), "{scenario}");
            assert_eq!(
                f.controller.send_text_with_selection(
                    "不得忽略选择",
                    Some(json!({"modelId":"agent.fast"})),
                ),
                Err(UiError::Protocol),
                "{scenario}"
            );
            assert!(f.controller.pending.is_empty());
            assert!(f.requests().is_empty());
            assert!(f.controller.view().can_send);
            f.controller.send_text("旧协议仍可明确发送").unwrap();
            f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
            let requests = f.requests();
            assert_eq!(requests.len(), 1);
            assert!(requests[0]["params"].get("modelSelection").is_none());
        }
    }
    #[test]
    fn model_selection_rejects_malformed_or_privileged_fields_before_enqueue() {
        let mut f = Fixture::new("finish");
        f.ready();
        for selection in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"modelId":null}),
            json!({"modelId":true}),
            json!({"modelId":""}),
            json!({"modelId":"provider/model"}),
            json!({"modelId":"模型"}),
            json!({"modelId":"m".repeat(129)}),
            json!({"modelId":"agent.fast","effortId":"deep"}),
            json!({"modelId":"agent.fast","policyRevision":"policy-1"}),
            json!({"modelId":"agent.fast","effortId":null,"policyRevision":"policy-1"}),
            json!({"modelId":"agent.fast","effortId":"deep","policyRevision":null}),
            json!({"modelId":"agent.fast","effortId":"deep","policyRevision":1}),
            json!({"modelId":"agent.fast","effortId":"","policyRevision":"policy-1"}),
            json!({"modelId":"agent.fast","effortId":"deep","policyRevision":"r".repeat(129)}),
            json!({"modelId":"agent.fast","apiKey":"must-not-forward"}),
            json!({"modelId":"agent.fast","maxSteps":8}),
            json!({"modelId":"agent.fast","budgetId":"old-budget"}),
        ] {
            assert_eq!(
                f.controller
                    .send_text_with_selection("新问题", Some(selection)),
                Err(UiError::Configuration)
            );
            assert!(f.controller.pending.is_empty());
            assert!(f.controller.view().can_send);
        }
        assert!(f.requests().is_empty());
    }
    #[test]
    fn model_selection_accepts_only_bounded_ids_and_nullish_legacy_pairs() {
        for selection in [
            json!({"modelId":"agent.fast"}),
            json!({"modelId":"agent.fast","effortId":null}),
            json!({"modelId":"agent.fast","policyRevision":null}),
            json!({"modelId":"agent.fast","effortId":null,"policyRevision":null}),
            json!({"modelId":"m".repeat(128),"effortId":"e".repeat(128),"policyRevision":"r".repeat(128)}),
            json!({"modelId":"A-z_0.9:1","effortId":"deep","policyRevision":"policy-1"}),
        ] {
            assert!(valid_model_selection(&selection));
        }
    }
    #[test]
    fn model_selection_cannot_replace_a_running_turn_or_change_stop() {
        let mut f = Fixture::new("stop");
        f.ready();
        let selection =
            json!({"modelId":"agent.fast","effortId":"balanced","policyRevision":"policy-1"});
        f.controller
            .send_text_with_selection("执行原请求", Some(selection.clone()))
            .unwrap();
        f.until(|c| c.view().phase == Phase::AwaitingApproval);
        assert_eq!(
            f.controller
                .send_text_with_selection("改成另一模型", Some(json!({"modelId":"agent.deep"}))),
            Err(UiError::Busy)
        );
        f.controller.stop().unwrap();
        f.until(|c| c.view().outcome == Some(TaskOutcome::Stopped));
        let requests = f.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["params"]["modelSelection"], selection);
        assert_eq!(
            requests[1],
            json!({"method":"stop","params":{"sessionId":"fixture-session","turnId":"turn-1"}})
        );
    }
    #[test]
    fn model_selection_cannot_bypass_unknown_or_attach_to_reconciliation() {
        let mut f = Fixture::new("managed");
        f.until(|c| c.view().can_reconcile);
        assert!(f.controller.supports_model_selection());
        assert_eq!(
            f.controller
                .send_text_with_selection("不能替换旧操作", Some(json!({"modelId":"agent.deep"}))),
            Err(UiError::ModelVerifyRequired)
        );
        f.controller.reconcile().unwrap();
        f.ready();
        assert_eq!(
            f.requests(),
            vec![json!({"method":"reconcile","params":{"sessionId":"fixture-session"}})]
        );
    }
    #[test]
    fn model_selection_policy_rejection_is_explicit_and_never_retried() {
        let mut f = Fixture::new("selection-policy-rejected");
        f.ready();
        f.controller
            .send_text_with_selection("策略已经变更", Some(json!({"modelId":"agent.fast","effortId":"balanced","policyRevision":"old-policy"})))
            .unwrap();
        f.until(|c| c.view().error == Some(UiError::ModelSelectionUnavailable));
        assert_eq!(f.controller.view().phase, Phase::Ready);
        assert!(f.controller.view().can_send);
        assert!(f.controller.view().outcome.is_none());
        assert_eq!(f.requests().len(), 1);
        assert_eq!(
            remote_error("invalid_model_selection"),
            UiError::Configuration
        );
        assert_eq!(remote_error("idempotency_conflict"), UiError::Protocol);
    }
    #[test]
    fn restore_uses_the_saved_identity_and_never_sends_history() {
        let mut f = Fixture::with_session("finish", "saved-session", true);
        assert!(f.controller.process_present());
        assert!(!f.controller.session_opened());
        f.until(|c| c.view().phase == Phase::OpeningSession);
        assert!(!f.controller.session_opened());
        f.ready();
        assert!(f.controller.session_opened());
        assert_eq!(f.controller.session_id, "saved-session");
        assert_eq!(f.controller.view().text, "");
        assert_eq!(
            fs::read_to_string(f.directory.join("methods.txt")).unwrap(),
            "initialize\nsession.restore\n"
        );
        f.controller.disconnect();
        assert!(f.controller.process_present());
        f.until(|c| !c.process_present());
        assert!(f.controller.session_opened());
    }
    #[test]
    fn restore_requires_advertised_rebinding_without_create_fallback() {
        let mut f = Fixture::with_session("old-runtime", "saved-session", true);
        f.until(|c| !c.process_present());
        assert_eq!(f.controller.view().phase, Phase::Failed);
        assert_eq!(f.controller.view().error, Some(UiError::Protocol));
        assert!(!f.controller.session_opened());
        let methods = fs::read_to_string(f.directory.join("methods.txt")).unwrap();
        assert!(methods.starts_with("initialize\n"));
        assert!(!methods.contains("session.create"));
        assert!(!methods.contains("session.restore"));
        assert!(!methods.lines().any(|method| method == "send"));
    }
    #[test]
    fn lost_create_receipt_opens_existing_state_without_replaying_input() {
        for (scenario, expected_error) in [
            ("create-existing-clean", None),
            (
                "create-existing-pending",
                Some(UiError::SessionRecoveryRequired),
            ),
            ("create-existing-unknown", Some(UiError::VerifyRequired)),
            ("managed", Some(UiError::ModelVerifyRequired)),
        ] {
            // The host still thinks this session is new because its original
            // create receipt was lost. The fixture returns the saved state.
            let mut f = Fixture::with_session(scenario, "unconfirmed-session", false);
            f.until(Controller::session_opened);
            assert!(f.controller.recover_existing);
            assert_eq!(f.controller.view().error, expected_error);
            assert_eq!(f.controller.view().can_send, expected_error.is_none());
            assert_eq!(
                fs::read_to_string(f.directory.join("methods.txt")).unwrap(),
                "initialize\nsession.create\n"
            );
            if expected_error.is_some() {
                assert_eq!(f.controller.view().phase, Phase::OutcomeUnknown);
            }
        }
    }
    #[test]
    fn legacy_runtime_keeps_new_session_creation_without_recovery_flags() {
        let mut f = Fixture::with_session("old-runtime", "new-session", false);
        f.ready();
        assert!(f.controller.session_opened());
        assert!(!f.controller.recover_existing);
        assert_eq!(
            fs::read_to_string(f.directory.join("methods.txt")).unwrap(),
            "initialize\nsession.create\n"
        );
    }
    #[test]
    fn restored_session_new_input_ids_do_not_repeat_process_counters() {
        fn accepted_input_id(f: &mut Fixture) -> String {
            f.ready();
            f.controller.send_text("用户明确的新问题").unwrap();
            f.controller
                .pending
                .iter()
                .find_map(|pending| match &pending.request {
                    Request::Send { input_id } => Some(input_id.clone()),
                    _ => None,
                })
                .unwrap()
        }
        let first = {
            let mut f = Fixture::with_session("finish", "saved-session", false);
            let id = accepted_input_id(&mut f);
            f.until(|c| c.view().outcome.is_some());
            id
        };
        let mut restored = Fixture::with_session("finish", "saved-session", true);
        let second = accepted_input_id(&mut restored);
        assert_ne!(first, second);
        assert!(uuid::Uuid::parse_str(first.strip_prefix("input-").unwrap()).is_ok());
        assert!(uuid::Uuid::parse_str(second.strip_prefix("input-").unwrap()).is_ok());
        restored.until(|c| c.view().outcome.is_some());
    }
    #[test]
    fn restore_preserves_pending_and_unknown_but_allows_clean_interruption() {
        for (scenario, expected_error) in [
            ("restore-interrupted", None),
            ("restore-pending", Some(UiError::SessionRecoveryRequired)),
            ("restore-unknown", Some(UiError::VerifyRequired)),
            ("managed", Some(UiError::ModelVerifyRequired)),
        ] {
            let mut f = Fixture::with_session(scenario, "saved-session", true);
            f.until(Controller::session_opened);
            assert_eq!(f.controller.view().error, expected_error);
            assert_eq!(f.controller.view().can_send, expected_error.is_none());
            assert_eq!(
                fs::read_to_string(f.directory.join("methods.txt")).unwrap(),
                "initialize\nsession.restore\n"
            );
            if expected_error.is_none() {
                f.controller.send_text("停止后用户发送的新问题").unwrap();
                f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
            } else if scenario == "managed" {
                f.controller.reconcile().unwrap();
                f.ready();
                assert!(f.controller.view().can_send);
            }
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
        assert_eq!(c.view().budget_label, "5");
        state["outcomeUnknown"] = json!(true);
        state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().billing_revision, revision);
        state["budget"]["held_credits"] = json!(0);
        state["budget"]["spent_credits"] = json!(12);
        state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().billing_revision, revision + 1);
        assert_eq!(c.view().budget_label, "12");
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
    fn cost_state(turn: &str, spent: u64) -> Value {
        let mut state = managed_state(false);
        state["budget"] = json!({"budget_id":format!("budget-{turn}"),"local_session_id":"managed-test","turn_id":turn,
            "state":"active","max_credits":100,"held_credits":0,"spent_credits":spent,"max_steps":8,"operation_count":2});
        state
    }
    #[test]
    fn numeric_cost_uses_task_snapshot_and_survives_steps_and_replays() {
        let mut c = managed_controller();
        assert_eq!(c.view().budget_label, "0");
        let state = cost_state("turn-1", 1);
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().budget_label, "1");
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().budget_label, "1");
        assert!(c.apply_model_state(&managed_state(false)));
        assert_eq!(c.view().budget_label, "1");
        assert!(c.apply_model_state(&cost_state("turn-1", 2)));
        assert_eq!(c.view().budget_label, "2");
        // A current authoritative snapshot replaces any partial-operation fallback.
        c.budget_label = "9".into();
        assert!(c.apply_model_state(&cost_state("turn-1", 2)));
        assert_eq!(c.view().budget_label, "2");
        let mut foreign = cost_state("foreign", 8);
        foreign["budget"]["local_session_id"] = json!("other-session");
        assert!(!c.apply_model_state(&foreign));
        assert_eq!(c.view().budget_label, "2");
    }
    #[test]
    fn numeric_cost_binds_cached_snapshot_when_turn_started_arrives_late() {
        let mut c = managed_controller();
        assert!(c.apply_model_state(&cost_state("old-turn", 7)));
        c.cost_turn_id = None;
        c.cost_waiting_for_turn = true;
        c.budget_label = "0".into();
        c.pending.push_back(Pending {
            id: "send".into(),
            request: Request::Send {
                input_id: "input".into(),
            },
            since: Instant::now(),
        });
        let state = cost_state("new-turn", 1);
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().budget_label, "0");
        c.runtime_event(&json!({"sessionId":"managed-test","seq":1,"type":"turn.started","turnId":"new-turn","data":{}}));
        assert_eq!(c.view().budget_label, "1");
        assert!(c.apply_model_state(&state));
        assert_eq!(c.view().budget_label, "1");
        let previous = cost_state("old-turn", 7);
        assert!(c.apply_model_state(&previous));
        assert_eq!(c.view().budget_label, "1");
    }
    #[test]
    fn numeric_cost_new_question_resets_and_ignores_previous_task() {
        let mut f = Fixture::new("finish");
        f.ready();
        f.controller.budget_label = "5".into();
        f.controller.cost_turn_id = Some("previous".into());
        f.controller.send_text("next task").unwrap();
        assert_eq!(f.controller.view().budget_label, "0");
        f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
        assert_eq!(f.controller.view().budget_label, "0");
    }
    #[test]
    fn numeric_cost_exit_and_restore_keep_same_scope_and_clear_pending_turn() {
        let mut f = Fixture::with_session("managed", "managed-test", true);
        f.until(|c| c.view().can_reconcile);
        f.controller.reconcile().unwrap();
        f.ready();
        let mut state = cost_state("saved-turn", 3);
        state["revision"] = json!(1000);
        assert!(f.controller.apply_model_state(&state));
        f.controller.cost_waiting_for_turn = true;
        f.controller.disconnect();
        f.until(|c| c.process.is_none());
        assert!(f.controller.secrets.is_empty());
        fn reconnect(f: &mut Fixture, token: &str, document: DocumentIdentity) {
            let entry = f.directory.join("fixture.py");
            f.controller.connect(ConnectionConfig {
                launch: LaunchSpec { executable: fs::canonicalize("/usr/bin/python3").unwrap(), entrypoint: entry.clone(), environment: BTreeMap::new() },
                document, client_id: "client-1".into(), mcp_client_path: entry,
                mcp_environment: BTreeMap::from([("TMPDIR".into(), f.directory.to_string_lossy().into_owned())]),
                session_directory: f.directory.clone(), local_session_id: "managed-test".into(), restore: true,
                provider: json!({"protocol":"seecut-managed","model":"managed","sessionToken":token}),
                read_token: "read-secret".into(), write_token: Some("write-secret".into()),
            }).unwrap();
        }
        let identity = f.identity.clone();
        reconnect(&mut f, "inference-secret", identity.clone());
        assert_eq!(f.controller.view().budget_label, "3");
        assert!(!f.controller.cost_waiting_for_turn);
        f.until(|c| c.view().can_reconcile);
        assert_eq!(f.controller.view().budget_label, "3");
        f.controller.disconnect();
        f.until(|c| c.process.is_none());
        reconnect(&mut f, "different-account-token", identity.clone());
        assert_eq!(f.controller.view().budget_label, "0");
        // This fixture only authenticates inference-secret; a foreign token
        // must not restore its old financial display before authentication.
        f.controller.budget_label = "4".into();
        f.controller.disconnect();
        f.until(|c| c.process.is_none());
        let mut other = identity;
        other.project_id = "other-project".into();
        reconnect(&mut f, "different-account-token", other);
        assert_eq!(f.controller.view().budget_label, "0");
    }
    #[test]
    fn numeric_cost_restore_adopts_explicit_budget_and_has_no_cross_session_fallback() {
        let mut c = managed_controller();
        assert!(c.apply_model_state(&cost_state("saved-turn", 3)));
        assert_eq!(c.view().budget_label, "3");
        let mut new_session = managed_controller();
        assert!(new_session.apply_model_state(&managed_state(false)));
        assert_eq!(new_session.view().budget_label, "0");
        let mut old = cost_state("saved-turn", 3);
        old["budget"]["local_session_id"] = json!("different-session");
        assert!(!new_session.apply_model_state(&old));
        assert_eq!(new_session.view().budget_label, "0");
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
            tool: "move_selected_image".into(),
            arguments: json!({}),
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
        assert_eq!(c.view().progress, "读取当前页面…");
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
        assert_eq!(c.view().progress, "读取当前页面 · 已返回");
        assert_eq!(c.tool_progress.len(), 1);
        display_event(
            &mut c,
            7,
            "text.delta",
            json!({"text":"tool.started fake model text"}),
        );
        assert_eq!(c.view().progress, "读取当前页面 · 已返回");
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
                tool: "move_selected_image".into(),
                arguments: json!({}),
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
    fn unknown_reconnect_is_original_restore_only_and_never_replays_input() {
        let mut f = Fixture::new("finish");
        f.ready();
        f.controller.editor_recovery.push(EditorRecovery {
            operation_id: "scm2:1:8:client-1:5:doc-1:1".into(),
            client_sequence: 1,
            project_id: f.identity.project_id.clone(),
            document_session_id: f.identity.document_session_id.clone(),
            retry_registration: false,
        });
        f.controller.write_in_flight = true;
        f.controller.write_identity = Some((
            "original-turn".into(),
            "original-invocation".into(),
            "export_canvas_png".into(),
        ));
        f.controller.unknown();
        f.controller.disconnect();
        f.until(|c| c.process.is_none());
        assert!(f.controller.view().can_connect);
        let config = |f: &Fixture| {
            let entry = f.directory.join("fixture.py");
            ConnectionConfig {
                launch: LaunchSpec {
                    executable: fs::canonicalize("/usr/bin/python3").unwrap(),
                    entrypoint: entry.clone(),
                    environment: BTreeMap::new(),
                },
                document: f.identity.clone(),
                client_id: "client-1".into(),
                mcp_client_path: entry,
                mcp_environment: BTreeMap::from([(
                    "TMPDIR".into(),
                    f.directory.to_string_lossy().into_owned(),
                )]),
                session_directory: f.directory.clone(),
                local_session_id: "fixture-session".into(),
                restore: true,
                provider: json!({"protocol":"fixture","model":"finish","apiKey":"inference-secret"}),
                read_token: "read-secret".into(),
                write_token: None,
            }
        };
        for changed in 0..4 {
            let mut c = config(&f);
            match changed {
                0 => c.restore = false,
                1 => c.client_id = "other-client".into(),
                2 => c.document.document_session_id = "other-document".into(),
                _ => c.provider["protocol"] = json!("seecut-managed"),
            }
            assert_eq!(f.controller.connect(c), Err(UiError::VerifyRequired));
        }
        f.controller.connect(config(&f)).unwrap();
        assert!(f.controller.unknown_latched && f.controller.write_in_flight);
        assert_eq!(
            f.controller.write_identity.as_ref().unwrap().1,
            "original-invocation"
        );
        // The fixture's bare clean restore cannot prove the original operation.
        f.until(|c| c.process.is_none());
        assert!(f.controller.unknown_latched && !f.controller.view().can_send);
        assert_eq!(f.controller.editor_recovery.len(), 1);
        let methods = fs::read_to_string(f.directory.join("methods.txt")).unwrap();
        assert!(methods.ends_with("initialize\nsession.restore\n"));
        assert!(!methods.contains("send\n"));
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
    fn first_canvas_write_child_exit_restores_durable_original_without_final_result() {
        for scenario in ["exit-insert", "exit-export", "first-terminal-lost"] {
            let mut f = Fixture::new(scenario);
            f.ready();
            f.controller.send_text("执行原画布操作").unwrap();
            f.until(|c| c.process.is_none());
            assert!(f.controller.editor_recovery.is_empty());
            assert!(f.controller.unknown_latched && f.controller.write_in_flight);
            assert!(f.controller.view().can_connect);
            for changed in 0..6 {
                let mut config = f.original_restore_config(scenario);
                match changed {
                    0 => config.restore = false,
                    1 => config.client_id = "other-client".into(),
                    2 => config.document.document_session_id = "other-document".into(),
                    3 => config.local_session_id = "other-session".into(),
                    4 => config.document.instance_id = "other-instance".into(),
                    _ => config.document.project_id = "other-project".into(),
                }
                assert_eq!(f.controller.connect(config), Err(UiError::VerifyRequired));
            }
            // Managed recovery additionally retains the original account fingerprint.
            f.controller.managed_mode = true;
            f.controller.cost_account_fingerprint =
                Some(sha2::Sha256::digest(b"original-account").to_vec());
            let mut wrong_account = f.original_restore_config(scenario);
            wrong_account.provider =
                json!({"protocol":"seecut-managed","sessionToken":"other-account"});
            assert_eq!(
                f.controller.connect(wrong_account),
                Err(UiError::VerifyRequired)
            );
            f.controller.managed_mode = false;
            f.controller.cost_account_fingerprint = None;
            f.controller
                .connect(f.original_restore_config(scenario))
                .unwrap();
            f.until(|c| c.view().can_reconcile);
            assert_eq!(
                f.controller.send_text("不可重放"),
                Err(UiError::VerifyRequired)
            );
            let original = f.controller.editor_recovery[0].clone();
            f.controller.reconcile_editor(&original).unwrap();
            f.until(|c| c.view().outcome == Some(TaskOutcome::OperationVerified));
            let record: Value = serde_json::from_slice(
                &fs::read(f.directory.join("original-operation.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(record["writes"], 1);
            assert_eq!(record["queries"], 1);
            let requests = f.requests();
            assert_eq!(requests.iter().filter(|r| r["method"] == "send").count(), 1);
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r["method"] == "editor.reconcile")
                    .count(),
                1
            );
            assert!(!f.controller.unknown_latched && !f.controller.write_in_flight);
        }
    }
    #[test]
    fn durable_reconciliation_lost_reply_restores_and_queries_same_terminal_receipt() {
        let mut f = Fixture::new("lost-reconcile-reply");
        f.ready();
        f.controller.send_text("导出原画布").unwrap();
        f.until(|c| c.view().can_reconcile);
        let original = f.controller.editor_recovery[0].clone();
        f.controller.reconcile_editor(&original).unwrap();
        f.until(|c| c.process.is_none());
        assert!(f.controller.unknown_latched && f.controller.view().can_connect);
        f.controller
            .connect(f.original_restore_config("lost-reconcile-reply"))
            .unwrap();
        f.until(|c| c.view().can_reconcile);
        assert_eq!(
            f.controller.editor_recovery[0].query_arguments(),
            original.query_arguments()
        );
        assert!(!f.controller.editor_recovery[0].retry_registration);
        f.controller.reconcile_editor(&original).unwrap();
        f.until(|c| c.view().outcome == Some(TaskOutcome::OperationVerified));
        let record: Value =
            serde_json::from_slice(&fs::read(f.directory.join("original-operation.json")).unwrap())
                .unwrap();
        assert_eq!(record["writes"], 1);
        assert_eq!(record["queries"], 2);
        assert_eq!(
            f.requests()
                .iter()
                .filter(|r| r["method"] == "send")
                .count(),
            1
        );
        assert!(!f.controller.unknown_latched && !f.controller.write_in_flight);
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
