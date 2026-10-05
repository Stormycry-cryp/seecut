// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local Agent reads and one scoped move from the UI-owned Studio. Grants live
//! only in this App process and are issued or revoked by the trusted native UI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use base64::Engine;
use concat_editor_mcp::{Method, MoveParameters, Request, Response};
use serde_json::{Value, json};
use slint::ComponentHandle;

use crate::host::Shell;
use crate::i18n::{t, tf};
use crate::panes::canvas::{CanvasMoveCheckedError, CanvasMoveError};
use crate::studio::Studio;
use crate::ui::{App, Editor, SeeCut};

#[path = "editor_mcp_endpoint.rs"]
mod endpoint_cleanup;

#[path = "editor_mcp_write_registry.rs"]
mod write_registry;
#[path = "editor_mcp_write_request.rs"]
mod write_request;

use write_registry::{Begin, LeaseStatus, OperationKey, RegistryError, Scope, WriteRegistry};
use write_request::{WriteRequest, WriteRequestOutcome};

static BOUND_ENDPOINT: OnceLock<endpoint_cleanup::BoundEndpoint> = OnceLock::new();

const MAX_PROJECT_OBJECTS: usize = 10_000;

fn bounded_object_count(total: usize, additional: usize) -> Result<usize, ()> {
    total
        .checked_add(additional)
        .filter(|&count| count <= MAX_PROJECT_OBJECTS)
        .ok_or(())
}

#[derive(Clone)]
struct DocumentIds {
    project: String,
    session: String,
}

struct Grant {
    token: String,
    project: String,
    media: bool,
}

pub const ASSISTANT_CLIENT_ID: &str = "seecut-assistant";

// Legacy Settings exposes external-client credentials for manual setup. The
// fixed assistant is configured through the trusted Rust host, never Slint.
fn settings_visible_token(client: &str, token: &str) -> String {
    if client == ASSISTANT_CLIENT_ID {
        String::new()
    } else {
        token.to_owned()
    }
}

#[derive(Clone, Copy)]
pub enum AssistantPermission {
    GrantRead,
    RevokeRead,
    GrantWrite,
    RenewWrite,
    RevokeWrite,
}

pub(crate) enum AssistantPermissionError {
    NotAllowed,
    WriterUnavailable,
}

impl AssistantPermissionError {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::NotAllowed => "权限状态已变化，请重新检查",
            Self::WriterUnavailable => "当前画布无法授权移动，请检查工程状态",
        }
    }
}

/// Ephemeral credentials for the trusted host only. Deliberately not Debug or
/// serializable; never publish either token to Slint, logs, or disk.
pub struct AssistantSnapshot {
    pub document: Option<crate::agent_controller::DocumentIdentity>,
    pub project_name: String,
    pub read_token: Option<String>,
    pub write_token: Option<String>,
    pub read_status: String,
    pub write_status: String,
    pub can_grant_read: bool,
    pub can_revoke_read: bool,
    pub can_grant_write: bool,
    pub can_renew_write: bool,
    pub can_revoke_write: bool,
}

// Pure registry policy shared by rendering and the trusted action preflight.
// In particular WriteRegistry allows replacing an expired foreign lease, but
// this fixed-client UI must not take it over, even after its deadline.
#[derive(Clone, Copy)]
struct AssistantPermissionContext<'a> {
    scope: Option<&'a Scope>,
    canvas: bool,
    write_ready: bool,
    now: Instant,
}

fn assistant_registry_snapshot<P: PartialEq + Clone, R: Clone>(
    context: AssistantPermissionContext<'_>,
    project_name: String,
    grants: &HashMap<String, Grant>,
    writes: &WriteRegistry<P, R>,
    write_token: &str,
) -> AssistantSnapshot {
    let AssistantPermissionContext {
        scope,
        canvas,
        write_ready,
        now,
    } = context;
    let grant = scope.and_then(|scope| {
        grants
            .get(ASSISTANT_CLIENT_ID)
            .filter(|grant| grant.project == scope.project)
    });
    let read_ready = grant.is_some_and(|grant| grant.media);
    let mut active = false;
    let mut expired = false;
    let mut conflict = false;
    let mut minutes = 0;
    match writes.lease_status(now) {
        LeaseStatus::Active {
            scope: lease_scope,
            client,
            remaining_minutes,
        } if Some(lease_scope) == scope => {
            active = client == ASSISTANT_CLIENT_ID;
            conflict = !active;
            minutes = remaining_minutes;
        }
        LeaseStatus::Expired {
            scope: lease_scope,
            client,
        } if Some(lease_scope) == scope => {
            expired = client == ASSISTANT_CLIENT_ID;
            conflict = !expired;
        }
        _ => {}
    }
    let writable = scope.is_some() && canvas && read_ready && write_ready;
    AssistantSnapshot {
        document: scope.map(|scope| crate::agent_controller::DocumentIdentity {
            instance_id: scope.instance.clone(),
            project_id: scope.project.clone(),
            document_session_id: scope.session.clone(),
        }),
        project_name: if scope.is_some() {
            project_name
        } else {
            String::new()
        },
        read_token: grant
            .filter(|grant| grant.media)
            .map(|grant| grant.token.clone()),
        write_token: if canvas && read_ready && active && !write_token.is_empty() {
            Some(write_token.to_owned())
        } else {
            None
        },
        read_status: if scope.is_none() {
            "请打开工程"
        } else if read_ready {
            "已允许读取与预览"
        } else {
            "未允许读取与预览"
        }
        .into(),
        write_status: if scope.is_none() {
            "请打开画布工程".into()
        } else if !canvas {
            "当前工程不支持移动".into()
        } else if conflict {
            "移动权限由其他客户端持有".into()
        } else if !read_ready {
            "请先允许读取与预览".into()
        } else if active {
            format!("已允许移动 · 剩余{minutes}分钟")
        } else if expired {
            "移动权限已到期，请续期".into()
        } else if !write_ready {
            "当前画布暂不可移动".into()
        } else {
            "未允许移动".into()
        },
        can_grant_read: scope.is_some(),
        can_revoke_read: grant.is_some(),
        can_grant_write: writable && !active && !expired && !conflict,
        can_renew_write: writable && (active || expired),
        can_revoke_write: canvas && (active || expired),
    }
}

enum AssistantWriteChange {
    None,
    Issued,
    Revoked,
}

// No App, Settings client draft, file system, or logging access. The caller
// supplies freshly synchronized identity/readiness and generates a token only
// after preflight. Return None when a stale/forged action is not permitted.
fn assistant_registry_permission<P: PartialEq + Clone, R: Clone>(
    context: AssistantPermissionContext<'_>,
    grants: &mut HashMap<String, Grant>,
    writes: &mut WriteRegistry<P, R>,
    write_token: &mut String,
    action: AssistantPermission,
    fresh_token: impl FnOnce() -> String,
) -> Option<AssistantWriteChange> {
    let AssistantPermissionContext {
        scope,
        canvas,
        write_ready,
        now,
    } = context;
    let snapshot = assistant_registry_snapshot(
        AssistantPermissionContext {
            scope,
            canvas,
            write_ready,
            now,
        },
        String::new(),
        grants,
        writes,
        write_token,
    );
    let allowed = match action {
        AssistantPermission::GrantRead => snapshot.can_grant_read,
        AssistantPermission::RevokeRead => snapshot.can_revoke_read,
        AssistantPermission::GrantWrite => snapshot.can_grant_write,
        AssistantPermission::RenewWrite => snapshot.can_renew_write,
        AssistantPermission::RevokeWrite => snapshot.can_revoke_write,
    };
    if !allowed {
        return None;
    }
    let scope = scope?;
    match action {
        AssistantPermission::GrantRead => {
            grants.insert(
                ASSISTANT_CLIENT_ID.into(),
                Grant {
                    token: fresh_token(),
                    project: scope.project.clone(),
                    media: true,
                },
            );
            Some(AssistantWriteChange::None)
        }
        AssistantPermission::RevokeRead => {
            grants.remove(ASSISTANT_CLIENT_ID);
            if snapshot.can_revoke_write {
                writes.revoke();
                write_token.clear();
                Some(AssistantWriteChange::Revoked)
            } else {
                Some(AssistantWriteChange::None)
            }
        }
        AssistantPermission::GrantWrite | AssistantPermission::RenewWrite => {
            let token = fresh_token();
            let result = match action {
                AssistantPermission::GrantWrite => {
                    writes.trusted_grant(scope, ASSISTANT_CLIENT_ID, token.clone(), now)
                }
                _ => writes.trusted_renew(scope, ASSISTANT_CLIENT_ID, token.clone(), now),
            };
            result.ok()?;
            *write_token = token;
            Some(AssistantWriteChange::Issued)
        }
        AssistantPermission::RevokeWrite => {
            writes.revoke();
            write_token.clear();
            Some(AssistantWriteChange::Revoked)
        }
    }
}

fn active_document_kind(
    page: i32,
    gallery: bool,
    on_start: bool,
    canvas_open: bool,
    clip_open: bool,
) -> Option<&'static str> {
    match page {
        6 if !gallery && canvas_open => Some("canvas"),
        0 if !on_start && clip_open => Some("clip"),
        _ => None,
    }
}

fn bounded_name(name: &str) -> String {
    name.chars().take(256).collect()
}

