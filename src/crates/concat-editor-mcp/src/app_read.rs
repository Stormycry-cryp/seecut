// SPDX-License-Identifier: AGPL-3.0-or-later
//! Closed read-only App scope and frozen page queries. No document/write fields.

use crate::{Method, Request};
use serde::{Deserialize, Serialize};

/// JS-safe navigation and cache version ceiling.
pub const APP_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Exact current signed-in App scope, supplied by the trusted host at launch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppReadScope {
    /// SHA256(canonical base URL + NUL + trusted signed-in user ID).
    pub account_scope: String,
    /// Host account lifetime; ordinary page navigation preserves this identity.
    pub app_session_id: String,
    /// Independent navigation negotiation; omitted for legacy AppRead v1.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::app_control::deserialize_protocol_version"
    )]
    pub app_control_protocol_version: Option<u8>,
}

/// Closed ordinary App pages; open documents/Flow keep the existing Doc API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AppPageId {
    /// Generation form with the current results/inspiration tab.
    Generation,
    /// Current filtered personal media list.
    PersonalLibrary,
    /// Current filtered team media list.
    TeamLibrary,
    /// Current template list/editor.
    Templates,
    /// Whitelisted current account view.
    Account,
    /// Whitelisted wallet/history view.
    Credits,
    /// Whitelisted current settings tab.
    Settings,
    /// Current clip/canvas project gallery, never project paths.
    Projects,
}

/// User-view identity captured before asynchronous attachment preparation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppViewBinding {
    /// Closed current page.
    pub page_id: AppPageId,
    /// Navigation generation, JS-safe and nonzero.
    #[cfg_attr(
        feature = "stdio",
        schemars(range(min = 1, max = 9_007_199_254_740_991u64))
    )]
    pub page_epoch: u64,
    /// User-view hash, excluding ordinary progress/charges.
    pub page_state_id: String,
}
impl AppViewBinding {
    /// Validate identity format only, never authorize a request.
    pub fn valid(&self) -> bool {
        (1..=APP_MAX_SAFE_INTEGER).contains(&self.page_epoch)
            && self
                .page_state_id
                .strip_prefix("app-page-v1-")
                .is_some_and(hex64)
    }
}

/// Explicit App context query. Session identity is hidden/injected by managed runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppContextQuery {
    /// Must equal the independently trusted startup App scope.
    pub app_session_id: String,
}

/// Frozen current filtered-list page, never viewport coordinates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppItemsQuery {
    /// Must equal the trusted startup App scope.
    pub app_session_id: String,
    /// Exact current ordinary page.
    pub page_id: AppPageId,
    /// Original navigation epoch.
    pub page_epoch: u64,
    /// Original user-view digest, excluding pure progress/charges.
    pub page_state_id: String,
    /// First item in the filtered model; default zero, null is invalid.
    #[serde(default)]
    pub offset: usize,
    /// Requested count, 1..=50; default 20, null is invalid.
    #[serde(default = "default_page_limit")]
    pub limit: usize,
}
fn default_page_limit() -> usize {
    20
}

/// Original item preview. Managed runtime injects the observed cache version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppPreviewQuery {
    /// Must equal trusted startup scope.
    pub app_session_id: String,
    /// Exact current page.
    pub page_id: AppPageId,
    /// Original navigation epoch.
    pub page_epoch: u64,
    /// Original user-view digest.
    pub page_state_id: String,
    /// Host opaque item identity from actual context/items ToolResult.
    pub item_id: String,
    /// Original image-handle version, never a file SHA or model authorization.
    pub preview_version: u64,
}

/// Flat closed method-specific payload; variant identity follows the IPC method.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AppPageQuery {
    /// Exact item pixels.
    Preview(AppPreviewQuery),
    /// Exact filtered page.
    Items(AppItemsQuery),
    /// Current context only.
    Context(AppContextQuery),
}

