//! In-memory permission for ordinary steps of one human-started task.
//! It never replaces a native exact-call permit or grants paid generation.
use crate::agent_identity::DocumentIdentity;
use crate::agent_tasks::{TaskStatus, TaskSummary};
use serde_json::Value;

// No Debug or serialization: actual editor lease tokens remain private memory.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GrantScope {
    pub(crate) task_id: String,
    pub(crate) document: DocumentIdentity,
    pub(crate) ipc_generation: u64,
    pub(crate) account_epoch: u64,
    pub(crate) read_lease: String,
    pub(crate) write_lease: String,
}

pub(crate) struct TaskGrant {
    scope: GrantScope,
    epoch: u64,
}

pub(crate) struct UpdateGrant {
    scope: GrantScope,
    update_id: String,
    epoch: u64,
    previous_turn: Option<String>,
}

fn ordinary_tool(tool: &str, args: &Value) -> bool {
    if !args.is_object() {
        return false;
    }
    match tool {
        "canvas_edit" => matches!(
            args["edit"]["kind"].as_str(),
            Some(
                "layerSelect"
                    | "layerAdd"
                    | "layerDelete"
                    | "layerMove"
                    | "groupAdd"
                    | "layerMoveInto"
                    | "groupSetExpanded"
                    | "layerSetVisibility"
                    | "layerSetOpacity"
                    | "selectionRect"
                    | "selectionAll"
                    | "selectionClear"
                    | "selectionFill"
                    | "selectionDelete"
                    | "maskAdd"
                    | "maskRemove"
                    | "maskSetEnabled"
                    | "maskSelect"
                    | "brushSet"
                    | "brushStroke"
                    | "adjustmentAdd"
                    | "adjustmentSetParameter"
                    | "curveSet"
                    | "curveAdd"
                    | "curveRemove"
            )
        ),
        "clip_edit" => matches!(
            args["edit"]["kind"].as_str(),
            Some(
                "placeMedia"
                    | "addTrack"
                    | "removeTrack"
                    | "setTrackFlag"
                    | "addTimeline"
                    | "removeTimeline"
                    | "renameTimeline"
                    | "selectTimeline"
                    | "moveTimeline"
                    | "setTimelineVideo"
                    | "move"
                    | "trim"
                    | "split"
                    | "delete"
                    | "merge"
                    | "freezeImage"
                    | "setSpeed"
                    | "setSpeedCurve"
                    | "transform"
                    | "update"
                    | "setAudioStream"
                    | "detachAudio"
                    | "reattachAudio"
                    | "addText"
                    | "setText"
                    | "setKey"
                    | "clearKey"
                    | "clearKeys"
                    | "importLibrary"
            )
        ),
        "move_selected_image"
        | "transform_canvas_layer"
        | "library_images"
        | "library_assets"
        | "insert_library_image"
        | "save_canvas_project"
        | "export_canvas_png"
        | "canvas_operation"
        | "clip_operation" => true,
        _ => false,
    }
}

impl TaskGrant {
    /// Called only after the host accepts an explicit human create/resume.
    pub(crate) fn from_human(scope: GrantScope, expected_epoch: u64) -> Option<Self> {
        if scope.task_id.is_empty()
            || scope.ipc_generation == 0
            || scope.read_lease.is_empty()
            || scope.write_lease.is_empty()
            || expected_epoch == 0
        {
            return None;
        }
        Some(Self {
            scope,
            epoch: expected_epoch,
        })
    }

    pub(crate) fn remains_bound(&self, scope: &GrantScope, summary: Option<&TaskSummary>) -> bool {
        self.scope == *scope
            && summary.is_none_or(|task| {
                task.id == self.scope.task_id
                    && task.control_epoch <= self.epoch
                    && (task.control_epoch < self.epoch
                        || !matches!(
                            task.status,
                            TaskStatus::Paused
                                | TaskStatus::Stopped
                                | TaskStatus::Completed
                                | TaskStatus::Failed
                        ))
            })
    }