fn canvas_objects(
    document: &concat_canvas::ImageDocument,
    offset: usize,
    limit: usize,
) -> Result<(usize, Vec<Value>), ()> {
    enum Entry<'a> {
        Group(
            &'a concat_canvas::LayerGroup,
            Option<concat_canvas::LayerId>,
        ),
        Leaf(&'a concat_canvas::LayerNode, concat_canvas::LayerId),
    }
    let mut stack = vec![Entry::Group(&document.root, None)];
    let mut rows = Vec::with_capacity(limit);
    let mut total = 0usize;
    while let Some(entry) = stack.pop() {
        let included = total >= offset && total < offset.saturating_add(limit);
        total = bounded_object_count(total, 1)?;
        match entry {
            Entry::Group(group, parent) => {
                // Account for pending siblings before allocating any child entries.
                bounded_object_count(
                    bounded_object_count(total, stack.len())?,
                    group.children.len(),
                )?;
                if included {
                    rows.push(json!({"kind":"group", "id":format!("layer:{}", group.id.as_u64()),
                        "parentId":parent.map(|id| format!("layer:{}", id.as_u64())),
                        "name":bounded_name(&group.name), "hidden":group.hidden,
                        "opacity":group.opacity, "blend":group.blend, "hasMask":group.mask.is_some()}));
                }
                for child in group.children.iter().rev() {
                    match child {
                        concat_canvas::LayerNode::Group(nested) => {
                            stack.push(Entry::Group(nested, Some(group.id)))
                        }
                        _ => stack.push(Entry::Leaf(child, group.id)),
                    }
                }
            }
            Entry::Leaf(node, parent) if included => {
                let parent = format!("layer:{}", parent.as_u64());
                match node {
                    concat_canvas::LayerNode::Layer(layer) => rows.push(json!({
                        "kind":"image", "id":format!("layer:{}", layer.id.as_u64()),
                        "parentId":parent, "name":bounded_name(&layer.name),
                        "hidden":layer.hidden, "opacity":layer.opacity, "blend":layer.blend,
                        "hasMask":layer.mask.is_some(), "transform":layer.transform,
                        "clipsTo":layer.clips_to.map(|id| format!("layer:{}", id.as_u64())),
                        "sampling":layer.sampling,
                    })),
                    concat_canvas::LayerNode::Adjustment(adjustment) => rows.push(json!({
                        "kind":"adjustment", "id":format!("layer:{}", adjustment.id.as_u64()),
                        "parentId":parent, "name":bounded_name(&adjustment.name),
                        "hidden":adjustment.hidden, "opacity":adjustment.opacity,
                        "blend":adjustment.blend, "hasMask":adjustment.mask.is_some(),
                        "parameters":adjustment.adjustment,
                    })),
                    concat_canvas::LayerNode::Group(_) => unreachable!(),
                }
            }
            Entry::Leaf(_, _) => {}
        }
    }
    Ok((total, rows))
}

enum ClipObject<'a> {
    Media(&'a concat_project::model::MediaItem),
    Timeline(&'a concat_project::model::Timeline),
    Track(
        &'a concat_project::model::Track,
        &'a concat_project::model::Timeline,
    ),
    Clip(
        &'a concat_project::model::Clip,
        &'a concat_project::model::Timeline,
    ),
}

impl ClipObject<'_> {
    fn into_value(self) -> Value {
        match self {
            Self::Media(item) => json!({
                "kind":"media", "id":format!("media:{}", item.id),
                "name":bounded_name(&item.name), "duration":item.duration,
            }),
            Self::Timeline(timeline) => json!({
                "kind":"timeline", "id":format!("timeline:{}", timeline.id),
                "name":bounded_name(&timeline.name), "video":timeline.video,
            }),
            Self::Track(track, timeline) => json!({
                "kind":"track", "id":format!("track:{}", track.id),
                "parentId":format!("timeline:{}", timeline.id),
                "visible":track.visible, "muted":track.muted,
            }),
            Self::Clip(clip, timeline) => json!({
                "kind":"clip", "id":format!("clip:{}", clip.id),
                "parentId":format!("track:{}", clip.track_id),
                "timelineId":format!("timeline:{}", timeline.id),
                "name":bounded_name(&clip.name), "mediaId":format!("media:{}", clip.media_id),
                "start":clip.start, "duration":clip.duration,
                "sourceStart":clip.source_start, "opacity":clip.opacity,
                "scale":clip.scale, "rotation":clip.rotation, "volume":clip.volume,
                "reverse":clip.reverse,
            }),
        }
    }
}

fn clip_object_page(
    project: &concat_project::model::Project,
    offset: usize,
    limit: usize,
) -> impl Iterator<Item = ClipObject<'_>> {
    project
        .media
        .iter()
        .map(ClipObject::Media)
        .chain(project.timelines.iter().flat_map(|timeline| {
            std::iter::once(ClipObject::Timeline(timeline))
                .chain(
                    timeline
                        .tracks
                        .iter()
                        .map(|track| ClipObject::Track(track, timeline)),
                )
                .chain(
                    timeline
                        .clips
                        .iter()
                        .map(|clip| ClipObject::Clip(clip, timeline)),
                )
        }))
        .skip(offset)
        .take(limit)
}

fn clip_objects(
    project: &concat_project::model::Project,
    offset: usize,
    limit: usize,
) -> Result<(usize, Vec<Value>), ()> {
    let mut total = bounded_object_count(project.media.len(), project.timelines.len())?;
    for timeline in &project.timelines {
        total = bounded_object_count(total, timeline.tracks.len())?;
        total = bounded_object_count(total, timeline.clips.len())?;
    }
    let rows = clip_object_page(project, offset, limit)
        .map(ClipObject::into_value)
        .collect();
    Ok((total, rows))
}

#[derive(Clone, Debug, PartialEq)]
struct MoveFingerprint {
    revision: u64,
    parameters: MoveParameters,
}

pub(crate) struct WriteView {
    pub project_name: String,
    pub project_path: String,
    pub client: String,
    pub status: String,
    pub reason: String,
    pub token: String,
    pub can_grant: bool,
    pub can_renew: bool,
    pub can_revoke: bool,
}

/// App-side, ephemeral grant and session registry.
pub(crate) struct BridgeUi {
    pub instance_id: String,
    pub socket_status: String,
    pub client_draft: String,
    grants: HashMap<String, Grant>,
    canvas_ids: Option<(String, DocumentIds)>,
    clip_ids: Option<(String, DocumentIds)>,
    active_canvas: bool,
    active_clip: bool,
    writes: WriteRegistry<MoveFingerprint, Response>,
    write_token: String,
    write_message: String,
    write_revoked: bool,
    write_timer: slint::Timer,
}

impl BridgeUi {
    pub(crate) fn assistant_snapshot(&mut self, studio: &Studio, app: &App) -> AssistantSnapshot {
        let active = self.active_ids(studio, app);
        let scope = active.as_ref().map(|(_, ids)| self.scope(ids));
        let canvas = active.as_ref().is_some_and(|(kind, _)| kind == "canvas");
        let project_name = match active.as_ref().map(|(kind, _)| kind.as_str()) {
            Some("canvas") => studio.canvas.name.clone(),
            Some("clip") => studio.project_name.clone(),
            _ => String::new(),
        };
        // A 40ms render tick must not validate filesystem ownership. This only
        // enables an explicit request; permission and writes recheck ownership.
        let ready = canvas
            && !modal_busy(studio, app, false)
            && studio.canvas.mcp_move_permission_request_ready().is_ok();
        assistant_registry_snapshot(
            AssistantPermissionContext {
                scope: scope.as_ref(),
                canvas,
                write_ready: ready,
                now: Instant::now(),
            },
            project_name,
            &self.grants,
            &self.writes,
            &self.write_token,
        )
    }

    /// Called only by explicit trusted native assistant permission callbacks.
    /// Recheck the live document, modal/owner readiness and fixed-client policy;
    /// neither a model request nor a Settings client draft can authorize this.
    pub(crate) fn assistant_permission(
        &mut self,
        studio: &Studio,
        app: &App,
        action: AssistantPermission,
    ) -> Result<(), AssistantPermissionError> {
        let active = self.active_ids(studio, app);
        let scope = active.as_ref().map(|(_, ids)| self.scope(ids));
        let canvas = active.as_ref().is_some_and(|(kind, _)| kind == "canvas");
        let ready = if matches!(
            action,
            AssistantPermission::GrantWrite | AssistantPermission::RenewWrite
        ) {
            self.grant_ready(studio, app)
                .map_err(|_| AssistantPermissionError::WriterUnavailable)?;
            true
        } else {
            // Reading and revoking do not inspect an unrelated writer's files.
            false
        };
        match assistant_registry_permission(
            AssistantPermissionContext {
                scope: scope.as_ref(),
                canvas,
                write_ready: ready,
                now: Instant::now(),
            },
            &mut self.grants,
            &mut self.writes,
            &mut self.write_token,
            action,
            || uuid::Uuid::new_v4().to_string(),
        ) {
            Some(AssistantWriteChange::Issued) => {
                self.write_revoked = false;
                self.write_message.clear();
                self.refresh_write_minutes();
            }
            Some(AssistantWriteChange::Revoked) => {
                self.write_revoked = true;
                self.write_message.clear();
                self.write_timer.stop();
            }
            Some(AssistantWriteChange::None) => {}
            None => return Err(AssistantPermissionError::NotAllowed),
        }
        Ok(())
    }

