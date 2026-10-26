// SPDX-License-Identifier: AGPL-3.0-or-later
//! Original Doc-grant Photo reads. Host I/O stays on the existing Photo worker.
use super::*;
use crate::photo_workspace::{
    self, Cancellation, PendingState, PhotoWorkerClient, ReadBinding, ReadOrigin,
};

// BEGIN PHOTO_READER_POLICY
/// Native UI projection for the currently selected ordinary Photo reader.
/// No private backing path or fixed Assistant credential enters this value.
pub(crate) struct PhotoReaderView {
    pub(crate) client: String,
    pub(crate) instance: String,
    pub(crate) token: String,
    pub(crate) can_grant: bool,
    pub(crate) can_revoke: bool,
    pub(crate) status: String,
}

fn ordinary_reader_client(client: &str) -> Option<&str> {
    let client = client.trim();
    (!client.is_empty() && client.len() <= 64 && client != ASSISTANT_CLIENT_ID).then_some(client)
}

fn reader_projection(
    instance: &str,
    client: &str,
    project: Option<&str>,
    grants: &HashMap<String, Grant>,
) -> PhotoReaderView {
    let selected = ordinary_reader_client(client);
    let grant = project
        .zip(selected)
        .and_then(|(project, client)| grants.get(client).filter(|grant| grant.project == project));
    PhotoReaderView {
        client: client.to_owned(),
        instance: project.map(|_| instance.to_owned()).unwrap_or_default(),
        token: grant.map(|grant| grant.token.clone()).unwrap_or_default(),
        can_grant: project.is_some() && selected.is_some(),
        can_revoke: grant.is_some(),
        status: if project.is_none() {
            "当前图像文档信息尚未就绪"
        } else if selected.is_none() {
            "请输入有效的普通客户端名称（最多 64 字节）"
        } else if grant.is_some() {
            "已允许读取当前图像文档结构"
        } else {
            "尚未允许读取当前图像文档结构"
        }
        .into(),
    }
}

fn grant_reader_scope(
    grants: &mut HashMap<String, Grant>,
    project: &str,
    client: &str,
    issue_token: impl FnOnce() -> String,
) -> bool {
    let Some(client) = ordinary_reader_client(client) else {
        return false;
    };
    grants.insert(
        client.to_owned(),
        Grant {
            token: issue_token(),
            project: project.to_owned(),
            media: false,
        },
    );
    true
}

fn revoke_reader_scope(grants: &mut HashMap<String, Grant>, project: &str, client: &str) -> bool {
    let Some(client) = ordinary_reader_client(client) else {
        return false;
    };
    if grants
        .get(client)
        .is_some_and(|grant| grant.project == project)
    {
        grants.remove(client);
        true
    } else {
        false
    }
}
// END PHOTO_READER_POLICY

impl BridgeUi {
    fn photo_reader_scope(&mut self, studio: &Studio, app: &App) -> Option<Scope> {
        if !photo_workspace::is_photo_target() {
            return None;
        }
        // active_ids -> photo_ids rechecks the installed saved metadata's
        // original account, UI, managed child and explicit Photo target.
        let (kind, ids) = self.active_ids(studio, app)?;
        (kind == "photo").then(|| self.scope(&ids))
    }

    pub(crate) fn photo_reader_view(&mut self, studio: &Studio, app: &App) -> PhotoReaderView {
        let scope = self.photo_reader_scope(studio, app);
        reader_projection(
            &self.instance_id,
            &self.client_draft,
            scope.as_ref().map(|scope| scope.project.as_str()),
            &self.grants,
        )
    }

    /// Trusted native UI action only. No protocol method issues this grant.
    pub(crate) fn grant_photo_reader(&mut self, studio: &Studio, app: &App, client: &str) {
        let Some(client) = ordinary_reader_client(client) else {
            return;
        };
        if ordinary_reader_client(&self.client_draft) != Some(client) {
            return;
        }
        let Some(scope) = self.photo_reader_scope(studio, app) else {
            return;
        };
        self.photo_reads.invalidate();
        grant_reader_scope(&mut self.grants, &scope.project, client, || {
            uuid::Uuid::new_v4().to_string()
        });
    }

    /// Revoke only this Photo scope's selected ordinary client. A matching
    /// client name on a different parent document is not an eligible grant.
    pub(crate) fn revoke_photo_reader(&mut self, studio: &Studio, app: &App, client: &str) {
        let Some(client) = ordinary_reader_client(client) else {
            return;
        };
        if ordinary_reader_client(&self.client_draft) != Some(client) {
            return;
        }
        let Some(scope) = self.photo_reader_scope(studio, app) else {
            return;
        };
        if revoke_reader_scope(&mut self.grants, &scope.project, client) {
            self.photo_reads.invalidate();
        }
    }
}

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

// BEGIN PHOTO_READER_POLICY_TESTS
#[cfg(test)]
mod reader_policy_tests {
    use super::*;

