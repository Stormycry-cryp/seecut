// SPDX-License-Identifier: AGPL-3.0-or-later
//! Closed Photo request syntax only. No registration, dispatch or authorization.
//! The parent must map opaque handles to exact native u64 bindings and retain
//! original typed parameters under the existing account/Doc/Task write registry.

use crate::{Method, Request, operation_id};
use serde::{Deserialize, Serialize};

/// Highest parent operation sequence that survives the existing JS boundary exactly.
pub const PHOTO_MAX_SAFE_SEQUENCE: u64 = 9_007_199_254_740_991;

fn canonical_handle(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
macro_rules! opaque_handle {
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $pattern:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
        #[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
        #[serde(transparent)]
        pub struct $name(
            #[cfg_attr(feature = "stdio", schemars(pattern($pattern)))]
            String,
        );
        impl $name {
            /// Parse only this canonical handle kind; this does not resolve or authorize it.
            pub fn parse(value: &str) -> Result<Self, &'static str> {
                if canonical_handle(value, $prefix) {
                    Ok(Self(value.to_owned()))
                } else {
                    Err("invalidInput: Photo opaque handle")
                }
            }
            /// Exact opaque text, never a native integer or filesystem path.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::parse(&text).map_err(serde::de::Error::custom)
            }
        }
    };
}
opaque_handle!(
    /// Opaque parent mapping of an owned native document opening, never a title or DocId.
    PhotoDocumentId, "photo-document-v1-", "^photo-document-v1-[0-9a-f]{64}$"
);
opaque_handle!(
    /// Opaque parent mapping of a layer under its exact native document binding.
    PhotoLayerId, "photo-layer-v1-", "^photo-layer-v1-[0-9a-f]{64}$"
);
opaque_handle!(
    /// Reference to the parent's retained original read, including proven selected-layer
    /// and pixel-selection facts. Never a revision hash or a current-target relookup;
    /// full native view proof belongs to snapshot/validation, not this handle.
    PhotoStateId, "photo-state-v1-", "^photo-state-v1-[0-9a-f]{64}$"
);
opaque_handle!(
    /// Opaque parent mapping of an original historical adopted snapshot.
    PhotoSnapshotId, "photo-snapshot-v1-", "^photo-snapshot-v1-[0-9a-f]{64}$"
);

/// Exact original read binding, resolved from the parent's stored native facts.
/// Parsing establishes neither existence nor authorization. The parent must compare
/// original selection facts too; matching document/revision cannot replace them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhotoExpected {
    /// Original opaque document handle, independent of Doc grant identity.
    pub document_id: PhotoDocumentId,
    /// Stored original read and selection binding, never a numeric native revision.
    pub state_id: PhotoStateId,
}

/// The only Photo mutations in this protocol version; no arbitrary engine command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PhotoAction {
    /// Change the explicitly mapped layer's opacity only.
    LayerSetOpacity {
        /// Opaque parent layer handle; never a native number or selected-layer alias.
        layer_id: PhotoLayerId,
        /// Finite inclusive zero-to-one opacity.
        #[cfg_attr(feature = "stdio", schemars(range(min = 0, max = 1)))]
        opacity: f64,
    },
    /// One independent undo, under its own exact-call approval and original operation.
    Undo {},
}
impl PhotoAction {
    /// Validate closed scalar syntax only, without native ownership or permission checks.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::LayerSetOpacity { opacity, .. }
                if !opacity.is_finite() || !(0.0..=1.0).contains(opacity) =>
            {
                Err("invalidInput: Photo opacity".into())
            }
            _ => Ok(()),
        }
    }
}

/// Original parent operation and complete exact-call payload, retained by PartialEq.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhotoParameters {
    /// Existing scm2 identity allocated by the trusted parent, never a new child ID.
    pub operation_id: String,
    /// Original nonzero JS-safe parent sequence, never a native document counter.
    #[cfg_attr(
        feature = "stdio",
        schemars(range(min = 1, max = 9_007_199_254_740_991u64))
    )]
    pub client_sequence: u64,
    /// Exact opaque document/state observed before approval.
    pub expected: PhotoExpected,
    /// Complete closed semantic action.
    pub action: PhotoAction,
}

