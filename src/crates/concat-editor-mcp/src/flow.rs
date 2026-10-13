//! Bounded Flow requests. The App owns graph, account and execution authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Exact scoped Flow request, shared by native IPC and the stdio adapter.
pub struct FlowParameters {
    /// Stable graph identity.
    pub graph_id: String,
    /// Opaque hash of the current account/service, returned by context.
    pub account_scope: String,
    /// Selection epoch from the current context.
    pub selection_revision: u64,
    /// Read: schema/graph/models/runs/results/operation. Write: edit/run/adopt/pause.
    pub action: String,
    #[serde(default)]
    /// Bounded typed command or action-specific arguments.
    pub payload: Value,
    #[serde(default)]
    /// Original host-allocated write identity, or queried receipt identity.
    pub operation_id: Option<String>,
    #[serde(default)]
    /// Monotonic original write sequence.
    pub client_sequence: Option<u64>,
    #[serde(default)]
    /// First item in a bounded result page.
    pub offset: usize,
    #[serde(default = "default_limit")]
    /// Maximum items in a result page, at most fifty.
    pub limit: usize,
}
fn default_limit() -> usize {
    5
}
impl FlowParameters {
    /// Freeze business arguments before the runtime allocates write identity.
    pub fn intent(&self) -> Self {
        let mut value = self.clone();
        if matches!(self.action.as_str(), "edit" | "run" | "adopt" | "pause") {
            value.operation_id = None;
            value.client_sequence = None;
        }
        value
    }
}
pub fn validate(request: &super::Request) -> Result<(), String> {
    use super::Method;
    let p = request
        .flow
        .as_ref()
        .ok_or("invalidInput: Flow parameters")?;
    let id = |s: &str| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_graphic());
    if request.project_id.as_deref().is_none_or(|s| !id(s))
        || request
            .document_session_id
            .as_deref()
            .is_none_or(|s| !id(s))
        || !id(&p.graph_id)
        || p.account_scope.len() != 64
        || !p.account_scope.bytes().all(|b| b.is_ascii_hexdigit())
        || p.offset > 1_000_000
        || p.limit == 0
        || p.limit > 50
        || serde_json::to_vec(&p.payload)
            .map_err(|_| "invalidInput")?
            .len()
            > 32_768
        || request.move_parameters.is_some()
        || request.canvas_parameters.is_some()
        || request.library_query.is_some()
        || request.operation_query.is_some()
        || request.object_id.is_some()
        || request.context_revision.is_some()
        || request.offset.is_some()
        || request.limit.is_some()
        || request.max_edge.is_some()
    {
        return Err("invalidInput: Flow fields".into());
    }
    match request.method {
        Method::FlowRead
            if matches!(
                p.action.as_str(),
                "schema" | "graph" | "models" | "runs" | "results" | "operation"
            ) =>
        {
            if p.action == "operation" {
                if p.operation_id.as_ref()
                    != Some(&super::operation_id(
                        &request.client_id,
                        request.document_session_id.as_deref().unwrap(),
                        p.client_sequence.unwrap_or(0),
                    )?)
                {
                    return Err("invalidInput: Flow receipt".into());
                }
            } else if p.operation_id.is_some()
                || p.client_sequence.is_some()
                || request.revision.is_none()
            {
                return Err("invalidInput: Flow read".into());
            }
        }
        Method::FlowWrite if matches!(p.action.as_str(), "edit" | "run" | "adopt" | "pause") => {
            if request.revision.is_none()
                || p.operation_id.as_ref()
                    != Some(&super::operation_id(
                        &request.client_id,
                        request.document_session_id.as_deref().unwrap(),
                        p.client_sequence.unwrap_or(0),
                    )?)
            {
                return Err("invalidInput: Flow operation".into());
            }
        }
        _ => return Err("invalidInput: Flow action".into()),
    }
    Ok(())
}
