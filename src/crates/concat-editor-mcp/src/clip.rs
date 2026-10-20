//! Original operation identity and frozen timeline edit context.
use crate::{ClipEdit, operation_id};
use serde::{Deserialize, Serialize};

/// One exact timeline edit approved before its operation identity is allocated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipAction {
    /// Canonical current timeline identity; never an implicit active-tab lookup.
    pub timeline_id: String,
    /// Complete native edit state captured before approval.
    pub expected_edit_state_id: String,
    /// Closed local edit and explicit stable targets.
    pub edit: ClipEdit,
}

/// A durably allocated original operation, using the existing write registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipParameters {
    /// Canonical authenticated client/session/sequence identity.
    pub operation_id: String,
    /// Nonzero original sequence, never reassigned to recover an unknown edit.
    pub client_sequence: u64,
    /// Exact native selection epoch before the edit.
    pub selection_revision: u64,
    /// Exact timeline, edit state and semantic action.
    pub action: ClipAction,
}

/// Validate bounded syntax before native permission, ownership and state checks.
pub fn validate_clip_parameters(
    client: &str,
    session: &str,
    p: &ClipParameters,
) -> Result<(), String> {
    if p.operation_id != operation_id(client, session, p.client_sequence)? {
        return Err("invalidInput: operation ID mismatch".into());
    }
    clip_edit::clip_native_id(&p.action.timeline_id, "timeline").map_err(str::to_owned)?;
    let hash = p
        .action
        .expected_edit_state_id
        .strip_prefix("clip-edit-v1-")
        .unwrap_or_default();
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("invalidInput: clip edit state".into());
    }
    p.action.edit.validate().map_err(str::to_owned)?;
    if matches!(p.action.edit, ClipEdit::Save {})
        && (p.client_sequence > 9_007_199_254_740_991
            || p.selection_revision > 9_007_199_254_740_991)
    {
        return Err("invalidInput: unsafe save count".into());
    }
    if serde_json::to_vec(&p.action.edit)
        .map_err(|_| "invalidInput")?
        .len()
        > 16 * 1024
    {
        return Err("invalidInput: clip edit bytes".into());
    }
    Ok(())
}

use crate::clip_edit;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Method, Request, validate_request_shape};
    use serde_json::json;
    #[test]
    fn clip_contract_binds_original_operation_stamp_and_closed_method() {
        let mut r:Request=serde_json::from_value(json!({"instanceId":"i","clientId":"a","clientToken":"t","method":"clipEdit","projectId":"p","documentSessionId":"s","revision":1})).unwrap();
        r.clip_parameters = Some(ClipParameters {
            operation_id: operation_id("a", "s", 1).unwrap(),
            client_sequence: 1,
            selection_revision: 2,
            action: ClipAction {
                timeline_id: "timeline:TL1".into(),
                expected_edit_state_id: format!("clip-edit-v1-{}", "a".repeat(64)),
                edit: ClipEdit::AddTrack,
            },
        });
        assert!(validate_request_shape(&r).is_ok());
        r.method = Method::CanvasEdit;
        assert!(validate_request_shape(&r).is_err());
        r.method = Method::ClipEdit;
        r.clip_parameters
            .as_mut()
            .unwrap()
            .action
            .expected_edit_state_id = "model-supplied".into();
        assert!(validate_request_shape(&r).is_err());
    }
    #[test]
    fn clip_query_and_atomic_target_lookup_have_no_write_or_paging_alias() {
        let mut r:Request=serde_json::from_value(json!({"instanceId":"i","clientId":"a","clientToken":"t","method":"clipOperation","projectId":"p","documentSessionId":"s","operationQuery":{"operationId":operation_id("a","s",1).unwrap(),"clientSequence":1}})).unwrap();
        assert!(validate_request_shape(&r).is_ok());
        r.revision = Some(1);
        assert!(validate_request_shape(&r).is_err());
        r.revision = None;
        r.operation_query = None;
        r.method = Method::Project;
        r.object_ids = Some(vec!["clip:c1".into(), "track:T1".into()]);
        assert!(validate_request_shape(&r).is_ok());
        r.offset = Some(0);
        assert!(validate_request_shape(&r).is_err());
        r.offset = None;
        r.object_ids.as_mut().unwrap().push("clip:c1".into());
        assert!(validate_request_shape(&r).is_err());
    }
}