    pub(crate) fn allows(
        &self,
        scope: &GrantScope,
        current: &TaskSummary,
        admitted_turn: &TaskSummary,
        actual_turn_id: &str,
        tool: &str,
        args: &Value,
        blocked: bool,
    ) -> bool {
        !blocked
            && self.scope == *scope
            && args.get("projectId").and_then(Value::as_str)
                == Some(scope.document.project_id.as_str())
            && args.get("documentSessionId").and_then(Value::as_str)
                == Some(scope.document.document_session_id.as_str())
            && current.id == scope.task_id
            && current.status == TaskStatus::Running
            && current.control_epoch == self.epoch
            && admitted_turn.id == current.id
            && admitted_turn.control_epoch == current.control_epoch
            && admitted_turn.active_turn_id.as_deref() == Some(actual_turn_id)
            && current.active_turn_id.as_deref() == Some(actual_turn_id)
            && admitted_turn.active_input_id.is_some()
            && admitted_turn.active_input_id == current.active_input_id
            && ordinary_tool(tool, args)
    }

    /// A human update withdraws this grant immediately. Only its exact next
    /// admitted turn can consume the candidate, with acknowledgement already
    /// present at turn admission; a later projection cannot restore it.
    pub(crate) fn for_update(
        self,
        update_id: String,
        previous_turn: Option<String>,
    ) -> Option<UpdateGrant> {
        if update_id.is_empty() {
            return None;
        }
        Some(UpdateGrant {
            scope: self.scope,
            update_id,
            epoch: self.epoch.checked_add(1)?,
            previous_turn,
        })
    }
}

impl UpdateGrant {
    pub(crate) fn remains_bound(&self, scope: &GrantScope, summary: Option<&TaskSummary>) -> bool {
        self.scope == *scope
            && summary.is_none_or(|task| {
                task.id == self.scope.task_id
                    && task.control_epoch <= self.epoch
                    && !matches!(
                        task.status,
                        TaskStatus::Paused
                            | TaskStatus::Stopped
                            | TaskStatus::Completed
                            | TaskStatus::Failed
                    )
            })
    }

    /// The caller takes this value once, when a different real task turn starts.
    pub(crate) fn next_turn(
        self,
        scope: &GrantScope,
        admitted: &TaskSummary,
        actual_turn_id: &str,
        previous_ended: bool,
        blocked: bool,
    ) -> Option<TaskGrant> {
        if blocked
            || self.scope != *scope
            || !previous_ended
            || self.previous_turn.as_deref() == Some(actual_turn_id)
            || admitted.id != scope.task_id
            || admitted.status != TaskStatus::Running
            || admitted.control_epoch != self.epoch
            || admitted.last_update_id.as_deref() != Some(self.update_id.as_str())
            || admitted.active_turn_id.as_deref() != Some(actual_turn_id)
            || admitted.active_input_id.is_none()
        {
            return None;
        }
        TaskGrant::from_human(self.scope, self.epoch)
    }

