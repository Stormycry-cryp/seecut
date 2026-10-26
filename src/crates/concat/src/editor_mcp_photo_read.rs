// SPDX-License-Identifier: AGPL-3.0-or-later
//! Original Doc-grant Photo reads. Host I/O stays on the existing Photo worker.
use super::*;
use crate::photo_workspace::{
    self, Cancellation, PendingState, PhotoWorkerClient, ReadBinding, ReadOrigin,
};

pub(super) struct ReadState {
    pending: Option<(ReadOrigin, Cancellation)>,
    map: photo_state::StateMap,
}
impl ReadState {
    pub(super) fn new() -> Self {
        Self {
            pending: None,
            map: photo_state::StateMap::new(),
        }
    }
    pub(super) fn invalidate(&mut self) {
        if let Some((_, cancel)) = self.pending.take() {
            cancel.cancel();
        }
        self.map.invalidate();
    }
}
impl Drop for ReadState {
    fn drop(&mut self) {
        self.invalidate();
    }
}

struct Job {
    origin: ReadOrigin,
    fence: u64,
    scope: Scope,
    client: PhotoWorkerClient,
    pending: PendingState,
}

fn gate(
    bridge: &mut BridgeUi,
    request: &Request,
    studio: &Studio,
    app: &App,
) -> Result<Scope, Response> {
    bridge.sync_photo_source_fence();
    if !photo_workspace::is_photo_target() {
        return Err(Response::error("unsupportedWorkspace"));
    }
    if request.instance_id != bridge.instance_id {
        return Err(Response::error("appUnavailable"));
    }
    let Some((kind, ids)) = bridge.active_ids(studio, app) else {
        return Err(Response::error("wrongProject"));
    };
    if kind != "photo"
        || request.project_id.as_deref() != Some(ids.project.as_str())
        || request.document_session_id.as_deref() != Some(ids.session.as_str())
    {
        return Err(Response::error("wrongProject"));
    }
    if !bridge.authorized(request, &ids.project, false) {
        return Err(Response::error("notAuthorized"));
    }
    Ok(bridge.scope(&ids))
}

// A callback queued beyond on_ui's receive timeout cannot install a new read.
struct CancelOnReturn(Cancellation);
impl Drop for CancelOnReturn {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct DispatchLife(Arc<AtomicBool>);
impl Drop for DispatchLife {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub(super) fn dispatch(request: Request) -> Response {
    let live = Arc::new(AtomicBool::new(true));
    let _life = DispatchLife(live.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    let admitted = on_ui({
        let request = request.clone();
        let live = live.clone();
        move |shell, app| -> Result<Job, Response> {
            if !live.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return Err(Response::error("busy"));
            }
            let studio = shell
                .studio
                .try_borrow()
                .map_err(|_| Response::error("busy"))?;
            let mut bridge = studio
                .editor_mcp
                .try_borrow_mut()
                .map_err(|_| Response::error("busy"))?;
            let scope = gate(&mut bridge, &request, &studio, &app)?;
            bridge.photo_reads.invalidate();
            let fence =
                photo_workspace::source_fence().ok_or_else(|| Response::error("wrongProject"))?;
            let client =
                photo_workspace::read_client().ok_or_else(|| Response::error("appUnavailable"))?;
            let (metadata, _) = bridge
                .photo_binding
                .as_ref()
                .ok_or_else(|| Response::error("wrongProject"))?;
            let origin = ReadOrigin {
                request_id: uuid::Uuid::new_v4(),
                source: metadata.origin().source,
                deadline,
            };
            let pending = client
                .prepare_agent_state(origin, ReadBinding::from_owned(metadata))
                .map_err(|_| Response::error("busy"))?;
            let cancel = pending.cancellation();
            photo_workspace::register_read_cancel(cancel.clone());
            bridge.photo_reads.pending = Some((origin, cancel));
            Ok(Job {
                origin,
                fence,
                scope,
                client,
                pending,
            })
        }
    });
    let job = match admitted {
        Ok(Ok(job)) => job,
        Ok(Err(error)) | Err(error) => return error,
    };
    let Job {
        origin,
        fence,
        scope,
        client,
        pending,
    } = job;
    let _cancel = CancelOnReturn(pending.cancellation());
    let result = pending.wait(&client);
    on_ui(move |shell, app| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return Response::error("busy");
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return Response::error("busy");
        };
        if bridge
            .photo_reads
            .pending
            .as_ref()
            .map(|(old, _)| old.request_id)
            != Some(origin.request_id)
        {
            return Response::error("wrongProject");
        }
        if !live.load(Ordering::SeqCst) || Instant::now() >= origin.deadline {
            bridge.photo_reads.invalidate();
            return Response::error("busy");
        }
        let valid = gate(&mut bridge, &request, &studio, &app)
            .is_ok_and(|current| current == scope)
            && photo_workspace::source_fence() == Some(fence)
            && photo_workspace::current_read_source() == Some(origin.source)
            && bridge
                .photo_reads
                .pending
                .as_ref()
                .map(|(old, _)| old.request_id)
                == Some(origin.request_id);
        if !valid {
            bridge.photo_reads.invalidate();
            return Response::error("wrongProject");
        }
        let state = match result {
            Ok(state) => state,
            Err(_) => {
                // A different SaveAs/document/generation must require fresh native
                // metadata and a fresh grant; never silently adopt it here.
                bridge.clear_photo_binding();
                return Response::error("wrongProject");
            }
        };
        let reply = bridge
            .photo_reads
            .map
            .observe(&scope, origin.source.managed.instance, &state);
        match reply
            .ok()
            .and_then(|reply| serde_json::to_value(reply).ok())
        {
            Some(data) => {
                bridge.photo_reads.pending.take();
                Response::ok(data)
            }
            None => {
                bridge.photo_reads.invalidate();
                Response::error("invalidInput")
            }
        }
    })
    .unwrap_or_else(|error| error)
}

/// Discovery uses the same existing Doc grant; it conveys no media/edit authority.
pub(super) fn capabilities(
    bridge: &mut BridgeUi,
    request: &Request,
    studio: &Studio,
    app: &App,
) -> Response {
    if bridge.instance_id != request.instance_id {
        return Response::error("appUnavailable");
    }
    let Some((kind, ids)) = bridge.active_ids(studio, app) else {
        return Response::error("wrongProject");
    };
    if kind != "photo" {
        return Response::error("unsupportedWorkspace");
    }
    if !bridge.authorized(request, &ids.project, false) {
        return Response::error("notAuthorized");
    }
    Response::ok(json!({
        "appInstanceId": bridge.instance_id, "activeWorkspace": "photo",
        "projectId": ids.project, "documentSessionId": ids.session,
        "protocolVersion": 1, "tools": ["capabilities", "photo_state"], "granted": ["R"],
        "metadataOnly": true, "mediaAvailable": false, "writeAvailable": false,
        "managedAgentAvailable": false
    }))
}
