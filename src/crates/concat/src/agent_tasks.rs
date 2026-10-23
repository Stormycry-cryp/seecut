//! Bounded durable-task IPC projections and explicit host-created requests.
use crate::agent_observation::DocumentObservationBinding;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    WaitingJob,
    Blocked,
    Paused,
    Stopped,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskSummary {
    pub id: String,
    pub session_id: String,
    pub task_revision: u64,
    pub state_revision: u64,
    pub control_epoch: u64,
    pub status: TaskStatus,
    pub blocked_reason: Option<String>,
    pub rounds: u64,
    pub active_input_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub round_ordinal: Option<u64>,
    pub verified_steps: u64,
    pub total_steps: u64,
    pub evidence_count: u64,
    pub last_update_id: Option<String>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"_.:-".contains(&v))
}

impl TaskStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Completed | Self::Failed)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::WaitingJob => "等待生成任务",
            Self::Blocked => "需要处理",
            Self::Paused => "已暂停",
            Self::Stopped => "已停止",
            Self::Completed => "已核对完成",
            Self::Failed => "失败",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskIntent {
    pub(crate) client_request_id: String,
    pub(crate) task_id: String,
    pub(crate) goal: String,
    pub(crate) max_task_credits: u64,
    /// Original human create observation; absence denotes legacy history only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_observation"
    )]
    pub(crate) initial_observation_binding: Option<DocumentObservationBinding>,
}
impl TaskIntent {
    pub(crate) fn valid(&self) -> bool {
        identifier(&self.client_request_id)
            && self.task_id == task_id_for(&self.client_request_id)
            && !self.goal.trim().is_empty()
            && self.goal.len() <= 32000
            && !self.goal.contains('\0')
            && (1..=MAX_SAFE_INTEGER).contains(&self.max_task_credits)
            && self
                .initial_observation_binding
                .as_ref()
                .is_none_or(DocumentObservationBinding::valid)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskUpdateIntent {
    pub(crate) client_request_id: String,
    pub(crate) task_id: String,
    pub(crate) message: String,
    pub(crate) expected_revision: u64,
    pub(crate) retries: u8,
    pub(crate) confirmed: bool,
    /// Original human update observation; retries retain the same reference.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_observation"
    )]
    pub(crate) observation_binding: Option<DocumentObservationBinding>,
}
impl TaskUpdateIntent {
    pub(crate) fn valid(&self) -> bool {
        identifier(&self.client_request_id)
            && valid_task_id(&self.task_id)
            && !self.message.trim().is_empty()
            && self.message.len() <= 32000
            && !self.message.contains('\0')
            && (1..=MAX_SAFE_INTEGER).contains(&self.expected_revision)
            && self.retries <= 2
            && self
                .observation_binding
                .as_ref()
                .is_none_or(DocumentObservationBinding::valid)
    }
}
// Preserve legacy absence while rejecting an explicitly null observation.
// Current document identity must be checked by the dispatching controller.
fn present_observation<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DocumentObservationBinding>, D::Error> {
    DocumentObservationBinding::deserialize(deserializer).map(Some)
}

pub(crate) fn parse_task_budget(text: &str, turn_cap: u64) -> Result<u64, &'static str> {
    let value = text.trim();
    if value.is_empty() || value.len() > 16 || !value.bytes().all(|v| v.is_ascii_digit()) {
        return Err("请填写任务推理积分上限的正整数");
    }
    let cap = value
        .parse::<u64>()
        .ok()
        .filter(|v| (1..=MAX_SAFE_INTEGER).contains(v))
        .ok_or("任务积分上限超出安全整数范围")?;
    if turn_cap == 0 || turn_cap > MAX_SAFE_INTEGER {
        return Err("当前模型的单轮额度暂不可用");
    }
    if cap < turn_cap {
        return Err("任务上限低于当前模型完整单轮预留，请调整后发送");
    }
    Ok(cap)
}

pub fn task_id_for(client_request_id: &str) -> String {
    format!("task-{:x}", Sha256::digest(client_request_id.as_bytes()))
}

