//! Explicit canvas and library contracts; no paths, reflected fields or arbitrary tool names.
use crate::{Method, operation_id, parse_layer_id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Immutable identity of the exact managed image bytes.
pub struct AssetVersion {
    /// Lowercase SHA256 of the managed content.
    pub sha256: String,
    /// Existing global-library registration identity.
    #[serde(rename = "registration_id")]
    pub registration_id: String,
    /// Existing predecessor identity, when this version supersedes an asset.
    #[serde(
        default,
        rename = "previous_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub previous_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
/// Explicit actions; parameters never contain arbitrary filesystem paths.
pub enum CanvasAction {
    /// Insert the exact existing global-library image version.
    Insert {
        /// Existing asset identity.
        asset_id: String,
        /// Frozen selected version.
        version: AssetVersion,
    },
    /// Set bounded position and uniform scale of the selected image.
    Transform {
        /// Canonical selected layer identity.
        object_id: String,
        /// Horizontal position in document pixels.
        x: f64,
        /// Vertical position in document pixels.
        y: f64,
        /// Uniform scale, retaining existing mirror directions.
        scale: f64,
    },
    /// Ordinary asynchronous save to the existing App binding.
    Save,
    /// Formal asynchronous PNG export with default global-library registration.
    Export,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Original operation identity and frozen semantic action.
pub struct CanvasParameters {
    /// Canonical original client/session/sequence identity.
    pub operation_id: String,
    /// Nonzero sequence allocated durably by the trusted runtime.
    pub client_sequence: u64,
    /// Exact selection epoch before the action.
    pub selection_revision: u64,
    /// Named action and its exact bounded parameters.
    pub action: CanvasAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Bounded read-only search in the existing global library.
pub struct LibraryQuery {
    /// Optional exact asset lookup, including retrieval of an exported result.
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    /// Case-insensitive name query, at most 160 characters.
    pub query: String,
    #[serde(default)]
    /// Start within matching image assets.
    pub offset: usize,
    #[serde(default = "page_size")]
    /// Maximum returned rows, 1..50; output bytes are separately bounded.
    pub limit: usize,
}
fn page_size() -> usize {
    20
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Read only the original operation; never replay a write.
pub struct OperationQuery {
    /// Canonical original client/session/sequence identity.
    pub operation_id: String,
    /// Nonzero sequence allocated durably by the trusted runtime.
    pub client_sequence: u64,
}

impl CanvasAction {
    /// Exact protocol method corresponding to this action.
    pub fn method(&self) -> Method {
        match self {
            Self::Insert { .. } => Method::InsertLibraryImage,
            Self::Transform { .. } => Method::TransformCanvasLayer,
            Self::Save => Method::SaveCanvasProject,
            Self::Export => Method::ExportCanvasPng,
        }
    }
}

/// Validate syntax before native ownership, permission and revision checks.
pub fn validate_canvas_parameters(
    client: &str,
    session: &str,
    parameters: &CanvasParameters,
) -> Result<(), String> {
    if parameters.operation_id != operation_id(client, session, parameters.client_sequence)? {
        return Err("invalidInput: operation ID mismatch".into());
    }
    match &parameters.action {
        CanvasAction::Insert { asset_id, version } => {
            if asset_id.is_empty()
                || asset_id.len() > 128
                || !asset_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || version.sha256.len() != 64
                || !version
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                || version.previous_id.as_ref().is_some_and(|id| {
                    id.is_empty()
                        || id.len() > 128
                        || !id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                || version.registration_id.is_empty()
                || version.registration_id.len() > 512
                || version.registration_id.chars().any(char::is_control)
            {
                return Err("invalidInput: asset version".into());
            }
        }
        CanvasAction::Transform {
            object_id,
            x,
            y,
            scale,
        } => {
            parse_layer_id(object_id)?;
            if !x.is_finite()
                || !y.is_finite()
                || x.abs() > 32768.0
                || y.abs() > 32768.0
                || !scale.is_finite()
                || !(0.01..=64.0).contains(scale)
            {
                return Err("invalidInput: transform".into());
            }
        }
        CanvasAction::Save | CanvasAction::Export => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, validate_request_shape};
    use serde_json::json;
    fn request(method: Method) -> Request {
        serde_json::from_value(
            json!({"instanceId":"i","clientId":"a","clientToken":"t","method":method,
            "projectId":"p","documentSessionId":"s"}),
        )
        .unwrap()
    }
    fn parameters(action: CanvasAction) -> CanvasParameters {
        CanvasParameters {
            operation_id: operation_id("a", "s", 1).unwrap(),
            client_sequence: 1,
            selection_revision: 2,
            action,
        }
    }
    fn version() -> AssetVersion {
        AssetVersion {
            sha256: "a".repeat(64),
            registration_id: "operation:r".into(),
            previous_id: None,
        }
    }
    #[test]
    fn typed_writes_bind_method_and_original_identity() {
        for action in [
            CanvasAction::Insert {
                asset_id: "asset-1".into(),
                version: version(),
            },
            CanvasAction::Transform {
                object_id: "layer:2".into(),
                x: 0.0,
                y: 2.0,
                scale: 0.5,
            },
            CanvasAction::Save,
            CanvasAction::Export,
        ] {
            let mut r = request(action.method());
            r.revision = Some(3);
            r.canvas_parameters = Some(parameters(action));
            assert!(validate_request_shape(&r).is_ok());
            r.client_id = "b".into();
            assert!(validate_request_shape(&r).is_err());
            r.client_id = "a".into();
            r.method = Method::MoveSelectedImage;
            assert!(validate_request_shape(&r).is_err());
        }
    }
    #[test]
    fn semantic_fields_cannot_smuggle_paths_or_non_finite_transform() {
        let mut p = parameters(CanvasAction::Insert {
            asset_id: "../x".into(),
            version: version(),
        });
        assert!(validate_canvas_parameters("a", "s", &p).is_err());
        let mut v = version();
        v.sha256 = "A".repeat(64);
        p.action = CanvasAction::Insert {
            asset_id: "a".into(),
            version: v,
        };
        assert!(validate_canvas_parameters("a", "s", &p).is_err());
        for (x, y, scale) in [
            (f64::NAN, 0.0, 1.0),
            (0.0, f64::INFINITY, 1.0),
            (32769.0, 0.0, 1.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 64.01),
        ] {
            p.action = CanvasAction::Transform {
                object_id: "layer:2".into(),
                x,
                y,
                scale,
            };
            assert!(validate_canvas_parameters("a", "s", &p).is_err());
        }
        assert!(
            serde_json::from_value::<CanvasAction>(
                json!({"kind":"insert","assetId":"a","version":version(),"path":"/tmp/a"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<AssetVersion>(
                json!({"sha256":"a".repeat(64),"registration_id":"r","path":"/tmp/a"})
            )
            .is_err()
        );
    }
    #[test]
    fn reads_cannot_become_writes_or_accept_unbounded_pages() {
        let mut r = request(Method::LibraryImages);
        r.library_query = Some(LibraryQuery {
            asset_id: None,
            query: "q".into(),
            offset: 0,
            limit: 20,
        });
        assert!(validate_request_shape(&r).is_ok());
        r.canvas_parameters = Some(parameters(CanvasAction::Save));
        assert!(validate_request_shape(&r).is_err());
        r.canvas_parameters = None;
        r.library_query.as_mut().unwrap().limit = 51;
        assert!(validate_request_shape(&r).is_err());
        r.library_query.as_mut().unwrap().limit = 20;
        r.revision = Some(0);
        assert!(validate_request_shape(&r).is_err());
    }
    #[test]
    fn operation_query_only_accepts_original_client_session_sequence() {
        let mut r = request(Method::CanvasOperation);
        r.operation_query = Some(OperationQuery {
            operation_id: operation_id("a", "s", 7).unwrap(),
            client_sequence: 7,
        });
        assert!(validate_request_shape(&r).is_ok());
        r.operation_query.as_mut().unwrap().client_sequence = 8;
        assert!(validate_request_shape(&r).is_err());
        r.operation_query.as_mut().unwrap().client_sequence = 7;
        r.document_session_id = Some("other".into());
        assert!(validate_request_shape(&r).is_err());
    }
    #[test]
    fn targeted_layer_read_rejects_pagination_and_noncanonical_ids() {
        let mut r = request(Method::Project);
        r.object_id = Some("layer:3".into());
        assert!(validate_request_shape(&r).is_ok());
        r.offset = Some(0);
        assert!(validate_request_shape(&r).is_err());
        r.offset = None;
        for id in ["layer:0", "layer:03", "clip:3", "layer:-1"] {
            r.object_id = Some(id.into());
            assert!(validate_request_shape(&r).is_err());
        }
        r.object_id = Some("layer:3".into());
        r.method = Method::Context;
        assert!(validate_request_shape(&r).is_err());
    }
}