    pub fn new() -> Self {
        Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            socket_status: "Starting".into(),
            client_draft: String::new(),
            grants: HashMap::new(),
            canvas_ids: None,
            clip_ids: None,
            active_canvas: false,
            active_clip: false,
            writes: WriteRegistry::new(),
            write_token: String::new(),
            write_message: String::new(),
            write_revoked: false,
            write_timer: slint::Timer::default(),
        }
    }

    fn ids(&mut self, kind: &str, generation: String) -> DocumentIds {
        let slot = if kind == "canvas" {
            &mut self.canvas_ids
        } else {
            &mut self.clip_ids
        };
        if slot.as_ref().is_none_or(|(old, _)| *old != generation) {
            if let Some((_, old)) = slot.take() {
                self.grants.retain(|_, grant| grant.project != old.project);
            }
            *slot = Some((
                generation,
                DocumentIds {
                    project: uuid::Uuid::new_v4().to_string(),
                    session: uuid::Uuid::new_v4().to_string(),
                },
            ));
        }
        slot.as_ref().unwrap().1.clone()
    }

    fn active_ids(&mut self, studio: &Studio, app: &App) -> Option<(String, DocumentIds)> {
        if studio.canvas.document.is_some() {
            self.ids(
                "canvas",
                format!(
                    "{}:{}",
                    studio.canvas.mcp_state().0,
                    studio.canvas.mcp_binding_epoch()
                ),
            );
        } else if let Some((_, old)) = self.canvas_ids.take() {
            self.grants.retain(|_, grant| grant.project != old.project);
        }
        if studio.session.is_some() {
            self.ids("clip", studio.mcp_clip_state().0.to_string());
        } else if let Some((_, old)) = self.clip_ids.take() {
            self.grants.retain(|_, grant| grant.project != old.project);
        }
        let page = app.global::<SeeCut>().get_page();
        let kind = active_document_kind(
            page,
            app.global::<SeeCut>().get_canvas_gallery_open(),
            studio.on_start,
            studio.canvas.document.is_some(),
            studio.session.is_some(),
        );
        self.sync_workspace(kind);
        let scope = self.canvas_ids.as_ref().map(|(_, ids)| self.scope(ids));
        self.writes.bind_scope(scope);
        if !matches!(
            self.writes.lease_status(Instant::now()),
            LeaseStatus::Active { .. }
        ) {
            self.write_token.clear();
        }
        if kind == Some("canvas") {
            return self
                .canvas_ids
                .as_ref()
                .map(|(_, ids)| ("canvas".into(), ids.clone()));
        }
        if kind == Some("clip") {
            return self
                .clip_ids
                .as_ref()
                .map(|(_, ids)| ("clip".into(), ids.clone()));
        }
        None
    }

    fn sync_workspace(&mut self, kind: Option<&str>) {
        if self.active_canvas && kind != Some("canvas") {
            // Leaving the granted workspace withdraws E, but preserves this
            // document session's sequence/results so returning cannot replay an edit.
            self.revoke_move();
        }
        self.active_canvas = kind == Some("canvas");
        self.active_clip = kind == Some("clip");
    }

    fn scope(&self, ids: &DocumentIds) -> Scope {
        Scope {
            instance: self.instance_id.clone(),
            project: ids.project.clone(),
            session: ids.session.clone(),
        }
    }

    /// Refresh identities before publishing Settings. This never grants access.
    pub(crate) fn sync_document(&mut self, studio: &Studio, app: &App) {
        let previous = self.canvas_ids.as_ref().map(|(_, ids)| ids.session.clone());
        self.active_ids(studio, app);
        if previous != self.canvas_ids.as_ref().map(|(_, ids)| ids.session.clone()) {
            self.write_message.clear();
            self.write_revoked = false;
            self.write_timer.stop();
        }
    }

    fn current_scope(&self) -> Option<Scope> {
        self.canvas_ids.as_ref().map(|(_, ids)| self.scope(ids))
    }

    fn current_lease_status(&self, now: Instant) -> LeaseStatus<'_> {
        let status = self.writes.lease_status(now);
        match &status {
            LeaseStatus::Active { scope, .. } | LeaseStatus::Expired { scope, .. }
                if self.current_scope().as_ref() != Some(scope) =>
            {
                LeaseStatus::Unauthorized
            }
            _ => status,
        }
    }

    fn grant_ready(&self, studio: &Studio, app: &App) -> Result<Scope, String> {
        if !self.active_canvas {
            return Err(t("No canvas project"));
        }
        if modal_busy(studio, app, false) {
            return Err(t("Canvas is busy"));
        }
        studio
            .canvas
            .mcp_writer_ready()
            .map_err(|error| canvas_ui_reason(&error))?;
        self.current_scope().ok_or_else(|| t("No canvas project"))
    }

    fn refresh_write_minutes(&self) {
        self.write_timer
            .start(slint::TimerMode::Repeated, Duration::from_secs(60), || {
                Shell::with(|shell, app| {
                    let studio = shell.studio.borrow();
                    if studio.settings.open {
                        studio.publish(&app, &shell.models);
                    }
                });
            });
    }

    /// Only the trusted Settings button calls this; reads cannot obtain a lease.
    pub(crate) fn grant_move(&mut self, studio: &Studio, app: &App) {
        self.sync_document(studio, app);
        let result = (|| {
            let scope = self.grant_ready(studio, app)?;
            let client = self.client_draft.trim();
            if client.is_empty() || client.len() > 64 {
                return Err(t("Enter a client ID (up to 64 bytes)"));
            }
            let token = uuid::Uuid::new_v4().to_string();
            self.writes
                .trusted_grant(&scope, client, token.clone(), Instant::now())
                .map_err(|error| self.grant_error(error))?;
            self.write_token = token;
            self.write_revoked = false;
            self.refresh_write_minutes();
            Ok(())
        })();
        self.write_message = result.err().unwrap_or_default();
    }

    /// Renew the actual lease owner, even if the client draft has since changed.
    pub(crate) fn renew_move(&mut self, studio: &Studio, app: &App) {
        self.sync_document(studio, app);
        let result = (|| {
            let scope = self.grant_ready(studio, app)?;
            let client = match self.current_lease_status(Instant::now()) {
                LeaseStatus::Active { client, .. } | LeaseStatus::Expired { client, .. } => {
                    client.to_owned()
                }
                LeaseStatus::Unauthorized => return Err(t("No edit grant")),
            };
            let token = uuid::Uuid::new_v4().to_string();
            self.writes
                .trusted_renew(&scope, &client, token.clone(), Instant::now())
                .map_err(|error| self.grant_error(error))?;
            self.write_token = token;
            self.write_revoked = false;
            self.refresh_write_minutes();
            Ok(())
        })();
        self.write_message = result.err().unwrap_or_default();
    }

    pub(crate) fn revoke_move(&mut self) {
        self.writes.revoke();
        self.write_token.clear();
        self.write_timer.stop();
        self.write_message.clear();
        self.write_revoked = true;
    }

    fn grant_error(&self, error: RegistryError) -> String {
        match error {
            RegistryError::ClientConflict | RegistryError::AlreadyGranted => {
                let client = match self.current_lease_status(Instant::now()) {
                    LeaseStatus::Active { client, .. } | LeaseStatus::Expired { client, .. } => {
                        client
                    }
                    LeaseStatus::Unauthorized => "",
                };
                tf("Editing is already allowed for {0}", &[&client])
            }
            _ => t("Unable to grant editing access"),
        }
    }

    pub(crate) fn write_view(&self, studio: &Studio) -> WriteView {
        let ready_reason = if !self.active_canvas {
            Some(t("No canvas project"))
        } else if studio.settings.open && studio.settings.tab == 4 {
            studio
                .canvas
                .mcp_writer_ready()
                .err()
                .map(|error| canvas_ui_reason(&error))
        } else {
            None
        };
        let draft = self.client_draft.trim();
        let draft_valid = !draft.is_empty() && draft.len() <= 64;
        let (client, status, can_grant, can_renew, can_revoke, token) =
            match self.current_lease_status(Instant::now()) {
                LeaseStatus::Unauthorized => (
                    draft.to_owned(),
                    if self.write_revoked {
                        t("Editing access revoked")
                    } else {
                        t("No edit grant")
                    },
                    draft_valid,
                    false,
                    false,
                    String::new(),
                ),
                LeaseStatus::Active {
                    client,
                    remaining_minutes,
                    ..
                } => (
                    client.to_owned(),
                    tf("Granted · {0} min remaining", &[&remaining_minutes]),
                    false,
                    true,
                    true,
                    settings_visible_token(client, &self.write_token),
                ),
                LeaseStatus::Expired { client, .. } => (
                    client.to_owned(),
                    t("Expired"),
                    false,
                    true,
                    true,
                    String::new(),
                ),
            };
        let reason = ready_reason
            .clone()
            .or_else(|| {
                if !self.write_message.is_empty() {
                    Some(self.write_message.clone())
                } else if can_revoke && can_renew && client != draft && !token.is_empty() {
                    Some(tf("Editing is already allowed for {0}", &[&client]))
                } else if !draft_valid && !can_revoke {
                    Some(t("Enter a client ID (up to 64 bytes)"))
                } else {
                    None
                }
            })
            .unwrap_or_default();
        WriteView {
            project_name: if self.active_canvas {
                studio.canvas.name.clone()
            } else if self.active_clip {
                studio.project_name.clone()
            } else {
                String::new()
            },
            project_path: if self.active_canvas {
                studio
                    .canvas
                    .mcp_project_path()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else if self.active_clip {
                studio
                    .session
                    .as_ref()
                    .map(|session| session.path().to_owned())
                    .unwrap_or_default()
            } else {
                String::new()
            },
            client,
            status,
            reason,
            token,
            can_grant: can_grant && ready_reason.is_none(),
            can_renew: can_renew && ready_reason.is_none(),
            can_revoke,
        }
    }

    /// The user enters a client name in Settings and grants only the currently
    /// active project. Regranting rotates its token; revoke deletes it.
    pub fn grant(&mut self, studio: &Studio, app: &App, media: bool) {
        let client = self.client_draft.trim();
        if client.is_empty() || client.len() > 64 {
            return;
        }
        let client = client.to_owned();
        let Some((_, ids)) = self.active_ids(studio, app) else {
            return;
        };
        self.grants.insert(
            client,
            Grant {
                token: uuid::Uuid::new_v4().to_string(),
                project: ids.project,
                media,
            },
        );
    }

    pub fn revoke(&mut self) {
        self.grants.remove(self.client_draft.trim());
    }

    pub fn grant_view(&self) -> (String, String) {
        match self.grants.get(self.client_draft.trim()) {
            Some(grant) => (
                if grant.media {
                    t("Read structure and preview (R + M)")
                } else {
                    t("Read structure (R)")
                },
                settings_visible_token(self.client_draft.trim(), &grant.token),
            ),
            None => (t("No grant"), String::new()),
        }
    }

    fn authorized(&self, request: &Request, project: &str, media: bool) -> bool {
        self.grants.get(&request.client_id).is_some_and(|grant| {
            grant.token == request.client_token
                && grant.project == project
                && (!media || grant.media)
        })
    }

    fn handle(&mut self, request: &Request, studio: &Studio, app: &App) -> UiReply {
        if self.instance_id != request.instance_id {
            return UiReply::Response(Response::error("appUnavailable"));
        }
        let active = self.active_ids(studio, app);
        let Some((kind, ids)) = active else {
            if self
                .grants
                .get(&request.client_id)
                .is_none_or(|grant| grant.token != request.client_token)
            {
                return UiReply::Response(Response::error("notAuthorized"));
            }
            return UiReply::Response(if matches!(request.method, Method::Context) {
                Response::ok(
                    json!({"appInstanceId": self.instance_id, "activeWorkspace": "none", "documents": []}),
                )
            } else {
                Response::error("wrongProject")
            });
        };
        if !self.authorized(request, &ids.project, false) {
            return UiReply::Response(Response::error("notAuthorized"));
        }
        if matches!(request.method, Method::Capabilities) {
            let mut granted = vec!["R"];
            if self
                .grants
                .get(&request.client_id)
                .is_some_and(|grant| grant.media)
            {
                granted.push("M");
            }
            let edit_minutes = match self.current_lease_status(Instant::now()) {
                LeaseStatus::Active {
                    client,
                    remaining_minutes,
                    scope,
                } if kind == "canvas"
                    && client == request.client_id
                    && scope.project == ids.project =>
                {
                    granted.push("E");
                    Some(remaining_minutes)
                }
                _ => None,
            };
            return UiReply::Response(Response::ok(json!({
                "appInstanceId": self.instance_id,
                "tools": ["capabilities", "context", "project", "preview", "move_selected_image"],
                "permissions": ["R", "M", "E"], "readOnly": false,
                "writeAction": "moveSelectedImage", "writeLeaseSeconds": 300,
                "writeWorkspace": "canvas", "maxMoveDelta": 32768, "maxMovePosition": 1_000_000,
                "writeAuthorization": "trustedUI", "writeTokenEnvironment": "SEECUT_MCP_WRITE_TOKEN",
                "writeGrantRequired": true, "grantedPermissions": granted, "editLeaseRemainingMinutes": edit_minutes,
                "maxPageSize": 100, "maxProjectObjects": MAX_PROJECT_OBJECTS,
                "previewWorkspace": "canvas", "maxPreviewEdge": 256,
                "maxPreviewSourcePixels": 4194304,
            })));
        }
        let (generation, revision, busy) = if kind == "canvas" {
            studio.canvas.mcp_state()
        } else {
            let (generation, revision, _, busy) = studio.mcp_clip_state();
            (generation, revision, busy)
        };
        let context_revision = studio.mcp_context_epoch();
        let dirty = if kind == "canvas" {
            studio.canvas.is_modified()
        } else {
            studio.mcp_clip_state().2
        };
        if matches!(request.method, Method::Context) {
            let selected = if kind == "canvas" {
                studio
                    .canvas
                    .active
                    .map(|id| format!("layer:{}", id.as_u64()))
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                studio
                    .selection
                    .iter()
                    .map(|id| format!("clip:{id}"))
                    .collect()
            };
            return UiReply::Response(Response::ok(json!({
                "appInstanceId": self.instance_id, "activeWorkspace": kind,
                "documents": [{"kind": kind, "projectId": ids.project,
                    "documentSessionId": ids.session, "revision": revision,
                    "contextRevision": context_revision, "selectionRevision": studio.mcp_selection_epoch(),
                    "selectedObjectIds": selected,
                    "dirty": dirty, "busy": if busy { Some("gesture") } else { None }}]
            })));
        }
        if request.project_id.as_deref() != Some(&ids.project)
            || request.document_session_id.as_deref() != Some(&ids.session)
        {
            return UiReply::Response(Response::error("wrongProject"));
        }
        if matches!(request.method, Method::Project) {
            if busy {
                return UiReply::Response(Response::error("busy"));
            }
            let offset = request.offset.unwrap_or(0);
            let limit = request.limit.unwrap_or(50);
            if limit == 0 || limit > 100 || offset > 1_000_000 {
                return UiReply::Response(Response::error("invalidInput"));
            }
            let (total, objects, dimensions) = if kind == "canvas" {
                let document = studio.canvas.document.as_ref().unwrap();
                let (total, objects) = match canvas_objects(document, offset, limit) {
                    Ok(page) => page,
                    Err(()) => return UiReply::Response(Response::error("resourceLimit")),
                };
                (
                    total,
                    objects,
                    json!({"width": document.width, "height": document.height}),
                )
            } else {
                let project = studio.session.as_ref().unwrap().project();
                let (total, objects) = match clip_objects(project, offset, limit) {
                    Ok(page) => page,
                    Err(()) => return UiReply::Response(Response::error("resourceLimit")),
                };
                (total, objects, json!(project.active().video))
            };
            return UiReply::Response(Response::ok(json!({
                "projectId": ids.project, "documentSessionId": ids.session,
                "revision": revision, "contextRevision": context_revision,
                "generation": generation, "total": total, "offset": offset,
                "objects": objects, "dimensions": dimensions,
            })));
        }
        if kind != "canvas" {
            return UiReply::Response(Response::error("unsupportedPreview"));
        }
        if !self.authorized(request, &ids.project, true) {
            return UiReply::Response(Response::error("notAuthorized"));
        }
        if busy {
            return UiReply::Response(Response::error("busy"));
        }
        if !concat_editor_mcp::preview_matches(
            request,
            &ids.project,
            &ids.session,
            revision,
            context_revision,
        ) {
            return UiReply::Response(Response::error("stalePreview"));
        }
        let max_edge = request.max_edge.unwrap_or(256);
        if max_edge == 0 || max_edge > 256 {
            return UiReply::Response(Response::error("invalidInput"));
        }
        let document = studio.canvas.document.as_ref().unwrap();
        if u64::from(document.width) * u64::from(document.height) > 4_194_304 {
            return UiReply::Response(Response::error("unsupportedPreview"));
        }
        UiReply::Preview(Box::new(PreviewJob {
            request: request.clone(),
            ids,
            revision,
            context_revision,
            max_edge,
            document: document.clone(),
            store: studio.canvas.store.clone(),
        }))
    }
}

