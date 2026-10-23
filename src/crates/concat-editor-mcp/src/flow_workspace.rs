//! The embedded Flow protocol. Retired FlowRead/FlowWrite remain unavailable.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Frozen embedded workspace identity and one named operation.
pub struct FlowWorkspace {
    /// Persistent native project UUID.
    pub project_id: String,
    /// Actual active canvas identity.
    pub canvas_id: String,
    /// Identity of this opening of the canvas.
    pub session_id: String,
    /// Native account scope and login epoch, without credentials.
    pub account_scope: String,
    /// Observed canvas revision before dispatch.
    pub revision: u64,
    #[serde(default)]
    /// Named tool selected by the MCP router.
    pub name: String,
    #[serde(default)]
    /// Bounded arguments validated by the actual canvas tool.
    pub args: Value,
    #[serde(default)]
    /// Durable original-operation identity for writes and queries.
    pub operation_id: Option<String>,
}

impl FlowWorkspace {
    /// Whether the operation only observes current state or a saved receipt.
    pub fn is_read(&self) -> bool {
        matches!(
            self.name.as_str(),
            "flow_get_workspace"
                | "flow_read_canvas"
                | "flow_list_node_types"
                | "flow_get_node_tools"
                | "flow_get_operation"
                | "flow_get_node_config"
                | "flow_get_task"
        )
    }
    /// Reject unknown tools, invalid identities and unbounded arguments.
    pub fn valid(&self) -> bool {
        [
            self.project_id.as_str(),
            self.canvas_id.as_str(),
            self.session_id.as_str(),
            self.account_scope.as_str(),
        ]
        .iter()
        .all(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
            && (self.is_read()
                || matches!(
                    self.name.as_str(),
                    "flow_add_node"
                        | "flow_update_node"
                        | "flow_connect_nodes"
                        | "flow_delete_nodes"
                        | "flow_switch_canvas"
                        | "flow_execute_node"
                ))
            && self.args.is_object()
            && self.args.to_string().len() <= 32768
            && ((self.is_read() && self.name != "flow_get_operation")
                || self
                    .operation_id
                    .as_ref()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 192))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(name: &str) -> FlowWorkspace {
        FlowWorkspace {
            project_id: "project".into(),
            canvas_id: "canvas".into(),
            session_id: "opening".into(),
            account_scope: "account:1".into(),
            revision: 4,
            name: name.into(),
            args: serde_json::json!({}),
            operation_id: None,
        }
    }
    #[test]
    fn only_reads_can_omit_original_operation_identity() {
        assert!(call("flow_get_node_config").valid());
        assert!(call("flow_get_task").valid());
        assert!(!call("flow_execute_node").valid());
        assert!(!call("flow_get_operation").valid());
        let mut query = call("flow_get_operation");
        query.operation_id = Some("original".into());
        assert!(query.valid());
        assert!(!call("flow_write").valid());
    }
    #[test]
    fn rejects_unbounded_or_unrecognized_call_fields() {
        let mut value = serde_json::to_value(call("flow_read_canvas")).unwrap();
        value["writeToken"] = serde_json::json!("unexpected");
        assert!(serde_json::from_value::<FlowWorkspace>(value).is_err());
        let mut oversized = call("flow_read_canvas");
        oversized.args = serde_json::json!({"text":"x".repeat(32769)});
        assert!(!oversized.valid());
    }
}
