// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded, path-free metadata search in the local personal media library.
use serde::{Deserialize, Serialize};

/// Explicit personal-library category; `all` searches one combined page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LibraryAssetKind {
    /// Search images, video and audio together.
    #[default]
    All,
    /// Search still images.
    Image,
    /// Search video files.
    Video,
    /// Search audio files.
    Audio,
}

/// One bounded local metadata query, independent of legacy image-only queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryAssetsQuery {
    /// Required category selection after defaulting to one combined page.
    #[serde(default)]
    pub kind: LibraryAssetKind,
    /// Optional stable asset identity, never a filesystem path.
    #[serde(default)]
    pub asset_id: Option<String>,
    /// Case-insensitive name query, at most 160 Unicode characters.
    #[serde(default)]
    pub query: String,
    /// First matching metadata row, at most 1,000,000.
    #[serde(default)]
    pub offset: usize,
    /// Requested page length, from one through fifty; default twenty.
    #[serde(default = "page_size")]
    pub limit: usize,
}

fn page_size() -> usize {
    20
}

impl LibraryAssetsQuery {
    /// Check metadata-query limits without opening any asset content.
    pub fn validate(&self) -> Result<(), String> {
        if self.asset_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }) || self.query.chars().count() > 160
            || self.query.chars().any(char::is_control)
            || self.limit == 0
            || self.limit > 50
            || self.offset > 1_000_000
        {
            return Err("invalidInput: library assets page".into());
        }
        Ok(())
    }
}

/// Validate the existing immutable asset/version contract without filesystem IO.
pub fn validate_library_asset(asset_id: &str, version: &crate::AssetVersion) -> Result<(), String> {
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
        || version.registration_id.is_empty()
        || version.registration_id.len() > 512
        || version.registration_id.chars().any(char::is_control)
        || version.previous_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
    {
        return Err("invalidInput: asset version".into());
    }
    Ok(())
}

pub(crate) fn validate_request(request: &crate::Request) -> Result<(), String> {
    if request.method != crate::Method::LibraryAssets
        || request
            .project_id
            .as_ref()
            .is_none_or(|v| v.is_empty() || v.len() > 512)
        || request
            .document_session_id
            .as_ref()
            .is_none_or(|v| v.is_empty() || v.len() > 512)
        || request.revision.is_some()
        || request.context_revision.is_some()
        || request.offset.is_some()
        || request.limit.is_some()
        || request.max_edge.is_some()
        || request.move_parameters.is_some()
        || request.canvas_parameters.is_some()
        || request.clip_parameters.is_some()
        || request.object_id.is_some()
        || request.object_ids.is_some()
        || request.library_query.is_some()
        || request.operation_query.is_some()
        || request.flow.is_some()
        || request.flow_workspace.is_some()
    {
        return Err("invalidInput: library assets request".into());
    }
    request
        .library_assets_query
        .as_ref()
        .ok_or("invalidInput: library assets query")?
        .validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn combined_query_defaults_and_explicit_kinds_are_closed() {
        let defaults: LibraryAssetsQuery = serde_json::from_value(json!({})).unwrap();
        assert_eq!(defaults.kind, LibraryAssetKind::All);
        assert_eq!(defaults.limit, 20);
        assert!(defaults.validate().is_ok());
        for kind in ["all", "image", "video", "audio"] {
            let q: LibraryAssetsQuery = serde_json::from_value(json!({"kind":kind})).unwrap();
            assert_eq!(serde_json::to_value(q).unwrap()["kind"], kind);
        }
        assert!(serde_json::from_value::<LibraryAssetsQuery>(json!({"kind":"cloud"})).is_err());
        assert!(serde_json::from_value::<LibraryAssetsQuery>(json!({"path":"/tmp/a"})).is_err());
    }

    #[test]
    fn metadata_limits_are_checked_without_opening_content() {
        for value in [
            json!({"assetId":"../asset"}),
            json!({"assetId":""}),
            json!({"query":"x".repeat(161)}),
            json!({"query":"x\ny"}),
            json!({"limit":0}),
            json!({"limit":51}),
            json!({"offset":1_000_001}),
        ] {
            let q: LibraryAssetsQuery = serde_json::from_value(value).unwrap();
            assert!(q.validate().is_err());
        }
        let q: LibraryAssetsQuery = serde_json::from_value(json!({
            "assetId":"existing_asset-1", "kind":"video", "query":"视频", "limit":50,
            "offset":1_000_000
        }))
        .unwrap();
        assert!(q.validate().is_ok());
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;
    use serde_json::json;
    fn request() -> crate::Request {
        serde_json::from_value(json!({"instanceId":"app","clientId":"client","clientToken":"read",
            "method":"libraryAssets","projectId":"p","documentSessionId":"s","libraryAssetsQuery":{}})).unwrap()
    }
    #[test]
    fn only_new_query_method_accepts_asset_query_and_no_write_or_target_fields() {
        let query = request();
        assert!(crate::validate_request_shape(&query).is_ok());
        let mut wrong = query.clone();
        wrong.method = crate::Method::Project;
        assert!(crate::validate_request_shape(&wrong).is_err());
        let mut mixed = query.clone();
        mixed.revision = Some(1);
        assert!(crate::validate_request_shape(&mixed).is_err());
        let mut mixed = query;
        mixed.object_ids = Some(vec!["media:m1".into()]);
        assert!(crate::validate_request_shape(&mixed).is_err());
    }
    #[test]
    fn null_asset_id_has_the_same_unfiltered_query_semantics() {
        let query: LibraryAssetsQuery = serde_json::from_value(json!({"assetId":null})).unwrap();
        assert!(query.asset_id.is_none());
        assert_eq!(query.kind, LibraryAssetKind::All);
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn import_accepts_only_existing_fixed_identity_and_rejects_paths_or_unversioned_input() {
        let value = json!({"kind":"importLibrary","assetId":"asset-1","version":{"sha256":"a".repeat(64),"registration_id":"registered-output","previous_id":null}});
        let edit: crate::ClipEdit = serde_json::from_value(value.clone()).unwrap();
        assert!(edit.validate().is_ok());
        let mut path = value.clone();
        path["path"] = json!("/tmp/unapproved");
        assert!(serde_json::from_value::<crate::ClipEdit>(path).is_err());
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("version");
        assert!(serde_json::from_value::<crate::ClipEdit>(missing).is_err());
    }
    #[test]
    fn fixed_identity_limits_include_registration_utf8_bytes_and_predecessor_id() {
        let version = crate::AssetVersion {
            sha256: "a".repeat(64),
            registration_id: "图".repeat(171),
            previous_id: None,
        };
        assert!(validate_library_asset("asset-1", &version).is_err());
        let version = crate::AssetVersion {
            registration_id: "registered".into(),
            previous_id: Some("../prior".into()),
            ..version
        };
        assert!(validate_library_asset("asset-1", &version).is_err());
    }
}