pub(crate) fn blocked_reason_label(reason: &str) -> &'static str {
    match reason {
        "human_observation_conflict" => {
            "任务要求引用了不同页面或选区，已停止推进。请停止旧任务后重新创建"
        }
        "stale_observation" => {
            "原问题引用的页面、选择或内容已变化，已停止推进。请停止旧任务并重新提出要求"
        }
        "task_observation_source" => "无法核对任务的原始发问记录，已停止推进。请查询原任务状态",
        "runtime_tools_unavailable" => "当前服务暂未开放持续任务，请更新服务后重新连接",
        "budget_insufficient" => "任务推理额度不足以预留下一完整轮次，请核对预算",
        "outcome_unknown"
        | "dispatch_outcome_unknown"
        | "input_lookup_uncertain"
        | "resume_recovery_required"
        | "stale_dispatch_requires_resolution" => "原请求结果尚未核实，请先查询原状态",
        "managed_finalizing" => "正在核对原轮次的推理费用，请稍候",
        "binding_unavailable" | "job_scope_unavailable" => {
            "当前工程或任务范围已变化，请回到原工程核对"
        }
        "permission_required" => "当前任务缺少页面操作授权，请检查权限后继续",
        "deliverable_unverifiable" => "交付结果还没有可核对的证据，请补充要求后继续",
        "deliverable_state_changed" => "已核对的结果又有变化，请查询并重新核对",
        "selection_policy_changed" => "原模型或强度策略已变化，请核对后重新发起任务",
        "job_pending" => "正在等待原生成任务完成",
        "job_identity_unavailable" | "job_artifact_unverified" => {
            "原生成任务或产物暂时无法核对，请查询原状态"
        }
        "job_query_unavailable" => "暂时无法查询原生成任务，请稍后核对",
        "job_wait_deadline" | "job_poll_limit" | "job_manual_check_limit" => {
            "原生成任务等待或查询次数已达上限，请手动核对"
        }
        "task_limit" => "任务轮次、时长或无进展次数已达上限，请核对结果",
        "storage_capacity" | "checkpoint_capacity" | "task_context_capacity" => {
            "任务内容或记录已达容量上限，请精简要求后重新发起"
        }
        "other_task_active" => "同一会话还有任务在处理，请先核对该任务",
        "round_running" => "当前轮次仍在处理，请稍候",
        "cancel_outcome_unknown" => "停止请求的结果尚未核实，请查询原状态",
        "process_restart" => "连接已重新建立，任务保持暂停，请核对后继续",
        _ => "任务暂时无法继续，请查询原状态",
    }
}