struct PreviewJob {
    request: Request,
    ids: DocumentIds,
    revision: u64,
    context_revision: u64,
    max_edge: u32,
    document: concat_canvas::ImageDocument,
    store: concat_canvas::PixelStore,
}

enum UiReply {
    Response(Response),
    Preview(Box<PreviewJob>),
}

fn preview_slot() -> &'static Mutex<()> {
    static PREVIEW: OnceLock<Mutex<()>> = OnceLock::new();
    PREVIEW.get_or_init(|| Mutex::new(()))
}

fn on_ui<T: Send + 'static>(
    body: impl FnOnce(&Shell, App) -> T + Send + 'static,
) -> Result<T, Response> {
    let (sender, receiver) = mpsc::sync_channel(1);
    slint::invoke_from_event_loop(move || {
        Shell::with(|shell, app| {
            let _ = sender.send(body(shell, app));
        });
    })
    .map_err(|_| Response::error("appUnavailable"))?;
    receiver
        .recv_timeout(Duration::from_millis(750))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => Response::error("busy"),
            mpsc::RecvTimeoutError::Disconnected => Response::error("appUnavailable"),
        })
}

fn registry_status(error: RegistryError) -> &'static str {
    match error {
        RegistryError::ScopeMismatch => "wrongProject",
        RegistryError::Unauthorized
        | RegistryError::AlreadyGranted
        | RegistryError::ClientConflict => "notAuthorized",
        RegistryError::LeaseExpired => "leaseExpired",
        RegistryError::InvalidInput
        | RegistryError::InvalidSequence
        | RegistryError::ParameterMismatch => "invalidInput",
        RegistryError::OutcomeUnknown => "outcomeUnknown",
    }
}

fn canvas_status(error: &CanvasMoveError) -> &'static str {
    use concat_host::ownership::OwnershipError;
    match error {
        CanvasMoveError::UnsupportedPlatform => "unsupportedPlatform",
        CanvasMoveError::NoDocument => "wrongProject",
        CanvasMoveError::Busy | CanvasMoveError::UnstableBinding => "busy",
        CanvasMoveError::MissingOwner => "ownershipUnavailable",
        CanvasMoveError::OwnerMismatch => "ownershipChanged",
        CanvasMoveError::Ownership(error) => match error {
            OwnershipError::Conflict { .. } => "ownershipConflict",
            OwnershipError::IdentityAmbiguous { .. } => "ownershipChanged",
            _ => "ownershipUnavailable",
        },
        CanvasMoveError::SelectionChanged => "selectionChanged",
        CanvasMoveError::ObjectGone | CanvasMoveError::MissingPixels => "objectGone",
        CanvasMoveError::PixelSelection
        | CanvasMoveError::MaskTarget
        | CanvasMoveError::NonImageTarget => "unsupportedTarget",
        CanvasMoveError::InvalidDelta
        | CanvasMoveError::InvalidTransform
        | CanvasMoveError::InvalidAnchor => "invalidInput",
    }
}

fn canvas_ui_reason(error: &CanvasMoveError) -> String {
    match error {
        CanvasMoveError::UnsupportedPlatform => t("This platform cannot edit local projects"),
        CanvasMoveError::NoDocument => t("No canvas project"),
        CanvasMoveError::Busy => t("Canvas is busy"),
        CanvasMoveError::UnstableBinding => t("Project binding is not stable"),
        CanvasMoveError::OwnerMismatch => t("Project ownership changed"),
        _ => t("Project ownership is unavailable"),
    }
}

