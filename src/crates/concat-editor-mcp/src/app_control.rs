// SPDX-License-Identifier: AGPL-3.0-or-later
//! Closed App navigation wire contract. Syntax and negotiation never grant control.
use crate::{AppPageId, AppReadScope, AppViewBinding, Method, Request};
use serde::{Deserialize, Deserializer, Serialize};

/// Independently negotiated App navigation protocol; AppRead remains version 1.
pub const APP_CONTROL_PROTOCOL_VERSION: u8 = 1;
/// JSON/JavaScript lossless sequence ceiling.
pub const APP_CONTROL_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Accept omission only; explicit null and unknown versions are invalid.
pub(crate) fn deserialize_protocol_version<'de, D>(d: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    match u8::deserialize(d)? {
        APP_CONTROL_PROTOCOL_VERSION => Ok(Some(APP_CONTROL_PROTOCOL_VERSION)),
        _ => Err(serde::de::Error::custom("unsupported App control protocol")),
    }
}
fn identity(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_control)
}
/// Canonical original COTO operation identity. UTF-8 byte lengths, no normalization.
pub fn app_operation_id(client: &str, session: &str, sequence: u64) -> Result<String, String> {
    if !identity(client)
        || !identity(session)
        || !(1..=APP_CONTROL_MAX_SAFE_INTEGER).contains(&sequence)
    {
        return Err("invalidInput".into());
    }
    Ok(format!(
        "sca1:1:{}:{client}:{}:{session}:{sequence}",
        client.len(),
        session.len()
    ))
}

/// Raw trusted-wrapper parameters. The future model-facing wrapper exposes target only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppNavigateParameters {
    /// Current native App session, distinct from original COTO sessionId.
    pub app_session_id: String,
    /// Original canonical operation identity; never regenerate after ambiguity.
    pub operation_id: String,
    /// Original COTO session, not current page or native App session.
    pub session_id: String,
    /// Original JS-safe nonzero sequence.
    #[cfg_attr(
        feature = "stdio",
        schemars(range(min = 1, max = 9_007_199_254_740_991u64))
    )]
    pub client_sequence: u64,
    /// Frozen actual AppRead source binding.
    pub before: AppViewBinding,
    /// One closed ordinary page, never a general action or path.
    pub target: AppPageId,
}
impl AppNavigateParameters {
    /// Validate syntax and startup scope only. The native host must verify real grants.
    pub fn valid(&self, client: &str, scope: &AppReadScope) -> bool {
        scope.valid()
            && scope.app_control_protocol_version == Some(APP_CONTROL_PROTOCOL_VERSION)
            && identity(&self.app_session_id)
            && self.app_session_id == scope.app_session_id
            && self.before.valid()
            && app_operation_id(client, &self.session_id, self.client_sequence)
                .is_ok_and(|id| id == self.operation_id)
    }
}

/// Query original identity; current page, old control token and old App session are absent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppOperationQuery {
    /// Unchanged original canonical identity.
    pub operation_id: String,
    /// Original COTO session, used with the real current account and client.
    pub session_id: String,
    /// Original sequence, never advanced by a query.
    #[cfg_attr(
        feature = "stdio",
        schemars(range(min = 1, max = 9_007_199_254_740_991u64))
    )]
    pub client_sequence: u64,
}
impl AppOperationQuery {
    /// This check grants neither read nor write permission.
    pub fn valid(&self, client: &str) -> bool {
        app_operation_id(client, &self.session_id, self.client_sequence)
            .is_ok_and(|id| id == self.operation_id)
    }
}