impl AppReadScope {
    /// Validate pathless scope identity only, never grant authorization.
    pub fn valid(&self) -> bool {
        hex64(&self.account_scope)
            && identity(&self.app_session_id, 128)
            && self.app_control_protocol_version.is_none_or(|v| v == 1)
    }
}
fn identity(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn frozen(session: &str, page_epoch: u64, state: &str, scope: &AppReadScope) -> bool {
    session == scope.app_session_id
        && (1..=APP_MAX_SAFE_INTEGER).contains(&page_epoch)
        && state.strip_prefix("app-page-v1-").is_some_and(hex64)
}

/// Reject all App/Doc mixing before dispatch. This is shape validation, not a grant.
pub fn validate_app_request(request: &Request) -> Result<(), String> {
    let scope = request
        .app_scope
        .as_ref()
        .filter(|s| s.valid())
        .ok_or("invalidInput")?;
    if request.project_id.is_some()
        || request.document_session_id.is_some()
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
        || request.library_assets_query.is_some()
        || request.operation_query.is_some()
        || request.flow.is_some()
        || request.flow_workspace.is_some()
    {
        return Err("invalidInput".into());
    }
    if matches!(request.method, Method::AppNavigate | Method::AppOperation) {
        return if crate::app_control::validate_app_control_request(request, scope) {
            Ok(())
        } else {
            Err("invalidInput".into())
        };
    }
    if request.app_navigate.is_some()
        || request.app_operation_query.is_some()
        || request.app_control_token.is_some()
    {
        return Err("invalidInput".into());
    }
    let valid = match (request.method, request.app_page.as_ref()) {
        (Method::Capabilities, None) => true,
        (Method::AppContext, Some(AppPageQuery::Context(q))) => {
            q.app_session_id == scope.app_session_id
        }
        (Method::PageItems, Some(AppPageQuery::Items(q))) => {
            frozen(&q.app_session_id, q.page_epoch, &q.page_state_id, scope)
                && q.offset <= 2048
                && (1..=50).contains(&q.limit)
        }
        (Method::PagePreview, Some(AppPageQuery::Preview(q))) => {
            frozen(&q.app_session_id, q.page_epoch, &q.page_state_id, scope)
                && q.item_id
                    .strip_prefix("app-item-")
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                && (1..=APP_MAX_SAFE_INTEGER).contains(&q.preview_version)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("invalidInput".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn request(method: Method, app_page: serde_json::Value) -> Request {
        serde_json::from_value(
            json!({"instanceId":"i","clientId":"c","clientToken":"r","method":method,
        "appScope":{"accountScope":"a".repeat(64),"appSessionId":"app"},"appPage":app_page}),
        )
        .unwrap()
    }
    #[test]
    fn closed_context_and_doc_mix() {
        let mut r = request(Method::AppContext, json!({"appSessionId":"app"}));
        assert!(validate_app_request(&r).is_ok());
        r.project_id = Some("doc".into());
        assert!(validate_app_request(&r).is_err());
        r.project_id = None;
        r.method = Method::Context;
        assert!(validate_app_request(&r).is_err());
        assert!(
            serde_json::from_value::<AppContextQuery>(
                json!({"appSessionId":"app","accountScope":"forged"})
            )
            .is_err()
        );
    }
    #[test]
    fn items_defaults_null_and_frozen_identity() {
        let input = json!({"appSessionId":"app","pageId":"generation","pageEpoch":1,"pageStateId":format!("app-page-v1-{}","a".repeat(64))});
        let q: AppItemsQuery = serde_json::from_value(input.clone()).unwrap();
        assert_eq!((q.offset, q.limit), (0, 20));
        let mut bad = input.clone();
        bad["limit"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<AppItemsQuery>(bad).is_err());
        let mut r = request(Method::PageItems, input);
        assert!(validate_app_request(&r).is_ok());
        r.app_scope.as_mut().unwrap().app_session_id = "other".into();
        assert!(validate_app_request(&r).is_err());
    }
    #[test]
    fn app_context_never_becomes_capabilities_or_preview() {
        let mut r = request(Method::AppContext, json!({"appSessionId":"app"}));
        r.method = Method::Capabilities;
        assert!(validate_app_request(&r).is_err());
        r.method = Method::PagePreview;
        assert!(validate_app_request(&r).is_err());
    }
}