/// Settings itself may grant a lease, but external edits wait until every sheet
/// and relevant popup closes. All these values are UI-owned in this callback.
fn modal_busy(studio: &Studio, app: &App, include_settings: bool) -> bool {
    let cloud = app.global::<SeeCut>();
    let editor = app.global::<Editor>();
    studio.exit_pending
        || (include_settings && studio.settings.open)
        || studio.export.open
        || studio.project_sheet.open
        || studio.captions.open
        || studio.speech.open
        || studio.open_menu != 0
        || app.get_canvas_open_menu()
        || app.get_clip_create_open()
        || cloud.get_auth_open()
        || cloud.get_handoff_open()
        || cloud.get_canvas_export_open()
        || cloud.get_clip_export_open()
        || cloud.get_result_preview_open()
        || cloud.get_media_preview_open()
        || cloud.get_asset_picker_open()
        || cloud.get_personal_dialog() > 0
        || cloud.get_personal_menu() > 0
        || !cloud.get_template_menu_id().is_empty()
        || !cloud.get_template_delete_id().is_empty()
        || cloud.get_template_editor_open()
        || editor.get_canvas_open_confirm()
        || editor.get_canvas_transform_confirm()
        || editor.get_canvas_exit_confirm()
}

fn request_scope(request: &Request) -> Option<Scope> {
    Some(Scope {
        instance: request.instance_id.clone(),
        project: request.project_id.clone()?,
        session: request.document_session_id.clone()?,
    })
}

/// Reserve and finish under short bridge borrows. The Canvas action and the
/// final lease check run in this same exclusive UI callback with no reentry.
fn execute_move(request: &Request, studio: &mut Studio, app: &App) -> (Response, bool) {
    let Some(claimed) = request_scope(request) else {
        return (Response::error("invalidInput"), false);
    };
    let Some(parameters) = request.move_parameters.as_ref() else {
        return (Response::error("invalidInput"), false);
    };
    let Some(revision) = request.revision else {
        return (Response::error("invalidInput"), false);
    };
    let fingerprint = MoveFingerprint {
        revision,
        parameters: parameters.clone(),
    };
    let key = OperationKey {
        client: request.client_id.clone(),
        document_session: claimed.session.clone(),
        client_sequence: parameters.client_sequence,
    };
    let current = {
        let mut bridge = studio.editor_mcp.borrow_mut();
        if request.instance_id != bridge.instance_id {
            return (Response::error("appUnavailable"), false);
        }
        let Some((kind, ids)) = bridge.active_ids(studio, app) else {
            return (Response::error("wrongProject"), false);
        };
        if kind != "canvas" {
            return (Response::error("wrongProject"), false);
        }
        let current = bridge.scope(&ids);
        // Authentication precedes validation/dedup; rejected clients allocate no history.
        if let Err(error) = bridge.writes.authorize(
            &current,
            &claimed,
            &request.client_id,
            &request.client_token,
            Instant::now(),
        ) {
            return (Response::error(registry_status(error)), false);
        }
        if concat_editor_mcp::validate_move_parameters(
            &request.client_id,
            &claimed.session,
            parameters,
        )
        .is_err()
        {
            return (Response::error("invalidInput"), false);
        }
        match bridge.writes.begin(
            &current,
            &claimed,
            &key,
            &request.client_token,
            &fingerprint,
            Instant::now(),
        ) {
            Ok(Begin::Cached(response)) => return (response, false),
            Err(error) => return (Response::error(registry_status(error)), false),
            Ok(Begin::New) => {}
        }
        current
    };
    let generation = studio.canvas.mcp_state().0;
    let binding_epoch = studio.canvas.mcp_binding_epoch();
    let selection_revision = studio.mcp_selection_epoch();
    let modal = modal_busy(studio, app, true);
    let layer_id = concat_editor_mcp::parse_layer_id(&parameters.object_id)
        .ok()
        .and_then(|id| serde_json::from_value::<concat_canvas::LayerId>(json!(id)).ok());
    let result = (|| {
        if modal {
            return Err(Response::error("busy"));
        }
        if revision != studio.canvas.mcp_state().1 {
            return Err(Response::error("staleRevision"));
        }
        if parameters.selection_revision != selection_revision {
            return Err(Response::error("selectionChanged"));
        }
        let layer_id = layer_id.ok_or_else(|| Response::error("invalidInput"))?;
        // Disjoint field borrow: no RefMut<BridgeUi> is held during preflight/history/render.
        let bridge_cell = &studio.editor_mcp;
        studio
            .canvas
            .mcp_translate_selected_checked(
                layer_id,
                parameters.delta_x,
                parameters.delta_y,
                |canvas| {
                    let bridge = bridge_cell.borrow();
                    bridge
                        .writes
                        .authorize(
                            &current,
                            &claimed,
                            &request.client_id,
                            &request.client_token,
                            Instant::now(),
                        )
                        .map_err(|error| Response::error(registry_status(error)))?;
                    if bridge.current_scope().as_ref() != Some(&current)
                        || canvas.mcp_state().0 != generation
                        || canvas.mcp_binding_epoch() != binding_epoch
                    {
                        return Err(Response::error("wrongProject"));
                    }
                    if canvas.mcp_state().1 != revision {
                        return Err(Response::error("staleRevision"));
                    }
                    // Exclusive Studio + no event-loop reentry keep the selection epoch
                    // and modal snapshot unchanged while read-only transform math runs.
                    if parameters.selection_revision != selection_revision
                        || canvas.active != Some(layer_id)
                    {
                        return Err(Response::error("selectionChanged"));
                    }
                    Ok(())
                },
            )
            .map_err(|error| match error {
                CanvasMoveCheckedError::Canvas(error) => Response::error(canvas_status(&error)),
                CanvasMoveCheckedError::Commit(response) => response,
            })
    })();
    let (response, changed) = match result {
        Ok(result) => (
            Response {
                status: if result.changed { "ok" } else { "unchanged" }.into(),
                data: Some(json!({
                    "operationId": parameters.operation_id, "projectId": current.project,
                    "documentSessionId": current.session, "objectId": parameters.object_id,
                    "revision": studio.canvas.mcp_state().1, "selectionRevision": selection_revision,
                    "x": result.x, "y": result.y, "changed": result.changed,
                    "coordinateSystem": "documentPixels",
                })),
            },
            result.changed,
        ),
        Err(response) => (response, false),
    };
    // Finish cannot reauthorize: the actual result survives expiry after commit.
    // Failure here leaves InProgress/unknown and must never schedule another edit.
    let stored = studio.editor_mcp.borrow_mut().writes.finish(
        &current,
        &key,
        response.clone(),
        Instant::now(),
    );
    if stored.is_err() {
        return (Response::error("outcomeUnknown"), changed);
    }
    (response, changed)
}

fn request_outcome(outcome: WriteRequestOutcome<Response>) -> Response {
    match outcome {
        WriteRequestOutcome::Completed(response) => response,
        WriteRequestOutcome::Busy => Response::error("busy"),
        WriteRequestOutcome::OutcomeUnknown => Response::error("outcomeUnknown"),
        WriteRequestOutcome::AppUnavailable => Response::error("appUnavailable"),
    }
}

fn dispatch_move(request: Request) -> Response {
    let state = Arc::new(WriteRequest::new());
    let queued = Arc::clone(&state);
    let scheduled = slint::invoke_from_event_loop(move || {
        let mut installed = false;
        Shell::with(|shell, app| {
            installed = true;
            // Cancellation wins before registry access, sequence consumption, or history.
            if !queued.claim() {
                return;
            }
            let mut studio = shell.studio.borrow_mut();
            let (response, changed) = execute_move(&request, &mut studio, &app);
            queued.complete(response);
            // Store both cache and request result before rendering can publish Settings.
            if changed {
                let mut canvas = std::mem::take(&mut studio.canvas);
                canvas.mcp_move_committed(&mut studio);
                studio.canvas = canvas;
                studio.publish(&app, &shell.models);
            }
        });
        if !installed {
            queued.scheduling_failed();
        }
    });
    if scheduled.is_err() {
        return request_outcome(state.scheduling_failed());
    }
    request_outcome(state.wait(Duration::from_millis(750)))
}

fn dispatch(request: Request) -> Response {
    if concat_editor_mcp::validate_request_shape(&request).is_err() {
        return Response::error("invalidInput");
    }
    if matches!(request.method, Method::MoveSelectedImage) {
        return dispatch_move(request);
    }
    let reply = on_ui({
        let request = request.clone();
        move |shell, app| {
            let studio = shell.studio.borrow();
            studio
                .editor_mcp
                .borrow_mut()
                .handle(&request, &studio, &app)
        }
    });
    let Ok(reply) = reply else {
        return reply.err().unwrap();
    };
    match reply {
        UiReply::Response(response) => response,
        UiReply::Preview(job) => {
            let Ok(_slot) = preview_slot().try_lock() else {
                return Response::error("busy");
            };
            let frame = concat_canvas::compose(&job.document, &job.store);
            let Some(rgba) =
                image::RgbaImage::from_raw(frame.width(), frame.height(), frame.pixels().to_vec())
            else {
                return Response::error("ioFailure");
            };
            let thumb = image::DynamicImage::ImageRgba8(rgba).thumbnail(job.max_edge, job.max_edge);
            let mut bytes = std::io::Cursor::new(Vec::new());
            if thumb.write_to(&mut bytes, image::ImageFormat::Png).is_err() {
                return Response::error("ioFailure");
            }
            let stable = on_ui({
                let request = job.request.clone();
                let ids = job.ids.clone();
                move |shell, app| {
                    let studio = shell.studio.borrow();
                    let mut bridge = studio.editor_mcp.borrow_mut();
                    let current = bridge.active_ids(&studio, &app);
                    current.is_some_and(|(kind, now)| {
                        kind == "canvas"
                            && now.project == ids.project
                            && now.session == ids.session
                            && bridge.authorized(&request, &ids.project, true)
                            && concat_editor_mcp::preview_matches(
                                &request,
                                &now.project,
                                &now.session,
                                studio.canvas.mcp_state().1,
                                studio.mcp_context_epoch(),
                            )
                            && !studio.canvas.mcp_state().2
                    })
                }
            });
            match stable {
                Ok(true) => {}
                Ok(false) => return Response::error("stalePreview"),
                Err(error) => return error,
            }
            Response::ok(json!({
                "projectId": job.ids.project, "documentSessionId": job.ids.session,
                "revision": job.revision, "contextRevision": job.context_revision,
                "requestId": uuid::Uuid::new_v4().to_string(), "stable": true,
                "width": thumb.width(), "height": thumb.height(), "coordinateSystem": "documentPixels",
                "mimeType": "image/png",
                "base64": base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
            }))
        }
    }
}

