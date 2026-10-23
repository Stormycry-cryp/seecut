// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal UI-independent controller for the documented HOST-IPC-V1 protocol.
//! Only the trusted host constructs configuration; no credential discovery or automatic retry.
use super::agent_process::{self, BridgeError, Event, Handle, LaunchSpec};
use crate::agent_app_control_input::{
    InputBinding, Proposal as AppControlProposal, Tracker as AppControlTracker,
};
use crate::agent_observation::DocumentObservationBinding;
use crate::agent_tasks::{self, TaskProjection, TaskStatus, TaskSummary};
use serde_json::{Value, json};
use sha2::Digest;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const TEXT_LIMIT: usize = 64 * 1024;
const MAX_TOOL_PROGRESS: usize = 12;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub use super::agent_identity::{AgentScope, DocumentIdentity};
#[cfg(test)]
use concat_editor_mcp::app_read::AppViewBinding;
/// Credentials deliberately have no Debug implementation and are never written to disk/logs.
pub struct ConnectionConfig {
    pub launch: LaunchSpec,
    pub scope: AgentScope,
    pub client_id: String,
    pub mcp_client_path: PathBuf,
    pub mcp_environment: BTreeMap<String, String>,
    pub session_directory: PathBuf,
    pub local_session_id: String,
    pub restore: bool,
    pub provider: Value,
    pub read_token: String,
    pub write_token: Option<String>,
    pub app_control_token: Option<String>,
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
    TaskRevisionChanged,
    ObservationUnavailable,
    ObservationChanged,
    ObservationInvalid,
    ObservationUnsupported,
    TaskObservationUnavailable,
    TaskObservationMissing,
    TaskObservationChanged,
    TaskObservationConflict,
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
            Self::TaskRevisionChanged => "任务修订已变化，补充要求已保留，请核对后重试原请求",
            Self::InvalidInput => "当前模型无法处理这些内容，请调整文字或附件",
            Self::ObservationUnavailable => {
                "当前运行组件不支持保存发问时的文档状态，请更新运行组件"
            }
            Self::ObservationChanged => "发问时的工程、选择或视图已变化，请核对后重新发送",
            Self::ObservationInvalid => "无法核对发问时的文档记录，请检查运行组件",
            Self::ObservationUnsupported => "当前输入方式尚不支持文档观察绑定",
            Self::TaskObservationUnavailable => {
                "当前运行组件不支持保存任务发起和补充时的文档状态，请更新运行组件"
            }
            Self::TaskObservationMissing => {
                "原任务请求缺少发问时的文档记录，已保留且不会重发，请先查询原任务状态"
            }
            Self::TaskObservationChanged => {
                "原问题引用的页面、选择或内容已变化，请重新提出要求；已有任务请先停止后重建"
            }
            Self::TaskObservationConflict => {
                agent_tasks::blocked_reason_label("human_observation_conflict")
            }
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
    pub is_flow: bool,
    pub is_clip: bool,
}