/// Closed externally visible status. Dispatch never implies Completed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppOperationStatus {
    /// Journal queued intent.
    Queued,
    /// Preparing dispatch, native processing or human leave gate.
    Running,
    /// Actual navigation completion with confirmed terminal durability.
    Completed,
    /// Confirmed cancellation before dispatch.
    Cancelled,
    /// Original outcome requires query; replay is forbidden.
    OutcomeUnknown,
}
/// Durability is a host journal barrier result, never JSON equality or a query result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppOperationDurability {
    /// A live owner acknowledged all required write and directory barriers.
    Confirmed,
    /// No valid durable acknowledgement for the current record revision.
    Unconfirmed,
}
/// Original native origin, deliberately excluding account objects, paths and tokens.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppOperationOrigin {
    /// Original running instance, preserved after recovery.
    pub app_instance_id: String,
    /// Original named client.
    pub client_id: String,
    /// Original native App session, preserved after recovery.
    pub app_session_id: String,
}
/// Actual completion binding from the original navigation's native hook.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppNavigationCompletion {
    /// Actual adopted target page; a queued callback cannot supply this proof.
    pub page: AppViewBinding,
}
/// Optional independently captured current AppRead reference, not the original result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppCurrentObservation {
    /// Real currently observed instance, possibly different after recovery.
    pub app_instance_id: String,
    /// Current native App read session.
    pub app_session_id: String,
    /// Actual current page, allowed to differ from original target.
    pub page: AppViewBinding,
}
/// Only the existing native document-leave gate is available in this protocol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppNavigationHumanGate {
    /// Human uses the native confirmation UI; no model resume tool.
    DocumentLeaveConfirmation,
}
/// Project an internal record into Response::ok; do not serialize the internal record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppNavigationReceipt {
    /// Independent wire protocol version, exactly one.
    pub app_control_protocol_version: u8,
    /// Original canonical operation identity.
    pub operation_id: String,
    /// Original COTO session identity.
    pub session_id: String,
    /// Original sequence.
    pub client_sequence: u64,
    /// Original native source identity.
    pub origin: AppOperationOrigin,
    /// Frozen source AppRead binding.
    pub before: AppViewBinding,
    /// Frozen requested ordinary page.
    pub target: AppPageId,
    /// Raw recorded fact; status remains outcomeUnknown when persistence is unresolved.
    pub recorded_status: AppOperationStatus,
    /// Effective status suitable for the agent's decision.
    pub status: AppOperationStatus,
    /// Current revision's actual persistence acknowledgement.
    pub durability: AppOperationDurability,
    /// Present only for an actual completed navigation, even if terminal save is pending.
    pub completion: Option<AppNavigationCompletion>,
    /// Existing native confirmation, only while recorded running.
    pub human_gate: Option<AppNavigationHumanGate>,
    /// Separate current observation; null means none was obtained.
    pub current_observation: Option<AppCurrentObservation>,
}
impl AppNavigationReceipt {
    /// Validate projection consistency, not provenance or durable I/O.
    pub fn valid(&self) -> bool {
        if self.app_control_protocol_version != APP_CONTROL_PROTOCOL_VERSION
            || !identity(&self.origin.app_instance_id)
            || !identity(&self.origin.app_session_id)
            || !self.before.valid()
            || !app_operation_id(
                &self.origin.client_id,
                &self.session_id,
                self.client_sequence,
            )
            .is_ok_and(|id| id == self.operation_id)
        {
            return false;
        }
        if self.recorded_status == AppOperationStatus::Completed {
            if !self
                .completion
                .as_ref()
                .is_some_and(|v| v.page.valid() && v.page.page_id == self.target)
            {
                return false;
            }
        } else if self.completion.is_some() {
            return false;
        }
        if self.human_gate.is_some() && self.recorded_status != AppOperationStatus::Running {
            return false;
        }
        let valid_status = if self.durability == AppOperationDurability::Unconfirmed
            && matches!(
                self.recorded_status,
                AppOperationStatus::Running
                    | AppOperationStatus::Completed
                    | AppOperationStatus::OutcomeUnknown
            ) {
            self.status == AppOperationStatus::OutcomeUnknown
        } else if self.recorded_status == AppOperationStatus::Running {
            // Recovery or a fenced owner cannot turn durable running into execution proof.
            matches!(
                self.status,
                AppOperationStatus::Running | AppOperationStatus::OutcomeUnknown
            )
        } else {
            self.status == self.recorded_status
        };
        valid_status
            && self.current_observation.as_ref().is_none_or(|v| {
                identity(&v.app_instance_id) && identity(&v.app_session_id) && v.page.valid()
            })
    }
}