pub fn start(instance: &str) -> String {
    let path = match concat_editor_mcp::endpoint(instance) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let listener = match concat_editor_mcp::bind(&path) {
        Ok(listener) => listener,
        Err(error) => return error,
    };
    let endpoint = match endpoint_cleanup::BoundEndpoint::capture(path.clone()) {
        Ok(endpoint) => endpoint,
        Err(error) => return error.to_string(),
    };
    if let Err(endpoint) = BOUND_ENDPOINT.set(endpoint) {
        endpoint.remove();
        return "ioFailure: editor MCP already started".into();
    }
    #[cfg(target_os = "macos")]
    if unsafe { libc::atexit(remove_bound_endpoint_at_exit) } != 0 {
        remove_bound_endpoint();
        return "ioFailure: could not register editor MCP cleanup".into();
    }
    let instance = instance.to_owned();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let thread = std::thread::spawn(move || {
        concat_editor_mcp::serve(listener, stop_for_thread, move |request| {
            if request.instance_id != instance {
                return Response::error("appUnavailable");
            }
            dispatch(request)
        });
    });
    *server_slot().lock().unwrap() = Some((stop, thread));
    format!("Ready: {}", path.display())
}

type ServerThread = (Arc<AtomicBool>, std::thread::JoinHandle<()>);

fn server_slot() -> &'static Mutex<Option<ServerThread>> {
    static SERVER: OnceLock<Mutex<Option<ServerThread>>> = OnceLock::new();
    SERVER.get_or_init(|| Mutex::new(None))
}

fn remove_bound_endpoint() {
    if let Some(endpoint) = BOUND_ENDPOINT.get() {
        endpoint.remove();
    }
}

#[cfg(target_os = "macos")]
extern "C" fn remove_bound_endpoint_at_exit() {
    // Cocoa's Cmd+Q can exit before app.run() returns. Only unlink the saved
    // endpoint here: never access UI state, lock SERVER, or wait for threads.
    remove_bound_endpoint();
}

pub fn remove_endpoint(_instance: &str) {
    if let Some((stop, thread)) = server_slot().lock().unwrap().take() {
        stop.store(true, Ordering::Release);
        let _ = thread.join();
    }
    remove_bound_endpoint();
}

#[cfg(test)]
mod assistant_permission_tests {
    use super::*;
    use write_registry::LEASE_DURATION;

    type Registry = WriteRegistry<(), ()>;

    fn scope() -> Scope {
        Scope {
            instance: "app".into(),
            project: "canvas".into(),
            session: "session".into(),
        }
    }

    fn reader(scope: &Scope, media: bool) -> HashMap<String, Grant> {
        HashMap::from([(
            ASSISTANT_CLIENT_ID.into(),
            Grant {
                token: "assistant-read".into(),
                project: scope.project.clone(),
                media,
            },
        )])
    }

    fn snapshot(
        scope: &Scope,
        grants: &HashMap<String, Grant>,
        writes: &Registry,
        token: &str,
        now: Instant,
    ) -> AssistantSnapshot {
        assistant_registry_snapshot(
            AssistantPermissionContext {
                scope: Some(scope),
                canvas: true,
                write_ready: true,
                now,
            },
            "画布工程".into(),
            grants,
            writes,
            token,
        )
    }

    #[test]
    fn settings_redacts_assistant_credentials_and_preserves_external_setup() {
        assert!(settings_visible_token(ASSISTANT_CLIENT_ID, "private-assistant-token").is_empty());
        assert!(settings_visible_token("external", "external-token") == "external-token");
    }

    #[test]
    fn foreign_active_and_expired_leases_cannot_be_exposed_or_changed() {
        let scope = scope();
        let now = Instant::now();
        for at in [now, now + LEASE_DURATION] {
            let mut writes = Registry::new();
            writes.bind_scope(Some(scope.clone()));
            writes
                .trusted_grant(&scope, "external", "external-write".into(), now)
                .unwrap();
            let mut grants = reader(&scope, true);
            grants.insert(
                "external".into(),
                Grant {
                    token: "external-read".into(),
                    project: scope.project.clone(),
                    media: true,
                },
            );
            let mut token = "external-write".to_owned();
            let view = snapshot(&scope, &grants, &writes, &token, at);
            assert!(view.write_token.is_none());
            assert!(!view.can_grant_write && !view.can_renew_write && !view.can_revoke_write);
            assert!(view.write_status == "移动权限由其他客户端持有");
            for action in [
                AssistantPermission::GrantWrite,
                AssistantPermission::RenewWrite,
                AssistantPermission::RevokeWrite,
            ] {
                assert!(
                    assistant_registry_permission(
                        AssistantPermissionContext {
                            scope: Some(&scope),
                            canvas: true,
                            write_ready: true,
                            now: at
                        },
                        &mut grants,
                        &mut writes,
                        &mut token,
                        action,
                        || panic!("denied actions must not issue credentials")
                    )
                    .is_none()
                );
            }
            assert!(
                assistant_registry_permission(
                    AssistantPermissionContext {
                        scope: Some(&scope),
                        canvas: true,
                        write_ready: true,
                        now: at
                    },
                    &mut grants,
                    &mut writes,
                    &mut token,
                    AssistantPermission::RevokeRead,
                    || panic!("revoke must not issue credentials")
                )
                .is_some()
            );
            assert!(!grants.contains_key(ASSISTANT_CLIENT_ID));
            assert!(
                grants
                    .get("external")
                    .is_some_and(|grant| grant.token == "external-read")
            );
            assert!(token == "external-write");
            assert!(matches!(
                writes.lease_status(at),
                LeaseStatus::Active {
                    client: "external",
                    ..
                } | LeaseStatus::Expired {
                    client: "external",
                    ..
                }
            ));
            assert!(
                writes.authorize(&scope, &scope, "external", &token, at)
                    == if at == now {
                        Ok(())
                    } else {
                        Err(RegistryError::LeaseExpired)
                    }
            );
        }
    }

    #[test]
    fn assistant_expiry_withholds_token_and_explicit_renew_rotates_it() {
        let scope = scope();
        let now = Instant::now();
        let mut writes = Registry::new();
        writes.bind_scope(Some(scope.clone()));
        writes
            .trusted_grant(&scope, ASSISTANT_CLIENT_ID, "old-write".into(), now)
            .unwrap();
        let mut grants = reader(&scope, true);
        let mut token = "old-write".to_owned();
        let at = now + LEASE_DURATION;
        let view = snapshot(&scope, &grants, &writes, &token, at);
        assert!(view.write_token.is_none());
        assert!(!view.can_grant_write && view.can_renew_write && view.can_revoke_write);
        assert!(view.write_status == "移动权限已到期，请续期");
        assert!(
            assistant_registry_permission(
                AssistantPermissionContext {
                    scope: Some(&scope),
                    canvas: true,
                    write_ready: true,
                    now: at
                },
                &mut grants,
                &mut writes,
                &mut token,
                AssistantPermission::RenewWrite,
                || "new-write".into()
            )
            .is_some()
        );
        assert!(
            writes.authorize(&scope, &scope, ASSISTANT_CLIENT_ID, "old-write", at)
                == Err(RegistryError::Unauthorized)
        );
        assert!(
            writes
                .authorize(&scope, &scope, ASSISTANT_CLIENT_ID, &token, at)
                .is_ok()
        );
        assert!(
            snapshot(&scope, &grants, &writes, &token, at)
                .write_token
                .as_deref()
                == Some("new-write")
        );
        assert!(
            assistant_registry_permission(
                AssistantPermissionContext {
                    scope: Some(&scope),
                    canvas: true,
                    write_ready: false,
                    now: at
                },
                &mut grants,
                &mut writes,
                &mut token,
                AssistantPermission::RevokeRead,
                || panic!("revoke must not issue credentials")
            )
            .is_some()
        );
        assert!(token.is_empty());
        assert!(matches!(writes.lease_status(at), LeaseStatus::Unauthorized));
    }