    pub(crate) fn expected_epoch(&self) -> u64 {
        self.epoch
    }
    pub(crate) fn previous_turn(&self) -> Option<&str> {
        self.previous_turn.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn scoped(mut value: Value) -> Value {
        let object = value.as_object_mut().unwrap();
        object
            .entry("projectId")
            .or_insert(json!(scope().document.project_id));
        object
            .entry("documentSessionId")
            .or_insert(json!(scope().document.document_session_id));
        value
    }
    fn scope() -> GrantScope {
        GrantScope {
            task_id: crate::agent_tasks::task_id_for("request-1"),
            document: DocumentIdentity {
                instance_id: "instance-1".into(),
                project_id: "project-1".into(),
                document_session_id: "document-1".into(),
            },
            ipc_generation: 1,
            account_epoch: 2,
            read_lease: "fixture-read".into(),
            write_lease: "fixture-write".into(),
        }
    }
    fn summary(epoch: u64, turn: &str, update: Option<&str>) -> TaskSummary {
        serde_json::from_value(json!({"id":scope().task_id,"sessionId":"session-1","taskRevision":4,"stateRevision":5,"controlEpoch":epoch,"status":"running","blockedReason":null,"rounds":2,"activeInputId":format!("input-{turn}"),"activeTurnId":turn,"roundOrdinal":2,"verifiedSteps":0,"totalSteps":1,"evidenceCount":0,"lastUpdateId":update})).unwrap()
    }
    #[test]
    fn task_grant_uses_closed_tools_and_never_authorizes_history_flow_or_generation() {
        let scope = scope();
        let task = summary(1, "turn-1", None);
        let grant = TaskGrant::from_human(scope.clone(), 1).unwrap();
        for tool in [
            "canvas_edit",
            "clip_edit",
            "move_selected_image",
            "transform_canvas_layer",
            "library_images",
            "library_assets",
            "insert_library_image",
            "save_canvas_project",
            "export_canvas_png",
        ] {
            let kind = if tool == "canvas_edit" {
                "layerSetOpacity"
            } else {
                "transform"
            };
            assert!(
                grant.allows(
                    &scope,
                    &task,
                    &task,
                    "turn-1",
                    tool,
                    &scoped(json!({"edit":{"kind":kind}})),
                    false
                ),
                "{tool}"
            );
        }
        for tool in [
            "flow_execute_node",
            "flow_add_node",
            "retry_canvas_registration",
            "task_checkpoint",
            "generate_image",
            "unknown",
        ] {
            assert!(
                !grant.allows(
                    &scope,
                    &task,
                    &task,
                    "turn-1",
                    tool,
                    &scoped(json!({})),
                    false
                ),
                "{tool}"
            );
        }
        for tool in ["canvas_edit", "clip_edit"] {
            for kind in ["undo", "redo", "futureEdit"] {
                assert!(!grant.allows(
                    &scope,
                    &task,
                    &task,
                    "turn-1",
                    tool,
                    &scoped(json!({"edit":{"kind":kind}})),
                    false
                ));
            }
        }
        assert!(!grant.allows(
            &scope,
            &task,
            &task,
            "turn-1",
            "canvas_edit",
            &scoped(json!({"projectId":"other","edit":{"kind":"layerAdd"}})),
            false
        ));
        assert!(!grant.allows(
            &scope,
            &task,
            &task,
            "turn-1",
            "canvas_edit",
            &scoped(json!({"edit":{"kind":"layerAdd"}})),
            true
        ));
    }
    #[test]
    fn task_asset_import_is_an_explicit_clip_action_and_other_new_kinds_stay_manual() {
        let scope = scope();
        let task = summary(1, "turn-1", None);
        let grant = TaskGrant::from_human(scope.clone(), 1).unwrap();
        assert!(grant.allows(
            &scope,
            &task,
            &task,
            "turn-1",
            "clip_edit",
            &scoped(json!({"edit":{"kind":"importLibrary"}})),
            false
        ));
        assert!(grant.allows(
            &scope,
            &task,
            &task,
            "turn-1",
            "library_assets",
            &scoped(json!({"projectId":"project-1","documentSessionId":"document-1"})),
            false
        ));
        assert!(!grant.allows(
            &scope,
            &task,
            &task,
            "turn-1",
            "canvas_edit",
            &scoped(json!({"edit":{"kind":"importLibrary"}})),
            false
        ));
        assert!(!grant.allows(
            &scope,
            &task,
            &task,
            "turn-1",
            "clip_edit",
            &scoped(json!({"edit":{"kind":"importArbitraryPath"}})),
            false
        ));
    }
    #[test]
    fn task_original_receipt_query_requires_same_document_running_turn_and_live_human_grant() {
        let scope = scope();
        let task = summary(1, "turn-1", None);
        let grant = TaskGrant::from_human(scope.clone(), 1).unwrap();
        for tool in ["canvas_operation", "clip_operation"] {
            let original = scoped(
                json!({"operationId":"scm2:1:16:seecut-assistant:10:document-1:1","clientSequence":1}),
            );
            // The native named permit separately verifies this exact original
            // operation against its client/scope/family receipt entry.
            assert!(grant.allows(&scope, &task, &task, "turn-1", tool, &original, false));
            let mut wrong = original.clone();
            wrong["documentSessionId"] = json!("other");
            assert!(!grant.allows(&scope, &task, &task, "turn-1", tool, &wrong, false));
            let missing = json!({"operationId":original["operationId"],"clientSequence":1});
            assert!(!grant.allows(&scope, &task, &task, "turn-1", tool, &missing, false));
            let mut paused = task.clone();
            paused.status = TaskStatus::Paused;
            assert!(!grant.allows(&scope, &paused, &task, "turn-1", tool, &original, false));
            assert!(!grant.allows(&scope, &task, &task, "turn-1", tool, &original, true));
        }
    }
    #[test]
    fn task_grant_refuses_every_changed_scope_epoch_and_turn_binding() {
        let original = scope();
        let task = summary(1, "turn-1", None);
        let grant = TaskGrant::from_human(original.clone(), 1).unwrap();
        let mut variants = Vec::new();
        let mut changed = original.clone();
        changed.ipc_generation += 1;
        variants.push(changed);
        let mut changed = original.clone();
        changed.account_epoch += 1;
        variants.push(changed);
        let mut changed = original.clone();
        changed.document.project_id = "other".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.document.document_session_id = "other".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.document.instance_id = "other".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.read_lease = "other".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.write_lease = "other".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.task_id = "other".into();
        variants.push(changed);
        for changed in variants {
            assert!(!grant.remains_bound(&changed, Some(&task)));
            assert!(!grant.allows(
                &changed,
                &task,
                &task,
                "turn-1",
                "library_images",
                &scoped(json!({})),
                false
            ));
        }
        let newer = summary(2, "turn-1", None);
        assert!(!grant.remains_bound(&original, Some(&newer)));
        assert!(!grant.allows(
            &original,
            &newer,
            &task,
            "turn-1",
            "library_images",
            &scoped(json!({})),
            false
        ));
        assert!(!grant.allows(
            &original,
            &task,
            &summary(1, "other-turn", None),
            "turn-1",
            "library_images",
            &scoped(json!({})),
            false
        ));
    }
    #[test]
    fn task_grant_update_requires_confirmed_next_turn_and_never_uses_a_late_projection() {
        let original = scope();
        let candidate = || {
            TaskGrant::from_human(original.clone(), 1)
                .unwrap()
                .for_update("update-1".into(), Some("turn-1".into()))
                .unwrap()
        };
        let admitted = summary(2, "turn-2", Some("update-1"));
        assert!(
            candidate()
                .next_turn(&original, &admitted, "turn-2", true, false)
                .is_some()
        );
        assert!(
            candidate()
                .next_turn(&original, &admitted, "turn-2", false, false)
                .is_none()
        );
        assert!(
            candidate()
                .next_turn(&original, &admitted, "turn-2", true, true)
                .is_none()
        );
        assert!(
            candidate()
                .next_turn(
                    &original,
                    &summary(3, "turn-2", Some("update-1")),
                    "turn-2",
                    true,
                    false
                )
                .is_none()
        );
        assert!(
            candidate()
                .next_turn(
                    &original,
                    &summary(2, "turn-2", Some("other-update")),
                    "turn-2",
                    true,
                    false
                )
                .is_none()
        );
        let at_start = summary(2, "turn-2", None);
        let later_confirmed = summary(2, "turn-2", Some("update-1"));
        assert_eq!(later_confirmed.last_update_id.as_deref(), Some("update-1"));
        assert!(
            candidate()
                .next_turn(&original, &at_start, "turn-2", true, false)
                .is_none()
        );
        let mut reconnected = original.clone();
        reconnected.ipc_generation += 1;
        assert!(
            candidate()
                .next_turn(&reconnected, &admitted, "turn-2", true, false)
                .is_none()
        );
    }
    #[test]
    fn task_grant_human_resume_waits_for_its_new_epoch_without_reviving_old_turn() {
        let original = scope();
        let grant = TaskGrant::from_human(original.clone(), 2).unwrap();
        let mut paused = summary(1, "turn-1", None);
        paused.status = TaskStatus::Paused;
        assert!(grant.remains_bound(&original, Some(&paused)));
        assert!(!grant.allows(
            &original,
            &paused,
            &paused,
            "turn-1",
            "library_images",
            &scoped(json!({})),
            false
        ));
        let next = summary(2, "turn-2", None);
        assert!(grant.allows(
            &original,
            &next,
            &next,
            "turn-2",
            "library_images",
            &scoped(json!({})),
            false
        ));
        assert!(!grant.allows(
            &original,
            &next,
            &paused,
            "turn-1",
            "library_images",
            &scoped(json!({})),
            false
        ));
    }
}