    fn grant(project: &str, token: &str, media: bool) -> Grant {
        Grant {
            project: project.into(),
            token: token.into(),
            media,
        }
    }

    #[test]
    fn photo_reader_requires_an_ordinary_client_before_issuing_a_token() {
        for invalid in [
            String::new(),
            " \t\n ".into(),
            "x".repeat(65),
            ASSISTANT_CLIENT_ID.into(),
        ] {
            let mut grants = HashMap::from([(
                ASSISTANT_CLIENT_ID.into(),
                grant("photo", "fixed-private", false),
            )]);
            assert!(!grant_reader_scope(
                &mut grants,
                "photo",
                &invalid,
                || panic!("invalid client must not issue token")
            ));
            assert_eq!(grants.len(), 1);
            assert_eq!(grants[ASSISTANT_CLIENT_ID].token, "fixed-private");
        }
        assert_eq!(
            ordinary_reader_client(&format!(" {} ", "x".repeat(64))),
            Some("x".repeat(64).as_str())
        );
    }

    #[test]
    fn photo_reader_grant_rotates_the_existing_doc_ledger_and_always_disables_media() {
        let mut grants = HashMap::from([
            ("reader".into(), grant("old-parent", "old-token", true)),
            ("other".into(), grant("other-parent", "untouched", true)),
        ]);
        assert!(grant_reader_scope(
            &mut grants,
            "current-photo",
            " reader ",
            || "first-read-token".into()
        ));
        assert_eq!(grants.len(), 2);
        assert_eq!(grants["reader"].project, "current-photo");
        assert_eq!(grants["reader"].token, "first-read-token");
        assert!(!grants["reader"].media);
        assert_eq!(grants["other"].token, "untouched");
        assert!(grant_reader_scope(
            &mut grants,
            "current-photo",
            "reader",
            || "rotated-read-token".into()
        ));
        assert_eq!(grants["reader"].token, "rotated-read-token");
        assert!(!grants["reader"].media);
    }

    #[test]
    fn photo_reader_revocation_cannot_remove_a_different_parent_or_fixed_client_grant() {
        let mut grants = HashMap::from([
            ("reader".into(), grant("parent", "parent-token", true)),
            (
                ASSISTANT_CLIENT_ID.into(),
                grant("photo", "fixed-private", false),
            ),
        ]);
        assert!(!revoke_reader_scope(&mut grants, "photo", "reader"));
        assert!(!revoke_reader_scope(
            &mut grants,
            "photo",
            ASSISTANT_CLIENT_ID
        ));
        assert!(!revoke_reader_scope(&mut grants, "photo", " \n "));
        assert_eq!(grants["reader"].token, "parent-token");
        assert_eq!(grants[ASSISTANT_CLIENT_ID].token, "fixed-private");
        grants.insert("reader".into(), grant("photo", "photo-token", false));
        assert!(revoke_reader_scope(&mut grants, "photo", " reader "));
        assert!(!grants.contains_key("reader"));
        assert_eq!(grants[ASSISTANT_CLIENT_ID].token, "fixed-private");
    }

    #[test]
    fn photo_reader_view_only_projects_the_selected_current_scope_ordinary_token() {
        let grants = HashMap::from([
            ("reader".into(), grant("photo", "selected-token", false)),
            ("other".into(), grant("photo", "other-private", false)),
            (
                "parent-reader".into(),
                grant("parent", "parent-private", true),
            ),
            (
                ASSISTANT_CLIENT_ID.into(),
                grant("photo", "fixed-private", false),
            ),
        ]);
        let view = reader_projection("owned-app", " reader ", Some("photo"), &grants);
        assert_eq!(view.client, " reader ");
        assert_eq!(view.instance, "owned-app");
        assert_eq!(view.token, "selected-token");
        assert!(view.can_grant && view.can_revoke);
        assert_eq!(view.status, "已允许读取当前图像文档结构");
        let parent = reader_projection("owned-app", "parent-reader", Some("photo"), &grants);
        assert!(parent.token.is_empty());
        assert!(parent.can_grant && !parent.can_revoke);
        let fixed = reader_projection("owned-app", ASSISTANT_CLIENT_ID, Some("photo"), &grants);
        assert!(fixed.token.is_empty());
        assert!(!fixed.can_grant && !fixed.can_revoke);
    }

    #[test]
    fn photo_reader_source_loss_removes_credentials_and_both_actions_from_the_view() {
        let grants = HashMap::from([("reader".into(), grant("photo", "old-private", false))]);
        let view = reader_projection("owned-app", "reader", None, &grants);
        assert_eq!(view.client, "reader");
        assert!(view.instance.is_empty() && view.token.is_empty());
        assert!(!view.can_grant && !view.can_revoke);
        assert_eq!(view.status, "当前图像文档信息尚未就绪");
        let next = reader_projection("owned-app", "reader", Some("replacement-photo"), &grants);
        assert!(next.token.is_empty());
        assert!(next.can_grant && !next.can_revoke);
    }
}
// END PHOTO_READER_POLICY_TESTS