/// A bounded host projection. Only the current pending queues decide recovery;
/// historical samples are display evidence and are never replayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppReadRecovery {
    pub can_abandon: bool,
    pub reason: String,
    evidence: Value,
}
impl AppReadRecovery {
    fn parse(value: &Value) -> Option<Self> {
        let can_abandon = value["canAbandon"].as_bool()?;
        let reason = value["reason"].as_str()?;
        if !matches!(
            reason,
            "ready"
                | "nothing_pending"
                | "active_turn"
                | "billing_unavailable"
                | "account_scope"
                | "billing_pending"
                | "unrecognized_history"
        ) || can_abandon != (reason == "ready")
        {
            return None;
        }
        let ids = |key: &str, max: usize| -> Option<usize> {
            let values = value[key].as_array()?;
            if values.len() > max {
                return None;
            }
            let mut unique = std::collections::BTreeSet::new();
            for id in values {
                let id = id.as_str()?;
                if id.is_empty() || id.len() > 256 || !unique.insert(id) {
                    return None;
                }
            }
            Some(values.len())
        };
        let pending = ids("pendingInputIds", 8)?;
        let reads = ids("readInvocationIds", 128)?;
        if can_abandon && pending == 0 && reads == 0 {
            return None;
        }
        if reason == "nothing_pending" && (pending != 0 || reads != 0) {
            return None;
        }
        for (sample, total, truncated) in [
            (
                "recentWithdrawnInputIds",
                "withdrawnInputTotal",
                "withdrawnInputTruncated",
            ),
            (
                "recentAbandonedReadInvocationIds",
                "abandonedReadInvocationTotal",
                "abandonedReadInvocationTruncated",
            ),
        ] {
            let count = ids(sample, 64)? as u64;
            let total = value[total]
                .as_u64()
                .filter(|n| *n <= 9_007_199_254_740_991)?;
            if count != total.min(64) || value[truncated].as_bool()? != (total > 64) {
                return None;
            }
        }
        Some(Self {
            can_abandon,
            reason: reason.into(),
            evidence: value.clone(),
        })
    }
    pub fn message(&self) -> &'static str {
        match self.reason.as_str() {
            "ready" => "旧问题无法继续读取原页面。结束后可发送新问题，历史记录会保留。",
            "active_turn" => "旧问题仍在处理，请先停止并等待结束",
            "billing_pending" => "请先核实原问题的推理和积分，再结束旧问题",
            "billing_unavailable" => "暂时无法核实原问题的积分，请稍后查询",
            "account_scope" => "请使用原账户恢复此会话",
            "unrecognized_history" => "旧问题的记录暂时无法核实，请保留会话并检查运行时版本",
            _ => "",
        }
    }
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
impl View<'_> {
    pub fn unknown_or_blocked(&self) -> bool {
        self.phase == Phase::OutcomeUnknown
            || self.outcome == Some(TaskOutcome::OutcomeUnknown)
            || self.account_expired
            || self
                .error
                .is_some_and(|error| !matches!(error, UiError::Busy | UiError::TaskRevisionChanged))
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolAction {
    Context,
    Project,
    Preview,
    Capabilities,
    Move,
    Library,
    LibraryAssets,
    Insert,
    Transform,
    CanvasEdit,
    ClipEdit,
    Save,
    Export,
    Operation,
    RegistrationRetry,
    FlowRead,
    FlowWrite,
    TaskProgress,
    AppRead,
    AppNavigate,
    AppOperation,
}
impl ToolAction {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "app_navigate" => Some(Self::AppNavigate),
            "app_operation" => Some(Self::AppOperation),
            "app_context" | "page_items" | "page_preview" => Some(Self::AppRead),
            "task_status" | "task_checkpoint" => Some(Self::TaskProgress),
            "context" => Some(Self::Context),
            "project" => Some(Self::Project),
            "preview" => Some(Self::Preview),
            "capabilities" => Some(Self::Capabilities),
            "move_selected_image" => Some(Self::Move),
            "library_images" => Some(Self::Library),
            "library_assets" => Some(Self::LibraryAssets),
            "insert_library_image" => Some(Self::Insert),
            "transform_canvas_layer" => Some(Self::Transform),
            "canvas_edit" => Some(Self::CanvasEdit),
            "clip_edit" => Some(Self::ClipEdit),
            "save_canvas_project" => Some(Self::Save),
            "export_canvas_png" => Some(Self::Export),
            "canvas_operation" | "clip_operation" => Some(Self::Operation),
            "retry_canvas_registration" => Some(Self::RegistrationRetry),
            "flow_get_workspace"
            | "flow_read_canvas"
            | "flow_list_node_types"
            | "flow_get_node_tools"
            | "flow_get_node_config"
            | "flow_get_task"
            | "flow_get_operation" => Some(Self::FlowRead),
            "flow_add_node" | "flow_update_node" | "flow_connect_nodes" | "flow_delete_nodes"
            | "flow_switch_canvas" | "flow_execute_node" => Some(Self::FlowWrite),
            _ => None,
        }
    }
    fn is_write(self) -> bool {
        matches!(
            self,
            Self::AppNavigate
                | Self::Move
                | Self::Insert
                | Self::Transform
                | Self::CanvasEdit
                | Self::ClipEdit
                | Self::Save
                | Self::Export
                | Self::RegistrationRetry
                | Self::FlowWrite
        )
    }
    fn label(self) -> &'static str {
        match self {
            Self::AppRead => "读取当前应用页面",
            Self::AppNavigate => "切换应用页面",
            Self::AppOperation => "核对页面切换结果",
            Self::TaskProgress => "核对任务进度",
            Self::Context => "读取当前页面",
            Self::Project => "读取工程信息",
            Self::Preview => "查看画布预览",
            Self::Capabilities => "检查可用操作",
            Self::Move => "发送移动请求",
            Self::Library => "查找库内图片",
            Self::LibraryAssets => "查找本地素材",
            Self::Insert => "放入画布",
            Self::Transform => "调整图层",
            Self::CanvasEdit => "编辑画布",
            Self::ClipEdit => "编辑剪辑",
            Self::Save => "保存画布",
            Self::Export => "导出并入库",
            Self::Operation => "核对操作结果",
            Self::RegistrationRetry => "登记原 PNG",
            Self::FlowRead => "读取 Flow",
            Self::FlowWrite => "操作 Flow",
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskCommand {
    Pause,
    Resume,
    Stop,
    Status,
    Update,
}
impl TaskCommand {
    fn method(self) -> &'static str {
        match self {
            Self::Pause => "task.pause",
            Self::Resume => "task.resume",
            Self::Stop => "task.stop",
            Self::Status => "task.status",
            Self::Update => "task.update",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppRecovery {
    pub invocation_id: String,
}
enum Request {
    Initialize,
    Permissions {
        revision: u64,
        operate: bool,
    },
    Create,
    Send {
        input_id: String,
    },
    TaskCreate {
        task_id: String,
        previous_task_id: Option<String>,
    },
    TaskControl {
        task_id: String,
        action: TaskCommand,
        update_id: Option<String>,
        fence_revision: u64,
    },
    Stop,
    Approve {
        approval_id: String,
    },
    Close,
    Reconcile,
    RecoveryStatus,
    AbandonAppReads,
    AppReconcile {
        invocation_id: String,
    },
    AppControlAuthorize {
        allowed: bool,
    },
    EditorReconcile {
        operation_id: String,
    },
}
struct Pending {
    id: String,
    request: Request,
    since: Instant,
}

enum InputObservation {
    Document(String, Box<DocumentObservationBinding>),
    App(String, InputBinding),
}
impl InputObservation {
    fn input_id(&self) -> &str {
        match self {
            Self::Document(id, _) | Self::App(id, _) => id,
        }
    }
}

pub struct Controller {
    process: Option<Handle>,
    scope: Option<AgentScope>,
    session_id: String,
    restore: bool,
    recover_existing: bool,
    model_selection_supported: bool,
    document_observation_supported: bool,
    task_observation_supported: bool,
    tasks_supported: bool,
    ipc_generation: u64,
    tasks: TaskProjection,
    task_fenced: bool,
    retired_task_turns: VecDeque<String>,
    task_permit_revision: u64,
    task_requested_epoch: Option<u64>,
    active_task_turn: bool,
    task_turn_binding: Option<TaskSummary>,
    ended_task_turns: VecDeque<String>,
    acknowledged_task_update: Option<String>,
    rejected_task_update: Option<String>,
    permissions_supported: bool,
    permission_revision: u64,
    session_opened: bool,
    phase: Phase,
    pending: VecDeque<Pending>,
    counter: u64,
    turn_id: Option<String>,
    // One host-created ordinary input, before or after its real turn starts.
    send_turn: Option<(String, Option<String>)>,
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
    app_read_recovery: Option<AppReadRecovery>,
    app_control_supported: bool,
    app_recovery: Vec<AppRecovery>,
    app_control_inputs: AppControlTracker,
    app_restore_required: bool,
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
            scope: None,
            session_id: String::new(),
            restore: false,
            recover_existing: false,
            model_selection_supported: false,
            document_observation_supported: false,
            task_observation_supported: false,
            tasks_supported: false,
            ipc_generation: 0,
            tasks: TaskProjection::default(),
            task_fenced: false,
            retired_task_turns: VecDeque::new(),
            task_permit_revision: 0,
            task_requested_epoch: None,
            active_task_turn: false,
            task_turn_binding: None,
            ended_task_turns: VecDeque::new(),
            acknowledged_task_update: None,
            rejected_task_update: None,
            permissions_supported: false,
            permission_revision: 0,
            session_opened: false,
            phase: Phase::Disconnected,
            pending: VecDeque::new(),
            counter: 0,
            turn_id: None,
            send_turn: None,
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
            app_read_recovery: None,
            app_control_supported: false,
            app_recovery: Vec::new(),
            app_control_inputs: AppControlTracker::default(),
            app_restore_required: false,
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
                && self
                    .scope
                    .as_ref()
                    .is_some_and(|old| old.same_restore_scope(&config.scope))
                && self.managed_mode == (config.provider["protocol"] == "seecut-managed"))
        {
            return Err(UiError::VerifyRequired);
        }
        if self.process.is_some() {
            return Err(UiError::Busy);
        }
        let valid_id = |id: &str| max64(id);
        if config.scope.instance_id().is_empty()
            || match &config.scope {
                AgentScope::Document(document) => {
                    document.project_id.is_empty()
                        || !valid_id(&document.document_session_id)
                        || config.app_control_token.is_some()
                }
                AgentScope::App(app) => {
                    !concat_editor_mcp::AppReadScope {
                        account_scope: app.account_scope.clone(),
                        app_session_id: app.app_session_id.clone(),
                        app_control_protocol_version: None,
                    }
                    .valid()
                        || config.write_token.is_some()
                }
            }
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
            && self
                .scope
                .as_ref()
                .is_some_and(|old| old.same_restore_scope(&config.scope))
            && self.managed_mode
            && config.provider["protocol"] == "seecut-managed"
            && account_fingerprint.is_some()
            && self.cost_account_fingerprint == account_fingerprint;
        self.cost_account_fingerprint = account_fingerprint;
        self.cost_waiting_for_turn = false;
        self.send_turn = None;
        if !preserve_cost {
            self.cost_turn_id = None;
            self.budget_label = "0".into();
        }
        self.session_id = config.local_session_id;
        self.restore = config.restore;
        self.recover_existing = false;
        self.app_control_supported = false;
        self.app_control_inputs = AppControlTracker::default();
        self.model_selection_supported = false;
        self.document_observation_supported = false;
        self.task_observation_supported = false;
        self.tasks_supported = false;
        self.ipc_generation = self
            .ipc_generation
            .checked_add(1)
            .ok_or(UiError::Protocol)?;
        crate::clip_export_fence::revoke_session(
            &self.session_id,
            concat_host::export_publication::PublishRevocation::SessionChanged,
        );
        self.tasks.begin_connection(self.ipc_generation);
        self.task_fenced = self.tasks.current_task_id.is_some();
        self.permissions_supported = false;
        self.permission_revision = 0;
        self.secrets = vec![config.read_token.clone()];
        if let Some(token) = &config.app_control_token {
            self.secrets.push(token.clone());
        }
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
        self.app_read_recovery = None;
        self.last_model_state = None;
        self.billing_fingerprint = Value::Null;
        self.secrets.retain(|value| !value.is_empty());
        let mut params = json!({"protocolVersion":1,"appProtocolVersion":1,"instanceId":config.scope.instance_id(),"clientId":config.client_id,"clientPath":config.mcp_client_path,"mcpEnvironment":config.mcp_environment,"sessionDirectory":config.session_directory,"provider":config.provider,"readToken":config.read_token});
        match &config.scope {
            AgentScope::Document(_) => {
                params["taskProtocolVersion"] = json!(1);
                params["documentObservationProtocolVersion"] = json!(1);
                params["taskObservationProtocolVersion"] = json!(1);
                if let Some(token) = &config.write_token {
                    params["writeToken"] = json!(token);
                }
            }
            AgentScope::App(app) => {
                params["scopeKind"] = json!("app");
                params["appReadProtocolVersion"] = json!(1);
                params["appControlProtocolVersion"] = json!(1);
                if let Some(token) = &config.app_control_token {
                    params["appControlToken"] = json!(token);
                }
                params["accountScope"] = json!(app.account_scope);
                params["appSessionId"] = json!(app.app_session_id);
            }
        }
        // The runtime distinguishes absent writeToken from null.
        if params["writeToken"].is_null() {
            params.as_object_mut().unwrap().remove("writeToken");
        }
        let handle = agent_process::start(config.launch).map_err(bridge_error)?;
        self.process = Some(handle);
        self.scope = Some(config.scope);
        self.pending.clear();
        self.send_turn = None;
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
    pub fn supports_app_control(&self) -> bool {
        self.process.is_some() && self.app_scope() && self.app_control_supported
    }
    pub(crate) fn take_app_control_proposals(&mut self) -> Vec<AppControlProposal> {
        self.app_control_inputs.take_pending()
    }
    pub(crate) fn app_control_proposal_current(&self, proposal: &AppControlProposal) -> bool {
        self.supports_app_control()
            && !self.blocked()
            && self.turn_id.as_deref() == Some(&proposal.turn_id)
            && self.app_control_inputs.current(proposal)
    }
    pub(crate) fn app_control_should_fence(&self) -> bool {
        self.app_scope()
            && (self.turn_id.is_none()
                || self.blocked()
                || self.ignore_runtime_events
                || matches!(
                    self.phase,
                    Phase::Disconnected | Phase::Failed | Phase::Closing | Phase::Stopping
                ))
    }
    pub(crate) fn fence_app_control(&mut self) {
        self.app_control_inputs.fence();
    }
    /// Host-only ACK follows installation of the exact native permit.
    pub(crate) fn authorize_app_control(
        &mut self,
        proposal: &AppControlProposal,
        allowed: bool,
    ) -> Result<(), UiError> {
        if !self.supports_app_control() || (allowed && !self.app_control_proposal_current(proposal))
        {
            return Err(UiError::Protocol);
        }
        self.enqueue("app.control.authorize", json!({"sessionId": proposal.session_id,"inputId":proposal.intent.input_id,"turnId":proposal.turn_id,"invocationId":proposal.invocation_id,"operationId":proposal.intent.operation_id,"allowed":allowed}), Request::AppControlAuthorize { allowed })
    }
    pub fn supports_model_selection(&self) -> bool {
        self.process.is_some() && self.model_selection_supported
    }
    pub fn supports_document_observation(&self) -> bool {
        self.process.is_some()
            && self.document_observation_supported
            && self
                .scope
                .as_ref()
                .is_some_and(|scope| scope.document().is_some())
    }
    pub fn document_observation_unavailable(&self) -> bool {
        self.phase == Phase::Ready
            && self.session_opened
            && self.process.is_some()
            && self
                .scope
                .as_ref()
                .is_some_and(|scope| scope.document().is_some())
            && !self.supports_document_observation()
    }
    pub fn acknowledged_task_update(&self) -> Option<&str> {
        self.acknowledged_task_update.as_deref().or_else(|| {
            self.current_task()
                .and_then(|v| v.last_update_id.as_deref())
        })
    }
    pub fn rejected_task_update(&self) -> Option<&str> {
        self.rejected_task_update.as_deref()
    }
    pub fn retry_task_update(
        &mut self,
        intent: &agent_tasks::TaskUpdateIntent,
    ) -> Result<(), UiError> {
        if !intent.valid() || self.current_task_id() != Some(intent.task_id.as_str()) {
            return Err(UiError::InvalidInput);
        }
        let binding = self.checked_task_observation(intent.observation_binding.as_ref())?;
        if !self.can_task_command(TaskCommand::Update) {
            return Err(UiError::Busy);
        }
        self.task_requested_epoch = self
            .current_task()
            .and_then(|v| v.control_epoch.checked_add(1));
        self.fence_task();
        self.enqueue("task.update",json!({"sessionId":self.session_id,"taskId":intent.task_id,"clientRequestId":intent.client_request_id,"expectedRevision":intent.expected_revision,"message":intent.message,"observationBinding":binding}),Request::TaskControl {task_id:intent.task_id.clone(),action:TaskCommand::Update,update_id:Some(intent.client_request_id.clone()),fence_revision:self.task_permit_revision})
    }
    pub fn supports_tasks(&self) -> bool {
        self.process.is_some() && self.tasks_supported
    }
    pub fn supports_task_observation(&self) -> bool {
        self.supports_tasks()
            && self.task_observation_supported
            && self
                .scope
                .as_ref()
                .is_some_and(|scope| scope.document().is_some())
    }
    pub fn task_observation_unavailable(&self) -> bool {
        self.session_opened
            && self.supports_tasks()
            && matches!(
                self.phase,
                Phase::Ready | Phase::Running | Phase::AwaitingApproval | Phase::Stopping
            )
            && !self.supports_task_observation()
    }
    fn checked_task_observation(
        &self,
        binding: Option<&DocumentObservationBinding>,
    ) -> Result<Value, UiError> {
        if !self.supports_task_observation() {
            return Err(UiError::TaskObservationUnavailable);
        }
        let binding = binding.ok_or(UiError::TaskObservationMissing)?;
        let identity = self
            .scope
            .as_ref()
            .and_then(AgentScope::document)
            .ok_or(UiError::DocumentChanged)?;
        if !binding.matches_identity(identity, &self.editor_client) {
            return Err(UiError::ObservationChanged);
        }
        serde_json::to_value(binding).map_err(|_| UiError::ObservationInvalid)
    }
    pub fn restore_task(&mut self, task_id: Option<String>) -> bool {
        if self.process.is_some() {
            return false;
        }
        let Some(tasks) = TaskProjection::restored(task_id) else {
            return false;
        };
        self.tasks = tasks;
        true
    }
    pub fn current_task_id(&self) -> Option<&str> {
        self.tasks.current_task_id.as_deref()
    }
    pub fn current_task(&self) -> Option<&TaskSummary> {
        self.tasks.current()
    }
    pub fn task_permit_revision(&self) -> u64 {
        self.task_permit_revision
    }
    pub fn withdraw_task_approvals(&mut self) {
        if self.current_task_id().is_some() {
            self.fence_task();
        }
    }
    pub fn ipc_generation(&self) -> u64 {
        self.ipc_generation
    }
    pub fn task_turn_binding(&self) -> Option<&TaskSummary> {
        self.task_turn_binding.as_ref()
    }
    pub fn task_turn_ended(&self, turn: &str) -> bool {
        self.ended_task_turns.iter().any(|v| v == turn)
    }
    pub fn current_turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }
    pub fn is_task_turn(&self) -> bool {
        self.active_task_turn && self.turn_id.is_some()
    }
    pub fn task_idle(&self) -> bool {
        self.supports_tasks()
            && self.turn_id.is_none()
            && self.pending.is_empty()
            && self.current_task().is_some_and(|v| {
                matches!(
                    v.status,
                    TaskStatus::Paused
                        | TaskStatus::Stopped
                        | TaskStatus::Completed
                        | TaskStatus::Failed
                        | TaskStatus::Blocked
                )
            })
    }
    pub fn task_active(&self) -> bool {
        self.tasks_supported
            && self.current_task_id().is_some()
            && self
                .current_task()
                .is_none_or(|task| !task.status.terminal())
    }
    fn task_approval_fenced(&self) -> bool {
        self.tasks_supported
            && self.active_task_turn
            && (self.task_fenced
                || self
                    .turn_id
                    .as_ref()
                    .is_some_and(|turn| self.retired_task_turns.contains(turn)))
    }
    fn fence_task(&mut self) {
        crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::Stopped);
        for turn in [
            self.turn_id.as_ref(),
            self.approval.as_ref().map(|v| &v.turn_id),
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        {
            if !self.retired_task_turns.contains(&turn) {
                self.retired_task_turns.push_back(turn);
            }
        }
        while self.retired_task_turns.len() > 128 {
            self.retired_task_turns.pop_front();
        }
        self.task_fenced = true;
        self.approval = None;
        self.task_permit_revision = self.task_permit_revision.saturating_add(1);
    }
    fn task_state(&mut self, generation: u64, value: &Value) {
        if generation != self.ipc_generation {
            return;
        }
        let Some(next) = TaskSummary::parse(value, &self.session_id) else {
            self.fail(UiError::Protocol);
            return;
        };
        if self.current_task_id() != Some(next.id.as_str()) {
            return;
        }
        let old_epoch = self.current_task().map(|v| v.control_epoch);
        let old_turn = self.current_task().and_then(|v| v.active_turn_id.clone());
        if !self.tasks.accept(generation, &self.session_id, value) {
            return;
        }
        if old_epoch.is_some_and(|v| v != next.control_epoch)
            || matches!(next.status, TaskStatus::Paused | TaskStatus::Stopped)
        {
            self.fence_task();
            if let Some(turn) = old_turn
                && !self.retired_task_turns.contains(&turn)
            {
                self.retired_task_turns.push_back(turn);
            }
        }
        if matches!(
            next.status,
            TaskStatus::Paused | TaskStatus::Stopped | TaskStatus::Failed
        ) {
            crate::clip_export_fence::revoke_session(
                &self.session_id,
                concat_host::export_publication::PublishRevocation::Stopped,
            );
        }
        if next.status == TaskStatus::Running
            && self.task_requested_epoch == Some(next.control_epoch)
        {
            self.task_fenced = false;
        }
        if self.turn_id.is_none() {
            self.outcome = match next.status {
                TaskStatus::Completed => Some(TaskOutcome::Completed),
                TaskStatus::Failed => Some(TaskOutcome::Failed),
                TaskStatus::Stopped => Some(TaskOutcome::Stopped),
                _ => self.outcome,
            };
        }
    }
    pub fn can_task_command(&self, action: TaskCommand) -> bool {
        if !self.supports_tasks()
            || !self.session_opened
            || self.ignore_runtime_events
            || self.current_task_id().is_none()
            || self.pending.iter().any(|p| match (&p.request, action) {
                // Human withdrawal has priority over unknown create/status ACKs
                // and pending resume/update. Do not queue duplicate withdrawals.
                (
                    Request::TaskControl {
                        action: TaskCommand::Pause | TaskCommand::Stop,
                        ..
                    },
                    TaskCommand::Pause | TaskCommand::Stop,
                ) => true,
                (
                    Request::TaskControl { .. } | Request::TaskCreate { .. },
                    TaskCommand::Pause | TaskCommand::Stop,
                ) => false,
                (Request::TaskControl { .. } | Request::TaskCreate { .. }, _) => true,
                _ => false,
            })
        {
            return false;
        }
        let status = self.current_task().map(|v| v.status);
        match action {
            TaskCommand::Status => true,
            TaskCommand::Pause => matches!(
                status,
                Some(TaskStatus::Running | TaskStatus::WaitingJob | TaskStatus::Blocked)
            ),
            TaskCommand::Stop => status.is_some_and(|v| {
                !matches!(
                    v,
                    TaskStatus::Completed | TaskStatus::Stopped | TaskStatus::Failed
                )
            }),
            TaskCommand::Resume => {
                !self.blocked()
                    && matches!(
                        status,
                        Some(TaskStatus::Paused | TaskStatus::Stopped | TaskStatus::Blocked)
                    )
            }
            TaskCommand::Update => {
                !self.blocked()
                    && status
                        .is_some_and(|v| !matches!(v, TaskStatus::Completed | TaskStatus::Failed))
            }
        }
    }
    pub fn task_command(&mut self, action: TaskCommand) -> Result<(), UiError> {
        if action == TaskCommand::Update {
            return Err(UiError::InvalidInput);
        }
        if !self.can_task_command(action) {
            return Err(UiError::Busy);
        }
        let task_id = self.current_task_id().ok_or(UiError::Protocol)?.to_owned();
        let params = json!({"sessionId":self.session_id,"taskId":task_id});
        if matches!(action, TaskCommand::Resume | TaskCommand::Update) {
            self.task_requested_epoch = self
                .current_task()
                .and_then(|v| v.control_epoch.checked_add(1));
        }
        if matches!(
            action,
            TaskCommand::Pause | TaskCommand::Stop | TaskCommand::Update
        ) {
            if let Some(turn) = self.current_task().and_then(|v| v.active_turn_id.clone())
                && !self.retired_task_turns.contains(&turn)
            {
                self.retired_task_turns.push_back(turn);
            }
            self.fence_task();
            self.task_requested_epoch = None;
        }
        self.enqueue(
            action.method(),
            params,
            Request::TaskControl {
                task_id,
                action,
                update_id: None,
                fence_revision: self.task_permit_revision,
            },
        )
    }
    pub(crate) fn create_task(
        &mut self,
        intent: &agent_tasks::TaskIntent,
        content: Vec<Value>,
        selection: Value,
    ) -> Result<(), UiError> {
        if !self.supports_tasks() {
            return Err(UiError::Protocol);
        }
        if !intent.valid() {
            return Err(UiError::InvalidInput);
        }
        let binding = self.checked_task_observation(intent.initial_observation_binding.as_ref())?;
        if !self.view().can_send || self.task_active() {
            return Err(UiError::Busy);
        }
        if !valid_model_selection(&selection) {
            return Err(UiError::Configuration);
        }
        let mut params = agent_tasks::create_request(
            &self.session_id,
            &intent.client_request_id,
            &intent.goal,
            &content,
            &selection,
            intent.max_task_credits,
        )
        .ok_or(UiError::InvalidInput)?;
        params["initialObservationBinding"] = binding;
        let task_id = intent.task_id.clone();
        let previous_task_id = self.current_task_id().map(str::to_owned);
        self.enqueue(
            "task.create",
            params,
            Request::TaskCreate {
                task_id: task_id.clone(),
                previous_task_id,
            },
        )?;
        crate::clip_export_fence::revoke_session(
            &self.session_id,
            concat_host::export_publication::PublishRevocation::Stopped,
        );
        self.tasks.select_created_task(&task_id);
        self.task_fenced = false;
        self.retired_task_turns.clear();
        self.text.clear();
        self.tool_progress.clear();
        self.progress.clear();
        self.outcome = None;
        self.error = None;
        Ok(())
    }
    fn can_restore_original(&self) -> bool {
        self.session_opened
            && !self.session_id.is_empty()
            && !self.editor_client.is_empty()
            && self.scope.is_some()
    }
    pub fn view(&self) -> View<'_> {
        View {
            can_connect: self.process.is_none()
                && (!self.unknown_latched || self.can_restore_original()),
            can_reconcile: ((self.managed_mode && self.model_unknown)
                || (!self.editor_recovery.is_empty() && self.unknown_latched)
                || (!self.app_recovery.is_empty()
                    && self.app_control_supported
                    && self.unknown_latched))
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
            can_send: self.phase == Phase::Ready
                && self.pending.is_empty()
                && !self.blocked()
                && (!self.tasks_supported
                    || self.tasks.current_task_id.is_none()
                    || self.tasks.current().is_some_and(|t| t.status.terminal())
                    || self.task_idle()),
            can_stop: self.turn_id.is_some()
                && !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.request, Request::Stop))
                && matches!(
                    self.phase,
                    Phase::Running | Phase::AwaitingApproval | Phase::OutcomeUnknown
                ),
            can_approve: !self.task_approval_fenced()
                && self.phase == Phase::AwaitingApproval
                && self.approval.is_some()
                && !self.blocked()
                && !self.pending.iter().any(|p| {
                    matches!(
                        p.request,
                        Request::Stop | Request::Approve { .. } | Request::Permissions { .. }
                    )
                }),
        }
    }
    /// Replace only the editor transport credentials within the opened session.
    pub fn update_permissions(&mut self, write_token: Option<String>) -> Result<(), UiError> {
        if self
            .scope
            .as_ref()
            .is_some_and(|scope| scope.app().is_some())
            || !self.session_opened
            || self.process.is_none()
            || !self.permissions_supported
        {
            return Err(UiError::Protocol);
        }
        self.permission_revision += 1;
        let revision = self.permission_revision;
        let operate = write_token.is_some();
        if let Some(token) = &write_token {
            self.secrets.push(token.clone());
        }
        self.approval = None;
        self.enqueue("session.permissions", json!({"version":1,"sessionId":self.session_id,
            "revision":revision,"mode":if operate {"operate"} else {"read"},"writeToken":write_token}),
            Request::Permissions { revision, operate })
    }
    pub fn update_app_control_permissions(&mut self, token: Option<String>) -> Result<(), UiError> {
        if !self.supports_app_control() || !self.permissions_supported || !self.session_opened {
            return Err(UiError::Protocol);
        }
        self.fence_app_control();
        self.permission_revision = self
            .permission_revision
            .checked_add(1)
            .ok_or(UiError::Protocol)?;
        let revision = self.permission_revision;
        let operate = token.is_some();
        if let Some(value) = &token {
            self.secrets.push(value.clone());
        }
        self.enqueue("session.permissions", json!({"version":1,"sessionId":self.session_id,"revision":revision,"mode":if operate {"operate"}else{"read"},"appControlToken":token}), Request::Permissions{revision,operate})
    }
    #[cfg(test)]
    pub(crate) fn send_text(&mut self, text: &str) -> Result<(), UiError> {
        self.send_text_with_selection(text, None)
    }
    /// Selection is consumed by this new input only. It is never installed as
    /// mutable controller state or attached to stop, reconciliation or recovery.
    #[cfg(test)]
    pub(crate) fn send_text_with_selection(
        &mut self,
        text: &str,
        selection: Option<Value>,
    ) -> Result<(), UiError> {
        self.send_content_with_selection(vec![json!({"type":"text","text":text})], selection)
    }
    #[cfg(test)]
    pub(crate) fn send_content_with_selection(
        &mut self,
        content: Vec<Value>,
        selection: Option<Value>,
    ) -> Result<(), UiError> {
        // Existing controller fixtures provide a deliberate native observation.
        // Production callers must supply the binding captured at the UI click.
        let observation = self
            .scope
            .as_ref()
            .and_then(AgentScope::document)
            .map(|identity| {
                let binding = serde_json::from_value(json!({
                    "appInstanceId": identity.instance_id,
                    "clientId": self.editor_client,
                    "kind": "canvas",
                    "projectId": identity.project_id,
                    "documentSessionId": identity.document_session_id,
                    "revision": 1, "contextRevision": 1, "selectionRevision": 1,
                    "selectedObjectIds": [], "viewStateId": null
                }))
                .unwrap();
                InputObservation::Document(
                    format!("input-{}", uuid::Uuid::new_v4()),
                    Box::new(binding),
                )
            });
        self.send_frozen_content(content, selection, observation)
    }
    pub(crate) fn send_document_content(
        &mut self,
        input_id: &str,
        content: Vec<Value>,
        selection: Option<Value>,
        observation: DocumentObservationBinding,
    ) -> Result<(), UiError> {
        self.send_frozen_content(
            content,
            selection,
            Some(InputObservation::Document(
                input_id.to_owned(),
                Box::new(observation),
            )),
        )
    }
    /// App observations are frozen by the human send action before attachment work.
    pub fn send_app_content(
        &mut self,
        input_id: &str,
        content: Vec<Value>,
        selection: Option<Value>,
        view: InputBinding,
    ) -> Result<(), UiError> {
        self.send_frozen_content(
            content,
            selection,
            Some(InputObservation::App(input_id.to_owned(), view)),
        )
    }
    fn send_frozen_content(
        &mut self,
        content: Vec<Value>,
        selection: Option<Value>,
        observation: Option<InputObservation>,
    ) -> Result<(), UiError> {
        if let Some(error) = self.blocking_error() {
            return Err(error);
        }
        let is_app = self
            .scope
            .as_ref()
            .is_some_and(|scope| scope.app().is_some());
        let valid_observation = match (&self.scope, &observation) {
            (Some(AgentScope::App(identity)), Some(InputObservation::App(id, view))) => {
                max64(id) && view.matches_scope(identity, &self.editor_client)
            }
            (
                Some(AgentScope::Document(identity)),
                Some(InputObservation::Document(id, binding)),
            ) => max64(id) && binding.matches_identity(identity, &self.editor_client),
            (None, None) => true,
            _ => false,
        };
        if !valid_observation {
            return Err(UiError::Configuration);
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
        if !is_app && self.scope.is_some() && !self.supports_document_observation() {
            return Err(UiError::ObservationUnavailable);
        }
        // IPC counters restart with the process; persisted sessions must never
        // mistake a new explicit question for an already accepted input.
        let input_id = observation
            .as_ref()
            .map(|observation| observation.input_id().to_owned())
            .unwrap_or_else(|| format!("input-{}", uuid::Uuid::new_v4()));
        let mut params = json!({"sessionId":self.session_id,"inputId":input_id,"mode":"follow_up","content":content});
        let app_input = match &observation {
            Some(InputObservation::App(_, view)) => Some(view.clone()),
            _ => None,
        };
        if self.app_control_supported
            && app_input
                .as_ref()
                .is_some_and(|binding| !self.app_control_inputs.can_remember(&input_id, binding))
        {
            return Err(UiError::InvalidInput);
        }
        match observation {
            Some(InputObservation::App(_, view)) => params["appViewBinding"] = json!(view.page()),
            Some(InputObservation::Document(_, binding)) => {
                params["observationBinding"] = json!(binding);
            }
            None => {}
        }
        if let Some(selection) = selection {
            params["modelSelection"] = selection;
        }
        self.enqueue(
            "send",
            params,
            Request::Send {
                input_id: input_id.clone(),
            },
        )?;
        if let Some(binding) = app_input.filter(|_| self.app_control_supported) {
            self.app_control_inputs.remember(&input_id, binding);
        }
        self.send_turn = Some((input_id, None));
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
        crate::clip_export_fence::revoke_session(
            &self.session_id,
            concat_host::export_publication::PublishRevocation::Stopped,
        );
        crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::Stopped);
        self.fence_app_control();
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
    pub fn app_scope(&self) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|scope| scope.app().is_some())
    }
    pub fn requires_app_restore(&self) -> bool {
        self.app_scope() && self.app_restore_required
    }
    pub fn app_read_recovery(&self) -> Option<&AppReadRecovery> {
        self.app_read_recovery.as_ref()
    }
    pub fn can_query_app_reads(&self) -> bool {
        self.app_scope()
            && self.session_opened
            && self.process.is_some()
            && self.turn_id.is_none()
            && self.pending.is_empty()
            && !self.ignore_runtime_events
            && !self.account_expired
    }
    pub fn can_abandon_app_reads(&self) -> bool {
        self.can_query_app_reads()
            && !self.model_unknown
            && self
                .app_read_recovery
                .as_ref()
                .is_some_and(|r| r.can_abandon)
    }
    pub fn query_app_reads(&mut self) -> Result<(), UiError> {
        if !self.can_query_app_reads() {
            return Err(UiError::Busy);
        }
        self.enqueue(
            "status",
            json!({"sessionId":self.session_id}),
            Request::RecoveryStatus,
        )
    }
    /// Human-only recovery. Never sends the old input or invokes a model/tool.
    pub fn abandon_app_reads(&mut self) -> Result<(), UiError> {
        if !self.can_abandon_app_reads() {
            return Err(UiError::Busy);
        }
        self.enqueue(
            "app.abandonReads",
            json!({"sessionId":self.session_id}),
            Request::AbandonAppReads,
        )
    }
    fn apply_app_summary(&mut self, summary: &Value) -> bool {
        if !self.app_scope() {
            return false;
        }
        let Some(recovery) = AppReadRecovery::parse(&summary["readRecovery"]) else {
            return false;
        };
        let Some(pending) = summary["pendingInputCount"].as_u64().filter(|n| *n <= 8) else {
            return false;
        };
        if summary["sessionId"].as_str() != Some(self.session_id.as_str())
            || recovery.evidence["pendingInputIds"]
                .as_array()
                .map(Vec::len)
                != Some(pending as usize)
            || !summary["recoveryRequired"].is_boolean()
            || !summary["toolState"]["outcomeUnknown"].is_boolean()
            || !self.update_editor_recovery(&summary["toolState"])
            || (self.managed_mode && !self.apply_model_state(&summary["modelState"]))
        {
            return false;
        }
        self.app_read_recovery = Some(recovery);
        self.recovery_pending = pending > 0
            || summary["recoveryRequired"] == true
            || summary["activeTurnId"].as_str().is_some();
        self.unknown_latched = summary["toolState"]["outcomeUnknown"] == true;
        if !self.blocked() && self.outcome == Some(TaskOutcome::OutcomeUnknown) {
            self.outcome = None;
        }
        self.error = self.blocking_error();
        if self.turn_id.is_none() {
            self.phase = if self.blocked() {
                Phase::OutcomeUnknown
            } else {
                Phase::Ready
            };
        }
        true
    }
    fn update_app_recovery(&mut self, tools: &Value) -> bool {
        let Some(items) = tools.get("unresolved") else {
            self.app_recovery.clear();
            return true;
        };
        let Some(items) = items.as_array().filter(|items| items.len() <= 128) else {
            return false;
        };
        let mut recovery = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for item in items {
            let Some(object) = item.as_object().filter(|object| {
                object.len() == 2
                    && object.contains_key("invocationId")
                    && object.contains_key("tool")
            }) else {
                return false;
            };
            let Some(id) = object["invocationId"].as_str().filter(|id| {
                !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
            }) else {
                return false;
            };
            if !seen.insert(id) {
                return false;
            }
            match object["tool"].as_str() {
                Some("app_navigate") if self.app_control_supported => recovery.push(AppRecovery {
                    invocation_id: id.into(),
                }),
                Some("capabilities" | "app_context" | "page_items" | "page_preview") => {}
                Some("app_operation") if self.app_control_supported => {}
                _ => return false,
            }
        }
        self.app_recovery = recovery;
        true
    }
    pub(crate) fn app_recovery(&self) -> Option<&AppRecovery> {
        if self.view().can_reconcile {
            self.app_recovery.first()
        } else {
            None
        }
    }
    pub(crate) fn reconcile_app(&mut self, recovery: &AppRecovery) -> Result<(), UiError> {
        if self.app_recovery() != Some(recovery) {
            return Err(UiError::Busy);
        }
        self.enqueue(
            "app.reconcileOperation",
            json!({"sessionId":self.session_id,"invocationId":recovery.invocation_id}),
            Request::AppReconcile {
                invocation_id: recovery.invocation_id.clone(),
            },
        )
    }
    fn settled_app_write(&self, invocation: &str, data: &Value) -> bool {
        let Some(proposal) = self.app_control_inputs.proposal(invocation) else {
            return false;
        };
        let metadata = &data["metadata"];
        if metadata["version"].as_u64() == Some(1)
            && metadata["status"] == "not_dispatched"
            && metadata["navigationSent"].as_bool() == Some(false)
            && metadata["resultAvailable"].as_bool() == Some(false)
            && metadata["operationId"].as_str() == Some(&proposal.intent.operation_id)
            && metadata["clientSequence"].as_u64() == Some(proposal.intent.client_sequence)
            && metadata["reasonCode"].as_str().is_some_and(|code| {
                !code.is_empty() && code.len() <= 128 && !code.chars().any(char::is_control)
            })
            && metadata.as_object().is_some_and(|object| object.len() == 7)
            && data["message"]["isError"].as_bool() == Some(true)
        {
            return true;
        }
        let Ok(receipt) = serde_json::from_value::<concat_editor_mcp::AppNavigationReceipt>(
            metadata["appControlReceipt"].clone(),
        ) else {
            return false;
        };
        receipt.valid()
            && receipt.session_id == proposal.session_id
            && receipt.operation_id == proposal.intent.operation_id
            && receipt.client_sequence == proposal.intent.client_sequence
            && receipt.before == proposal.intent.before
            && receipt.target == proposal.intent.target
            && receipt.origin.app_instance_id == proposal.source.app_instance_id
            && receipt.origin.app_session_id == proposal.source.app_session_id
            && receipt.origin.client_id == proposal.source.client_id
            && receipt.durability == concat_editor_mcp::AppOperationDurability::Confirmed
            && matches!(
                receipt.status,
                concat_editor_mcp::AppOperationStatus::Completed
                    | concat_editor_mcp::AppOperationStatus::Cancelled
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
        if self.app_scope() && !self.update_app_recovery(tools) {
            return false;
        }
        let Some(items) = tools.get("recoveryOperations").and_then(Value::as_array) else {
            self.editor_recovery.clear();
            return true;
        };
        if items.len() > 128
            || (self
                .scope
                .as_ref()
                .is_some_and(|scope| scope.app().is_some())
                && !items.is_empty())
        {
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
                            | "canvas_edit"
                            | "clip_edit"
                            | "save_canvas_project"
                            | "export_canvas_png"
                            | "flow_add_node"
                            | "flow_update_node"
                            | "flow_connect_nodes"
                            | "flow_delete_nodes"
                            | "flow_switch_canvas"
                            | "flow_execute_node"
                    )
                )
            {
                continue;
            }
            let binding = &item["binding"];
            let parameters = &item["parameters"];
            if binding["instanceId"].as_str() != self.scope.as_ref().map(AgentScope::instance_id)
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
                || (parameters["documentSessionId"]
                    .as_str()
                    .or_else(|| parameters["sessionId"].as_str()))
                    != Some(session)
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
                is_clip: item["toolName"] == "clip_edit",
                is_flow: item["toolName"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("flow_")),
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
        crate::clip_export_fence::revoke_session(
            &self.session_id,
            concat_host::export_publication::PublishRevocation::SessionChanged,
        );
        crate::clip_save_fence::invalidate(
            concat_host::projects::SaveInvalidation::ConversationChanged,
        );
        self.fence_app_control();
        self.task_observation_supported = false;
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
        crate::clip_export_fence::revoke_session(
            &self.session_id,
            concat_host::export_publication::PublishRevocation::SessionChanged,
        );
        crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::Discarded);
        self.task_observation_supported = false;
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
    pub fn tick(&mut self, current: Option<&AgentScope>) {
        if self.process.is_some() && self.scope.as_ref() != current && !self.ignore_runtime_events {
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
        // Lost cleanup receipts are resolved through the original status, never
        // by replaying the old input or silently repeating the human action.
        if let Some(index) = self.pending.iter().position(|pending| {
            matches!(pending.request, Request::AbandonAppReads)
                && pending.since.elapsed() >= RESPONSE_TIMEOUT
        }) {
            self.pending.remove(index);
            if let Err(error) = self.query_app_reads() {
                self.fail(error);
            }
        }
        if self.app_scope()
            && (self.blocked() || self.last_result_turn.is_some())
            && self.app_read_recovery.is_none()
            && self.can_query_app_reads()
            && let Err(error) = self.query_app_reads()
        {
            self.fail(error);
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
        crate::clip_export_fence::revoke_session(
            &self.session_id,
            concat_host::export_publication::PublishRevocation::SessionChanged,
        );
        crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::Stopped);
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
        self.send_turn = None;
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
        if self.app_scope() {
            self.app_read_recovery = None;
        }
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
            Event::Task(value) if !self.ignore_runtime_events => {
                if !self.tasks_supported
                    || value["sessionId"].as_str() != Some(self.session_id.as_str())
                {
                    self.fail(UiError::Protocol);
                } else {
                    self.task_state(self.ipc_generation, &value["task"]);
                }
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
                self.task_observation_supported = false;
                self.pending.clear();
                self.send_turn = None;
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
        if tool == "app_navigate"
            && !self.unknown_latched
            && self.write_in_flight
            && event["turnId"].as_str() == Some(turn.as_str())
            && matches!(
                event["type"].as_str(),
                Some("tool.failed" | "tool.completed")
            )
            && data["invocationId"].as_str() == Some(invocation.as_str())
            && data["message"]["toolName"] == "app_navigate"
            && self.settled_app_write(invocation, data)
        {
            self.write_in_flight = false;
            self.write_identity = None;
            self.write_operation = None;
            return;
        }
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
            if self.app_scope()
                && matches!(pending.request, Request::Create)
                && code == "rebind_required"
            {
                self.app_restore_required = true;
                self.fail(UiError::SessionRecoveryRequired);
                return;
            }
            if self.app_scope() && matches!(pending.request, Request::AbandonAppReads) {
                self.app_read_recovery = None;
                if let Err(error) = self.query_app_reads() {
                    self.fail(error);
                }
                return;
            }
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
            let error = if code == "stale_observation"
                && matches!(
                    pending.request,
                    Request::TaskCreate { .. } | Request::TaskControl { .. }
                ) {
                UiError::TaskObservationChanged
            } else {
                remote_error(code)
            };
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
                Request::TaskCreate {
                    previous_task_id, ..
                } => {
                    self.tasks = TaskProjection::restored(previous_task_id).unwrap_or_default();
                    crate::clip_export_fence::revoke_session(
                        &self.session_id,
                        concat_host::export_publication::PublishRevocation::SessionChanged,
                    );
                    self.tasks.begin_connection(self.ipc_generation);
                    self.phase = if self.blocked() {
                        Phase::OutcomeUnknown
                    } else {
                        Phase::Ready
                    };
                    self.error = Some(error);
                }
                Request::TaskControl { update_id, .. } => {
                    if code == "task_revision_conflict" {
                        self.rejected_task_update = update_id;
                    }
                    self.task_requested_epoch = None;
                    self.error = Some(error);
                }
                Request::Send { .. } if !self.unknown_latched => {
                    self.send_turn = None;
                    self.phase = Phase::Ready;
                    self.error = Some(error);
                }
                Request::Approve { .. }
                | Request::Stop
                | Request::Reconcile
                | Request::EditorReconcile { .. }
                | Request::AppReconcile { .. } => {
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
                        != self.scope.as_ref().map(AgentScope::instance_id)
                    || (self.restore
                        && result["capabilities"]["session_restore_rebind"].as_bool() != Some(true))
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                let scope = self.scope.as_ref().unwrap();
                if scope.app().is_some()
                    && (result["capabilities"]["app_read_scope"] != true
                        || result["capabilities"]["app_read_protocol_version"].as_u64() != Some(1))
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                self.app_control_supported = scope.app().is_some()
                    && result["capabilities"]["app_control_scope"] == true
                    && result["capabilities"]["app_control_protocol_version"].as_u64() == Some(1);
                self.permissions_supported =
                    result["capabilities"]["session_permissions"].as_bool() == Some(true);
                self.model_selection_supported =
                    result["capabilities"]["turn_model_selection"].as_bool() == Some(true);
                self.document_observation_supported = scope.document().is_some()
                    && result["capabilities"]["document_observation_binding"].as_bool()
                        == Some(true)
                    && result["capabilities"]["document_observation_protocol_version"].as_u64()
                        == Some(1);
                self.task_observation_supported = scope.document().is_some()
                    && result["capabilities"]["task_observation_binding"].as_bool() == Some(true)
                    && result["capabilities"]["task_observation_protocol_version"].as_u64()
                        == Some(1);
                self.tasks_supported = scope.document().is_some()
                    && result["capabilities"]["durable_tasks"] == true
                    && result["capabilities"]["task_protocol_version"].as_u64() == Some(1);
                let mut params = match scope {
                    AgentScope::Document(document) => {
                        json!({"sessionId":self.session_id,"projectId":document.project_id,"documentSessionId":document.document_session_id})
                    }
                    AgentScope::App(app) => {
                        json!({"sessionId":self.session_id,"scopeKind":"app","accountScope":app.account_scope,"appSessionId":app.app_session_id})
                    }
                };
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
                        // App probing does not imply authority to rebind old reads.
                        if scope.document().is_some() {
                            params["rebind"] = json!(true);
                        }
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
                self.app_restore_required = false;
                if self.app_scope() {
                    if !self.apply_app_summary(result) {
                        self.fail(UiError::Protocol);
                    }
                    return;
                }
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
                if self.tasks_supported
                    && let Some(task_id) = self.current_task_id().map(str::to_owned)
                    && let Err(error) = self.enqueue(
                        "task.status",
                        json!({"sessionId":self.session_id,"taskId":task_id}),
                        Request::TaskControl {
                            task_id,
                            action: TaskCommand::Status,
                            update_id: None,
                            fence_revision: self.task_permit_revision,
                        },
                    )
                {
                    self.fail(error);
                }
            }
            Request::TaskCreate { task_id, .. }
            | Request::TaskControl {
                task_id,
                action: TaskCommand::Status,
                ..
            } => {
                if result["id"].as_str() != Some(task_id.as_str()) {
                    self.fail(UiError::Protocol);
                    return;
                }
                self.task_state(self.ipc_generation, result);
            }
            Request::TaskControl {
                task_id,
                action,
                update_id,
                fence_revision,
            } => {
                if result["id"].as_str() != Some(task_id.as_str()) {
                    self.fail(UiError::Protocol);
                    return;
                }
                self.task_state(self.ipc_generation, result);
                if action == TaskCommand::Update {
                    self.acknowledged_task_update = update_id;
                    self.rejected_task_update = None;
                }
                if matches!(action, TaskCommand::Resume | TaskCommand::Update)
                    && fence_revision == self.task_permit_revision
                {
                    if self.current_task().is_some_and(|task| {
                        task.status == TaskStatus::Running
                            && (Some(task.control_epoch) == self.task_requested_epoch
                                || action == TaskCommand::Update
                                    && result["controlEpoch"].as_u64() == Some(task.control_epoch))
                    }) {
                        self.task_fenced = false;
                    }
                    self.task_requested_epoch = None;
                }
            }
            Request::Permissions { revision, operate } => {
                if result["updated"].as_bool() != Some(true)
                    || result["sessionId"].as_str() != Some(self.session_id.as_str())
                    || result["revision"].as_u64() != Some(revision)
                    || result["mode"].as_str() != Some(if operate { "operate" } else { "read" })
                {
                    self.fail(UiError::Protocol);
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
                if self.app_scope() {
                    if !self.apply_app_summary(result) {
                        self.fail(UiError::Protocol);
                    }
                    return;
                }
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
                } else if self.app_scope()
                    && let Err(error) = self.query_app_reads()
                {
                    self.fail(error);
                }
            }
            Request::AbandonAppReads => {
                if result["sessionId"].as_str() != Some(self.session_id.as_str())
                    || !matches!(
                        result["status"].as_str(),
                        Some("stale_reads_abandoned" | "unchanged")
                    )
                    || result["resultAvailable"].as_bool() != Some(false)
                    || result["readRecovery"] != result["session"]["readRecovery"]
                    || !self.apply_app_summary(&result["session"])
                {
                    self.fail(UiError::Protocol);
                }
            }
            Request::AppReconcile { invocation_id } => {
                let Ok(receipt) = serde_json::from_value::<concat_editor_mcp::AppNavigationReceipt>(
                    result["receipt"].clone(),
                ) else {
                    self.fail(UiError::Protocol);
                    return;
                };
                let terminal = receipt.durability
                    == concat_editor_mcp::AppOperationDurability::Confirmed
                    && matches!(
                        receipt.status,
                        concat_editor_mcp::AppOperationStatus::Completed
                            | concat_editor_mcp::AppOperationStatus::Cancelled
                    );
                if !receipt.valid()
                    || receipt.session_id != self.session_id
                    || receipt.origin.client_id != self.editor_client
                    || result["verified"].as_bool() != Some(terminal)
                    || (terminal
                        && result["completed"].as_bool()
                            != Some(
                                receipt.status == concat_editor_mcp::AppOperationStatus::Completed,
                            ))
                    || (!terminal && result["status"] != "outcomeUnknown")
                    || !self.apply_app_summary(&result["session"])
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if terminal {
                    if self
                        .app_recovery
                        .iter()
                        .any(|entry| entry.invocation_id == invocation_id)
                    {
                        self.fail(UiError::Protocol);
                        return;
                    }
                    if self
                        .write_identity
                        .as_ref()
                        .is_some_and(|(_, id, tool)| id == &invocation_id && tool == "app_navigate")
                    {
                        self.write_in_flight = false;
                        self.write_identity = None;
                        self.write_operation = None;
                    }
                    self.outcome = Some(TaskOutcome::OperationVerified);
                }
            }
            Request::AppControlAuthorize { allowed } => {
                if result["accepted"].as_bool() != Some(true)
                    || result["allowed"].as_bool() != Some(allowed)
                {
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
            // ACK may remove Pending::Send before the real COTO start arrives.
            let explicit_send = self
                .send_turn
                .as_ref()
                .is_some_and(|(_, turn)| turn.is_none());
            let task_turn = turn.is_some_and(|turn| {
                self.tasks.current().is_some_and(|task| {
                    !self.retired_task_turns.iter().any(|v| v == turn)
                        && task.active_input_id.as_deref().is_some_and(|input| {
                            self.tasks
                                .owns_automatic_turn(self.ipc_generation, input, turn)
                        })
                })
            });
            if !explicit_send
                && !task_turn
                && (self.tasks_supported
                    || self
                        .scope
                        .as_ref()
                        .is_some_and(|scope| scope.app().is_some())
                    || self.phase != Phase::Running)
            {
                self.fail(UiError::Protocol);
                return;
            }
            let Some(turn) = turn else {
                self.fail(UiError::Protocol);
                return;
            };
            if explicit_send {
                if task_turn {
                    self.fail(UiError::Protocol);
                    return;
                }
                if let Some((_, admitted)) = self.send_turn.as_mut() {
                    *admitted = Some(turn.to_owned());
                }
            }
            if task_turn {
                if self.turn_id.as_deref().is_some_and(|old| old != turn) {
                    self.fail(UiError::Protocol);
                    return;
                }
                self.task_turn_binding = self.current_task().cloned();
                self.text.clear();
                self.tool_progress.clear();
                self.progress.clear();
                self.outcome = None;
                self.error = self.blocking_error();
            }
            self.active_task_turn = task_turn;
            self.last_result_turn = None;
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
            "input.applied"
                if self.tasks_supported
                    || self
                        .scope
                        .as_ref()
                        .is_some_and(|scope| scope.app().is_some()) =>
            {
                if self.app_control_supported && self.app_scope() {
                    if !turn
                        .zip(data["inputId"].as_str())
                        .is_some_and(|(turn, input)| {
                            self.app_control_inputs.applied(turn, input, seq)
                        })
                    {
                        self.fail(UiError::Protocol);
                    }
                    return;
                }
                if self.tasks.current().is_some_and(|task| {
                    task.active_turn_id.as_deref() == turn
                        && data["inputId"].as_str() != task.active_input_id.as_deref()
                }) || self.send_turn.as_ref().is_some_and(|(input, admitted)| {
                    admitted.as_deref() == turn && data["inputId"].as_str() != Some(input.as_str())
                }) {
                    self.fail(UiError::Protocol);
                }
            }
            "tool.started" => {
                if self
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.app().is_some())
                    && !matches!(
                        data["call"]["name"]
                            .as_str()
                            .and_then(ToolAction::from_name),
                        Some(ToolAction::AppRead | ToolAction::Capabilities)
                    )
                    && !(self.app_control_supported
                        && matches!(
                            data["call"]["name"].as_str(),
                            Some("app_navigate" | "app_operation")
                        ))
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if data["call"]["name"] == "app_navigate"
                    && !turn.is_some_and(|turn| {
                        self.app_control_inputs
                            .started(&self.session_id, turn, seq, data)
                            .is_some()
                    })
                {
                    self.fail(UiError::Protocol);
                    return;
                }
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
                if owns_write && self.app_scope() && data["message"]["toolName"] == "app_navigate" {
                    if data["invocationId"]
                        .as_str()
                        .is_some_and(|id| self.settled_app_write(id, data))
                    {
                        self.write_in_flight = false;
                        self.write_identity = None;
                        self.write_operation = None;
                    }
                } else if owns_write {
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
                    } else if data["metadata"]["postVerification"] == "verified" {
                        self.write_in_flight = false;
                        self.write_identity = None;
                        self.write_operation = None;
                    }
                } else if matches!(
                    data["message"]["toolName"].as_str(),
                    Some("canvas_operation" | "clip_operation" | "retry_canvas_registration")
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
                if self
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.app().is_some())
                {
                    self.fail(UiError::Protocol);
                    return;
                }
                if self.task_approval_fenced() {
                    return;
                }
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
                            | "library_assets"
                            | "insert_library_image"
                            | "transform_canvas_layer"
                            | "canvas_edit"
                            | "clip_edit"
                            | "save_canvas_project"
                            | "export_canvas_png"
                            | "canvas_operation"
                            | "clip_operation"
                            | "retry_canvas_registration"
                            | "flow_add_node"
                            | "flow_update_node"
                            | "flow_connect_nodes"
                            | "flow_delete_nodes"
                            | "flow_switch_canvas"
                            | "flow_execute_node"
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
        if self.active_task_turn {
            self.ended_task_turns.push_back(turn.to_owned());
            while self.ended_task_turns.len() > 128 {
                self.ended_task_turns.pop_front();
            }
        }
        self.last_result_turn = Some(turn.to_owned());
        if self.app_scope() {
            self.app_read_recovery = None;
        }
        self.send_turn = None;
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
        if self.active_task_turn
            && self.tasks.current().is_some_and(|t| !t.status.terminal())
            && self.outcome == Some(TaskOutcome::Completed)
        {
            self.outcome = Some(TaskOutcome::SubmittedPendingVerification);
        }
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
        "task_invalid"
        | "task_gate"
        | "task_capacity"
        | "task_context_capacity"
        | "invalid_input"
        | "input_limit"
        | "media_limit" => UiError::InvalidInput,
        "task_revision_conflict" => UiError::TaskRevisionChanged,
        "human_observation_conflict" => UiError::TaskObservationConflict,
        "stale_observation" => UiError::ObservationChanged,
        "invalid_observation_binding" | "input_observation_missing" | "task_observation_source" => {
            UiError::ObservationInvalid
        }
        "task_observation_not_implemented" | "observation_steer_unsupported" => {
            UiError::ObservationUnsupported
        }
        "task_stale" | "task_conflict" => UiError::Busy,
        "turn_settling" | "managed_busy" | "session_busy" => UiError::Busy,
        "managed_unknown" | "managed_transport" | "managed_timeout" => UiError::ModelVerifyRequired,
        "managed_protocol" | "idempotency_conflict" | "version_mismatch" => UiError::Protocol,
        "managed_account_expired" => UiError::AccountExpired,
        "managed_unavailable" | "managed_http" => UiError::ServiceUnavailable,
        "managed_policy_unavailable" => UiError::ModelSelectionUnavailable,
        "tool_denied" | "notAuthorized" | "approval_expired" => UiError::AuthorizationRequired,
        "project_conflict"
        | "session_scope"
        | "instance_conflict"
        | "managed_scope"
        | "account_scope"
        | "input_scope_expired" => UiError::DocumentChanged,
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

    #[test]
    fn clip_save_native_controller_stop_disconnect_and_shutdown_revoke_original() {
        for action in 0..3 {
            let (mut controller, _) = task_controller();
            let pending = crate::clip_save_fence::test_support::PendingSave::new();
            match action {
                0 => {
                    let _ = controller.stop();
                }
                1 => controller.disconnect(),
                _ => controller.shutdown_now(),
            }
            pending.assert_not_committed();
        }
    }
    fn document_mut(scope: &mut AgentScope) -> &mut DocumentIdentity {
        match scope {
            AgentScope::Document(document) => document,
            AgentScope::App(_) => panic!("document fixture"),
        }
    }
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
    fn frozen_document_observation(fixture: &Fixture) -> DocumentObservationBinding {
        frozen_observation(&fixture.identity)
    }
    fn frozen_task_intent(identity: &DocumentIdentity) -> agent_tasks::TaskIntent {
        agent_tasks::TaskIntent {
            client_request_id: "original-create".into(),
            task_id: agent_tasks::task_id_for("original-create"),
            goal: "保留完整原目标".into(),
            max_task_credits: 40,
            initial_observation_binding: Some(frozen_observation(identity)),
        }
    }
    fn frozen_observation(identity: &DocumentIdentity) -> DocumentObservationBinding {
        serde_json::from_value(json!({
            "appInstanceId": identity.instance_id,
            "clientId": "client-1",
            "kind": "clip",
            "projectId": identity.project_id,
            "documentSessionId": identity.document_session_id,
            "revision": 7, "contextRevision": 9, "selectionRevision": 11,
            "selectedObjectIds": (0..300).map(|index| format!("clip:{index}")).collect::<Vec<_>>(),
            "viewStateId": format!("view-v1-{}", "a".repeat(64))
        }))
        .unwrap()
    }
    #[test]
    fn document_input_preserves_original_identity_full_selection_and_view() {
        let mut fixture = Fixture::new("finish");
        fixture.ready();
        assert!(fixture.controller.supports_document_observation());
        let binding = frozen_document_observation(&fixture);
        fixture
            .controller
            .send_document_content(
                "original-human-input",
                vec![json!({"type":"text","text":"当前选择是什么"})],
                None,
                binding.clone(),
            )
            .unwrap();
        fixture.until(|controller| controller.view().outcome == Some(TaskOutcome::Completed));
        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["params"]["inputId"], "original-human-input");
        assert_eq!(requests[0]["params"]["observationBinding"], json!(binding));
        assert!(requests[0]["params"].get("appViewBinding").is_none());
        assert_eq!(requests[0]["params"]["mode"], "follow_up");
    }
    #[test]
    fn document_observation_requires_exact_capability_without_closing_connection() {
        for scenario in [
            "old-runtime",
            "doc-no-cap",
            "doc-false-cap",
            "doc-string-cap",
            "doc-wrong-version",
        ] {
            let mut fixture = Fixture::new(scenario);
            fixture.ready();
            let binding = frozen_document_observation(&fixture);
            assert!(
                !fixture.controller.supports_document_observation(),
                "{scenario}"
            );
            assert!(fixture.controller.document_observation_unavailable());
            assert_eq!(
                fixture.controller.send_document_content(
                    "original-human-input",
                    vec![json!({"type":"text","text":"不要退回未绑定发送"})],
                    None,
                    binding,
                ),
                Err(UiError::ObservationUnavailable),
                "{scenario}"
            );
            assert!(fixture.controller.process_present());
            assert!(fixture.controller.session_opened());
            assert!(fixture.requests().is_empty());
            fixture.controller.disconnect();
            assert!(!fixture.controller.document_observation_unavailable());
            fixture.until(|controller| !controller.process_present());
            assert!(!fixture.controller.document_observation_unavailable());
        }
    }
    #[test]
    fn document_observation_refuses_changed_scope_before_enqueue() {
        let mut fixture = Fixture::new("finish");
        fixture.ready();
        for axis in 0..4 {
            let mut binding = frozen_document_observation(&fixture);
            match axis {
                0 => binding.app_instance_id = "other-instance".into(),
                1 => binding.client_id = "other-client".into(),
                2 => binding.project_id = "other-project".into(),
                _ => binding.document_session_id = "other-document".into(),
            }
            assert_eq!(
                fixture.controller.send_document_content(
                    "original-human-input",
                    vec![json!({"type":"text","text":"只发送原文档"})],
                    None,
                    binding,
                ),
                Err(UiError::Configuration)
            );
        }
        assert_eq!(
            fixture.controller.send_frozen_content(
                vec![json!({"type":"text","text":"不能退回未绑定输入"})],
                None,
                None,
            ),
            Err(UiError::Configuration)
        );
        assert!(fixture.requests().is_empty());
    }
    #[test]
    fn document_observation_lost_ack_restores_original_session_without_resend() {
        let mut fixture = Fixture::new("doc-lost-ack");
        fixture.ready();
        let binding = frozen_document_observation(&fixture);
        fixture
            .controller
            .send_document_content(
                "original-human-input",
                vec![json!({"type":"text","text":"保留原问题"})],
                None,
                binding.clone(),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        fixture.controller.pending.front_mut().unwrap().since = Instant::now() - RESPONSE_TIMEOUT;
        fixture.until(|controller| !controller.process_present());
        let restore = fixture.original_restore_config("doc-lost-ack");
        fixture.controller.connect(restore).unwrap();
        fixture.ready();
        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["params"]["inputId"], "original-human-input");
        assert_eq!(requests[0]["params"]["observationBinding"], json!(binding));
        assert_eq!(
            fs::read_to_string(fixture.directory.join("methods.txt")).unwrap(),
            "initialize\nsession.create\nsend\ninitialize\nsession.restore\n"
        );
    }
    #[test]
    fn stale_document_observation_reports_changed_view_and_allows_new_human_input() {
        let mut fixture = Fixture::new("doc-stale");
        fixture.ready();
        fixture.controller.send_text("读取原视图").unwrap();
        fixture.until(|controller| controller.view().error == Some(UiError::ObservationChanged));
        assert!(fixture.controller.view().can_send);
        assert!(fixture.controller.process_present());
        assert_eq!(fixture.requests().len(), 1);
        fixture.controller.send_text("核对后的新问题").unwrap();
        fixture.until(|controller| controller.view().error == Some(UiError::ObservationChanged));
        let requests = fixture.requests();
        assert_eq!(requests.len(), 2);
        assert_ne!(
            requests[0]["params"]["inputId"],
            requests[1]["params"]["inputId"]
        );
        for code in [
            "stale_observation",
            "invalid_observation_binding",
            "input_observation_missing",
            "input_scope_expired",
            "task_observation_not_implemented",
            "observation_steer_unsupported",
        ] {
            assert_ne!(remote_error(code), UiError::Connection, "{code}");
        }
    }

    fn task_summary_value(id: &str, state: u64, epoch: u64, turn: Option<&str>) -> Value {
        json!({"id":id,"sessionId":"fixture-session","taskRevision":state,"controlEpoch":epoch,"status":"running","blockedReason":null,
            "rounds":if turn.is_some(){1}else{0},"activeInputId":turn.map(|_|"original-input"),"activeTurnId":turn,"roundOrdinal":turn.map(|_|1),
            "verifiedSteps":0,"totalSteps":1,"evidenceCount":0,"stateRevision":state,"lastUpdateId":null})
    }
    fn task_controller() -> (Controller, String) {
        let id = agent_tasks::task_id_for("original-request");
        let mut c = Controller {
            session_id: "fixture-session".into(),
            tasks_supported: true,
            ipc_generation: 1,
            phase: Phase::Ready,
            ..Controller::default()
        };
        c.tasks = agent_tasks::TaskProjection::restored(Some(id.clone())).unwrap();
        c.tasks.begin_connection(1);
        (c, id)
    }
    fn task_event(c: &mut Controller, seq: u64, kind: &str, turn: &str, data: Value) {
        c.handle_event(Event::Runtime(json!({"version":1,"type":"event","event":{"schemaVersion":1,"sessionId":"fixture-session","seq":seq,"turnId":turn,"type":kind,"data":data}})));
    }
    #[test]
    fn task_projection_admits_only_current_real_turn_and_fences_old_approval() {
        let (mut c, id) = task_controller();
        c.task_state(1, &task_summary_value(&id, 1, 1, Some("turn-1")));
        task_event(&mut c, 1, "turn.started", "turn-1", json!({}));
        assert_eq!(c.current_turn_id(), Some("turn-1"));
        task_event(
            &mut c,
            2,
            "approval.required",
            "turn-1",
            json!({"approvalId":"old","tool":"move_selected_image","arguments":{"objectId":"layer:1","deltaX":1,"deltaY":2}}),
        );
        assert!(c.view().approval.is_some());
        let old = c.task_permit_revision();
        c.withdraw_task_approvals();
        assert!(c.view().approval.is_none());
        assert!(c.task_permit_revision() > old);
        task_event(
            &mut c,
            3,
            "approval.required",
            "turn-1",
            json!({"approvalId":"late","tool":"move_selected_image","arguments":{"objectId":"layer:1","deltaX":1,"deltaY":2}}),
        );
        assert!(c.view().approval.is_none());
        c.task_state(0, &task_summary_value(&id, 99, 9, Some("old-connection")));
        assert_eq!(c.current_task().unwrap().control_epoch, 1);
        let (mut bad, id) = task_controller();
        bad.task_state(1, &task_summary_value(&id, 1, 1, Some("expected")));
        task_event(&mut bad, 1, "turn.started", "forged", json!({}));
        assert_eq!(bad.view().phase, Phase::Failed);
    }
    #[test]
    fn task_verified_not_sent_clears_only_exact_invocation_and_unverified_failure_stays_unknown() {
        for tool in ["canvas_edit", "flow_add_node", "move_selected_image"] {
            for (verified, exact) in [(true, true), (false, true), (true, false)] {
                let (mut c, id) = task_controller();
                c.task_state(1, &task_summary_value(&id, 1, 1, Some("turn-1")));
                task_event(&mut c, 1, "turn.started", "turn-1", json!({}));
                task_event(
                    &mut c,
                    2,
                    "tool.started",
                    "turn-1",
                    json!({"invocationId":"turn-1_allocated-task-write","call":{"id":"allocated-task-write","name":tool,"arguments":{}}}),
                );
                assert!(c.write_in_flight);
                task_event(
                    &mut c,
                    3,
                    "tool.failed",
                    "turn-1",
                    json!({"invocationId":if exact{"turn-1_allocated-task-write"}else{"turn-1_other"},
                    "message":{"id":"message-1","role":"tool","content":[{"type":"text","text":"任务已暂停，本次编辑未发送"}],"toolCallId":"allocated-task-write","toolName":tool,"isError":true},
                    "metadata":{"operationId":"scm2:1:14:fixture-client:5:doc-1:1","clientSequence":1,"appStatus":"not_sent","postVerification":if verified{"verified"}else{"pending"},"reasonCode":"task_execution_blocked"}}),
                );
                assert_eq!(c.write_in_flight, !(verified && exact));
                c.handle_event(Event::Exit {
                    code: Some(1),
                    requested: false,
                });
                assert_eq!(c.unknown_latched, !(verified && exact));
            }
        }
    }
    #[test]
    fn task_jsonl_create_keeps_original_content_and_continues_real_rounds() {
        let mut f = Fixture::new("task-rounds");
        f.ready();
        assert!(f.controller.supports_tasks());
        f.controller
            .create_task(
                &frozen_task_intent(&f.identity),
                vec![json!({"type":"text","text":"原附件引用"})],
                json!({"modelId":"agent.fast"}),
            )
            .unwrap();
        f.until(|c| {
            c.current_task()
                .is_some_and(|task| task.status == TaskStatus::Completed)
        });
        assert_eq!(f.controller.current_task().unwrap().rounds, 2);
        assert!(f.controller.task_turn_ended("turn-1"));
        assert!(f.controller.task_turn_ended("turn-2"));
        assert_eq!(f.requests().len(), 1);
        assert_eq!(f.requests()[0]["method"], "task.create");
        assert_eq!(
            f.requests()[0]["params"]["initialObservationBinding"],
            json!(frozen_document_observation(&f))
        );
        assert!(
            f.requests()[0]["params"]
                .get("observationBinding")
                .is_none()
        );
        assert_eq!(f.controller.view().outcome, Some(TaskOutcome::Completed));
    }
    #[test]
    fn task_observation_capability_is_independent_and_reset_on_reconnect() {
        for scenario in [
            "task-no-observation",
            "task-false-observation",
            "task-string-observation",
            "task-no-observation-version",
            "task-wrong-observation-version",
        ] {
            let mut f = Fixture::new(scenario);
            f.ready();
            assert!(f.controller.supports_tasks());
            assert!(f.controller.supports_document_observation());
            assert!(!f.controller.supports_task_observation());
            assert!(f.controller.task_observation_unavailable());
            let before = f.controller.task_permit_revision();
            assert_eq!(
                f.controller.create_task(
                    &frozen_task_intent(&f.identity),
                    vec![json!({"type":"text","text":"原附件引用"})],
                    json!({"modelId":"agent.fast"}),
                ),
                Err(UiError::TaskObservationUnavailable)
            );
            assert!(f.controller.current_task_id().is_none());
            assert_eq!(f.controller.task_permit_revision(), before);
            assert!(f.requests().is_empty());
            f.controller.disconnect();
            assert!(!f.controller.task_observation_unavailable());
            assert!(!f.controller.task_observation_supported);
            f.until(|c| !c.process_present());
            let config = f.original_restore_config("task-rounds");
            f.controller.connect(config).unwrap();
            assert!(!f.controller.task_observation_supported);
            f.ready();
            assert!(f.controller.supports_task_observation());
            assert!(!f.controller.task_observation_unavailable());
        }
        let mut f = Fixture::new("task-no-doc-observation");
        f.ready();
        assert!(!f.controller.supports_document_observation());
        assert!(f.controller.supports_task_observation());
        f.controller
            .create_task(
                &frozen_task_intent(&f.identity),
                vec![json!({"type":"text","text":"原附件引用"})],
                json!({"modelId":"agent.fast"}),
            )
            .unwrap();
        f.until(|c| c.view().approval.is_some());
    }
    #[test]
    fn task_create_and_update_reject_unbound_or_cross_scope_before_mutating_task() {
        let mut f = Fixture::new("task-controls");
        f.ready();
        let original = frozen_task_intent(&f.identity);
        for axis in 0..5 {
            let mut intent = original.clone();
            if axis == 4 {
                intent.initial_observation_binding = None;
            } else {
                let binding = intent.initial_observation_binding.as_mut().unwrap();
                match axis {
                    0 => binding.app_instance_id = "different-instance".into(),
                    1 => binding.client_id = "different-client".into(),
                    2 => binding.project_id = "different-project".into(),
                    _ => binding.document_session_id = "different-doc".into(),
                }
            }
            let before = f.controller.task_permit_revision();
            assert_eq!(
                f.controller.create_task(
                    &intent,
                    vec![json!({"type":"text","text":"原附件引用"})],
                    json!({"modelId":"agent.fast"}),
                ),
                Err(if axis == 4 {
                    UiError::TaskObservationMissing
                } else {
                    UiError::ObservationChanged
                })
            );
            assert!(f.controller.current_task_id().is_none());
            assert_eq!(f.controller.task_permit_revision(), before);
            assert!(f.requests().is_empty());
        }
        f.controller
            .create_task(
                &original,
                vec![json!({"type":"text","text":"原附件引用"})],
                json!({"modelId":"agent.fast"}),
            )
            .unwrap();
        f.until(|c| c.view().approval.is_some());
        f.controller.task_command(TaskCommand::Pause).unwrap();
        f.until(|c| {
            c.current_task()
                .is_some_and(|task| task.status == TaskStatus::Paused)
                && c.pending.is_empty()
                && c.turn_id.is_none()
        });
        let expected = f.controller.current_task().unwrap().clone();
        let requests = f.requests();
        for axis in 0..6 {
            let mut intent = agent_tasks::TaskUpdateIntent {
                client_request_id: "original-update".into(),
                task_id: original.task_id.clone(),
                message: "保留原补充".into(),
                expected_revision: expected.task_revision,
                retries: 0,
                confirmed: false,
                observation_binding: original.initial_observation_binding.clone(),
            };
            if axis == 4 {
                intent.observation_binding = None;
            } else if axis == 5 {
                f.controller.task_observation_supported = false;
            } else {
                let binding = intent.observation_binding.as_mut().unwrap();
                match axis {
                    0 => binding.app_instance_id = "different-instance".into(),
                    1 => binding.client_id = "different-client".into(),
                    2 => binding.project_id = "different-project".into(),
                    _ => binding.document_session_id = "different-doc".into(),
                }
            }
            let before = f.controller.task_permit_revision();
            let epoch = f.controller.task_requested_epoch;
            assert_eq!(
                f.controller.retry_task_update(&intent),
                Err(match axis {
                    4 => UiError::TaskObservationMissing,
                    5 => UiError::TaskObservationUnavailable,
                    _ => UiError::ObservationChanged,
                })
            );
            assert_eq!(f.controller.current_task(), Some(&expected));
            assert_eq!(f.controller.task_permit_revision(), before);
            assert_eq!(f.controller.task_requested_epoch, epoch);
            assert_eq!(f.requests(), requests);
        }
    }
    #[test]
    fn task_lost_create_and_update_receipts_restore_original_status_without_replay() {
        for scenario in ["task-create-lost-restore", "task-update-lost-restore"] {
            let mut f = Fixture::new(scenario);
            f.ready();
            let original = frozen_task_intent(&f.identity);
            f.controller
                .create_task(
                    &original,
                    vec![json!({"type":"text","text":"原附件引用"})],
                    json!({"modelId":"agent.fast"}),
                )
                .unwrap();
            if scenario == "task-update-lost-restore" {
                f.until(|c| c.view().approval.is_some());
                f.controller.task_command(TaskCommand::Pause).unwrap();
                f.until(|c| {
                    c.current_task()
                        .is_some_and(|task| task.status == TaskStatus::Paused)
                        && c.pending.is_empty()
                        && c.turn_id.is_none()
                });
                let update = agent_tasks::TaskUpdateIntent {
                    client_request_id: "original-update".into(),
                    task_id: original.task_id.clone(),
                    message: "保留原补充".into(),
                    expected_revision: f.controller.current_task().unwrap().task_revision,
                    retries: 0,
                    confirmed: false,
                    observation_binding: original.initial_observation_binding.clone(),
                };
                f.controller.retry_task_update(&update).unwrap();
            }
            thread::sleep(Duration::from_millis(100));
            f.controller.pending.front_mut().unwrap().since = Instant::now() - RESPONSE_TIMEOUT;
            f.until(|c| !c.process_present());
            assert!(!f.controller.task_observation_supported);
            let config = f.original_restore_config(scenario);
            f.controller.connect(config).unwrap();
            f.until(|c| c.current_task().is_some() && c.pending.is_empty());
            assert_eq!(
                f.controller.current_task_id(),
                Some(original.task_id.as_str())
            );
            let requests = f.requests();
            let creates = requests
                .iter()
                .filter(|r| r["method"] == "task.create")
                .collect::<Vec<_>>();
            assert_eq!(creates.len(), 1);
            assert_eq!(
                creates[0]["params"]["clientRequestId"],
                original.client_request_id
            );
            assert_eq!(
                creates[0]["params"]["initialObservationBinding"],
                json!(original.initial_observation_binding)
            );
            let updates = requests
                .iter()
                .filter(|r| r["method"] == "task.update")
                .collect::<Vec<_>>();
            assert_eq!(
                updates.len(),
                usize::from(scenario == "task-update-lost-restore")
            );
            if let Some(update) = updates.first() {
                assert_eq!(update["params"]["clientRequestId"], "original-update");
                assert_eq!(
                    update["params"]["observationBinding"],
                    json!(original.initial_observation_binding)
                );
                assert_eq!(
                    f.controller.acknowledged_task_update(),
                    Some("original-update")
                );
            }
            assert_eq!(requests.last().unwrap()["method"], "task.status");
            let methods = fs::read_to_string(f.directory.join("methods.txt")).unwrap();
            assert!(methods.ends_with("initialize\nsession.restore\ntask.status\n"));
        }
    }
    #[test]
    fn task_observation_rejections_keep_sources_and_show_actionable_reasons() {
        for (scenario, expected) in [
            ("task-stale-observation", UiError::TaskObservationChanged),
            (
                "task-human-observation-conflict",
                UiError::TaskObservationConflict,
            ),
            (
                "task-invalid-observation-source",
                UiError::ObservationInvalid,
            ),
        ] {
            let mut f = Fixture::new(scenario);
            f.ready();
            let intent = frozen_task_intent(&f.identity);
            f.controller
                .create_task(
                    &intent,
                    vec![json!({"type":"text","text":"原附件引用"})],
                    json!({"modelId":"agent.fast"}),
                )
                .unwrap();
            f.until(|c| c.view().error == Some(expected));
            assert!(f.controller.process_present());
            assert_eq!(f.requests().len(), 1);
            assert_eq!(
                f.requests()[0]["params"]["initialObservationBinding"],
                json!(intent.initial_observation_binding)
            );
            assert!(!expected.message().contains("连接"));
        }
    }
    #[test]
    fn task_jsonl_pause_resume_stop_use_original_id_and_revoke_old_card() {
        let mut f = Fixture::new("task-controls");
        f.ready();
        f.controller
            .create_task(
                &frozen_task_intent(&f.identity),
                vec![json!({"type":"text","text":"原附件引用"})],
                json!({"modelId":"agent.fast"}),
            )
            .unwrap();
        f.until(|c| c.view().approval.is_some());
        let pending_save = crate::clip_save_fence::test_support::PendingSave::new();
        f.controller.task_command(TaskCommand::Pause).unwrap();
        pending_save.assert_not_committed();
        f.until(|c| {
            c.current_task()
                .is_some_and(|task| task.status == TaskStatus::Paused)
                && c.turn_id.is_none()
        });
        assert!(f.controller.view().approval.is_none());
        f.controller.task_command(TaskCommand::Resume).unwrap();
        f.until(|c| c.view().approval.is_some_and(|a| a.id == "task-approval-2"));
        assert_eq!(
            f.controller.approve("task-approval-1", true),
            Err(UiError::AuthorizationRequired)
        );
        let pending_save = crate::clip_save_fence::test_support::PendingSave::new();
        f.controller.task_command(TaskCommand::Stop).unwrap();
        pending_save.assert_not_committed();
        f.until(|c| {
            c.current_task()
                .is_some_and(|task| task.status == TaskStatus::Stopped)
                && c.turn_id.is_none()
        });
        assert!(f.requests().iter().all(|r| r["method"] == "task.create"
            || r["params"]["taskId"] == agent_tasks::task_id_for("original-create")));
    }
    #[test]
    fn task_jsonl_update_retries_only_original_id_after_known_zero_mutation_conflict() {
        let mut f = Fixture::new("task-controls");
        f.ready();
        f.controller
            .create_task(
                &frozen_task_intent(&f.identity),
                vec![json!({"type":"text","text":"原附件引用"})],
                json!({"modelId":"agent.fast"}),
            )
            .unwrap();
        f.until(|c| c.view().approval.is_some());
        f.controller.task_command(TaskCommand::Pause).unwrap();
        f.until(|c| {
            c.current_task()
                .is_some_and(|t| t.status == TaskStatus::Paused)
                && c.turn_id.is_none()
        });
        let original_revision = f.controller.current_task().unwrap().task_revision;
        let mut original = agent_tasks::TaskUpdateIntent {
            client_request_id: "original-update".into(),
            task_id: agent_tasks::task_id_for("original-create"),
            message: "保留原补充".into(),
            expected_revision: original_revision + 1,
            retries: 0,
            confirmed: false,
            observation_binding: Some(frozen_document_observation(&f)),
        };
        let pending_save = crate::clip_save_fence::test_support::PendingSave::new();
        f.controller.retry_task_update(&original).unwrap();
        pending_save.assert_not_committed();
        f.until(|c| c.rejected_task_update() == Some("original-update"));
        assert_eq!(
            f.controller.current_task().unwrap().task_revision,
            original_revision
        );
        assert!(
            f.controller
                .current_task()
                .unwrap()
                .last_update_id
                .is_none()
        );
        original.expected_revision = original_revision;
        original.retries = 1;
        let pending_save = crate::clip_save_fence::test_support::PendingSave::new();
        f.controller.retry_task_update(&original).unwrap();
        pending_save.assert_not_committed();
        f.until(|c| c.acknowledged_task_update.as_deref() == Some("original-update"));
        let applied_revision = f.controller.current_task().unwrap().task_revision;
        f.controller.acknowledged_task_update = None;
        // A lost ACK is retrievable with the same original revision and ID.
        let pending_save = crate::clip_save_fence::test_support::PendingSave::new();
        f.controller.retry_task_update(&original).unwrap();
        pending_save.assert_not_committed();
        f.until(|c| c.acknowledged_task_update.as_deref() == Some("original-update"));
        assert_eq!(
            f.controller.current_task().unwrap().task_revision,
            applied_revision
        );
        assert_eq!(
            f.controller
                .current_task()
                .unwrap()
                .last_update_id
                .as_deref(),
            Some("original-update")
        );
        let updates = f
            .requests()
            .into_iter()
            .filter(|r| r["method"] == "task.update")
            .collect::<Vec<_>>();
        assert_eq!(updates.len(), 3);
        assert!(
            updates
                .iter()
                .all(|r| r["params"]["clientRequestId"] == "original-update"
                    && r["params"]["message"] == "保留原补充"
                    && r["params"]["observationBinding"] == json!(original.observation_binding))
        );
        assert_eq!(updates[1], updates[2]);
    }
    #[test]
    fn task_restore_selects_persisted_id_and_requires_new_connection_projection() {
        let (mut c, id) = task_controller();
        c.task_state(1, &task_summary_value(&id, 10, 2, None));
        c.ipc_generation = 2;
        c.tasks.begin_connection(2);
        c.task_fenced = true;
        assert_eq!(c.current_task_id(), Some(id.as_str()));
        assert!(c.current_task().is_none());
        c.task_state(1, &task_summary_value(&id, 99, 99, Some("old-connection")));
        c.task_state(
            2,
            &task_summary_value(&agent_tasks::task_id_for("other"), 99, 99, None),
        );
        assert!(c.current_task().is_none());
        let mut restored = task_summary_value(&id, 11, 2, None);
        restored["stateRevision"] = json!(1);
        restored["status"] = json!("paused");
        c.task_state(2, &restored);
        assert_eq!(c.current_task().unwrap().task_revision, 11);
        assert!(c.task_fenced);
        assert!(c.current_turn_id().is_none());
        assert!(c.view().approval.is_none());
    }
    #[test]
    fn task_unknown_create_ack_and_pending_status_never_prevent_human_withdrawal() {
        for (scenario, action) in [
            ("task-create-ack-lost", TaskCommand::Stop),
            ("task-status-ack-lost", TaskCommand::Pause),
        ] {
            let mut f = Fixture::new(scenario);
            f.ready();
            f.controller
                .create_task(
                    &frozen_task_intent(&f.identity),
                    vec![json!({"type":"text","text":"原附件引用"})],
                    json!({"modelId":"agent.fast"}),
                )
                .unwrap();
            f.until(|c| c.view().approval.is_some());
            if action == TaskCommand::Pause {
                f.controller.task_command(TaskCommand::Status).unwrap();
                f.until(|c| c.current_task().is_some_and(|t| t.state_revision >= 3));
            }
            assert!(f.controller.can_task_command(action));
            let before = f.controller.task_permit_revision();
            f.controller.task_command(action).unwrap();
            assert!(f.controller.task_permit_revision() > before);
            assert!(!f.controller.can_task_command(action));
            f.until(|c| {
                c.current_task()
                    .is_some_and(|t| matches!(t.status, TaskStatus::Paused | TaskStatus::Stopped))
                    && c.turn_id.is_none()
            });
            assert!(f.controller.task_fenced);
            assert!(f.controller.view().approval.is_none());
            assert!(f.requests().iter().any(|r| r["method"] == action.method()));
        }
    }
    #[test]
    fn task_late_resume_and_update_ack_cannot_undo_a_new_human_fence() {
        for action in [TaskCommand::Resume, TaskCommand::Update] {
            let (mut c, id) = task_controller();
            c.task_state(1, &task_summary_value(&id, 1, 1, Some("old-turn")));
            c.task_requested_epoch = Some(2);
            c.pending.push_back(Pending {
                id: "old-control".into(),
                request: Request::TaskControl {
                    task_id: id.clone(),
                    action,
                    update_id: (action == TaskCommand::Update).then(|| "original-update".into()),
                    fence_revision: c.task_permit_revision,
                },
                since: Instant::now(),
            });
            c.withdraw_task_approvals();
            c.task_requested_epoch = None;
            let newer = task_summary_value(&id, 2, 2, None);
            c.response(json!({"version":1,"type":"response","id":"old-control","result":newer}));
            assert!(c.task_fenced);
            assert!(c.view().approval.is_none());
        }
    }
    #[test]
    fn completed_task_does_not_redirect_new_read_chat_stop_to_task_control() {
        let mut f = Fixture::new("task-rounds");
        f.ready();
        f.controller
            .create_task(
                &frozen_task_intent(&f.identity),
                vec![json!({"type":"text","text":"原附件引用"})],
                json!({"modelId":"agent.fast"}),
            )
            .unwrap();
        f.until(|c| {
            c.current_task()
                .is_some_and(|t| t.status == TaskStatus::Completed)
        });
        assert!(!f.controller.is_task_turn());
        let task_id = f.controller.current_task_id().unwrap().to_owned();
        f.controller.send_text("读取新问题").unwrap();
        f.until(|c| c.view().phase == Phase::Running && c.current_turn_id().is_some());
        assert!(!f.controller.is_task_turn());
        assert_eq!(f.controller.current_task_id(), Some(task_id.as_str()));
        f.controller.stop().unwrap();
        f.until(|c| c.view().outcome == Some(TaskOutcome::Stopped));
        assert_eq!(f.requests().last().unwrap()["method"], "stop");
        assert_eq!(
            f.controller.current_task().unwrap().status,
            TaskStatus::Completed
        );
    }
    #[test]
    fn task_negotiated_chat_admission_is_one_host_input_and_checks_applied_identity() {
        for wrong_input in [false, true] {
            let (mut c, _) = task_controller();
            c.send_turn = Some(("human-input".into(), None));
            task_event(&mut c, 1, "turn.started", "human-turn", json!({}));
            assert_eq!(c.current_turn_id(), Some("human-turn"));
            assert!(!c.is_task_turn());
            if wrong_input {
                task_event(
                    &mut c,
                    2,
                    "input.applied",
                    "human-turn",
                    json!({"inputId":"other-input"}),
                );
            } else {
                task_event(
                    &mut c,
                    2,
                    "input.applied",
                    "human-turn",
                    json!({"inputId":"human-input"}),
                );
                assert_ne!(c.view().phase, Phase::Failed);
                task_event(&mut c, 3, "turn.started", "unsolicited-turn", json!({}));
            }
            assert_eq!(c.view().phase, Phase::Failed);
            assert!(c.send_turn.is_none());
        }
    }
    fn app_control_settlement_fixture() -> (Controller, Value) {
        let identity = crate::agent_identity::AppIdentity {
            instance_id: "instance".into(),
            account_scope: "a".repeat(64),
            app_session_id: "app-session".into(),
        };
        let page = concat_editor_mcp::AppViewBinding {
            page_id: concat_editor_mcp::AppPageId::Generation,
            page_epoch: 1,
            page_state_id: format!("app-page-v1-{}", "b".repeat(64)),
        };
        let source = crate::agent_app_control_input::InputBinding::new(&identity, "client", &page);
        let mut c = Controller {
            scope: Some(AgentScope::App(identity)),
            session_id: "session".into(),
            editor_client: "client".into(),
            app_control_supported: true,
            ..Controller::default()
        };
        c.app_control_inputs.remember("input", source.clone());
        c.app_control_inputs.applied("turn", "input", 1);
        let operation = concat_editor_mcp::app_operation_id("client", "session", 1).unwrap();
        c.app_control_inputs.started("session", "turn", 2, &json!({
            "invocationId":"invoke", "call":{"name":"app_navigate","arguments":{"target":"settings"}},
            "appViewBinding":source,"appControlIntent":{"version":1,"inputId":"input",
                "operationId":operation,"clientSequence":1,"before":page,"target":"settings"}
        })).unwrap();
        let receipt = json!({"appControlProtocolVersion":1,"operationId":operation,
            "sessionId":"session","clientSequence":1,
            "origin":{"appInstanceId":"instance","clientId":"client","appSessionId":"app-session"},
            "before":page,"target":"settings","recordedStatus":"cancelled","status":"cancelled",
            "durability":"confirmed","completion":null,"humanGate":null,"currentObservation":null});
        (c, receipt)
    }
    #[test]
    fn app_navigation_settlement_requires_exact_durable_original_receipt() {
        let (c, receipt) = app_control_settlement_fixture();
        let data = json!({"metadata":{"appControlReceipt":receipt}});
        assert!(c.settled_app_write("invoke", &data));
        assert!(!c.settled_app_write("another-invoke", &data));
        for (key, value) in [
            ("durability", json!("unconfirmed")),
            ("target", json!("account")),
            ("sessionId", json!("another-session")),
            ("status", json!("outcomeUnknown")),
        ] {
            let mut changed = data.clone();
            changed["metadata"]["appControlReceipt"][key] = value;
            assert!(!c.settled_app_write("invoke", &changed), "{key}");
        }
        assert!(!c.settled_app_write(
            "invoke",
            &json!({"metadata":{"postVerification":{"verified":true}}})
        ));
    }
    #[test]
    fn app_navigation_not_dispatched_settles_only_exact_closed_refusal() {
        let (c, receipt) = app_control_settlement_fixture();
        let data = json!({"message":{"isError":true},"metadata":{"version":1,
            "status":"not_dispatched","navigationSent":false,"resultAvailable":false,
            "operationId":receipt["operationId"],"clientSequence":1,"reasonCode":"revoked"}});
        assert!(c.settled_app_write("invoke", &data));
        let mut changed = data.clone();
        changed["metadata"]["navigationSent"] = json!(true);
        assert!(!c.settled_app_write("invoke", &changed));
        changed = data.clone();
        changed["metadata"]["extra"] = json!(true);
        assert!(!c.settled_app_write("invoke", &changed));
        changed = data.clone();
        changed["metadata"]["clientSequence"] = json!(2);
        assert!(!c.settled_app_write("invoke", &changed));
    }
    #[test]
    fn app_recovery_is_closed_bounded_and_independent_of_model_unknown() {
        let (mut c, _) = app_control_settlement_fixture();
        c.model_unknown = true;
        assert!(c.update_app_recovery(&json!({"unresolved":[
            {"invocationId":"invoke","tool":"app_navigate"},
            {"invocationId":"query","tool":"app_operation"}]})));
        assert_eq!(c.app_recovery.len(), 1);
        assert!(c.model_unknown);
        assert!(!c.update_app_recovery(&json!({"unresolved":[
            {"invocationId":"invoke","tool":"app_navigate","operationId":"untrusted"}]})));
        assert!(!c.update_app_recovery(&json!({"unresolved":[
            {"invocationId":"invoke","tool":"app_navigate"},
            {"invocationId":"invoke","tool":"app_navigate"}]})));
        c.app_control_supported = false;
        assert!(!c.update_app_recovery(&json!({"unresolved":[
            {"invocationId":"invoke","tool":"app_navigate"}]})));
    }
    #[test]
    fn app_scope_accepts_only_the_exact_four_read_tool_names() {
        for name in ["capabilities", "app_context", "page_items", "page_preview"] {
            let mut controller = Controller {
                scope: Some(AgentScope::App(crate::agent_identity::AppIdentity {
                    instance_id: "instance".into(),
                    account_scope: "a".repeat(64),
                    app_session_id: "app-session".into(),
                })),
                session_id: "fixture-session".into(),
                turn_id: Some("human-turn".into()),
                phase: Phase::Running,
                ..Controller::default()
            };
            task_event(
                &mut controller,
                1,
                "tool.started",
                "human-turn",
                json!({"invocationId":"read-1","call":{"name":name}}),
            );
            assert_ne!(controller.view().phase, Phase::Failed, "{name}");
            assert!(!controller.write_in_flight);
        }
        for name in [
            "context",
            "project",
            "preview",
            "canvas_edit",
            "clip_edit",
            "flow_get_workspace",
            "app_page_items",
            "app_page_preview",
            "task_checkpoint",
        ] {
            let mut controller = Controller {
                scope: Some(AgentScope::App(crate::agent_identity::AppIdentity {
                    instance_id: "instance".into(),
                    account_scope: "a".repeat(64),
                    app_session_id: "app-session".into(),
                })),
                session_id: "fixture-session".into(),
                turn_id: Some("human-turn".into()),
                phase: Phase::Running,
                ..Controller::default()
            };
            task_event(
                &mut controller,
                1,
                "tool.started",
                "human-turn",
                json!({"invocationId":"read-1","call":{"name":name}}),
            );
            assert_eq!(controller.view().phase, Phase::Failed, "{name}");
            assert!(!controller.write_in_flight);
        }
    }

    #[test]
    fn app_scope_negotiates_without_document_write_or_task_and_sends_original_view() {
        let mut fixture = Fixture::new("app-finish");
        fixture.ready();
        assert!(!fixture.controller.supports_tasks());
        assert_eq!(
            fixture
                .controller
                .update_permissions(Some("forbidden".into())),
            Err(UiError::Protocol)
        );
        assert_eq!(
            fixture.controller.send_text("unbound app read"),
            Err(UiError::Configuration)
        );
        let document_binding = frozen_document_observation(&fixture);
        assert_eq!(
            fixture.controller.send_document_content(
                "forbidden-document-input",
                vec![json!({"type":"text","text":"不能混用文档绑定"})],
                None,
                document_binding,
            ),
            Err(UiError::Configuration)
        );
        let view: AppViewBinding = serde_json::from_value(json!({"pageId":"personalLibrary","pageEpoch":3,"pageStateId":format!("app-page-v1-{}","a".repeat(64))})).unwrap();
        let view = InputBinding::new(
            fixture.controller.scope.as_ref().unwrap().app().unwrap(),
            &fixture.controller.editor_client,
            &view,
        );
        fixture
            .controller
            .send_app_content(
                "input-original",
                vec![json!({"type":"text","text":"这张图片是什么"})],
                None,
                view,
            )
            .unwrap();
        fixture.until(|controller| controller.view().outcome == Some(TaskOutcome::Completed));
        assert!(!fixture.controller.write_in_flight);
        let requests = fs::read_to_string(fixture.directory.join("requests.jsonl")).unwrap();
        let request: Value = serde_json::from_str(requests.lines().next().unwrap()).unwrap();
        assert_eq!(request["params"]["inputId"], "input-original");
        assert_eq!(
            request["params"]["appViewBinding"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
    }
    #[test]
    fn app_scope_refuses_missing_negotiation_and_document_business_recovery() {
        let mut fixture = Fixture::new("app-no-cap");
        fixture.until(|controller| controller.view().phase == Phase::Failed);
        assert_eq!(fixture.controller.view().error, Some(UiError::Protocol));
        let trace = fs::read_to_string(fixture.directory.join("methods.txt")).unwrap();
        assert!(!trace.contains("session.create"));
        assert!(!fixture.controller.update_editor_recovery(
            &json!({"recoveryOperations":[{"sessionId":"fixture-session"}]})
        ));
    }
    #[test]
    fn app_recovery_ends_only_old_reads_and_never_resends_input() {
        let mut fixture = Fixture::new("app-recovery");
        fixture.until(Controller::can_abandon_app_reads);
        assert!(!fixture.controller.view().can_send);
        fixture.controller.abandon_app_reads().unwrap();
        fixture.until(|c| c.view().can_send);
        assert_ne!(
            fixture.controller.view().outcome,
            Some(TaskOutcome::OperationVerified)
        );
        assert!(!fixture.controller.can_abandon_app_reads());
        let methods = fs::read_to_string(fixture.directory.join("methods.txt")).unwrap();
        assert_eq!(methods, "initialize\nsession.create\napp.abandonReads\n");
        assert!(Controller::default().abandon_app_reads().is_err());
    }
    #[test]
    fn app_abandon_lost_ack_queries_original_status_without_repeating_action() {
        let mut fixture = Fixture::new("app-abandon-lost");
        fixture.until(Controller::can_abandon_app_reads);
        fixture.controller.abandon_app_reads().unwrap();
        // Let the child commit cleanup, then expire just the receipt deadline.
        thread::sleep(Duration::from_millis(100));
        fixture.controller.pending.front_mut().unwrap().since = Instant::now() - RESPONSE_TIMEOUT;
        fixture.until(|c| c.view().can_send);
        let methods = fs::read_to_string(fixture.directory.join("methods.txt")).unwrap();
        assert_eq!(
            methods,
            "initialize\nsession.create\napp.abandonReads\nstatus\n"
        );
        assert_ne!(
            fixture.controller.view().outcome,
            Some(TaskOutcome::OperationVerified)
        );
    }
    #[test]
    fn app_create_probe_requires_a_second_explicit_restore_after_rebind_error() {
        let mut fixture = Fixture::new("app-rebind-required");
        fixture.until(|c| c.requires_app_restore() && !c.process_present());
        assert!(!fixture.controller.session_opened());
        assert_eq!(
            fs::read_to_string(fixture.directory.join("methods.txt")).unwrap(),
            "initialize\nsession.create\n"
        );
        let mut config = fixture.original_restore_config("app-rebind-required");
        config.scope = fixture.scope.clone();
        config.write_token = None;
        fixture.controller.connect(config).unwrap();
        fixture.ready();
        assert!(!fixture.controller.requires_app_restore());
        assert_eq!(
            fs::read_to_string(fixture.directory.join("methods.txt")).unwrap(),
            "initialize\nsession.create\ninitialize\nsession.restore\n"
        );
    }
    #[test]
    fn app_recovery_current_queues_do_not_require_complete_old_history() {
        let mut fixture = Fixture::new("app-recovery");
        fixture.until(Controller::can_abandon_app_reads);
        let mut value = fixture
            .controller
            .app_read_recovery()
            .unwrap()
            .evidence
            .clone();
        value["recentWithdrawnInputIds"] =
            json!((0..64).map(|n| format!("old-{n}")).collect::<Vec<_>>());
        value["withdrawnInputTotal"] = json!(200);
        value["withdrawnInputTruncated"] = json!(true);
        assert!(AppReadRecovery::parse(&value).unwrap().can_abandon);
        value["withdrawnInputTruncated"] = json!(false);
        assert!(AppReadRecovery::parse(&value).is_none());
        value["withdrawnInputTruncated"] = json!(true);
        value["canAbandon"] = json!(false);
        assert!(AppReadRecovery::parse(&value).is_none());
    }

    const FIXTURE: &str = r#"import sys,json,time,pathlib,os,hashlib
scenario='finish';session='';seq=0;managed=False
revision=0;app_abandoned=False
def app_summary():
    pending=scenario in ('app-recovery','app-abandon-lost') and not app_abandoned
    recovery={'canAbandon':pending,'reason':'ready' if pending else 'nothing_pending','pendingInputIds':['old-input'] if pending else [],'readInvocationIds':['old-read'] if pending else [],'recentWithdrawnInputIds':['old-input'] if app_abandoned else [],'withdrawnInputTotal':1 if app_abandoned else 0,'withdrawnInputTruncated':False,'recentAbandonedReadInvocationIds':['old-read'] if app_abandoned else [],'abandonedReadInvocationTotal':1 if app_abandoned else 0,'abandonedReadInvocationTruncated':False}
    return {'sessionId':session,'status':'interrupted' if pending else 'idle','activeTurnId':None,'recoveryRequired':pending,'pendingInputCount':1 if pending else 0,'toolState':{'outcomeUnknown':pending,'unresolved':[],'recoveryOperations':[],'observed':[]},'readRecovery':recovery}
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
def event(kind,data={},turn='turn-1'):
    global seq
    seq+=1
    print(json.dumps({'version':1,'type':'event','event':{'schemaVersion':1,'seq':seq,'eventId':str(seq),'timestamp':'2026-10-05','sessionId':session,'turnId':turn,'type':kind,'data':data}}),flush=True)
def final(status='completed',unknown=False,pending=False,failed=False):
    observed=[{'invocationId':'older_move','state':'failed','tool':'move_selected_image'}]
    if pending or failed: observed.append({'invocationId':'turn-1_move','state':'failed' if failed else 'completed','tool':'move_selected_image','appStatus':'notAuthorized' if failed else 'ok','postVerification':'pending' if pending else 'verified'})
    print(json.dumps({'version':1,'type':'result','sessionId':session,'result':{'turnId':'turn-1','status':status,'text':'模型结果 inference-secret read-secret write-secret'},'toolState':{'outcomeUnknown':unknown,'unresolved':[],'recoveryOperations':[],'observed':observed}}),flush=True)
task=None;task_state_revision=0
updates={}
def task_observation(binding):
    assert binding['appInstanceId']=='12345678-1234-1234-1234-123456789abc' and binding['clientId']=='client-1' and binding['projectId']=='project-1' and binding['documentSessionId']=='doc-1'
    assert binding['kind']=='clip' and binding['revision']==7 and binding['contextRevision']==9 and binding['selectionRevision']==11
    assert binding['selectedObjectIds']==['clip:'+str(n) for n in range(300)] and binding['viewStateId']=='view-v1-'+'a'*64

def task_state():
    global task_state_revision
    task_state_revision+=1;task['stateRevision']=task_state_revision
    print(json.dumps({'version':1,'type':'task.state','sessionId':session,'task':task}),flush=True)
def task_start(ordinal):
    task.update(rounds=ordinal,activeInputId='input-'+str(ordinal),activeTurnId='turn-'+str(ordinal),roundOrdinal=ordinal)
    task['taskRevision']+=1;task_state();event('turn.started',{},task['activeTurnId'])
def task_final(ordinal,status='completed'):
    print(json.dumps({'version':1,'type':'result','sessionId':session,'result':{'turnId':'turn-'+str(ordinal),'status':status,'text':'round-'+str(ordinal)},'toolState':{'outcomeUnknown':False,'unresolved':[],'recoveryOperations':[],'observed':[]}}),flush=True)
for line in sys.stdin:
    f=json.loads(line);p=f['params'];m=f['method']
    with pathlib.Path(__file__).with_name('methods.txt').open('a') as trace: trace.write(m+'\n')
    if m in ('send','stop','reconcile','editor.reconcile') or m.startswith('task.'):
        with pathlib.Path(__file__).with_name('requests.jsonl').open('a') as trace: trace.write(json.dumps({'method':m,'params':p})+'\n')
    if m=='initialize':
        assert p['protocolVersion']==1 and p['appProtocolVersion']==1
        managed=p['provider']['protocol']=='seecut-managed'
        assert p['readToken']=='read-secret'
        assert p['provider']['sessionToken' if managed else 'apiKey']=='inference-secret'
        assert p['mcpEnvironment']['TMPDIR']==p['sessionDirectory']
        scenario=p['provider']['model'];time.sleep(.08)
        if scenario=='init-fail': reply(f,error={'code':'provider_configuration','message':'raw inference-secret'});continue
        if scenario.startswith('app-'):
            assert p['scopeKind']=='app' and p['appReadProtocolVersion']==1 and p['accountScope']=='a'*64 and p['appSessionId']=='app-session'
            assert all(key not in p for key in ('projectId','documentSessionId','taskProtocolVersion','documentObservationProtocolVersion','taskObservationProtocolVersion','writeToken'))
        else: assert p['documentObservationProtocolVersion']==1 and p['taskObservationProtocolVersion']==1
        capabilities={} if scenario=='old-runtime' else {'session_restore_rebind':True,'session_create_or_restore':True,'turn_model_selection':True,'session_permissions':True}
        if scenario.startswith('app-') and scenario!='app-no-cap': capabilities.update(app_read_scope=True,app_read_protocol_version=1,durable_tasks=True,task_protocol_version=1,task_observation_binding=True,task_observation_protocol_version=1)
        if not scenario.startswith('app-') and scenario!='old-runtime': capabilities.update(document_observation_binding=True,document_observation_protocol_version=1)
        if scenario=='doc-no-cap': capabilities.pop('document_observation_binding')
        if scenario=='doc-false-cap': capabilities['document_observation_binding']=False
        if scenario=='doc-string-cap': capabilities['document_observation_binding']='true'
        if scenario=='doc-wrong-version': capabilities['document_observation_protocol_version']=2
        if scenario.startswith('task-'):
            assert p['taskProtocolVersion']==1
            capabilities.update(durable_tasks=True,task_protocol_version=1,task_observation_binding=True,task_observation_protocol_version=1)
        if scenario=='task-no-observation': capabilities.pop('task_observation_binding')
        if scenario=='task-false-observation': capabilities['task_observation_binding']=False
        if scenario=='task-string-observation': capabilities['task_observation_binding']='true'
        if scenario=='task-no-observation-version': capabilities.pop('task_observation_protocol_version')
        if scenario=='task-wrong-observation-version': capabilities['task_observation_protocol_version']=2
        if scenario=='task-no-doc-observation': capabilities.pop('document_observation_binding')
        if scenario=='selection-absent': capabilities.pop('turn_model_selection')
        if scenario=='selection-false': capabilities['turn_model_selection']=False
        if scenario=='selection-string': capabilities['turn_model_selection']='true'
        reply(f,{'protocolVersion':1,'instanceId':p['instanceId'],'capabilities':capabilities})
    elif m in ('session.create','session.restore'):
        if scenario.startswith('app-'):
            assert p['scopeKind']=='app' and p['accountScope']=='a'*64 and p['appSessionId']=='app-session'
            assert all(key not in p for key in ('projectId','documentSessionId','writeToken'))
        else: assert p['projectId']=='project-1' and p['documentSessionId']=='doc-1'
        if m=='session.restore': assert p['rebind'] is True and 'recoverExisting' not in p
        elif scenario.startswith('app-'): assert p['recoverExisting'] is True and 'rebind' not in p
        elif scenario=='old-runtime': assert 'rebind' not in p and 'recoverExisting' not in p
        else: assert p['rebind'] is True and p['recoverExisting'] is True
        session=p['sessionId'];time.sleep(.08)
        if scenario=='app-rebind-required' and m=='session.create': reply(f,error={'code':'rebind_required','message':'explicit restore required'});continue
        if scenario.startswith('app-'): reply(f,app_summary());continue
        if m=='session.restore' and scenario in ('task-create-lost-restore','task-update-lost-restore'): task=json.loads(pathlib.Path(__file__).with_name('task-status.json').read_text())
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
    elif m=='status' and scenario.startswith('app-'):
        assert p=={'sessionId':session};reply(f,app_summary())
    elif m=='app.abandonReads':
        assert scenario.startswith('app-') and p=={'sessionId':session}
        was_abandoned=app_abandoned;app_abandoned=True
        if scenario=='app-abandon-lost' and not was_abandoned: continue
        summary=app_summary();reply(f,{'sessionId':session,'status':'unchanged' if was_abandoned else 'stale_reads_abandoned','resultAvailable':False,'readRecovery':summary['readRecovery'],'session':summary})
    elif m=='session.permissions':
        assert p['sessionId']==session and p['version']==1
        assert p['writeToken'] is None if p['mode']=='read' else isinstance(p['writeToken'],str)
        reply(f,{'updated':True,'sessionId':session,'revision':p['revision']+1 if scenario=='bad-permission-ack' else p['revision'],'mode':p['mode']})
    elif m=='reconcile':
        assert managed and p=={'sessionId':session}
        reply(f,{'modelState':modelstate(False)})
    elif m=='editor.reconcile':
        record=json.loads(original_path.read_text())
        assert p=={'sessionId':session,'operationId':record['operationId']}
        record['queries']+=1;record['status']='verified';original_store(record)
        if scenario=='lost-reconcile-reply' and record['queries']==1: sys.exit(0)
        reply(f,{'verified':True,'success':True,'receipt':record['result'],'session':{'sessionId':session,'pendingInputCount':0,'toolState':{'outcomeUnknown':False,'unresolved':[],'recoveryOperations':[],'observed':[]}}})
    elif m=='task.create':
        task_observation(p['initialObservationBinding']);assert 'observationBinding' not in p
        if scenario in ('task-stale-observation','task-human-observation-conflict','task-invalid-observation-source'):
            code={'task-stale-observation':'stale_observation','task-human-observation-conflict':'human_observation_conflict','task-invalid-observation-source':'task_observation_source'}[scenario]
            reply(f,error={'code':code,'message':'fixture'});continue
        assert p['goal']=='保留完整原目标' and p['maxTaskCredits']==40
        assert p['content']==[{'type':'text','text':'原附件引用'}]
        assert p['deliverables']==[{'id':'result','description':'按用户目标完成并核对结果'}]
        task={'id':'task-'+hashlib.sha256(p['clientRequestId'].encode()).hexdigest(),'sessionId':session,'taskRevision':1,'controlEpoch':1,'status':'running','blockedReason':None,'rounds':0,'activeInputId':None,'activeTurnId':None,'roundOrdinal':None,'verifiedSteps':0,'totalSteps':1,'evidenceCount':0,'stateRevision':1,'lastUpdateId':None}
        if scenario=='task-create-lost-restore':
            pathlib.Path(__file__).with_name('task-status.json').write_text(json.dumps(task));continue
        late_create=json.loads(json.dumps(task)) if scenario=='task-create-ack-lost' else None
        if late_create is None: reply(f,task)
        else: late_create_frame=f
        task_state();task_start(1)
        if scenario=='task-rounds':
            event('text.delta',{'text':'第一轮'},'turn-1');task_final(1)
            task_start(2);event('text.delta',{'text':'第二轮'},'turn-2');task_final(2)
            task.update(status='completed',activeTurnId=None,activeInputId=None,roundOrdinal=None,verifiedSteps=1,evidenceCount=1);task['taskRevision']+=1;task_state()
        else:
            event('approval.required',{'approvalId':'task-approval-1','tool':'move_selected_image','arguments':{'projectId':'project-1','documentSessionId':'doc-1','revision':0,'selectionRevision':1,'objectId':'layer:1','deltaX':1,'deltaY':2}})
    elif m in ('task.pause','task.stop','task.resume','task.status','task.update'):
        assert p['taskId']==task['id'] and p['sessionId']==session
        if m=='task.status':
            task_state()
            if scenario!='task-status-ack-lost': reply(f,task)
            continue
        if m=='task.update':
            task_observation(p['observationBinding']);assert 'initialObservationBinding' not in p
            key=p['clientRequestId']
            if key not in updates:
                if p['expectedRevision']!=task['taskRevision']:
                    reply(f,error={'code':'task_revision_conflict','message':'zero mutation'});continue
                updates[key]={'message':p['message'],'observationBinding':p['observationBinding']};task.update(lastUpdateId=key,status='running');task['controlEpoch']+=1;task['taskRevision']+=1
            else: assert updates[key]=={'message':p['message'],'observationBinding':p['observationBinding']}
            if scenario=='task-update-lost-restore':
                pathlib.Path(__file__).with_name('task-status.json').write_text(json.dumps(task));continue
            task_state();reply(f,task);continue
        task['controlEpoch']+=1;task['taskRevision']+=1
        task['status']={'task.pause':'paused','task.stop':'stopped','task.resume':'running'}[m]
        if m!='task.resume': task.update(activeTurnId=None,activeInputId=None,roundOrdinal=None)
        task_state();reply(f,task)
        if late_create is not None:
            reply(late_create_frame,late_create);late_create=None
        if m=='task.pause': task_final(1,'interrupted')
        elif m=='task.resume':
            task_start(2);event('approval.required',{'approvalId':'task-approval-2','tool':'move_selected_image','arguments':{'objectId':'layer:1','deltaX':1,'deltaY':2}},'turn-2')
        elif m=='task.stop': task_final(task['rounds'],'interrupted')
    elif m=='send':
        assert p['sessionId']==session and p['mode']=='follow_up' and p['content'][0]['type']=='text'
        if not scenario.startswith('app-'):
            binding=p['observationBinding']
            assert binding['appInstanceId']=='12345678-1234-1234-1234-123456789abc' and binding['clientId']=='client-1' and binding['projectId']=='project-1' and binding['documentSessionId']=='doc-1'
            assert 'viewStateId' in binding and 'appViewBinding' not in p
        if scenario=='doc-lost-ack': continue
        if scenario=='doc-stale': reply(f,error={'code':'stale_observation','message':'fixture'});continue
        if scenario.startswith('app-'):
            assert p['inputId']=='input-original' and p['appViewBinding']=={'pageId':'personalLibrary','pageEpoch':3,'pageStateId':'app-page-v1-'+'a'*64}
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
        if scenario.startswith('task-'): continue
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
        scope: AgentScope,
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
            let scope = if scenario.starts_with("app-") {
                AgentScope::App(crate::agent_identity::AppIdentity {
                    instance_id: identity.instance_id.clone(),
                    account_scope: "a".repeat(64),
                    app_session_id: "app-session".into(),
                })
            } else {
                identity.clone().into()
            };
            let mut controller = Controller::default();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let result = controller.connect(ConnectionConfig{launch:LaunchSpec{executable:fs::canonicalize("/usr/bin/python3").unwrap(),entrypoint:entry.clone(),environment:BTreeMap::new()},scope:scope.clone(),client_id:"client-1".into(),mcp_client_path:entry.clone(),mcp_environment:BTreeMap::from([("TMPDIR".into(),directory.to_string_lossy().into_owned())]),session_directory:directory.clone(),local_session_id:session_id.into(),restore,provider:if scenario == "managed" {json!({"protocol":"seecut-managed","model":scenario,"sessionToken":"inference-secret"})} else {json!({"protocol":"fixture","model":scenario,"apiKey":"inference-secret"})},read_token:"read-secret".into(),write_token:(!scenario.starts_with("app-")).then(||"write-secret".into()),app_control_token:None});
                match result {
                    Ok(()) => break,
                    // Parallel fixtures can contend on the nonblocking registration/input
                    // locks. Busy means initialize was not accepted; never retry a send.
                    Err(UiError::Busy) => {
                        controller.shutdown_now();
                        while controller.process.is_some() {
                            controller.tick(Some(&scope));
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
                scope,
                directory,
            }
        }
        fn until(&mut self, condition: impl Fn(&Controller) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                self.controller.tick(Some(&self.scope));
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
                scope: self.identity.clone().into(),
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
                app_control_token: None,
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
                self.controller.tick(Some(&self.scope));
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
            if scenario == "old-runtime" {
                assert_eq!(
                    f.controller.send_text("旧协议不得发送未绑定问题"),
                    Err(UiError::ObservationUnavailable)
                );
                assert!(f.requests().is_empty());
                continue;
            }
            f.controller
                .send_text("模型选择旧协议仍可发送带观察的问题")
                .unwrap();
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
        c.send_turn = Some(("input".into(), None));
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
                scope: document.into(), client_id: "client-1".into(), mcp_client_path: entry,
                mcp_environment: BTreeMap::from([("TMPDIR".into(), f.directory.to_string_lossy().into_owned())]),
                session_directory: f.directory.clone(), local_session_id: "managed-test".into(), restore: true,
                provider: json!({"protocol":"seecut-managed","model":"managed","sessionToken":token}),
                read_token: "read-secret".into(), write_token: Some("write-secret".into()), app_control_token: None,
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
    fn permission_hot_switch_keeps_process_session_and_never_resends_input() {
        let mut f = Fixture::new("finish");
        f.ready();
        let session = f.controller.session_id.clone();
        f.controller
            .update_permissions(Some("transient-write-secret".into()))
            .unwrap();
        f.controller.update_permissions(None).unwrap();
        assert!(!f.controller.view().can_send);
        f.until(|c| c.pending.is_empty());
        assert!(f.controller.process_present() && f.controller.view().can_send);
        assert_eq!(f.controller.session_id, session);
        f.controller
            .update_permissions(Some("new-write-secret".into()))
            .unwrap();
        f.until(|c| c.pending.is_empty());
        f.controller.send_text("读取权限切换后的工程").unwrap();
        f.until(|c| c.view().outcome == Some(TaskOutcome::Completed));
        let trace = std::fs::read_to_string(f.directory.join("methods.txt")).unwrap();
        assert_eq!(trace.lines().filter(|m| *m == "initialize").count(), 1);
        assert_eq!(trace.lines().filter(|m| *m == "session.create").count(), 1);
        assert_eq!(trace.lines().filter(|m| *m == "send").count(), 1);
    }
    #[test]
    fn permission_bad_ack_fails_closed_and_old_runtime_cannot_hot_switch() {
        let mut f = Fixture::new("bad-permission-ack");
        f.ready();
        f.controller.update_permissions(None).unwrap();
        f.until(|c| c.view().error == Some(UiError::Protocol));
        assert!(!f.controller.view().can_send);
        let mut old = Fixture::new("old-runtime");
        old.ready();
        assert_eq!(
            old.controller.update_permissions(None),
            Err(UiError::Protocol)
        );
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
        fixture
            .controller
            .tick(Some(&fixture.identity.clone().into()));
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
            is_flow: false,
            is_clip: false,
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
                scope: f.identity.clone().into(),
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
                app_control_token: None,
            }
        };
        for changed in 0..4 {
            let mut c = config(&f);
            match changed {
                0 => c.restore = false,
                1 => c.client_id = "other-client".into(),
                2 => document_mut(&mut c.scope).document_session_id = "other-document".into(),
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
                    2 => {
                        document_mut(&mut config.scope).document_session_id =
                            "other-document".into()
                    }
                    3 => config.local_session_id = "other-session".into(),
                    4 => document_mut(&mut config.scope).instance_id = "other-instance".into(),
                    _ => document_mut(&mut config.scope).project_id = "other-project".into(),
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
