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
    pub library_assets: Option<concat_editor_mcp::LibraryAssetsQuery>,
    pub operation: Option<OperationQuery>,
    pub retry_registration: bool,
    pub clip_action: Option<concat_editor_mcp::ClipAction>,
    pub clip_query: bool,
    pub clip_export_control_action: Option<concat_editor_mcp::ClipExportControlAction>,
    pub clip_export_output: bool,
    pub move_action: Option<MoveAction>,
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MoveAction {
    pub object_id: String,
    pub delta_x: f64,
    pub delta_y: f64,
}
impl Intent {
    pub fn method(&self) -> Method {
        if self.library_assets.is_some() {
            return Method::LibraryAssets;
        }
        if self.move_action.is_some() {
            return Method::MoveSelectedImage;
        }
        if self.clip_action.is_some() {
            return Method::ClipEdit;
        }
        if self.clip_export_control_action.is_some() {
            return Method::ClipExportControl;
        }
        if self.clip_export_output {
            return Method::ClipExportOutput;
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
                })
                .or_else(|| {
                    request
                        .move_parameters
                        .as_ref()
                        .map(|p| p.selection_revision)
                }),
            action: request.canvas_parameters.as_ref().map(|p| p.action.clone()),
            library: request.library_query.clone(),
            operation: request.operation_query.clone(),
            retry_registration: request.method == Method::RetryCanvasRegistration,
            clip_action: request.clip_parameters.as_ref().map(|p| p.action.clone()),
            library_assets: request.library_assets_query.clone(),
            clip_query: request.method == Method::ClipOperation,
            clip_export_control_action: request.clip_export_control_action,
            clip_export_output: request.method == Method::ClipExportOutput,
            move_action: request.move_parameters.as_ref().map(|p| MoveAction {
                object_id: p.object_id.clone(),
                delta_x: p.delta_x,
                delta_y: p.delta_y,
            }),
        };
        (intent.method() == request.method).then_some(intent)
    }
    pub fn parse(tool: &str, args: Value) -> Option<Self> {
        let intent = match tool {
            "library_assets" => {
                let a: LibraryAssetsArgs = serde_json::from_value(args).ok()?;
                Self {
                    project: a.project_id,
                    session: a.document_session_id,
                    revision: None,
                    selection_revision: None,
                    action: None,
                    library: None,
                    operation: None,
                    retry_registration: false,
                    clip_action: None,
                    clip_query: false,
                    clip_export_control_action: None,
                    clip_export_output: false,
                    move_action: None,
                    library_assets: Some(concat_editor_mcp::LibraryAssetsQuery {
                        kind: a.kind,
                        asset_id: a.asset_id,
                        query: a.query,
                        offset: a.offset,
                        limit: a.limit.unwrap_or(20),
                    }),
                }
            }
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
                    library_assets: None,
                    clip_query: false,
                    clip_export_control_action: None,
                    clip_export_output: false,
                    move_action: None,
                }
            }
            "canvas_operation"
            | "retry_canvas_registration"
            | "clip_operation"
            | "clip_export_output" => {
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
                    library_assets: None,
                    clip_query: tool == "clip_operation",
                    clip_export_control_action: None,
                    clip_export_output: tool == "clip_export_output",
                    move_action: None,
                    operation: Some(OperationQuery {
                        operation_id: a.operation_id,
                        client_sequence: a.client_sequence,
                    }),
                }
            }
            "clip_export_control" => {
                let a: ClipExportControlArgs = serde_json::from_value(args).ok()?;
                Self {
                    project: a.project_id,
                    session: a.document_session_id,
                    revision: None,
                    selection_revision: None,
                    action: None,
                    library: None,
                    library_assets: None,
                    retry_registration: false,
                    clip_action: None,
                    clip_query: false,
                    clip_export_control_action: Some(a.action),
                    clip_export_output: false,
                    move_action: None,
                    operation: Some(OperationQuery {
                        operation_id: a.operation_id,
                        client_sequence: a.client_sequence,
                    }),
                }
            }
            "move_selected_image" => {
                let a: MoveArgs = serde_json::from_value(args).ok()?;
                Self {
                    project: a.project_id,
                    session: a.document_session_id,
                    revision: Some(a.revision),
                    selection_revision: Some(a.selection_revision),
                    action: None,
                    library: None,
                    operation: None,
                    retry_registration: false,
                    clip_action: None,
                    library_assets: None,
                    clip_query: false,
                    clip_export_control_action: None,
                    clip_export_output: false,
                    move_action: Some(MoveAction {
                        object_id: a.object_id,
                        delta_x: a.delta_x,
                        delta_y: a.delta_y,
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
                    library_assets: None,
                    clip_query: false,
                    clip_export_control_action: None,
                    clip_export_output: false,
                    move_action: None,
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
        request.library_assets_query = intent.library_assets.clone();
        request.operation_query = intent.operation.clone();
        request.clip_export_control_action = intent.clip_export_control_action;
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
        if let Some(action) = &intent.move_action {
            request.move_parameters = Some(concat_editor_mcp::MoveParameters {
                operation_id: concat_editor_mcp::operation_id(
                    "seecut-assistant",
                    &intent.session,
                    1,
                )
                .ok()?,
                client_sequence: 1,
                selection_revision: intent.selection_revision?,
                object_id: action.object_id.clone(),
                delta_x: action.delta_x,
                delta_y: action.delta_y,
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
            library_assets: None,
            clip_query: false,
            clip_export_control_action: None,
            clip_export_output: false,
            move_action: None,
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
struct MoveArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    selection_revision: u64,
    object_id: String,
    delta_x: f64,
    delta_y: f64,
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
struct LibraryAssetsArgs {
    project_id: String,
    document_session_id: String,
    #[serde(default)]
    kind: concat_editor_mcp::LibraryAssetKind,
    #[serde(default)]
    asset_id: Option<String>,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClipExportControlArgs {
    project_id: String,
    document_session_id: String,
    operation_id: String,
    client_sequence: u64,
    action: concat_editor_mcp::ClipExportControlAction,
}

#[derive(Clone)]
pub(super) struct Permit {
    pub id: u64,
    pub scope: Scope,
    pub client: String,
    pub intent: Intent,
    pub expires_at: Instant,
    pub clip_save_origin: Option<super::clip_save::ApprovedSource>,
    pub clip_export_origin: Option<super::clip_export::ApprovedSource>,
    used: bool,
}
#[derive(Default)]
pub(super) struct Permits {
    next: u64,
    entries: VecDeque<Permit>,
}
impl Permits {
    pub fn clear(&mut self) {
        crate::clip_save_fence::invalidate(concat_host::projects::SaveInvalidation::Revoked);
        self.entries.clear();
    }
    /// 批准卡过期/轮次清理不撤销已经受理的原导出。
    pub fn forget(&mut self, id: u64) {
        crate::clip_save_fence::revoke_permit(id);
        self.entries.retain(|p| p.id != id);
    }
    pub fn revoke(&mut self, id: u64) {
        crate::clip_save_fence::revoke_permit(id);
        crate::clip_export_fence::revoke_permit(id);
        self.entries.retain(|p| p.id != id);
    }
    pub fn grant(
        &mut self,
        scope: Scope,
        client: String,
        intent: Intent,
        now: Instant,
    ) -> Option<u64> {
        self.grant_with_clip_save_origin(scope, client, intent, now, None)
    }
    /// Attach only a source captured by the trusted approval path. Ordinary
    /// permits have no source, so they cannot authorize a Save snapshot.
    pub fn grant_with_clip_save_origin(
        &mut self,
        scope: Scope,
        client: String,
        intent: Intent,
        now: Instant,
        clip_save_origin: Option<super::clip_save::ApprovedSource>,
    ) -> Option<u64> {
        self.grant_with_clip_origins(scope, client, intent, now, clip_save_origin, None)
    }
    pub fn grant_with_clip_origins(
        &mut self,
        scope: Scope,
        client: String,
        intent: Intent,
        now: Instant,
        clip_save_origin: Option<super::clip_save::ApprovedSource>,
        clip_export_origin: Option<super::clip_export::ApprovedSource>,
    ) -> Option<u64> {
        if clip_export_origin.is_some()
            && !intent.clip_action.as_ref().is_some_and(|action| {
                matches!(action.edit, concat_editor_mcp::ClipEdit::ExportVideo { .. })
            })
        {
            return None;
        }
        if clip_save_origin.is_some()
            && !intent
                .clip_action
                .as_ref()
                .is_some_and(|action| matches!(action.edit, concat_editor_mcp::ClipEdit::Save {}))
        {
            return None;
        }
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
            clip_save_origin,
            clip_export_origin,
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
                && p.clip_save_origin == permit.clip_save_origin
                && p.clip_export_origin == permit.clip_export_origin
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
    origin_capture: Option<OutputOriginCapture>,
    clip_export: Option<super::clip_export::Attachment>,
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
            origin_capture: None,
            clip_export: None,
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
            .map(|r| {
                r.clip_export.as_ref().map_or_else(
                    || r.response.clone(),
                    |attachment| attachment.observed(&r.response),
                )
            })
            .unwrap_or_else(|| Response::error("outcomeUnknown"))
    }
}

/// Opaque original UI account/conversation source; never a read authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OutputOriginCapture {
    pub account_scope: String,
    pub assistant_session: Option<String>,
}

/// Read authority derived solely from one successful original PNG receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OwnedOutput {
    pub query: OperationQuery,
    pub asset_id: String,
    pub version: concat_editor_mcp::AssetVersion,
    pub origin: OutputOriginCapture,
}

impl OwnedOutput {
    /// Match the original version before any content IO; never substitute the
    /// currently indexed version of an asset for the frozen exported version.
    pub fn matches_asset(&self, asset_id: &str, version: &concat_editor_mcp::AssetVersion) -> bool {
        self.asset_id == asset_id && self.version == *version
    }
}

/// A named permit remains necessary for every broad or filtered list query.
pub(super) fn owned_output_query(query: &LibraryQuery) -> bool {
    query.asset_id.as_ref().is_some_and(|id| !id.is_empty())
        && query.query.is_empty()
        && query.offset == 0
        && (1..=50).contains(&query.limit)
}

impl Receipts {
    /// Freeze once, before the original export worker starts. Retried registration
    /// may update its result but cannot replace this original account source.
    pub fn capture_export_origin(
        &mut self,
        scope: &Scope,
        client: &str,
        parameters: &CanvasParameters,
        origin: OutputOriginCapture,
    ) -> bool {
        if !matches!(parameters.action, CanvasAction::Export) {
            return false;
        }
        let Some(receipt) = self.entries.iter_mut().find(|r| {
            !r.clip
                && r.scope == *scope
                && r.client == client
                && r.sequence == parameters.client_sequence
                && r.operation_id == parameters.operation_id
        }) else {
            return false;
        };
        if let Some(previous) = &receipt.origin_capture {
            return previous == &origin;
        }
        if receipt.response.status != "outcomeUnknown" {
            return false;
        }
        receipt.origin_capture = Some(origin);
        true
    }

    /// Find only a currently owned, successful, exact immutable output.
    pub fn owned_canvas_output(
        &self,
        scope: &Scope,
        client: &str,
        asset_id: &str,
        origin: &OutputOriginCapture,
        now: Instant,
    ) -> Option<OwnedOutput> {
        self.entries.iter().rev().find_map(|r| {
            if r.clip
                || r.scope != *scope
                || r.client != client
                || now.saturating_duration_since(r.at) >= Duration::from_secs(86400)
                || r.origin_capture.as_ref() != Some(origin)
                || r.response.status != "ok"
            {
                return None;
            }
            let data = r.response.data.as_ref()?;
            if data["tool"].as_str() != Some("export_canvas_png")
                || data["appInstanceId"].as_str() != Some(scope.instance.as_str())
                || data["clientId"].as_str() != Some(client)
                || data["projectId"].as_str() != Some(scope.project.as_str())
                || data["documentSessionId"].as_str() != Some(scope.session.as_str())
                || data["operationId"].as_str() != Some(r.operation_id.as_str())
                || data["clientSequence"].as_u64() != Some(r.sequence)
                || data["registrationPending"].as_bool() != Some(false)
                || data["asset"]["assetId"].as_str() != Some(asset_id)
            {
                return None;
            }
            let version: concat_editor_mcp::AssetVersion =
                serde_json::from_value(data["asset"]["version"].clone()).ok()?;
            if data["pngSha256"].as_str() != Some(version.sha256.as_str()) {
                return None;
            }
            let synthetic = CanvasParameters {
                operation_id: r.operation_id.clone(),
                client_sequence: r.sequence,
                selection_revision: 0,
                action: CanvasAction::Insert {
                    asset_id: asset_id.into(),
                    version: version.clone(),
                },
            };
            concat_editor_mcp::validate_canvas_parameters(client, &scope.session, &synthetic)
                .ok()?;
            Some(OwnedOutput {
                query: OperationQuery {
                    operation_id: r.operation_id.clone(),
                    client_sequence: r.sequence,
                },
                asset_id: asset_id.into(),
                version,
                origin: origin.clone(),
            })
        })
    }

    /// Revalidate this same original operation after worker IO; another successful
    /// export of an asset cannot substitute for the frozen original operation.
    pub fn owned_output_valid(
        &self,
        scope: &Scope,
        client: &str,
        proof: &OwnedOutput,
        current: &OutputOriginCapture,
        now: Instant,
    ) -> bool {
        if &proof.origin != current {
            return false;
        }
        let response = self.query(scope, client, &proof.query, now);
        response.status == "ok"
            && self
                .owned_canvas_output(scope, client, &proof.asset_id, current, now)
                .is_some_and(|fresh| fresh == *proof)
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

#[cfg(test)]
mod owned_output_tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            instance: "app".into(),
            project: "project".into(),
            session: "document".into(),
        }
    }
    fn origin() -> OutputOriginCapture {
        OutputOriginCapture {
            account_scope: "opaque-account-a".into(),
            assistant_session: Some("opaque-conversation-a".into()),
        }
    }
    fn setup(status: &str) -> (Receipts, Scope, CanvasParameters, Response, Instant) {
        let now = Instant::now();
        let scope = scope();
        let mut ledger = Receipts::default();
        let parameters = CanvasParameters {
            operation_id: concat_editor_mcp::operation_id("client", &scope.session, 1).unwrap(),
            client_sequence: 1,
            selection_revision: 7,
            action: CanvasAction::Export,
        };
        assert!(ledger.reserve(scope.clone(), "client".into(), &parameters, now));
        assert!(ledger.capture_export_origin(&scope, "client", &parameters, origin()));
        let response = Response {
            status: status.into(),
            data: Some(json!({
                "appInstanceId":scope.instance,"clientId":"client","projectId":scope.project,
                "documentSessionId":scope.session,"operationId":parameters.operation_id,
                "clientSequence":1,"tool":"export_canvas_png","registrationPending":false,
                "pngSha256":"a".repeat(64),"asset":{"assetId":"asset-1","version":{
                    "sha256":"a".repeat(64),"registration_id":"opaque-registration"
                }}
            })),
        };
        assert!(ledger.finish(&scope, "client", &parameters, response.clone()));
        (ledger, scope, parameters, response, now)
    }
    #[test]
    fn pending_partial_and_unknown_have_no_owned_authority_but_original_recovery_can_finish() {
        for status in [
            "pending",
            "partial",
            "outcomeUnknown",
            "pendingRegistration",
        ] {
            let (mut ledger, scope, p, mut response, now) = setup(status);
            assert!(
                ledger
                    .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
                    .is_none()
            );
            response.status = "ok".into();
            ledger.update(
                &scope,
                "client",
                &OperationQuery {
                    operation_id: p.operation_id,
                    client_sequence: 1,
                },
                response,
            );
            assert!(
                ledger
                    .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
                    .is_some()
            );
            let other = OutputOriginCapture {
                account_scope: "other".into(),
                ..origin()
            };
            assert!(
                ledger
                    .owned_canvas_output(&scope, "client", "asset-1", &other, now)
                    .is_none()
            );
        }
    }
    #[test]
    fn client_scope_account_session_ttl_and_missing_original_capture_fail_closed() {
        let (mut ledger, scope, p, _, now) = setup("ok");
        assert!(
            Receipts::default()
                .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
                .is_none()
        );
        let proof = ledger
            .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
            .unwrap();
        assert!(proof.matches_asset("asset-1", &proof.version));
        for field in ["sha256", "registration", "previous"] {
            let mut changed = proof.version.clone();
            match field {
                "sha256" => changed.sha256 = "b".repeat(64),
                "registration" => changed.registration_id = "new".into(),
                _ => changed.previous_id = Some("prior".into()),
            }
            assert!(!proof.matches_asset("asset-1", &changed));
        }
        assert!(ledger.owned_output_valid(&scope, "client", &proof, &origin(), now));
        assert!(
            ledger
                .owned_canvas_output(&scope, "other", "asset-1", &origin(), now)
                .is_none()
        );
        assert!(
            ledger
                .owned_canvas_output(&scope, "client", "asset-other", &origin(), now)
                .is_none()
        );
        for field in ["instance", "project", "session"] {
            let mut other = scope.clone();
            match field {
                "instance" => other.instance = "other".into(),
                "project" => other.project = "other".into(),
                _ => other.session = "other".into(),
            }
            assert!(
                ledger
                    .owned_canvas_output(&other, "client", "asset-1", &origin(), now)
                    .is_none()
            );
        }
        for other in [
            OutputOriginCapture {
                account_scope: "other".into(),
                ..origin()
            },
            OutputOriginCapture {
                assistant_session: Some("other".into()),
                ..origin()
            },
        ] {
            assert!(!ledger.capture_export_origin(&scope, "client", &p, other.clone()));
            assert!(
                ledger
                    .owned_canvas_output(&scope, "client", "asset-1", &other, now)
                    .is_none()
            );
            assert!(!ledger.owned_output_valid(&scope, "client", &proof, &other, now));
        }
        assert!(
            ledger
                .owned_canvas_output(
                    &scope,
                    "client",
                    "asset-1",
                    &origin(),
                    now + Duration::from_secs(86400)
                )
                .is_none()
        );
        ledger.entries[0].origin_capture = None;
        assert!(!ledger.capture_export_origin(&scope, "client", &p, origin()));
        assert!(
            ledger
                .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
                .is_none()
        );
    }
    #[test]
    fn mismatched_receipt_identity_clip_family_and_changed_version_are_never_owned() {
        for (key, value) in [
            ("appInstanceId", json!("other")),
            ("clientId", json!("other")),
            ("projectId", json!("other")),
            ("documentSessionId", json!("other")),
            ("operationId", json!("other")),
            ("clientSequence", json!(2)),
            ("tool", json!("save_canvas_project")),
            ("pngSha256", json!("b".repeat(64))),
            ("registrationPending", json!(true)),
        ] {
            let (mut ledger, scope, p, mut response, now) = setup("ok");
            response.data.as_mut().unwrap()[key] = value;
            ledger.finish(&scope, "client", &p, response);
            assert!(
                ledger
                    .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
                    .is_none(),
                "{key}"
            );
        }
        let (mut ledger, scope, _, _, now) = setup("ok");
        ledger.entries[0].clip = true;
        assert!(
            ledger
                .owned_canvas_output(&scope, "client", "asset-1", &origin(), now)
                .is_none()
        );
    }
    #[test]
    fn only_exact_asset_lookup_can_request_owned_authority() {
        let exact = LibraryQuery {
            asset_id: Some("asset-1".into()),
            query: String::new(),
            offset: 0,
            limit: 20,
        };
        assert!(owned_output_query(&exact));
        assert!(!owned_output_query(&LibraryQuery {
            asset_id: None,
            ..exact.clone()
        }));
        assert!(!owned_output_query(&LibraryQuery {
            query: "asset".into(),
            ..exact.clone()
        }));
        assert!(!owned_output_query(&LibraryQuery { offset: 1, ..exact }));
    }
}

#[cfg(test)]
mod library_assets_approval_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn new_query_defaults_and_null_asset_id_normalize_to_the_same_exact_intent() {
        let minimal = json!({"projectId":"p","documentSessionId":"s"});
        let full = json!({"projectId":"p","documentSessionId":"s","kind":"all","assetId":null,"query":"","offset":0,"limit":20});
        assert_eq!(
            Intent::parse("library_assets", minimal),
            Intent::parse("library_assets", full)
        );
    }
    #[test]
    fn every_query_field_is_bound_to_original_one_shot_named_approval() {
        let args = json!({"projectId":"p","documentSessionId":"s","kind":"all","assetId":null,"query":"","offset":0,"limit":20});
        let intent = Intent::parse("library_assets", args.clone()).unwrap();
        let scope = Scope {
            instance: "i".into(),
            project: "p".into(),
            session: "s".into(),
        };
        let now = Instant::now();
        let mut permits = Permits::default();
        permits
            .grant(scope.clone(), "client".into(), intent.clone(), now)
            .unwrap();
        for (key, value) in [
            ("projectId", json!("other")),
            ("documentSessionId", json!("other")),
            ("kind", json!("video")),
            ("assetId", json!("asset-1")),
            ("query", json!("name")),
            ("offset", json!(1)),
            ("limit", json!(1)),
        ] {
            let mut changed = args.clone();
            changed[key] = value;
            assert!(
                permits
                    .claim(
                        &scope,
                        "client",
                        &Intent::parse("library_assets", changed).unwrap(),
                        now
                    )
                    .is_none(),
                "{key}"
            );
        }
        let permit = permits.claim(&scope, "client", &intent, now).unwrap();
        assert!(permits.valid(&permit, now));
        assert!(permits.claim(&scope, "client", &intent, now).is_none());
        permits.revoke(permit.id);
        assert!(!permits.valid(&permit, now));
    }
}

impl Receipts {
    pub fn attach_clip_export(
        &mut self,
        scope: &Scope,
        client: &str,
        parameters: &concat_editor_mcp::ClipParameters,
        attachment: super::clip_export::Attachment,
    ) -> bool {
        let Some(r) = self.entries.iter_mut().find(|r| {
            r.clip
                && r.scope == *scope
                && r.client == client
                && r.sequence == parameters.client_sequence
                && r.operation_id == parameters.operation_id
                && r.clip_export.is_none()
        }) else {
            return false;
        };
        r.clip_export = Some(attachment);
        true
    }
    pub fn clip_export(
        &self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        now: Instant,
    ) -> Option<&super::clip_export::Attachment> {
        self.entries
            .iter()
            .find(|r| {
                r.clip
                    && r.scope == *scope
                    && r.client == client
                    && r.sequence == query.client_sequence
                    && r.operation_id == query.operation_id
                    && now.saturating_duration_since(r.at) < Duration::from_secs(86400)
            })?
            .clip_export
            .as_ref()
    }
    pub fn clip_export_mut(
        &mut self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        now: Instant,
    ) -> Option<&mut super::clip_export::Attachment> {
        self.entries
            .iter_mut()
            .find(|r| {
                r.clip
                    && r.scope == *scope
                    && r.client == client
                    && r.sequence == query.client_sequence
                    && r.operation_id == query.operation_id
                    && now.saturating_duration_since(r.at) < Duration::from_secs(86400)
            })?
            .clip_export
            .as_mut()
    }
    pub fn complete_clip_export(
        &mut self,
        scope: &Scope,
        client: &str,
        query: &OperationQuery,
        output_id: &str,
        response: Response,
        material: Option<super::clip_export::worker::VerifiedMaterial>,
    ) -> bool {
        let Some(r) = self.entries.iter_mut().find(|r| {
            r.clip
                && r.scope == *scope
                && r.client == client
                && r.sequence == query.client_sequence
                && r.operation_id == query.operation_id
        }) else {
            return false;
        };
        let Some(attachment) = r
            .clip_export
            .as_mut()
            .filter(|a| a.output_id() == output_id)
        else {
            return false;
        };
        attachment.complete(material);
        r.response = response;
        true
    }
}