    #[test]
    fn read_only_is_not_preview_or_editing_and_grants_are_separate() {
        let scope = scope();
        let now = Instant::now();
        let mut writes = Registry::new();
        writes.bind_scope(Some(scope.clone()));
        let mut grants = reader(&scope, false);
        let mut token = String::new();
        let view = snapshot(&scope, &grants, &writes, &token, now);
        assert!(view.read_token.is_none() && !view.can_grant_write && view.can_revoke_read);
        assert!(
            assistant_registry_permission(
                AssistantPermissionContext {
                    scope: Some(&scope),
                    canvas: true,
                    write_ready: true,
                    now
                },
                &mut grants,
                &mut writes,
                &mut token,
                AssistantPermission::GrantWrite,
                || panic!("R alone must not obtain E")
            )
            .is_none()
        );
        assert!(
            assistant_registry_permission(
                AssistantPermissionContext {
                    scope: Some(&scope),
                    canvas: true,
                    write_ready: true,
                    now
                },
                &mut grants,
                &mut writes,
                &mut token,
                AssistantPermission::GrantRead,
                || "full-read".into()
            )
            .is_some()
        );
        assert!(matches!(
            writes.lease_status(now),
            LeaseStatus::Unauthorized
        ));
        assert!(token.is_empty());
        assert!(
            assistant_registry_permission(
                AssistantPermissionContext {
                    scope: Some(&scope),
                    canvas: true,
                    write_ready: false,
                    now
                },
                &mut grants,
                &mut writes,
                &mut token,
                AssistantPermission::GrantWrite,
                || panic!("busy or owner failure must deny E")
            )
            .is_none()
        );
        assert!(
            assistant_registry_permission(
                AssistantPermissionContext {
                    scope: Some(&scope),
                    canvas: true,
                    write_ready: true,
                    now
                },
                &mut grants,
                &mut writes,
                &mut token,
                AssistantPermission::GrantWrite,
                || "first-write".into()
            )
            .is_some()
        );
        assert!(
            writes
                .authorize(&scope, &scope, ASSISTANT_CLIENT_ID, &token, now)
                .is_ok()
        );
        assert!(
            grants
                .get(ASSISTANT_CLIENT_ID)
                .is_some_and(|grant| grant.token == "full-read")
        );
    }