/// One closed historical snapshot request. The future parent must compare actual
/// capture identity and selection with the retained original expected read. This
/// syntax does not prove native view, pixels, currentness, existence or permission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhotoSnapshotQuery {
    /// Exact original document and state binding, not implicit active-tab lookup.
    pub expected: PhotoExpected,
    /// Maximum original composite edge, with no upscaling promised by this syntax.
    #[cfg_attr(feature = "stdio", schemars(range(min = 1, max = 256)))]
    pub max_edge: u32,
}
impl PhotoSnapshotQuery {
    /// Check the explicit bound; no pixels, currentness or read grant is established.
    pub fn validate(&self) -> Result<(), String> {
        if (1..=256).contains(&self.max_edge) {
            Ok(())
        } else {
            Err("invalidInput: Photo snapshot edge".into())
        }
    }
}

/// Internal read-only check of one stored historical snapshot. Resolving it must
/// use the parent's original mapping and native validator under the existing
/// read/account/Task gates. Syntax grants nothing and never requests a new capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhotoSnapshotValidationQuery {
    /// Original retained document/state binding, including proven selection facts.
    pub expected: PhotoExpected,
    /// Exact original parent snapshot handle; never a native ID or a credential.
    pub snapshot_id: PhotoSnapshotId,
}

fn validate_original_operation(
    client: &str,
    session: &str,
    id: &str,
    sequence: u64,
) -> Result<(), String> {
    if !(1..=PHOTO_MAX_SAFE_SEQUENCE).contains(&sequence)
        || id != operation_id(client, session, sequence)?
    {
        return Err("invalidInput: Photo original operation".into());
    }
    Ok(())
}
/// Reuse existing scm2 owner/session rules; never allocate or hash a new identity here.
pub fn validate_photo_parameters(
    client: &str,
    session: &str,
    parameters: &PhotoParameters,
) -> Result<(), String> {
    validate_original_operation(
        client,
        session,
        &parameters.operation_id,
        parameters.client_sequence,
    )?;
    parameters.action.validate()
}