pub(crate) fn valid_task_id(id: &str) -> bool {
    id.starts_with("task-")
        && id.len() == 69
        && id[5..]
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
impl TaskSummary {
    pub fn parse(value: &Value, session_id: &str) -> Option<Self> {
        let result: Self = serde_json::from_value(value.clone()).ok()?;
        let counters = [
            result.task_revision,
            result.state_revision,
            result.control_epoch,
            result.rounds,
            result.verified_steps,
            result.total_steps,
            result.evidence_count,
            result.round_ordinal.unwrap_or(0),
        ];
        if !valid_task_id(&result.id)
            || [
                "blockedReason",
                "activeInputId",
                "activeTurnId",
                "roundOrdinal",
                "lastUpdateId",
            ]
            .iter()
            .any(|key| value.get(*key).is_none())
            || result.session_id != session_id
            || !identifier(&result.session_id)
            || counters.iter().any(|v| *v > MAX_SAFE_INTEGER)
            || result.task_revision == 0
            || result.state_revision == 0
            || !(1..=128).contains(&result.total_steps)
            || result.verified_steps > result.total_steps
            || result.rounds > 128
            || result.active_input_id.is_some() != result.round_ordinal.is_some()
            || result.round_ordinal.is_some_and(|v| v > result.rounds)
            || result.status == TaskStatus::Completed
                && (result.verified_steps != result.total_steps
                    || result.evidence_count == 0
                    || result.active_input_id.is_some()
                    || result.active_turn_id.is_some())
            || result.evidence_count > 1024
            || result
                .blocked_reason
                .as_ref()
                .is_some_and(|v| v.len() > 2048 || v.contains('\0'))
            || [
                &result.active_input_id,
                &result.active_turn_id,
                &result.last_update_id,
            ]
            .into_iter()
            .flatten()
            .any(|v| !identifier(v))
            || result.active_turn_id.is_some() && result.active_input_id.is_none()
            || result.round_ordinal.is_some_and(|v| v == 0 || v > 128)
        {
            return None;
        }
        Some(result)
    }
}

/// Persist only current_task_id into the existing native conversation record.
/// The generation and projection watermark belong to this IPC connection only.
#[derive(Default)]
pub struct TaskProjection {
    pub current_task_id: Option<String>,
    generation: u64,
    state_revision: u64,
    task_revision: u64,
    control_epoch: u64,
    current: Option<TaskSummary>,
}

impl TaskProjection {
    pub fn current(&self) -> Option<&TaskSummary> {
        self.current.as_ref()
    }
    pub fn restored(task_id: Option<String>) -> Option<Self> {
        if task_id.as_deref().is_some_and(|id| !valid_task_id(id)) {
            return None;
        }
        Some(Self {
            current_task_id: task_id,
            ..Self::default()
        })
    }

    /// Call only when a newly spawned IPC process has a new host generation.
    pub fn begin_connection(&mut self, generation: u64) -> bool {
        if generation <= self.generation {
            return false;
        }
        crate::clip_save_fence::invalidate(
            concat_host::projects::SaveInvalidation::ConversationChanged,
        );
        self.generation = generation;
        self.state_revision = 0;
        self.current = None;
        true
    }

    /// Select only from the matching create response or explicit host action.
    /// Initialization may emit many tasks and never chooses the last replay.
    pub fn select_created_task(&mut self, id: &str) -> bool {
        if !valid_task_id(id) {
            return false;
        }
        crate::clip_save_fence::invalidate(
            concat_host::projects::SaveInvalidation::ConversationChanged,
        );
        self.current_task_id = Some(id.into());
        self.task_revision = 0;
        self.control_epoch = 0;
        self.state_revision = 0;
        self.current = None;
        true
    }

    pub fn accept(&mut self, generation: u64, session_id: &str, value: &Value) -> bool {
        if generation != self.generation || generation == 0 {
            return false;
        }
        let Some(summary) = TaskSummary::parse(value, session_id) else {
            return false;
        };
        if self.current_task_id.as_deref() != Some(&summary.id)
            || summary.state_revision <= self.state_revision
            || summary.task_revision < self.task_revision
            || summary.control_epoch < self.control_epoch
        {
            return false;
        }
        if self.current.as_ref().is_some_and(|old| {
            old.control_epoch != summary.control_epoch
                || old.active_turn_id != summary.active_turn_id
                || old.active_input_id != summary.active_input_id
                || old.status != summary.status
        }) {
            crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::Stopped);
        }
        self.state_revision = summary.state_revision;
        self.task_revision = summary.task_revision;
        self.control_epoch = summary.control_epoch;
        self.current = Some(summary);
        true
    }

    pub fn owns_automatic_turn(&self, generation: u64, input_id: &str, turn_id: &str) -> bool {
        generation == self.generation
            && self.current.as_ref().is_some_and(|task| {
                task.status == TaskStatus::Running
                    && task.active_input_id.as_deref() == Some(input_id)
                    && task.active_turn_id.as_deref() == Some(turn_id)
            })
    }
}

/// The UI must show this explicit task cap before calling the create builder.
pub fn default_task_budget(current_turn_cap: u64) -> Option<u64> {
    current_turn_cap
        .checked_mul(4)
        .filter(|v| *v > 0 && *v <= MAX_SAFE_INTEGER)
}

/// Content is the original already-resolved ContentPart array; do not recreate
/// attachments from displayed text. Formal integration should reuse the
/// existing send-content validator instead of adding an alternative parser.
pub fn create_request(
    session_id: &str,
    client_request_id: &str,
    goal: &str,
    content: &[Value],
    model_selection: &Value,
    max_task_credits: u64,
) -> Option<Value> {
    if !identifier(session_id)
        || !identifier(client_request_id)
        || goal.trim().is_empty()
        || goal.len() > 32_000
        || goal.contains('\0')
        || !(1..=MAX_SAFE_INTEGER).contains(&max_task_credits)
        || !model_selection.is_object()
        || content.len() > 7
        || !valid_content(content)
    {
        return None;
    }
    Some(json!({
        "sessionId":session_id,"clientRequestId":client_request_id,"goal":goal,
        "steps":[{"id":"work","label":"完成任务并核对结果"}],
        "deliverables":[{"id":"result","description":"按用户目标完成并核对结果"}],
        "modelSelection":model_selection,"maxTaskCredits":max_task_credits,
        "content":content
    }))
}