    #[test]
    fn no_active_document_and_changed_identity_never_reveal_background_credentials() {
        let scope = scope();
        let now = Instant::now();
        let grants = reader(&scope, true);
        let mut writes = Registry::new();
        writes.bind_scope(Some(scope.clone()));
        writes
            .trusted_grant(&scope, ASSISTANT_CLIENT_ID, "write".into(), now)
            .unwrap();
        let view = assistant_registry_snapshot(
            AssistantPermissionContext {
                scope: None,
                canvas: false,
                write_ready: true,
                now,
            },
            "后台画布".into(),
            &grants,
            &writes,
            "write",
        );
        assert!(view.document.is_none() && view.project_name.is_empty());
        assert!(view.read_token.is_none() && view.write_token.is_none());
        assert!(
            !view.can_grant_read
                && !view.can_revoke_read
                && !view.can_grant_write
                && !view.can_renew_write
                && !view.can_revoke_write
        );
        let next = Scope {
            instance: "next-app".into(),
            project: "next-project".into(),
            session: "next-session".into(),
        };
        writes.bind_scope(Some(next.clone()));
        let view = snapshot(&next, &grants, &writes, "write", now);
        assert!(view.read_token.is_none() && view.write_token.is_none());
        assert!(
            view.document
                .as_ref()
                .is_some_and(|id| id.instance_id == next.instance
                    && id.project_id == next.project
                    && id.document_session_id == next.session)
        );
        let clip = assistant_registry_snapshot(
            AssistantPermissionContext {
                scope: Some(&scope),
                canvas: false,
                write_ready: true,
                now,
            },
            "剪辑工程".into(),
            &grants,
            &writes,
            "write",
        );
        assert!(clip.read_token.is_some() && clip.write_token.is_none());
        assert!(!clip.can_grant_write && !clip.can_renew_write && !clip.can_revoke_write);
        assert!(clip.project_name == "剪辑工程" && clip.write_status == "当前工程不支持移动");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture() -> (BridgeUi, Scope, OperationKey, MoveFingerprint, Instant) {
        let mut bridge = BridgeUi::new();
        let ids = bridge.ids("canvas", "1:1".into());
        let scope = bridge.scope(&ids);
        let now = Instant::now();
        bridge.writes.bind_scope(Some(scope.clone()));
        bridge
            .writes
            .trusted_grant(&scope, "writer", "write-token".into(), now)
            .unwrap();
        let key = OperationKey {
            client: "writer".into(),
            document_session: scope.session.clone(),
            client_sequence: 7,
        };
        let parameters = MoveParameters {
            operation_id: concat_editor_mcp::operation_id("writer", &scope.session, 7).unwrap(),
            client_sequence: 7,
            selection_revision: 3,
            object_id: "layer:2".into(),
            delta_x: 4.0,
            delta_y: -5.0,
        };
        (
            bridge,
            scope,
            key,
            MoveFingerprint {
                revision: 11,
                parameters,
            },
            now,
        )
    }

    #[test]
    fn read_and_write_credentials_and_revocation_are_independent() {
        let (mut bridge, scope, _, _, now) = write_fixture();
        bridge.grants.insert(
            "writer".into(),
            Grant {
                token: "read-token".into(),
                project: scope.project.clone(),
                media: true,
            },
        );
        let mut request = Request {
            instance_id: scope.instance.clone(),
            client_id: "writer".into(),
            client_token: "write-token".into(),
            method: Method::Context,
            project_id: None,
            document_session_id: None,
            revision: None,
            context_revision: None,
            offset: None,
            limit: None,
            max_edge: None,
            move_parameters: None,
        };
        assert!(
            !bridge.authorized(&request, &scope.project, false),
            "write credential cannot read"
        );
        request.client_token = "read-token".into();
        assert!(bridge.authorized(&request, &scope.project, true));
        assert_eq!(
            bridge
                .writes
                .authorize(&scope, &scope, "writer", "read-token", now),
            Err(RegistryError::Unauthorized)
        );
        bridge.client_draft = "writer".into();
        bridge.revoke();
        assert!(!bridge.authorized(&request, &scope.project, false));
        assert!(
            bridge
                .writes
                .authorize(&scope, &scope, "writer", "write-token", now)
                .is_ok()
        );
        bridge.grants.insert(
            "writer".into(),
            Grant {
                token: "read-token".into(),
                project: scope.project.clone(),
                media: true,
            },
        );
        bridge.revoke_move();
        assert!(bridge.authorized(&request, &scope.project, true));
        assert_eq!(
            bridge
                .writes
                .authorize(&scope, &scope, "writer", "write-token", now),
            Err(RegistryError::Unauthorized)
        );
    }

    #[test]
    fn move_fingerprint_retains_original_revision_selection_object_and_both_deltas() {
        let (mut bridge, scope, key, params, now) = write_fixture();
        assert!(matches!(
            bridge
                .writes
                .begin(&scope, &scope, &key, "write-token", &params, now),
            Ok(Begin::New)
        ));
        let original = Response::ok(json!({"revision": 12, "x": 4.0, "y": -5.0}));
        bridge
            .writes
            .finish(&scope, &key, original.clone(), now)
            .unwrap();
        let Ok(Begin::Cached(cached)) =
            bridge
                .writes
                .begin(&scope, &scope, &key, "write-token", &params, now)
        else {
            panic!("original expected revision must replay cached result");
        };
        assert_eq!(cached.data, original.data);
        for mutate in 0..5 {
            let mut changed = params.clone();
            match mutate {
                0 => changed.revision += 1,
                1 => changed.parameters.selection_revision += 1,
                2 => changed.parameters.object_id = "layer:3".into(),
                3 => changed.parameters.delta_x += 1.0,
                _ => changed.parameters.delta_y += 1.0,
            }
            assert!(matches!(
                bridge
                    .writes
                    .begin(&scope, &scope, &key, "write-token", &changed, now),
                Err(RegistryError::ParameterMismatch)
            ));
        }
        bridge.client_draft = "someone-else".into();
        assert!(
            matches!(
                bridge.writes.lease_status(now),
                LeaseStatus::Active {
                    client: "writer",
                    ..
                }
            ),
            "draft does not rename actual grant"
        );
    }

    #[test]
    fn leaving_canvas_revokes_editing_without_losing_current_session_retry_history() {
        let (mut bridge, scope, key, params, now) = write_fixture();
        bridge.sync_workspace(Some("canvas"));
        assert!(matches!(
            bridge
                .writes
                .begin(&scope, &scope, &key, "write-token", &params, now),
            Ok(Begin::New)
        ));
        bridge
            .writes
            .finish(&scope, &key, Response::ok(json!({"revision": 12})), now)
            .unwrap();
        bridge.sync_workspace(Some("clip"));
        assert!(bridge.active_clip);
        assert!(!bridge.active_canvas);
        assert_eq!(
            bridge
                .writes
                .authorize(&scope, &scope, "writer", "write-token", now),
            Err(RegistryError::Unauthorized)
        );
        bridge.sync_workspace(Some("canvas"));
        bridge
            .writes
            .trusted_grant(&scope, "writer", "new-write-token".into(), now)
            .unwrap();
        assert!(
            matches!(
                bridge
                    .writes
                    .begin(&scope, &scope, &key, "new-write-token", &params, now),
                Ok(Begin::Cached(_))
            ),
            "returning and explicitly granting again cannot replay an already committed sequence"
        );
    }

    #[test]
    fn cached_move_and_request_completion_release_bridge_before_view_reborrow() {
        let (bridge, scope, key, params, now) = write_fixture();
        let cell = std::cell::RefCell::new(bridge);
        let request = WriteRequest::new();
        assert!(request.claim());
        assert!(matches!(
            cell.borrow_mut()
                .writes
                .begin(&scope, &scope, &key, "write-token", &params, now),
            Ok(Begin::New)
        ));
        let original = Response::ok(json!({"revision": 12}));
        cell.borrow_mut()
            .writes
            .finish(&scope, &key, original.clone(), now)
            .unwrap();
        assert!(request.complete(original.clone()));
        // Model the Settings reborrow performed after the production callback stores its result.
        cell.borrow().grant_view();
        let mut published = cell.borrow_mut();
        assert!(matches!(
            published
                .writes
                .begin(&scope, &scope, &key, "write-token", &params, now),
            Ok(Begin::Cached(_))
        ));
        let WriteRequestOutcome::Completed(result) = request.wait(Duration::ZERO) else {
            panic!("completion lost");
        };
        assert_eq!(result.data, original.data);
    }

    #[test]
    fn object_count_rejects_overflow_and_allows_exact_limit() {
        assert_eq!(
            bounded_object_count(MAX_PROJECT_OBJECTS - 1, 1),
            Ok(MAX_PROJECT_OBJECTS)
        );
        assert_eq!(bounded_object_count(MAX_PROJECT_OBJECTS, 1), Err(()));
        assert_eq!(bounded_object_count(1, usize::MAX), Err(()));
    }

    #[test]
    fn canvas_object_limit_includes_root_nested_groups_and_pending_siblings() {
        use concat_canvas::{ImageDocument, LayerGroup, LayerNode};

        let mut flat = ImageDocument::new(1, 1);
        for _ in 1..MAX_PROJECT_OBJECTS {
            flat.new_group("Group");
        }
        let (total, rows) = canvas_objects(&flat, MAX_PROJECT_OBJECTS - 1, 1).unwrap();
        assert_eq!(total, MAX_PROJECT_OBJECTS);
        assert_eq!(
            rows[0]["id"],
            format!("layer:{}", flat.root.children.last().unwrap().id().as_u64())
        );
        flat.new_group("Over limit");
        assert!(canvas_objects(&flat, usize::MAX, 1).is_err());

        let mut nested = ImageDocument::new(1, 1);
        let group_id = nested.mint_id();
        let mut group = LayerGroup::new(group_id, "Nested");
        for _ in 0..MAX_PROJECT_OBJECTS - 3 {
            let id = nested.mint_id();
            group
                .children
                .push(LayerNode::Group(LayerGroup::new(id, "Child")));
        }
        nested.root.children.push(LayerNode::Group(group));
        let sibling_id = nested.new_group("Pending sibling");
        let (total, rows) = canvas_objects(&nested, MAX_PROJECT_OBJECTS - 2, 2).unwrap();
        assert_eq!(total, MAX_PROJECT_OBJECTS);
        assert_eq!(rows[0]["parentId"], format!("layer:{}", group_id.as_u64()));
        assert_eq!(rows[1]["id"], format!("layer:{}", sibling_id.as_u64()));
        let extra_id = nested.mint_id();
        nested
            .group_mut(group_id)
            .unwrap()
            .children
            .push(LayerNode::Group(LayerGroup::new(extra_id, "Over limit")));
        assert!(canvas_objects(&nested, 0, 1).is_err());
    }

    #[test]
    fn canvas_structure_drops_deleted_ids_and_keeps_remaining_order() {
        let mut document = concat_canvas::ImageDocument::new(1, 1);
        let group = document.new_group("Group");
        let removed = document.new_group("Removed");
        let retained = document.new_group("Retained");
        assert!(document.move_node(removed, Some(group), 0));
        assert!(document.move_node(retained, Some(group), 1));
        let (_, before) = canvas_objects(&document, 0, 10).unwrap();
        let old_id = format!("layer:{}", removed.as_u64());
        assert!(before.iter().any(|row| row["id"] == old_id));
        assert!(document.remove(removed).is_some());
        let (total, after) = canvas_objects(&document, 0, 10).unwrap();
        assert_eq!(total, 3);
        assert!(after.iter().all(|row| row["id"] != old_id));
        assert_eq!(after[1]["id"], format!("layer:{}", group.as_u64()));
        assert_eq!(after[2]["id"], format!("layer:{}", retained.as_u64()));
        assert_eq!(after[2]["parentId"], after[1]["id"]);
    }

    #[test]
    fn clip_object_limit_counts_media_timelines_tracks_and_clips() {
        use concat_project::model::{Clip, ClipKind, MediaItem, Project, Timeline, Track};

        for kind in ["media", "timeline", "track", "clip"] {
            let mut project = Project::new();
            match kind {
                "media" => {
                    project.media = (0..MAX_PROJECT_OBJECTS - 5)
                        .map(|i| MediaItem {
                            id: format!("m{i}"),
                            ..MediaItem::default()
                        })
                        .collect();
                }
                "timeline" => {
                    project.timelines = (0..MAX_PROJECT_OBJECTS)
                        .map(|i| {
                            Arc::new(Timeline {
                                id: format!("tl{i}"),
                                ..Timeline::default()
                            })
                        })
                        .collect();
                }
                "track" => {
                    project.active_mut().tracks = (0..MAX_PROJECT_OBJECTS - 1)
                        .map(|i| Track {
                            id: format!("t{i}"),
                            ..Track::default()
                        })
                        .collect();
                }
                "clip" => {
                    project.active_mut().clips = (0..MAX_PROJECT_OBJECTS - 5)
                        .map(|i| {
                            Arc::new(Clip::blank(
                                format!("c{i}"),
                                "T1",
                                ClipKind::Video,
                                "Clip",
                                0.0,
                                1.0,
                            ))
                        })
                        .collect();
                }
                _ => unreachable!(),
            }
            let (total, rows) = clip_objects(&project, MAX_PROJECT_OBJECTS - 1, 1).unwrap();
            assert_eq!(total, MAX_PROJECT_OBJECTS, "{kind}");
            assert_eq!(rows.len(), 1, "{kind}");
            match kind {
                "media" => project.media.push(MediaItem::default()),
                "timeline" => project.timelines.push(Arc::new(Timeline::default())),
                "track" => project.active_mut().tracks.push(Track::default()),
                "clip" => project.active_mut().clips.push(Arc::new(Clip::default())),
                _ => unreachable!(),
            }
            assert!(clip_objects(&project, usize::MAX, 1).is_err(), "{kind}");
        }
    }

    #[test]
    fn clip_pages_cross_object_kinds_without_serializing_skipped_entries() {
        use concat_project::model::{Clip, ClipKind, MediaItem, Project, Timeline, Track};

        let mut project = Project::new();
        project.media = ["m1", "m2"]
            .into_iter()
            .map(|id| MediaItem {
                id: id.into(),
                ..MediaItem::default()
            })
            .collect();
        project.active_mut().tracks.truncate(2);
        project.active_mut().clips = ["c1", "c2"]
            .into_iter()
            .map(|id| {
                let mut clip = Clip::blank(id, "T1", ClipKind::Video, id, 0.0, 1.0);
                clip.media_id = "m1".into();
                Arc::new(clip)
            })
            .collect();
        project.timelines.push(Arc::new(Timeline {
            id: "TL2".into(),
            tracks: vec![Track {
                id: "T3".into(),
                ..Track::default()
            }],
            clips: vec![Arc::new(Clip::blank(
                "c3",
                "T3",
                ClipKind::Video,
                "Third",
                0.0,
                1.0,
            ))],
            ..Timeline::default()
        }));
        let (total, all) = clip_objects(&project, 0, 100).unwrap();
        assert_eq!(total, 10);
        assert_eq!(
            all.iter()
                .map(|row| row["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "media:m1",
                "media:m2",
                "timeline:TL1",
                "track:T1",
                "track:T2",
                "clip:c1",
                "clip:c2",
                "timeline:TL2",
                "track:T3",
                "clip:c3"
            ]
        );
        for (offset, limit) in [(1, 4), (4, 4), (7, 3)] {
            let (page_total, page) = clip_objects(&project, offset, limit).unwrap();
            assert_eq!(page_total, total);
            assert_eq!(page, all[offset..offset + limit]);
        }
        assert_eq!(all[3]["parentId"], "timeline:TL1");
        assert_eq!(all[5]["parentId"], "track:T1");
        assert_eq!(all[5]["timelineId"], "timeline:TL1");
        assert_eq!(all[5]["mediaId"], "media:m1");
        assert_eq!(all[9]["parentId"], "track:T3");
        assert_eq!(all[9]["timelineId"], "timeline:TL2");
        let mut serialized = 0;
        let page: Vec<_> = clip_object_page(&project, 4, 4)
            .map(|object| {
                serialized += 1;
                object.into_value()
            })
            .collect();
        assert_eq!(serialized, 4);
        assert_eq!(page, all[4..8]);
        assert_eq!(
            clip_objects(&project, usize::MAX, 1).unwrap(),
            (total, vec![])
        );
    }

    #[test]
    fn grants_are_per_client_project_permission_and_session() {
        let mut bridge = BridgeUi::new();
        let first = bridge.ids("canvas", "1:1".into());
        bridge.grants.insert(
            "reader".into(),
            Grant {
                token: "token".into(),
                project: first.project.clone(),
                media: false,
            },
        );
        let request = Request {
            instance_id: bridge.instance_id.clone(),
            client_id: "reader".into(),
            client_token: "token".into(),
            method: Method::Project,
            project_id: Some(first.project.clone()),
            document_session_id: Some(first.session.clone()),
            revision: None,
            context_revision: None,
            offset: None,
            limit: None,
            max_edge: None,
            move_parameters: None,
        };
        assert!(bridge.authorized(&request, &first.project, false));
        assert!(!bridge.authorized(&request, &first.project, true));
        assert!(!bridge.authorized(&request, "another-project", false));
        let next = bridge.ids("canvas", "1:2".into());
        assert_ne!(first.session, next.session);
        assert!(!bridge.authorized(&request, &first.project, false));
        bridge.grants.insert(
            "reader".into(),
            Grant {
                token: "new-token".into(),
                project: next.project.clone(),
                media: true,
            },
        );
        assert!(!bridge.authorized(&request, &next.project, true));
        bridge.client_draft = "reader".into();
        bridge.revoke();
        assert!(bridge.grants.is_empty());
    }

    #[test]
    fn canvas_gallery_cannot_expose_background_document() {
        assert_eq!(
            active_document_kind(6, false, false, true, false),
            Some("canvas")
        );
        assert_eq!(active_document_kind(6, true, false, true, false), None);
        assert_eq!(
            active_document_kind(6, false, false, true, false),
            Some("canvas")
        );
        assert_eq!(active_document_kind(0, false, true, false, true), None);
        assert_eq!(
            active_document_kind(0, false, false, false, true),
            Some("clip")
        );
    }
}