pub(crate) fn is_photo_method(method: Method) -> bool {
    matches!(
        method,
        Method::PhotoState
            | Method::PhotoSnapshot
            | Method::PhotoSnapshotValidate
            | Method::PhotoEdit
            | Method::PhotoOperation
    )
}
pub(crate) fn validate_request(r: &Request) -> Result<(), String> {
    // Run before every legacy family: no Photo payload may hide inside an early
    // App/Clip/Canvas/Flow return, and no foreign field may accompany a Photo call.
    if !is_photo_method(r.method)
        || (r.method != Method::PhotoSnapshotValidate
            && r.photo_snapshot_validation_query.is_some())
        || r.project_id.as_deref().is_none_or(str::is_empty)
        || r.document_session_id
            .as_deref()
            .is_none_or(|s| s.is_empty() || s.len() > 64)
        || r.client_id.is_empty()
        || r.client_id.len() > 64
        || r.app_scope.is_some()
        || r.app_page.is_some()
        || r.app_control_token.is_some()
        || r.app_navigate.is_some()
        || r.app_operation_query.is_some()
        || r.revision.is_some()
        || r.context_revision.is_some()
        || r.offset.is_some()
        || r.limit.is_some()
        || r.max_edge.is_some()
        || r.move_parameters.is_some()
        || r.canvas_parameters.is_some()
        || r.clip_parameters.is_some()
        || r.object_id.is_some()
        || r.object_ids.is_some()
        || r.read_source.is_some()
        || r.library_query.is_some()
        || r.library_assets_query.is_some()
        || r.clip_export_control_action.is_some()
        || r.clip_preview_query.is_some()
        || r.flow.is_some()
        || r.flow_workspace.is_some()
    {
        return Err("invalidInput: Photo request fields".into());
    }
    let session = r
        .document_session_id
        .as_deref()
        .ok_or("invalidInput: Photo Doc scope")?;
    match r.method {
        Method::PhotoSnapshotValidate
            if r.photo_parameters.is_none()
                && r.photo_snapshot_query.is_none()
                && r.operation_query.is_none()
                && r.photo_snapshot_validation_query.is_some() =>
        {
            Ok(())
        }
        Method::PhotoState
            if r.photo_parameters.is_none()
                && r.photo_snapshot_query.is_none()
                && r.operation_query.is_none() =>
        {
            Ok(())
        }
        Method::PhotoSnapshot if r.photo_parameters.is_none() && r.operation_query.is_none() => r
            .photo_snapshot_query
            .as_ref()
            .ok_or("invalidInput: Photo snapshot query")?
            .validate(),
        Method::PhotoEdit if r.photo_snapshot_query.is_none() && r.operation_query.is_none() => {
            validate_photo_parameters(
                &r.client_id,
                session,
                r.photo_parameters
                    .as_ref()
                    .ok_or("invalidInput: Photo parameters")?,
            )
        }
        Method::PhotoOperation
            if r.photo_parameters.is_none() && r.photo_snapshot_query.is_none() =>
        {
            let query = r
                .operation_query
                .as_ref()
                .ok_or("invalidInput: Photo original query")?;
            validate_original_operation(
                &r.client_id,
                session,
                &query.operation_id,
                query.client_sequence,
            )
        }
        _ => Err("invalidInput: unrelated Photo parameters".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate_request_shape;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    fn handle(prefix: &str) -> String {
        format!("{prefix}{}", "a".repeat(64))
    }
    fn expected() -> Value {
        json!({"documentId":handle("photo-document-v1-"),"stateId":handle("photo-state-v1-")})
    }
    fn parameters() -> Value {
        json!({"operationId":operation_id("client","session",1).unwrap(),"clientSequence":1,
            "expected":expected(),"action":{"kind":"layerSetOpacity","layerId":handle("photo-layer-v1-"),"opacity":0.4}})
    }
    fn query() -> Value {
        json!({"expected":expected(),"maxEdge":256})
    }
    fn request(method: Method) -> Value {
        let mut value = json!({"instanceId":"app","clientId":"client","clientToken":"test-only",
            "method":method,"projectId":"project","documentSessionId":"session"});
        match method {
            Method::PhotoEdit => value["photoParameters"] = parameters(),
            Method::PhotoSnapshot => value["photoSnapshotQuery"] = query(),
            Method::PhotoSnapshotValidate => {
                value["photoSnapshotValidationQuery"] = json!({
                "expected":expected(),"snapshotId":handle("photo-snapshot-v1-")})
            }
            Method::PhotoOperation => {
                value["operationQuery"] = json!({
                "operationId":operation_id("client","session",1).unwrap(),"clientSequence":1})
            }
            _ => {}
        }
        value
    }
    fn parsed_valid(value: Value) -> bool {
        serde_json::from_value::<Request>(value)
            .is_ok_and(|request| validate_request_shape(&request).is_ok())
    }
    fn methods() -> [Method; 4] {
        [
            Method::PhotoState,
            Method::PhotoSnapshot,
            Method::PhotoEdit,
            Method::PhotoOperation,
        ]
    }

    #[test]
    fn photo_wire_four_methods_reuse_doc_scope_and_only_edit_is_write() {
        for (method, name) in methods().into_iter().zip([
            "photo_state",
            "photo_snapshot",
            "photo_edit",
            "photo_operation",
        ]) {
            assert_eq!(method.tool_name(), name);
            assert_eq!(method.is_write(), method == Method::PhotoEdit);
            let value = request(method);
            assert!(parsed_valid(value.clone()));
            for field in ["projectId", "documentSessionId"] {
                for invalid in [Value::Null, json!("")] {
                    let mut broken = value.clone();
                    broken[field] = invalid;
                    assert!(!parsed_valid(broken));
                }
            }
            let mut unrelated = value.clone();
            unrelated["appControlToken"] = json!("not-a-doc-credential");
            assert!(!parsed_valid(unrelated));
        }
    }

    #[test]
    fn photo_wire_handles_are_canonical_strings_with_distinct_kinds() {
        fn check<T: DeserializeOwned + Serialize>(prefix: &str) {
            let text = handle(prefix);
            let parsed: T = serde_json::from_value(json!(text)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(text));
            for value in [
                json!(9_007_199_254_740_993u64),
                json!(u64::MAX),
                Value::Null,
                json!(true),
                json!({"id":text}),
                json!(format!("{prefix}{}", "A".repeat(64))),
                json!(format!("{prefix}{}", "a".repeat(63))),
                json!(format!("{prefix}{}", "a".repeat(65))),
                json!(format!("{text} ")),
                json!(format!("{prefix}{}", "０".repeat(64))),
                json!("layer:9007199254740993"),
                json!("/private/document.pcraft"),
            ] {
                assert!(serde_json::from_value::<T>(value).is_err());
            }
        }
        check::<PhotoDocumentId>("photo-document-v1-");
        check::<PhotoLayerId>("photo-layer-v1-");
        check::<PhotoStateId>("photo-state-v1-");
        check::<PhotoSnapshotId>("photo-snapshot-v1-");
        assert!(PhotoLayerId::parse(&handle("photo-document-v1-")).is_err());
        assert!(PhotoStateId::parse(&handle("photo-snapshot-v1-")).is_err());
        assert_eq!(
            PhotoDocumentId::parse(&handle("photo-document-v1-"))
                .unwrap()
                .as_str(),
            handle("photo-document-v1-")
        );
    }

    #[test]
    fn photo_wire_closed_payload_rejects_native_numbers_and_extra_fields() {
        for path in [
            "/photoParameters/expected/documentId",
            "/photoParameters/expected/stateId",
            "/photoParameters/action/layerId",
        ] {
            let mut value = request(Method::PhotoEdit);
            *value.pointer_mut(path).unwrap() = json!(9_007_199_254_740_993u64);
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
        for (path, field) in [
            ("/photoParameters", "payload"),
            ("/photoParameters/expected", "revision"),
            ("/photoParameters/action", "params"),
            ("/photoSnapshotQuery", "path"),
            ("/photoSnapshotQuery/expected", "docId"),
        ] {
            let mut value = request(if path.starts_with("/photoSnapshotQuery") {
                Method::PhotoSnapshot
            } else {
                Method::PhotoEdit
            });
            value
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), json!(1));
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
        for field in [
            "params",
            "path",
            "docId",
            "layerId",
            "generation",
            "selectionRevision",
            "snapshotId",
        ] {
            let mut value = request(Method::PhotoState);
            value[field] = json!(1);
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }

    #[test]
    fn photo_wire_snapshot_max_edge_is_required_bounded_and_not_a_generic_preview() {
        for edge in [1, 256] {
            let mut value = request(Method::PhotoSnapshot);
            value["photoSnapshotQuery"]["maxEdge"] = json!(edge);
            assert!(parsed_valid(value));
        }
        for invalid in [
            json!(0),
            json!(257),
            json!(u32::MAX),
            json!(-1),
            json!(1.5),
            json!("16"),
            Value::Null,
        ] {
            let mut value = request(Method::PhotoSnapshot);
            value["photoSnapshotQuery"]["maxEdge"] = invalid;
            assert!(!parsed_valid(value));
        }
        let mut missing = request(Method::PhotoSnapshot);
        missing["photoSnapshotQuery"]
            .as_object_mut()
            .unwrap()
            .remove("maxEdge");
        assert!(!parsed_valid(missing));
        let mut generic = request(Method::PhotoSnapshot);
        generic["maxEdge"] = json!(16);
        assert!(!parsed_valid(generic));
    }

    #[test]
    fn photo_wire_action_opacity_and_undo_are_closed() {
        for opacity in [0.0, 0.4, 1.0] {
            let mut value = request(Method::PhotoEdit);
            value["photoParameters"]["action"]["opacity"] = json!(opacity);
            assert!(parsed_valid(value));
        }
        for opacity in [-0.1, 1.01, f64::MAX] {
            let mut value = request(Method::PhotoEdit);
            value["photoParameters"]["action"]["opacity"] = json!(opacity);
            assert!(!parsed_valid(value));
        }
        let mut value = request(Method::PhotoEdit);
        value["photoParameters"]["action"] = json!({"kind":"undo"});
        assert!(parsed_valid(value.clone()));
        for field in ["layerId", "opacity", "expected", "params", "command"] {
            let mut extra = value.clone();
            extra["photoParameters"]["action"][field] = json!(1);
            assert!(serde_json::from_value::<Request>(extra).is_err());
        }
        for kind in ["redo", "execute", "layer.setProps", "app.quit"] {
            value["photoParameters"]["action"] = json!({"kind":kind});
            assert!(serde_json::from_value::<Request>(value.clone()).is_err());
        }
    }

    #[test]
    fn photo_wire_nan_and_infinity_never_pass_json_or_typed_validation() {
        for value in ["NaN", "Infinity", "-Infinity", "1e999"] {
            let action = format!(
                "{{\"kind\":\"layerSetOpacity\",\"layerId\":\"{}\",\"opacity\":{value}}}",
                handle("photo-layer-v1-")
            );
            assert!(serde_json::from_str::<PhotoAction>(&action).is_err());
        }
        for opacity in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                PhotoAction::LayerSetOpacity {
                    layer_id: PhotoLayerId::parse(&handle("photo-layer-v1-")).unwrap(),
                    opacity
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn photo_wire_original_operation_owner_sequence_and_exact_id_are_required() {
        for method in [Method::PhotoEdit, Method::PhotoOperation] {
            let field = if method == Method::PhotoEdit {
                "photoParameters"
            } else {
                "operationQuery"
            };
            for sequence in [1, PHOTO_MAX_SAFE_SEQUENCE] {
                let mut value = request(method);
                value[field]["clientSequence"] = json!(sequence);
                value[field]["operationId"] =
                    json!(operation_id("client", "session", sequence).unwrap());
                assert!(parsed_valid(value));
            }
            for sequence in [0, PHOTO_MAX_SAFE_SEQUENCE + 1, u64::MAX] {
                let mut value = request(method);
                value[field]["clientSequence"] = json!(sequence);
                value[field]["operationId"] =
                    json!(operation_id("client", "session", sequence).unwrap_or_default());
                assert!(!parsed_valid(value));
            }
            for (client, session) in [("other", "session"), ("client", "other")] {
                let mut value = request(method);
                value[field]["operationId"] = json!(operation_id(client, session, 1).unwrap());
                assert!(!parsed_valid(value));
            }
            let mut value = request(method);
            value[field]["operationId"] = json!("scm2:1:6:client:7:session:01");
            assert!(!parsed_valid(value));
        }
        let client = "人:c";
        let session = "s:会话";
        let mut value = request(Method::PhotoEdit);
        value["clientId"] = json!(client);
        value["documentSessionId"] = json!(session);
        value["photoParameters"]["operationId"] = json!(operation_id(client, session, 1).unwrap());
        assert!(parsed_valid(value));
    }

    #[test]
    fn photo_wire_operation_query_does_not_accept_a_write_or_new_sequence() {
        let original = request(Method::PhotoOperation);
        for field in [
            "newClientSequence",
            "nextSequence",
            "retry",
            "action",
            "expected",
        ] {
            let mut value = original.clone();
            value["operationQuery"][field] = json!(1);
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
        let mut changed = original.clone();
        changed["operationQuery"]["clientSequence"] = json!(2);
        assert!(!parsed_valid(changed));
        let mut p = original.clone();
        p["photoParameters"] = parameters();
        assert!(!parsed_valid(p));
        let mut p = original;
        p["photoSnapshotQuery"] = query();
        assert!(!parsed_valid(p));
    }

    #[test]
    fn photo_wire_parameters_keep_complete_typed_payload_for_exact_comparison() {
        let a: PhotoParameters = serde_json::from_value(parameters()).unwrap();
        let mut reordered = parameters();
        reordered["action"] =
            json!({"opacity":0.4,"layerId":handle("photo-layer-v1-"),"kind":"layerSetOpacity"});
        let b: PhotoParameters = serde_json::from_value(reordered).unwrap();
        assert_eq!(a, b);
        for pointer in [
            "/expected/documentId",
            "/expected/stateId",
            "/action/layerId",
            "/action/opacity",
            "/clientSequence",
            "/operationId",
        ] {
            let mut value = parameters();
            let target = value.pointer_mut(pointer).unwrap();
            *target = match pointer {
                "/expected/documentId" => json!(format!("photo-document-v1-{}", "b".repeat(64))),
                "/expected/stateId" => json!(format!("photo-state-v1-{}", "b".repeat(64))),
                "/action/layerId" => json!(format!("photo-layer-v1-{}", "b".repeat(64))),
                "/action/opacity" => json!(0.6),
                "/clientSequence" => json!(2),
                _ => json!(operation_id("client", "session", 2).unwrap()),
            };
            let changed: PhotoParameters = serde_json::from_value(value).unwrap();
            assert_ne!(a, changed);
        }
    }

    #[test]
    fn photo_wire_methods_and_payload_families_are_mutually_exclusive() {
        for source in methods() {
            let original = request(source);
            for target in methods() {
                let mut value = original.clone();
                value["method"] = json!(target);
                assert_eq!(parsed_valid(value), source == target);
            }
        }
        for method in [
            Method::Capabilities,
            Method::Context,
            Method::Project,
            Method::Preview,
            Method::ClipPreview,
            Method::AppContext,
            Method::AppNavigate,
            Method::AppOperation,
            Method::PageItems,
            Method::PagePreview,
            Method::LibraryAssets,
            Method::LibraryImages,
            Method::MoveSelectedImage,
            Method::InsertLibraryImage,
            Method::TransformCanvasLayer,
            Method::SaveCanvasProject,
            Method::ExportCanvasPng,
            Method::RetryCanvasRegistration,
            Method::CanvasEdit,
            Method::ClipEdit,
            Method::ClipOperation,
            Method::CanvasOperation,
            Method::ClipExportControl,
            Method::ClipExportOutput,
            Method::FlowRead,
            Method::FlowWrite,
            Method::FlowWorkspace,
        ] {
            for (field, payload) in [
                ("photoParameters", parameters()),
                ("photoSnapshotQuery", query()),
            ] {
                let mut value = request(Method::PhotoState);
                value["method"] = json!(method);
                value[field] = payload;
                let parsed: Request = serde_json::from_value(value).unwrap();
                assert!(validate_request_shape(&parsed).is_err());
            }
        }
    }

    #[test]
    fn photo_wire_rejects_each_existing_foreign_optional_field_before_legacy_routing() {
        let foreign = [
            (
                "appScope",
                json!({"accountScope":"a".repeat(64),"appSessionId":"app-session"}),
            ),
            ("appPage", json!({"appSessionId":"app-session"})),
            ("appControlToken", json!("unrelated")),
            (
                "appNavigate",
                json!({"appSessionId":"app-session","operationId":"id","sessionId":"s","clientSequence":1,
                "before":{"pageId":"generation","pageEpoch":1,"pageStateId":"state"},"target":"projects"}),
            ),
            (
                "appOperationQuery",
                json!({"operationId":"id","sessionId":"s","clientSequence":1}),
            ),
            ("revision", json!(9_007_199_254_740_993u64)),
            ("contextRevision", json!(1)),
            ("offset", json!(0)),
            ("limit", json!(1)),
            ("maxEdge", json!(16)),
            (
                "moveParameters",
                json!({"operationId":"id","clientSequence":1,"selectionRevision":1,"objectId":"layer:1","deltaX":0.0,"deltaY":0.0}),
            ),
            (
                "canvasParameters",
                json!({"operationId":"id","clientSequence":1,"selectionRevision":1,"action":{"kind":"save"}}),
            ),
            (
                "clipParameters",
                json!({"operationId":"id","clientSequence":1,"selectionRevision":1,
                "action":{"timelineId":"timeline:T1","expectedEditStateId":"state","edit":{"kind":"addTrack"}}}),
            ),
            ("objectId", json!("layer:1")),
            ("objectIds", json!(["clip:c1"])),
            ("readSource", json!("persistedClip")),
            ("libraryQuery", json!({})),
            ("libraryAssetsQuery", json!({})),
            ("clipExportControlAction", json!("cancel")),
            ("clipPreviewQuery", json!({"frameId":"frame"})),
            (
                "flow",
                json!({"graphId":"graph","accountScope":"a","selectionRevision":1,"action":"graph"}),
            ),
            (
                "flowWorkspace",
                json!({"projectId":"p","canvasId":"c","sessionId":"s","accountScope":"a","revision":1}),
            ),
        ];
        assert_eq!(foreign.len(), 22);
        for method in methods().into_iter().chain([Method::PhotoSnapshotValidate]) {
            for (field, payload) in &foreign {
                let mut value = request(method);
                value[*field] = payload.clone();
                // Every foreign payload parses as its real legacy type, then the
                // Photo gate rejects it, rather than relying on malformed JSON.
                let parsed: Request = serde_json::from_value(value).unwrap();
                assert!(validate_request_shape(&parsed).is_err(), "field {field}");
            }
        }
    }

    #[cfg(feature = "stdio")]
    #[test]
    fn photo_wire_stdio_schemas_preserve_opaque_strings_and_closed_actions() {
        let document = serde_json::to_value(schemars::schema_for!(PhotoDocumentId)).unwrap();
        assert_eq!(document["type"], "string");
        assert_eq!(document["pattern"], "^photo-document-v1-[0-9a-f]{64}$");
        let parameters = serde_json::to_value(schemars::schema_for!(PhotoParameters)).unwrap();
        assert_eq!(parameters["additionalProperties"], false);
        assert_eq!(
            parameters["properties"]["clientSequence"]["maximum"],
            json!(PHOTO_MAX_SAFE_SEQUENCE)
        );
        let snapshot = serde_json::to_value(schemars::schema_for!(PhotoSnapshotQuery)).unwrap();
        assert_eq!(snapshot["properties"]["maxEdge"]["maximum"], 256);
        let _ = schemars::schema_for!(PhotoAction);
        let _ = schemars::schema_for!(PhotoLayerId);
        let _ = schemars::schema_for!(PhotoStateId);
        let _ = schemars::schema_for!(PhotoSnapshotId);
    }
    #[test]
    fn photo_validation_helper_is_read_only_with_the_existing_doc_scope() {
        let method = Method::PhotoSnapshotValidate;
        assert!(!method.is_write());
        assert_eq!(method.tool_name(), "photo_snapshot_validate");
        assert_eq!(
            serde_json::to_value(method).unwrap(),
            json!("photoSnapshotValidate")
        );
        let original = request(method);
        let parsed: Request = serde_json::from_value(original.clone()).unwrap();
        assert!(validate_request_shape(&parsed).is_ok());
        assert!(
            parsed.photo_parameters.is_none()
                && parsed.photo_snapshot_query.is_none()
                && parsed.operation_query.is_none()
        );
        for field in ["projectId", "documentSessionId"] {
            for invalid in [Value::Null, json!("")] {
                let mut value = original.clone();
                value[field] = invalid;
                assert!(!parsed_valid(value));
            }
        }
        let query = parsed.photo_snapshot_validation_query.as_ref().unwrap();
        assert_eq!(
            query.expected.document_id.as_str(),
            handle("photo-document-v1-")
        );
        assert_eq!(query.expected.state_id.as_str(), handle("photo-state-v1-"));
        assert_eq!(query.snapshot_id.as_str(), handle("photo-snapshot-v1-"));
    }

    #[test]
    fn photo_validation_helper_query_is_exclusive_with_all_original_methods_and_payloads() {
        let original = request(Method::PhotoSnapshotValidate);
        for method in [
            Method::Capabilities,
            Method::AppContext,
            Method::AppNavigate,
            Method::AppOperation,
            Method::PageItems,
            Method::PagePreview,
            Method::Context,
            Method::Project,
            Method::Preview,
            Method::ClipPreview,
            Method::MoveSelectedImage,
            Method::LibraryImages,
            Method::LibraryAssets,
            Method::InsertLibraryImage,
            Method::TransformCanvasLayer,
            Method::CanvasEdit,
            Method::ClipEdit,
            Method::ClipOperation,
            Method::PhotoState,
            Method::PhotoSnapshot,
            Method::PhotoEdit,
            Method::PhotoOperation,
            Method::ClipExportControl,
            Method::ClipExportOutput,
            Method::SaveCanvasProject,
            Method::ExportCanvasPng,
            Method::CanvasOperation,
            Method::RetryCanvasRegistration,
            Method::FlowRead,
            Method::FlowWorkspace,
            Method::FlowWrite,
        ] {
            let mut value = original.clone();
            value["method"] = json!(method);
            let parsed: Request = serde_json::from_value(value).unwrap();
            assert!(validate_request_shape(&parsed).is_err());
        }
        for (field, payload) in [
            ("photoParameters", parameters()),
            ("photoSnapshotQuery", query()),
            (
                "operationQuery",
                json!({"operationId":operation_id("client","session",1).unwrap(),"clientSequence":1}),
            ),
        ] {
            let mut value = original.clone();
            value[field] = payload;
            let parsed: Request = serde_json::from_value(value).unwrap();
            assert!(validate_request_shape(&parsed).is_err());
        }
    }

    #[test]
    fn photo_validation_helper_never_accepts_native_numbers_paths_tokens_or_capture_parameters() {
        let original = request(Method::PhotoSnapshotValidate);
        for pointer in [
            "/photoSnapshotValidationQuery/expected/documentId",
            "/photoSnapshotValidationQuery/expected/stateId",
            "/photoSnapshotValidationQuery/snapshotId",
        ] {
            for native in [9_007_199_254_740_993u64, u64::MAX] {
                let mut value = original.clone();
                *value.pointer_mut(pointer).unwrap() = json!(native);
                assert!(serde_json::from_value::<Request>(value).is_err());
            }
        }
        for field in [
            "path",
            "token",
            "clientToken",
            "capture",
            "recapture",
            "maxEdge",
            "requestId",
            "params",
            "operationId",
            "clientSequence",
        ] {
            let mut value = original.clone();
            value["photoSnapshotValidationQuery"][field] = json!(1);
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
        let mut value = original.clone();
        value["photoSnapshotValidationQuery"]["snapshotId"] = json!(handle("photo-document-v1-"));
        assert!(serde_json::from_value::<Request>(value).is_err());
        let mut value = original.clone();
        value["photoSnapshotValidationQuery"]["expected"]["stateId"] =
            json!(handle("photo-snapshot-v1-"));
        assert!(serde_json::from_value::<Request>(value).is_err());
        for field in ["revision", "selectionStateId", "viewStateId"] {
            let mut value = original.clone();
            value["photoSnapshotValidationQuery"]["expected"][field] = json!(1);
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }

    #[test]
    fn photo_validation_helper_requires_original_expected_and_snapshot_and_keeps_typed_equality() {
        let original = request(Method::PhotoSnapshotValidate);
        for field in ["expected", "snapshotId"] {
            let mut value = original.clone();
            value["photoSnapshotValidationQuery"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(!parsed_valid(value));
            let mut value = original.clone();
            value["photoSnapshotValidationQuery"][field] = Value::Null;
            assert!(!parsed_valid(value));
        }
        let mut value = original.clone();
        value
            .as_object_mut()
            .unwrap()
            .remove("photoSnapshotValidationQuery");
        assert!(!parsed_valid(value));
        let mut value = original.clone();
        value["photoSnapshotValidationQuery"] = Value::Null;
        assert!(!parsed_valid(value));
        let first: PhotoSnapshotValidationQuery =
            serde_json::from_value(original["photoSnapshotValidationQuery"].clone()).unwrap();
        let restored: PhotoSnapshotValidationQuery =
            serde_json::from_str(&serde_json::to_string(&first).unwrap()).unwrap();
        assert_eq!(first, restored);
        let mut changed = restored;
        changed.snapshot_id =
            PhotoSnapshotId::parse(&format!("photo-snapshot-v1-{}", "b".repeat(64))).unwrap();
        assert_ne!(first, changed);
    }

    #[cfg(feature = "stdio")]
    #[test]
    fn photo_validation_helper_schema_is_closed_and_preserves_exact_handle_patterns() {
        let schema =
            serde_json::to_value(schemars::schema_for!(PhotoSnapshotValidationQuery)).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 2);
        assert!(properties.contains_key("expected") && properties.contains_key("snapshotId"));
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert!(required.contains(&json!("expected")) && required.contains(&json!("snapshotId")));
        assert_eq!(schema["$defs"]["PhotoSnapshotId"]["type"], "string");
        assert_eq!(
            schema["$defs"]["PhotoSnapshotId"]["pattern"],
            "^photo-snapshot-v1-[0-9a-f]{64}$"
        );
        assert_eq!(
            schema["$defs"]["PhotoDocumentId"]["pattern"],
            "^photo-document-v1-[0-9a-f]{64}$"
        );
        assert_eq!(
            schema["$defs"]["PhotoStateId"]["pattern"],
            "^photo-state-v1-[0-9a-f]{64}$"
        );
    }
}