/// Parse new trusted CLI environment fields without borrowing Doc write credentials.
/// Returned token stays private in the Bridge; none of these values authorize a host action.
pub fn app_control_environment(
    scope: &mut Option<AppReadScope>,
    version: Option<&str>,
    token: Option<&str>,
) -> Result<Option<String>, &'static str> {
    match (scope.as_mut(), version, token) {
        (_, None, None) => Ok(None),
        (Some(scope), Some("1"), token) if scope.valid() => {
            if token.is_some_and(|s| {
                !uuid::Uuid::parse_str(s)
                    .is_ok_and(|v| v.to_string() == s && v.get_version_num() == 4)
            }) {
                return Err("invalid App control token");
            }
            scope.app_control_protocol_version = Some(APP_CONTROL_PROTOCOL_VERSION);
            Ok(token.map(str::to_owned))
        }
        _ => Err("invalid App control negotiation; requires App scope and exact version 1"),
    }
}

/// Narrow control payload check after the common App/Doc isolation check.
pub(crate) fn validate_app_control_request(request: &Request, scope: &AppReadScope) -> bool {
    if scope.app_control_protocol_version != Some(APP_CONTROL_PROTOCOL_VERSION)
        || request.app_page.is_some()
    {
        return false;
    }
    match request.method {
        Method::AppNavigate => {
            request.app_operation_query.is_none()
                && request.app_control_token.as_ref().is_some_and(|s| {
                    uuid::Uuid::parse_str(s)
                        .is_ok_and(|v| v.to_string() == *s && v.get_version_num() == 4)
                })
                && request
                    .app_navigate
                    .as_ref()
                    .is_some_and(|q| q.valid(&request.client_id, scope))
        }
        Method::AppOperation => {
            request.app_navigate.is_none()
                && request.app_control_token.is_none()
                && request
                    .app_operation_query
                    .as_ref()
                    .is_some_and(|q| q.valid(&request.client_id))
        }
        _ => false,
    }
}

/// Constrain a successful native navigation response to the original request identity.
/// Invalid/partial navigation ACK remains ambiguous; this function never fabricates proof.
pub fn checked_app_navigation_response(
    request: &Request,
    response: crate::Response,
) -> crate::Response {
    if !matches!(request.method, Method::AppNavigate | Method::AppOperation) {
        return response;
    }
    if response.status != "ok" {
        return crate::Response::error(&response.status);
    }
    let failure = || {
        crate::Response::error(if request.method == Method::AppNavigate {
            "outcomeUnknown"
        } else {
            "ioFailure"
        })
    };
    let Some(receipt) = response
        .data
        .and_then(|v| serde_json::from_value::<AppNavigationReceipt>(v).ok())
    else {
        return failure();
    };
    let matches = match request.method {
        Method::AppNavigate => request.app_navigate.as_ref().is_some_and(|q| {
            receipt.operation_id == q.operation_id
                && receipt.session_id == q.session_id
                && receipt.client_sequence == q.client_sequence
                && receipt.origin.app_session_id == q.app_session_id
                && receipt.origin.app_instance_id == request.instance_id
                && receipt.before == q.before
                && receipt.target == q.target
        }),
        Method::AppOperation => request.app_operation_query.as_ref().is_some_and(|q| {
            receipt.operation_id == q.operation_id
                && receipt.session_id == q.session_id
                && receipt.client_sequence == q.client_sequence
        }),
        _ => false,
    };
    if !receipt.valid() || receipt.origin.client_id != request.client_id || !matches {
        return failure();
    }
    serde_json::to_value(receipt)
        .map(crate::Response::ok)
        .unwrap_or_else(|_| failure())
}