fn valid_content(content: &[Value]) -> bool {
    let mut text_bytes = 0usize;
    let mut images = 0usize;
    for part in content {
        let Some(object) = part.as_object() else {
            return false;
        };
        match part["type"].as_str() {
            Some("text") => {
                if object
                    .keys()
                    .any(|v| !matches!(v.as_str(), "type" | "text"))
                {
                    return false;
                }
                let Some(text) = part["text"].as_str() else {
                    return false;
                };
                if text.trim().is_empty() || text.contains('\0') {
                    return false;
                }
                text_bytes = text_bytes.saturating_add(text.len());
            }
            Some("image") => {
                if object
                    .keys()
                    .any(|v| !matches!(v.as_str(), "type" | "mimeType" | "data"))
                    || !matches!(
                        part["mimeType"].as_str(),
                        Some("image/png" | "image/jpeg" | "image/webp" | "image/gif")
                    )
                    || part["data"].as_str().is_none_or(|v| v.len() > 1_398_104)
                {
                    return false;
                }
                images += 1;
            }
            _ => return false,
        }
    }
    text_bytes <= 32_000 && images <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_save_task_source_transition_revokes_before_projection_changes() {
        for field in ["controlEpoch", "activeTurnId", "status"] {
            let mut projection = TaskProjection::restored(Some(task_id_for("task-1"))).unwrap();
            assert!(projection.begin_connection(1));
            assert!(projection.accept(1, "session-1", &summary("task-1", 1)));
            let pending = crate::clip_save_fence::test_support::PendingSave::new();
            let mut next = summary("task-1", 2);
            match field {
                "controlEpoch" => next[field] = json!(1),
                "activeTurnId" => next[field] = json!("turn-2"),
                _ => {
                    next[field] = json!("paused");
                    next["activeTurnId"] = Value::Null;
                    next["activeInputId"] = Value::Null;
                    next["roundOrdinal"] = Value::Null;
                }
            }
            assert!(projection.accept(1, "session-1", &next));
            pending.assert_not_committed();
        }
    }

    fn summary(task: &str, state: u64) -> Value {
        json!({"id":task_id_for(task),"sessionId":"session-1","taskRevision":3,
            "stateRevision":state,"controlEpoch":0,"status":"running",
            "blockedReason":null,"rounds":1,"activeInputId":"input-1",
            "activeTurnId":"turn-1","roundOrdinal":1,
            "verifiedSteps":0,"totalSteps":1,"evidenceCount":0,"lastUpdateId":null})
    }

    #[test]
    fn strict_summary_and_current_connection_bind_automatic_turn() {
        let mut projection = TaskProjection::restored(Some(task_id_for("task-1"))).unwrap();
        assert!(projection.begin_connection(1));
        assert!(!projection.accept(1, "session-1", &summary("task-2", 99)));
        assert!(projection.accept(1, "session-1", &summary("task-1", 1)));
        assert!(projection.owns_automatic_turn(1, "input-1", "turn-1"));
        assert!(!projection.owns_automatic_turn(1, "input-1", "foreign-turn"));
        assert!(!projection.accept(1, "session-1", &summary("task-1", 1)));
        assert!(projection.begin_connection(2));
        assert_eq!(
            projection.current_task_id.as_deref(),
            Some(task_id_for("task-1").as_str())
        );
        assert!(!projection.owns_automatic_turn(2, "input-1", "turn-1"));
        assert!(!projection.accept(1, "session-1", &summary("task-1", 200)));
        assert!(projection.accept(2, "session-1", &summary("task-1", 1)));
        let mut malformed = summary("task-1", 2);
        malformed["verifiedSteps"] = json!(2);
        assert!(!projection.accept(2, "session-1", &malformed));
        malformed = summary("task-1", 2);
        malformed["grant"] = json!(true);
        assert!(!projection.accept(2, "session-1", &malformed));
    }

    #[test]
    fn default_create_retains_goal_original_content_and_explicit_budget() {
        let content = vec![json!({"type":"image","mimeType":"image/png","data":"fixture"})];
        let request = create_request(
            "session-1",
            "request-1",
            "完成原目标",
            &content,
            &json!({"modelId":"model-1"}),
            default_task_budget(25).unwrap(),
        )
        .unwrap();
        assert_eq!(request["content"], json!(content));
        assert_eq!(request["goal"], "完成原目标");
        assert_eq!(
            request["deliverables"][0]["description"],
            "按用户目标完成并核对结果"
        );
        assert_eq!(request["maxTaskCredits"], 100);
        assert!(default_task_budget(MAX_SAFE_INTEGER).is_none());
        assert!(default_task_budget(0).is_none());
        for bytes in [2049, 32_000] {
            let goal = "a".repeat(bytes);
            let request = create_request(
                "session-1",
                "request-1",
                &goal,
                &[],
                &json!({"modelId":"model-1"}),
                100,
            )
            .unwrap();
            assert_eq!(request["goal"], goal);
        }
        assert!(
            create_request(
                "session-1",
                "request-1",
                &"a".repeat(32_001),
                &[],
                &json!({"modelId":"model-1"}),
                100
            )
            .is_none()
        );
        let excessive = vec![
            json!({"type":"text","text":"a".repeat(16000)}),
            json!({"type":"text","text":"b".repeat(16001)}),
        ];
        assert!(
            create_request(
                "session-1",
                "request-1",
                "goal",
                &excessive,
                &json!({"modelId":"model-1"}),
                100
            )
            .is_none()
        );
    }
    #[test]
    fn task_budget_is_a_visible_safe_integer_and_never_increases_a_low_choice() {
        assert_eq!(parse_task_budget("80", 20), Ok(80));
        for text in ["", "0", "-1", "1e3", "12.5", "9007199254740992"] {
            assert!(parse_task_budget(text, 20).is_err());
        }
        let draft = "19";
        assert!(parse_task_budget(draft, 20).is_err());
        assert_eq!(draft, "19");
        assert_eq!(
            parse_task_budget("9007199254740991", 20),
            Ok(MAX_SAFE_INTEGER)
        );
        for reason in [
            "budget_insufficient",
            "permission_required",
            "deliverable_unverifiable",
            "job_pending",
            "task_limit",
            "storage_capacity",
            "outcome_unknown",
        ] {
            assert!(!blocked_reason_label(reason).contains('_'));
        }
        assert_eq!(
            blocked_reason_label("runtime_tools_unavailable"),
            "当前服务暂未开放持续任务，请更新服务后重新连接"
        );
        assert_eq!(
            blocked_reason_label("unrecognized_reason"),
            "任务暂时无法继续，请查询原状态"
        );
    }
    #[test]
    fn task_summary_rejects_unknown_fields_invalid_ids_and_false_completion() {
        let original = summary("task-1", 1);
        for (key, value) in [
            ("id", json!("task-1")),
            ("stateRevision", json!(MAX_SAFE_INTEGER + 1)),
            ("roundOrdinal", json!(2)),
            ("lastUpdateId", json!("bad id")),
            ("status", json!("completed")),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            assert!(TaskSummary::parse(&changed, "session-1").is_none(), "{key}");
        }
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove("lastUpdateId");
        assert!(TaskSummary::parse(&missing, "session-1").is_none());
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    fn observation(kind: &str) -> Value {
        let mut binding = json!({
            "appInstanceId":"original-instance", "clientId":"original-client",
            "kind":kind, "projectId":"original-project", "documentSessionId":"original-doc",
            "revision":12, "contextRevision":15, "selectionRevision":4,
            "selectedObjectIds": (0..300).map(|i| format!("object-{i}")).collect::<Vec<_>>(),
            "viewStateId":format!("view-v1-{}", "a".repeat(64))
        });
        if kind == "flow" {
            binding["viewStateId"] = Value::Null;
            binding["flow"] = json!({"canvasId":"original-canvas", "accountScope":"original-account", "sessionId":"original-doc"});
        }
        binding
    }
    fn create() -> Value {
        json!({"client_request_id":"original-human-create",
            "task_id":task_id_for("original-human-create"),
            "goal":"完成原目标", "max_task_credits":100})
    }
    fn update() -> Value {
        json!({"client_request_id":"original-human-update",
            "task_id":task_id_for("original-human-create"),
            "message":"原补充内容", "expected_revision":5, "retries":0, "confirmed":false})
    }

    #[test]
    fn legacy_missing_observation_is_preserved_without_a_new_identity() {
        let initial: TaskIntent = serde_json::from_value(create()).unwrap();
        let followup: TaskUpdateIntent = serde_json::from_value(update()).unwrap();
        assert!(initial.valid() && followup.valid());
        assert!(initial.initial_observation_binding.is_none());
        assert!(followup.observation_binding.is_none());
        assert_eq!(serde_json::to_value(initial).unwrap(), create());
        assert_eq!(serde_json::to_value(followup).unwrap(), update());
    }

    #[test]
    fn explicit_null_wrong_case_and_unknown_authority_fields_are_rejected() {
        for invalid in [Value::Null, json!({}), json!(false), json!("binding")] {
            let mut initial = create();
            initial["initial_observation_binding"] = invalid.clone();
            assert!(serde_json::from_value::<TaskIntent>(initial).is_err());
            let mut followup = update();
            followup["observation_binding"] = invalid;
            assert!(serde_json::from_value::<TaskUpdateIntent>(followup).is_err());
        }
        for key in [
            "initialObservationBinding",
            "observation_binding",
            "permission",
            "appScope",
        ] {
            let mut initial = create();
            initial[key] = observation("canvas");
            assert!(
                serde_json::from_value::<TaskIntent>(initial).is_err(),
                "{key}"
            );
        }
        for key in [
            "observationBinding",
            "initial_observation_binding",
            "permission",
            "appScope",
        ] {
            let mut followup = update();
            followup[key] = observation("canvas");
            assert!(
                serde_json::from_value::<TaskUpdateIntent>(followup).is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn original_human_request_ids_and_complete_binding_survive_serde() {
        for kind in ["canvas", "clip", "flow"] {
            let binding = observation(kind);
            let mut initial = create();
            initial["initial_observation_binding"] = binding.clone();
            let task: TaskIntent = serde_json::from_value(initial.clone()).unwrap();
            assert!(task.valid());
            assert_eq!(task.client_request_id, "original-human-create");
            assert_eq!(
                task.task_id,
                "task-af759b1fc900878f420486c31247cda8ef3544a40045fcd6cca87754b3a3cde8"
            );
            assert_eq!(serde_json::to_value(&task).unwrap(), initial);
            let mut followup = update();
            followup["observation_binding"] = binding;
            let intent: TaskUpdateIntent = serde_json::from_value(followup.clone()).unwrap();
            assert!(intent.valid());
            assert_eq!(intent.client_request_id, "original-human-update");
            assert_ne!(intent.client_request_id, "task-automatic-round-input");
            assert_eq!(serde_json::to_value(intent).unwrap(), followup);
        }
    }

    #[test]
    fn invalid_observation_shape_and_internal_flow_scope_are_rejected() {
        let mut invalid = Vec::new();
        for (key, value) in [
            ("kind", json!("app")),
            ("revision", json!(MAX_SAFE_INTEGER + 1)),
            ("contextRevision", json!(-1)),
            ("selectionRevision", json!(0.5)),
            ("selectedObjectIds", json!(["same", "same"])),
            ("selectedObjectIds", json!(["\u{0000}"])),
            ("viewStateId", json!("unverified-view")),
            ("flow", Value::Null),
            ("permission", json!("read")),
            (
                "flow",
                json!({"canvasId":"c", "accountScope":"a", "sessionId":"original-doc"}),
            ),
        ] {
            let mut binding = observation("canvas");
            binding[key] = value;
            invalid.push(binding);
        }
        let mut missing_view = observation("canvas");
        missing_view.as_object_mut().unwrap().remove("viewStateId");
        invalid.push(missing_view);
        for key in ["canvasId", "accountScope", "sessionId"] {
            let mut missing_axis = observation("flow");
            missing_axis["flow"].as_object_mut().unwrap().remove(key);
            invalid.push(missing_axis);
        }
        let mut mixed_doc = observation("flow");
        mixed_doc["flow"]["sessionId"] = json!("different-doc");
        invalid.push(mixed_doc);
        let mut mixed_view = observation("flow");
        mixed_view["viewStateId"] = observation("canvas")["viewStateId"].clone();
        invalid.push(mixed_view);
        for binding in invalid {
            let mut initial = create();
            initial["initial_observation_binding"] = binding.clone();
            assert!(
                serde_json::from_value::<TaskIntent>(initial).is_err(),
                "{binding}"
            );
            let mut followup = update();
            followup["observation_binding"] = binding.clone();
            assert!(
                serde_json::from_value::<TaskUpdateIntent>(followup).is_err(),
                "{binding}"
            );
        }
    }

    #[test]
    fn in_memory_invalid_bindings_cannot_pass_task_validation() {
        let binding: DocumentObservationBinding =
            serde_json::from_value(observation("flow")).unwrap();
        let initial: TaskIntent = serde_json::from_value(create()).unwrap();
        let followup: TaskUpdateIntent = serde_json::from_value(update()).unwrap();
        let mut invalid = vec![];
        let mut wrong_session = binding.clone();
        wrong_session.flow.as_mut().unwrap().session_id = "different-doc".into();
        invalid.push(wrong_session);
        let mut wrong_kind = binding.clone();
        wrong_kind.kind = "canvas".into();
        invalid.push(wrong_kind);
        let mut stale_number = binding;
        stale_number.selection_revision = MAX_SAFE_INTEGER + 1;
        invalid.push(stale_number);
        for binding in invalid {
            let mut task = initial.clone();
            task.initial_observation_binding = Some(binding.clone());
            assert!(!task.valid());
            let mut intent = followup.clone();
            intent.observation_binding = Some(binding);
            assert!(!intent.valid());
        }
    }

    #[test]
    fn historical_shape_validation_does_not_claim_current_document_identity() {
        let binding: DocumentObservationBinding =
            serde_json::from_value(observation("canvas")).unwrap();
        let current = crate::agent_identity::DocumentIdentity {
            instance_id: "today-instance".into(),
            project_id: "today-project".into(),
            document_session_id: "today-doc".into(),
        };
        assert!(binding.valid());
        assert!(!binding.matches_identity(&current, "today-client"));
        let mut task: TaskIntent = serde_json::from_value(create()).unwrap();
        task.initial_observation_binding = Some(binding.clone());
        let mut intent: TaskUpdateIntent = serde_json::from_value(update()).unwrap();
        intent.observation_binding = Some(binding);
        assert!(task.valid() && intent.valid());
    }

    #[test]
    fn task_identity_budget_and_update_revision_limits_remain_enforced() {
        let mut initial = create();
        initial["initial_observation_binding"] = observation("canvas");
        for (key, value) in [
            ("client_request_id", json!("different-human")),
            ("max_task_credits", json!(0)),
            ("max_task_credits", json!(MAX_SAFE_INTEGER + 1)),
            ("goal", json!("\u{0000}")),
        ] {
            let mut changed = initial.clone();
            changed[key] = value;
            assert!(
                !serde_json::from_value::<TaskIntent>(changed)
                    .unwrap()
                    .valid()
            );
        }
        let mut followup = update();
        followup["observation_binding"] = observation("flow");
        for (key, value) in [
            ("client_request_id", json!("bad id")),
            ("task_id", json!("task-invalid")),
            ("expected_revision", json!(0)),
            ("expected_revision", json!(MAX_SAFE_INTEGER + 1)),
            ("retries", json!(3)),
            ("message", json!(" ")),
        ] {
            let mut changed = followup.clone();
            changed[key] = value;
            assert!(
                !serde_json::from_value::<TaskUpdateIntent>(changed)
                    .unwrap()
                    .valid()
            );
        }
        assert_eq!(parse_task_budget("100", 20), Ok(100));
        assert!(parse_task_budget("19", 20).is_err());
    }
}
