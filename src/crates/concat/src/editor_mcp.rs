// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local Agent reads from the UI-owned Studio. Grants live only in this App
//! process and are issued or revoked by the Settings UI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use base64::Engine;
use concat_editor_mcp::{Method, Request, Response};
use serde_json::{Value, json};
use slint::ComponentHandle;

use crate::host::Shell;
use crate::i18n::t;
use crate::studio::Studio;
use crate::ui::{App, SeeCut};

#[path = "editor_mcp_endpoint.rs"]
mod endpoint_cleanup;

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

/// App-side, ephemeral grant and session registry.
pub(crate) struct BridgeUi {
    pub instance_id: String,
    pub socket_status: String,
    pub client_draft: String,
    grants: HashMap<String, Grant>,
    canvas_ids: Option<(String, DocumentIds)>,
    clip_ids: Option<(String, DocumentIds)>,
}

impl BridgeUi {
    pub fn new() -> Self {
        Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            socket_status: "Starting".into(),
            client_draft: String::new(),
            grants: HashMap::new(),
            canvas_ids: None,
            clip_ids: None,
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
                if grant.media { "R + M" } else { "R" }.into(),
                grant.token.clone(),
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
            return UiReply::Response(Response::ok(json!({
                "appInstanceId": self.instance_id,
                "tools": ["capabilities", "context", "project", "preview"],
                "permissions": ["R", "M"], "readOnly": true,
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

fn dispatch(request: Request) -> Response {
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
mod tests {
    use super::*;

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
