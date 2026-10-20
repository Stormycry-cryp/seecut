// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exact named-tool permits from the frozen native approval card. Legacy R/M/E
//! authenticates a client but never grants any new canvas/library action.
use super::write_registry::Scope;
use concat_editor_mcp::{
    CanvasAction, CanvasParameters, LibraryQuery, Method, OperationQuery, Request, Response,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Intent {
    pub project: String,
    pub session: String,
    pub revision: Option<u64>,
    pub selection_revision: Option<u64>,
    pub action: Option<CanvasAction>,
    pub library: Option<LibraryQuery>,
    pub operation: Option<OperationQuery>,
    pub retry_registration: bool,
    pub clip_action: Option<concat_editor_mcp::ClipAction>,
    pub clip_query: bool,
}
impl Intent {
    pub fn method(&self) -> Method {
        if self.clip_action.is_some() {
            return Method::ClipEdit;
        }
        if self.clip_query {
            return Method::ClipOperation;
        }
        if let Some(action) = &self.action {
            action.method()
        } else if self.library.is_some() {
            Method::LibraryImages
        } else if self.retry_registration {
            Method::RetryCanvasRegistration
        } else {
            Method::CanvasOperation
        }
    }
    pub fn from_request(request: &Request) -> Option<Self> {
        let intent = Self {
            project: request.project_id.clone()?,
            session: request.document_session_id.clone()?,
            revision: request.revision,
            selection_revision: request
                .canvas_parameters
                .as_ref()
                .map(|p| p.selection_revision)
                .or_else(|| {
                    request
                        .clip_parameters
                        .as_ref()
                        .map(|p| p.selection_revision)
                }),
            action: request.canvas_parameters.as_ref().map(|p| p.action.clone()),
            library: request.library_query.clone(),
            operation: request.operation_query.clone(),
            retry_registration: request.method == Method::RetryCanvasRegistration,
            clip_action: request.clip_parameters.as_ref().map(|p| p.action.clone()),
            clip_query: request.method == Method::ClipOperation,
        };
        (intent.method() == request.method).then_some(intent)
    }
    pub fn parse(tool: &str, args: Value) -> Option<Self> {
        let intent = match tool {
            "library_images" => {
                let a: LibraryArgs = serde_json::from_value(args).ok()?;
                Self {
                    project: a.project_id,
                    session: a.document_session_id,
                    revision: None,
                    selection_revision: None,
                    action: None,
                    library: Some(LibraryQuery {
                        asset_id: a.asset_id,
                        query: a.query,
                        offset: a.offset,
                        limit: a.limit.unwrap_or(20),
                    }),
                    operation: None,
                    retry_registration: false,
                    clip_action: None,
                    clip_query: false,
                }
            }
            "canvas_operation" | "retry_canvas_registration" | "clip_operation" => {
                let a: OperationArgs = serde_json::from_value(args).ok()?;
                Self {
                    project: a.project_id,
                    session: a.document_session_id,
                    revision: None,
                    selection_revision: None,
                    action: None,
                    library: None,
                    retry_registration: tool == "retry_canvas_registration",
                    clip_action: None,
                    clip_query: tool == "clip_operation",
                    operation: Some(OperationQuery {
                        operation_id: a.operation_id,
                        client_sequence: a.client_sequence,
                    }),
                }
            }
            "clip_edit" => {
                let a: ClipArgs = serde_json::from_value(args).ok()?;
                Self {
                    project: a.project_id,
                    session: a.document_session_id,
                    revision: Some(a.revision),
                    selection_revision: Some(a.selection_revision),
                    action: None,
                    library: None,
                    operation: None,
                    retry_registration: false,
                    clip_query: false,
                    clip_action: Some(concat_editor_mcp::ClipAction {
                        timeline_id: a.timeline_id,
                        expected_edit_state_id: a.expected_edit_state_id,
                        edit: a.edit,
                    }),
                }
            }
            "insert_library_image" => {
                let a: InsertArgs = serde_json::from_value(args).ok()?;
                Self::write(
                    a.project_id,
                    a.document_session_id,
                    a.revision,
                    a.selection_revision,
                    CanvasAction::Insert {
                        asset_id: a.asset_id,
                        version: a.version,
                    },
                )
            }
            "transform_canvas_layer" => {
                let a: TransformArgs = serde_json::from_value(args).ok()?;
                Self::write(
                    a.project_id,
                    a.document_session_id,
                    a.revision,
                    a.selection_revision,
                    CanvasAction::Transform {
                        object_id: a.object_id,
                        x: a.x,
                        y: a.y,
                        scale: a.scale,
                    },
                )
            }
            "canvas_edit" => {
                let a: EditArgs = serde_json::from_value(args).ok()?;
                Self::write(
                    a.project_id,
                    a.document_session_id,
                    a.revision,
                    a.selection_revision,
                    CanvasAction::Edit {
                        edit: a.edit,
                        expected_edit_state_id: a.expected_edit_state_id,
                    },
                )
            }
            "save_canvas_project" | "export_canvas_png" => {
                let a: IdentityArgs = serde_json::from_value(args).ok()?;
                Self::write(
                    a.project_id,
                    a.document_session_id,
                    a.revision,
                    a.selection_revision,
                    if tool == "save_canvas_project" {
                        CanvasAction::Save
                    } else {
                        CanvasAction::Export
                    },
                )
            }
            _ => return None,
        };
        // Reuse the same typed wire validator with a synthetic operation solely
        // for semantic syntax; no runtime sequence is allocated by approval.
        let mut request: Request=serde_json::from_value(serde_json::json!({"instanceId":"approval","clientId":"seecut-assistant","clientToken":"syntax","method":intent.method(),
            "projectId":intent.project,"documentSessionId":intent.session})).ok()?;
        request.revision = intent.revision;
        request.library_query = intent.library.clone();
        request.operation_query = intent.operation.clone();
        if let Some(action) = &intent.action {
            request.canvas_parameters = Some(CanvasParameters {
                operation_id: concat_editor_mcp::operation_id(
                    "seecut-assistant",
                    &intent.session,
                    1,
                )
                .ok()?,
                client_sequence: 1,
                selection_revision: intent.selection_revision?,
                action: action.clone(),
            });
        }
        if let Some(action) = &intent.clip_action {
            request.clip_parameters = Some(concat_editor_mcp::ClipParameters {
                operation_id: concat_editor_mcp::operation_id(
                    "seecut-assistant",
                    &intent.session,
                    1,
                )
                .ok()?,
                client_sequence: 1,
                selection_revision: intent.selection_revision?,
                action: action.clone(),
            });
        }
        concat_editor_mcp::validate_request_shape(&request).ok()?;
        Some(intent)
    }
    fn write(
        project: String,
        session: String,
        revision: u64,
        selection_revision: u64,
        action: CanvasAction,
    ) -> Self {
        Self {
            project,
            session,
            revision: Some(revision),
            selection_revision: Some(selection_revision),
            action: Some(action),
            library: None,
            operation: None,
            retry_registration: false,
            clip_action: None,
            clip_query: false,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    selection_revision: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InsertArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    selection_revision: u64,
    asset_id: String,
    version: concat_editor_mcp::AssetVersion,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransformArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    selection_revision: u64,
    object_id: String,
    x: f64,
    y: f64,
    scale: f64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    selection_revision: u64,
    expected_edit_state_id: String,
    edit: concat_editor_mcp::CanvasEdit,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClipArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    selection_revision: u64,
    timeline_id: String,
    expected_edit_state_id: String,
    edit: concat_editor_mcp::ClipEdit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryArgs {
    #[serde(default)]
    asset_id: Option<String>,
    project_id: String,
    document_session_id: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationArgs {
    project_id: String,
    document_session_id: String,
    operation_id: String,
    client_sequence: u64,
}

#[derive(Clone)]
pub(super) struct Permit {
    pub id: u64,
    pub scope: Scope,
    pub client: String,
    pub intent: Intent,
    pub expires_at: Instant,
    used: bool,
}
#[derive(Default)]
pub(super) struct Permits {
    next: u64,
    entries: VecDeque<Permit>,
}
impl Permits {
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn revoke(&mut self, id: u64) {
        self.entries.retain(|p| p.id != id);
    }
    pub fn grant(
        &mut self,
        scope: Scope,
        client: String,
        intent: Intent,
        now: Instant,
    ) -> Option<u64> {
        self.entries.retain(|permit| permit.expires_at > now);
        if self.entries.len() >= 128 {
            return None;
        }
        self.next = self.next.checked_add(1)?;
        let id = self.next;
        self.entries.push_back(Permit {
            id,
            scope,
            client,
            intent,
            expires_at: now.checked_add(Duration::from_secs(300))?,
            used: false,
        });
        Some(id)
    }
    pub fn claim(
        &mut self,
        scope: &Scope,
        client: &str,
        intent: &Intent,
        now: Instant,
    ) -> Option<Permit> {
        let permit = self.entries.iter_mut().find(|p| {
            !p.used
                && p.expires_at > now
                && p.scope == *scope
                && p.client == client
                && p.intent == *intent
        })?;
        permit.used = true;
        Some(permit.clone())
    }
    /// Check the originally consumed permit after worker preparation; revocation
    /// clears entries, so a detached job cannot use its cloned proof alone.
    pub fn valid(&self, permit: &Permit, now: Instant) -> bool {
        self.entries.iter().any(|p| {
            p.id == permit.id
                && p.used
                && p.scope == permit.scope
                && p.client == permit.client
                && p.intent == permit.intent
                && p.expires_at > now
        })
    }
}

struct Receipt {
    scope: Scope,
    client: String,
    sequence: u64,
    operation_id: String,
    at: Instant,
    response: Response,
    clip: bool,
}

pub(super) trait OperationIdentity {
    fn sequence(&self) -> u64;
    fn operation_id(&self) -> &str;
    fn is_clip(&self) -> bool;
}
impl OperationIdentity for CanvasParameters {
    fn sequence(&self) -> u64 {
        self.client_sequence
    }
    fn operation_id(&self) -> &str {
        &self.operation_id
    }
    fn is_clip(&self) -> bool {
        false
    }
}
impl OperationIdentity for concat_editor_mcp::ClipParameters {
    fn sequence(&self) -> u64 {
        self.client_sequence
    }
    fn operation_id(&self) -> &str {
        &self.operation_id
    }
    fn is_clip(&self) -> bool {
        true
    }
}
#[derive(Default)]
pub(super) struct Receipts {
    entries: VecDeque<Receipt>,
}
impl Receipts {
    pub fn reserve(
        &mut self,
        scope: Scope,
        client: String,
        parameters: &impl OperationIdentity,
        now: Instant,
    ) -> bool {
        self.entries
            .retain(|r| now.saturating_duration_since(r.at) < Duration::from_secs(86400));
        if self.entries.len() >= 1024 {
            return false;
        }
        if self
            .entries
            .iter()
            .any(|r| r.scope == scope && r.client == client && r.sequence == parameters.sequence())
        {
            return false;
        }
        self.entries.push_back(Receipt {
            scope,
            client,
            sequence: parameters.sequence(),
            operation_id: parameters.operation_id().to_owned(),
            at: now,
            response: Response::error("outcomeUnknown"),
            clip: parameters.is_clip(),
        });
        true
    }
    pub fn finish(
        &mut self,
        scope: &Scope,
        client: &str,
        parameters: &impl OperationIdentity,
        response: Response,
    ) -> bool {
        let Some(r) = self.entries.iter_mut().find(|r| {
            r.scope == *scope
                && r.client == client
                && r.sequence == parameters.sequence()
                && r.operation_id == parameters.operation_id()
        }) else {
            return false;
        };
        r.response = response;
        true
    }
    pub fn update(
        &mut self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        response: Response,
    ) -> bool {
        let Some(r) = self.entries.iter_mut().find(|r| {
            r.scope == *scope
                && r.client == client
                && r.sequence == query.client_sequence
                && r.operation_id == query.operation_id
        }) else {
            return false;
        };
        r.response = response;
        true
    }
    #[cfg(test)]
    pub fn contains(
        &self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        now: Instant,
    ) -> bool {
        self.contains_family(scope, client, query, now, false)
    }
    pub fn query(
        &self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        now: Instant,
    ) -> Response {
        self.query_family(scope, client, query, now, false)
    }
    pub fn contains_family(
        &self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        now: Instant,
        clip: bool,
    ) -> bool {
        self.entries.iter().any(|r| {
            r.scope == *scope
                && r.client == client
                && r.sequence == query.client_sequence
                && r.operation_id == query.operation_id
                && r.clip == clip
                && now.saturating_duration_since(r.at) < Duration::from_secs(86400)
        })
    }
    pub fn query_family(
        &self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        now: Instant,
        clip: bool,
    ) -> Response {
        self.entries
            .iter()
            .find(|r| {
                r.scope == *scope
                    && r.client == client
                    && r.sequence == query.client_sequence
                    && r.operation_id == query.operation_id
                    && r.clip == clip
                    && now.saturating_duration_since(r.at) < Duration::from_secs(86400)
            })
            .map(|r| r.response.clone())
            .unwrap_or_else(|| Response::error("outcomeUnknown"))
    }
}

#[cfg(test)]
mod clip_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn clip_approval_and_unknown_receipt_keep_exact_tool_and_original_identity() {
        let stamp = format!("clip-edit-v1-{}", "a".repeat(64));
        let args = json!({"projectId":"p","documentSessionId":"s","revision":1,"selectionRevision":2,"timelineId":"timeline:TL1","expectedEditStateId":stamp,"edit":{"kind":"addTrack"}});
        let intent = Intent::parse("clip_edit", args.clone()).unwrap();
        assert_eq!(intent.method(), Method::ClipEdit);
        let mut wrong = args;
        wrong["expectedEditStateId"] = json!(format!("clip-edit-v1-{}", "b".repeat(64)));
        let wrong = Intent::parse("clip_edit", wrong).unwrap();
        let scope = Scope {
            instance: "i".into(),
            project: "p".into(),
            session: "s".into(),
        };
        let now = Instant::now();
        let mut permits = Permits::default();
        permits
            .grant(scope.clone(), "a".into(), intent.clone(), now)
            .unwrap();
        assert!(permits.claim(&scope, "a", &wrong, now).is_none());
        assert!(permits.claim(&scope, "a", &intent, now).is_some());
        let p = concat_editor_mcp::ClipParameters {
            operation_id: concat_editor_mcp::operation_id("a", "s", 1).unwrap(),
            client_sequence: 1,
            selection_revision: 2,
            action: intent.clip_action.unwrap(),
        };
        let q = OperationQuery {
            operation_id: p.operation_id.clone(),
            client_sequence: 1,
        };
        let mut receipts = Receipts::default();
        assert!(receipts.reserve(scope.clone(), "a".into(), &p, now));
        assert_eq!(
            receipts.query_family(&scope, "a", &q, now, true).status,
            "outcomeUnknown"
        );
        assert!(!receipts.contains(&scope, "a", &q, now));
        assert!(!receipts.contains_family(&scope, "b", &q, now, true));
        let response = Response::ok(json!({"operationId":q.operation_id,"tool":"clip_edit"}));
        assert!(receipts.finish(&scope, "a", &p, response.clone()));
        assert_eq!(
            receipts.query_family(&scope, "a", &q, now, true).data,
            response.data
        );
        assert_eq!(
            receipts.query(&scope, "a", &q, now).status,
            "outcomeUnknown"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn canvas_edit_frozen_permit_binds_action_target_parameters_and_expiry() {
        let now = Instant::now();
        let args = json!({"projectId":"p","documentSessionId":"s","revision":2,"selectionRevision":3,"expectedEditStateId":format!("edit-v1-{}","a".repeat(64)),"edit":{"kind":"layerSetOpacity","objectId":"layer:2","opacity":0.4}});
        let intent = Intent::parse("canvas_edit", args.clone()).unwrap();
        assert_eq!(intent.method(), Method::CanvasEdit);
        let mut permits = Permits::default();
        permits
            .grant(scope(), "c".into(), intent.clone(), now)
            .unwrap();
        for (field, value) in [("objectId", json!("layer:3")), ("opacity", json!(0.8))] {
            let mut changed = args.clone();
            changed["edit"][field] = value;
            assert!(
                permits
                    .claim(
                        &scope(),
                        "c",
                        &Intent::parse("canvas_edit", changed).unwrap(),
                        now
                    )
                    .is_none()
            );
        }
        assert!(
            permits
                .claim(&scope(), "c", &intent, now + Duration::from_secs(300))
                .is_none()
        );
        let mut changed_state = args.clone();
        changed_state["expectedEditStateId"] = json!(format!("edit-v1-{}", "b".repeat(64)));
        assert!(
            permits
                .claim(
                    &scope(),
                    "c",
                    &Intent::parse("canvas_edit", changed_state).unwrap(),
                    now
                )
                .is_none()
        );
        let permit = permits.claim(&scope(), "c", &intent, now).unwrap();
        assert!(permits.valid(&permit, now));
        assert!(permits.claim(&scope(), "c", &intent, now).is_none());
        let mut bad = args;
        bad["edit"]["path"] = json!("/tmp/a");
        assert!(Intent::parse("canvas_edit", bad).is_none());
    }
    fn scope() -> Scope {
        Scope {
            instance: "i".into(),
            project: "p".into(),
            session: "s".into(),
        }
    }
    fn intent() -> Intent {
        Intent::parse(
            "save_canvas_project",
            json!({"projectId":"p","documentSessionId":"s","revision":2,"selectionRevision":3}),
        )
        .unwrap()
    }
    #[test]
    fn explicit_permit_is_exact_one_shot_revocable_and_bounded_by_five_minutes() {
        let now = Instant::now();
        let mut permits = Permits::default();
        let intent = intent();
        assert!(permits.claim(&scope(), "c", &intent, now).is_none());
        permits
            .grant(scope(), "c".into(), intent.clone(), now)
            .unwrap();
        let mut different = intent.clone();
        different.revision = Some(3);
        assert!(permits.claim(&scope(), "c", &different, now).is_none());
        assert!(permits.claim(&scope(), "other", &intent, now).is_none());
        let permit = permits.claim(&scope(), "c", &intent, now).unwrap();
        assert!(permits.claim(&scope(), "c", &intent, now).is_none());
        assert!(permits.valid(&permit, now));
        assert!(!permits.valid(&permit, now + Duration::from_secs(300)));
        permits.clear();
        assert!(!permits.valid(&permit, now));
    }
    #[test]
    fn retired_flow_tools_cannot_obtain_a_native_permit() {
        for name in ["flow_read", "flow_write"] {
            assert!(
                Intent::parse(name, json!({"projectId":"p","documentSessionId":"s"})).is_none()
            );
        }
    }
    #[test]
    fn named_approval_rejects_extra_paths_and_unmanaged_operation_fields() {
        let mut a = json!({"projectId":"p","documentSessionId":"s","revision":2,"selectionRevision":3,"path":"/tmp/a"});
        assert!(Intent::parse("save_canvas_project", a.clone()).is_none());
        a.as_object_mut().unwrap().remove("path");
        a["operationId"] = json!("x");
        assert!(Intent::parse("save_canvas_project", a).is_none());
        assert!(Intent::parse("delete_layer", json!({})).is_none());
    }
    #[test]
    fn async_receipts_keep_original_owner_scope_and_sequence() {
        let now = Instant::now();
        let mut receipts = Receipts::default();
        let parameters = CanvasParameters {
            operation_id: concat_editor_mcp::operation_id("c", "s", 1).unwrap(),
            client_sequence: 1,
            selection_revision: 3,
            action: CanvasAction::Save,
        };
        let query = OperationQuery {
            operation_id: parameters.operation_id().to_owned(),
            client_sequence: 1,
        };
        assert!(receipts.reserve(scope(), "c".into(), &parameters, now));
        assert!(!receipts.reserve(scope(), "c".into(), &parameters, now));
        assert_eq!(
            receipts.query(&scope(), "c", &query, now).status,
            "outcomeUnknown"
        );
        assert!(receipts.finish(
            &scope(),
            "c",
            &parameters,
            Response::ok(json!({"persistedRevision":2}))
        ));
        assert_eq!(receipts.query(&scope(), "c", &query, now).status, "ok");
        assert_eq!(
            receipts.query(&scope(), "other", &query, now).status,
            "outcomeUnknown"
        );
        let mut other = scope();
        other.project = "other".into();
        assert_eq!(
            receipts.query(&other, "c", &query, now).status,
            "outcomeUnknown"
        );
    }
}
