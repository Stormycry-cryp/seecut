//! Original-operation Clip save and independent fresh manifest read.
//! Paths and document bytes are supplied only by opaque host Session snapshots.
use super::*;
use canvas_policy::OutputOriginCapture;
use concat_editor_mcp::clip_save::{ClipSaveDurability, ClipSaveReason, ClipSaveStage};
use concat_editor_mcp::{ClipManifestName, ClipPersistedState, ClipSaveData, ClipSaveProof};
use concat_host::projects::{
    AgentSaveNotCommitted, AgentSaveResult, AgentSaveStage, PersistedClipSnapshot,
    SaveInvalidation, SaveLane, SavePermitRefusal,
};
use std::collections::VecDeque;

struct Completion {
    registration: u64,
    epoch: u64,
    account: OutputOriginCapture,
    request: Request,
    scope: Scope,
    permit: Permit,
    generation: u64,
    lane: Arc<SaveLane>,
    response: Response,
    ticket: Option<u64>,
}

fn completions() -> &'static Mutex<VecDeque<Completion>> {
    static QUEUE: OnceLock<Mutex<VecDeque<Completion>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

thread_local! {
    static COMPLETION_TIMER: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
}

fn ensure_completion_drain() {
    COMPLETION_TIMER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(100),
                drain_completions,
            );
            *slot = Some(timer);
        }
    });
}

/// Check both real UI borrows before removing any original result. Keeping the
/// borrowed state with the batch prevents another UI delivery from intervening.
fn take_ready_completions<'a, T>(
    queue: &Mutex<VecDeque<Completion>>,
    state: &'a std::cell::RefCell<T>,
    bridge: impl Fn(&T) -> &std::cell::RefCell<BridgeUi>,
) -> Option<(std::cell::RefMut<'a, T>, Vec<Completion>)> {
    let state = state.try_borrow_mut().ok()?;
    if bridge(&state).try_borrow_mut().is_err() {
        return None;
    }
    let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
    let count = queue.len().min(16);
    let batch = queue.drain(..count).collect();
    Some((state, batch))
}

impl Completion {
    /// Source changes cannot cancel a committed result, but they must prevent
    /// that old result from applying a current-document UI change.
    fn apply_to_original_source(&self, apply: impl FnOnce(u64)) {
        if let Some(ticket) = self.ticket
            && crate::clip_save_fence::epoch() == self.epoch
        {
            apply(ticket);
        }
    }

    /// Preserve the immutable old outcome under its original receipt identity.
    fn store_original(self, bridge: &mut BridgeUi) -> bool {
        crate::clip_save_fence::finish(self.registration);
        let parameters = self.request.clip_parameters.as_ref().expect("frozen Save");
        bridge.canvas_receipts.finish(
            &self.scope,
            &self.request.client_id,
            parameters,
            self.response,
        )
    }
}

/// A native modal may pump events while holding Studio or Bridge. A busy
/// delivery leaves every completion queued for the next native event/timer.
fn drain_completions() {
    if completions()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        return;
    }
    Shell::with(|shell, app| {
        let Some((mut studio, batch)) =
            take_ready_completions(completions(), &shell.studio, |studio| {
                studio.editor_mcp.as_ref()
            })
        else {
            return;
        };
        let mut marked_saved = false;
        for done in batch {
            done.apply_to_original_source(|ticket| {
                if capture_origin(&studio.editor_mcp.borrow()).as_ref() == Ok(&done.account)
                    && clip_execute::final_check_no_io(
                        &studio,
                        &app,
                        &done.request,
                        &done.scope,
                        &done.permit,
                        done.generation,
                    )
                    .is_ok()
                    && clip_edit::approval_preflight(
                        &studio,
                        &done
                            .request
                            .clip_parameters
                            .as_ref()
                            .expect("frozen Save")
                            .action,
                    )
                    .is_ok()
                {
                    studio.mcp_clip_mark_saved(
                        done.generation,
                        done.request.revision.expect("frozen Save revision"),
                        &done.lane,
                        ticket,
                    );
                    marked_saved = true;
                }
            });
            done.store_original(&mut studio.editor_mcp.borrow_mut());
        }
        if marked_saved {
            studio.publish(&app, &shell.models);
        }
    });
}

fn capture_origin(bridge: &BridgeUi) -> Result<OutputOriginCapture, ()> {
    Ok(OutputOriginCapture {
        account_scope: crate::cloud::try_canvas_output_scope()?,
        assistant_session: bridge.assistant_session.clone(),
    })
}

fn manifest_name(name: &str) -> Result<ClipManifestName, &'static str> {
    match name {
        "concat.json" => Ok(ClipManifestName::Concat),
        "wolfcut.json" => Ok(ClipManifestName::Wolfcut),
        _ => Err("ioFailure"),
    }
}

fn response(mut source: ClipSaveData, proof: ClipSaveProof) -> Response {
    let status = match &proof {
        ClipSaveProof::Pending { .. } => "pending",
        ClipSaveProof::NotCommitted { .. } => "saveNotCommitted",
        ClipSaveProof::Verified { .. } => "ok",
        ClipSaveProof::OutcomeUnknown { .. } => "outcomeUnknown",
    };
    source.save_proof = proof;
    if source.validate_for_status(status).is_err() {
        return Response::error("outcomeUnknown");
    }
    match serde_json::to_value(source) {
        Ok(data) => Response {
            status: status.into(),
            data: Some(data),
        },
        Err(_) => Response::error("outcomeUnknown"),
    }
}

fn not_committed(source: ClipSaveData, claimed: bool, reason: ClipSaveReason) -> Response {
    response(
        source,
        ClipSaveProof::NotCommitted {
            version: 1,
            commit_claimed: claimed,
            rename_attempted: false,
            reason,
        },
    )
}

fn refusal(reason: AgentSaveNotCommitted) -> ClipSaveReason {
    match reason {
        AgentSaveNotCommitted::ExpiredAfterClaim => ClipSaveReason::Expired,
        AgentSaveNotCommitted::Permit(SavePermitRefusal::Invalidated(reason)) => match reason {
            SaveInvalidation::Expired => ClipSaveReason::Expired,
            SaveInvalidation::Superseded => ClipSaveReason::Superseded,
            SaveInvalidation::DocumentChanged
            | SaveInvalidation::RevisionChanged
            | SaveInvalidation::BindingUnstable => ClipSaveReason::SourceChanged,
            _ => ClipSaveReason::Cancelled,
        },
        AgentSaveNotCommitted::Permit(SavePermitRefusal::WrongLane)
        | AgentSaveNotCommitted::ManifestChanged => ClipSaveReason::TargetChanged,
        AgentSaveNotCommitted::ResourceLimit => ClipSaveReason::ResourceLimit,
        AgentSaveNotCommitted::Preparation(
            concat_host::projects::OwnedProjectError::Ownership(_),
        ) => ClipSaveReason::OwnershipChanged,
        AgentSaveNotCommitted::Preparation(concat_host::projects::OwnedProjectError::Project(
            _,
        )) => ClipSaveReason::IoFailure,
        AgentSaveNotCommitted::Engine(_) => ClipSaveReason::IoFailure,
    }
}

fn unknown(source: ClipSaveData, attempted: bool, renamed: bool, stage: ClipSaveStage) -> Response {
    response(
        source,
        ClipSaveProof::OutcomeUnknown {
            version: 1,
            commit_claimed: true,
            rename_attempted: attempted,
            rename_succeeded: renamed,
            stage,
        },
    )
}

/// Only the typed actual host result can construct a terminal save proof.
fn completed(source: ClipSaveData, result: AgentSaveResult) -> (Response, Option<u64>) {
    match result {
        AgentSaveResult::NotCommitted {
            commit_claimed,
            rename_attempted: false,
            reason,
            ..
        } => (not_committed(source, commit_claimed, refusal(reason)), None),
        AgentSaveResult::NotCommitted { .. } => (Response::error("outcomeUnknown"), None),
        AgentSaveResult::StartedUnverified {
            stage,
            rename_attempted,
            rename_succeeded,
            ..
        } => {
            let stage = match stage {
                AgentSaveStage::DeadlineAfterClaim | AgentSaveStage::AfterClaim => {
                    ClipSaveStage::AfterClaim
                }
                AgentSaveStage::Rename => ClipSaveStage::Rename,
                AgentSaveStage::DirectorySync => ClipSaveStage::DirectorySync,
                AgentSaveStage::AfterRename | AgentSaveStage::Readback => ClipSaveStage::Readback,
            };
            (
                unknown(source, rename_attempted, rename_succeeded, stage),
                None,
            )
        }
        AgentSaveResult::Verified(proof) => {
            let Some(name) = proof.manifest().file_name().and_then(|name| name.to_str()) else {
                return (unknown(source, true, true, ClipSaveStage::Readback), None);
            };
            let Ok(name) = manifest_name(name) else {
                return (unknown(source, true, true, ClipSaveStage::Readback), None);
            };
            let Ok(actual) = clip_edit::persisted_document_state_id(proof.project()) else {
                return (
                    unknown(source, true, true, ClipSaveStage::EngineParse),
                    None,
                );
            };
            if actual != source.source_document_state_id {
                return (
                    unknown(source, true, true, ClipSaveStage::EngineParse),
                    None,
                );
            }
            let ticket = proof.ticket();
            let saved = ClipSaveProof::Verified {
                version: 1,
                save_ticket: ticket.to_string(),
                persisted_revision: source.source_revision,
                manifest_name: name,
                byte_length: proof.bytes(),
                snapshot_sha256: proof.snapshot_sha256().to_owned(),
                manifest_sha256: proof.manifest_sha256().to_owned(),
                persisted_document_state_id: actual,
                document_version: proof.document_version(),
                durability: ClipSaveDurability::Confirmed,
            };
            let response = response(source, saved);
            let verified = (response.status == "ok").then_some(ticket);
            (response, verified)
        }
    }
}

/// Native source frozen once after the original approval preflight. There is
/// no deserializer: model arguments cannot supply or replace this identity.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ApprovedSource {
    generation: u64,
    document_state_id: String,
}

impl ApprovedSource {
    pub(super) fn capture(
        project: &concat_project::Project,
        generation: u64,
    ) -> Result<Self, Response> {
        Ok(Self {
            generation,
            document_state_id: clip_edit::persisted_document_state_id(project)
                .map_err(|error| Response::error(error.status()))?,
        })
    }
}

fn source(request: &Request, scope: &Scope, permit: &Permit) -> Result<ClipSaveData, Response> {
    let p = request
        .clip_parameters
        .as_ref()
        .ok_or_else(|| Response::error("invalidInput"))?;
    let original = permit
        .clip_save_origin
        .as_ref()
        .ok_or_else(|| Response::error("notAuthorized"))?;
    let source = ClipSaveData {
        app_instance_id: scope.instance.clone(),
        client_id: request.client_id.clone(),
        project_id: scope.project.clone(),
        document_session_id: scope.session.clone(),
        operation_id: p.operation_id.clone(),
        client_sequence: p.client_sequence,
        tool: "clip_edit".into(),
        edit: concat_editor_mcp::ClipEdit::Save {},
        generation: original.generation,
        source_revision: request
            .revision
            .ok_or_else(|| Response::error("invalidInput"))?,
        source_selection_revision: p.selection_revision,
        source_timeline_id: p.action.timeline_id.clone(),
        source_edit_state_id: p.action.expected_edit_state_id.clone(),
        source_document_state_id: original.document_state_id.clone(),
        save_proof: ClipSaveProof::Pending { version: 1 },
    };
    source
        .validate_for_status("pending")
        .map_err(|_| Response::error("invalidInput"))?;
    Ok(source)
}

/// The last memory-only source check and the host preparation are one path, so
/// a stale approval cannot allocate a snapshot for the newly edited document.
fn prepare_snapshot(
    source: &ClipSaveData,
    session: &mut concat_host::session::Session,
    lane: &Arc<SaveLane>,
    generation: u64,
    deadline: Instant,
) -> Result<
    (
        concat_host::projects::AgentSaveSnapshot,
        concat_host::projects::SaveRevoker,
    ),
    Response,
> {
    if generation != source.generation
        || clip_edit::persisted_document_state_id(session.project()).as_ref()
            != Ok(&source.source_document_state_id)
    {
        return Err(not_committed(
            source.clone(),
            false,
            ClipSaveReason::SourceChanged,
        ));
    }
    session.prepare_agent_save(lane, deadline).map_err(|error| {
        not_committed(
            source.clone(),
            false,
            refusal(AgentSaveNotCommitted::Preparation(error)),
        )
    })
}

/// execute_clip has consumed the exact original permit and reserved both caches.
pub(super) fn queue_save(
    request: &Request,
    scope: &Scope,
    permit: &Permit,
    studio: &mut Studio,
    app: &App,
) -> Response {
    let source = match source(request, scope, permit) {
        Ok(source) => source,
        Err(error) => return error,
    };
    let generation = source.generation;
    if let Err(error) =
        clip_execute::final_check_no_io(studio, app, request, scope, permit, generation)
    {
        let reason = match error.status.as_str() {
            "leaseExpired" => ClipSaveReason::Expired,
            "notAuthorized" => ClipSaveReason::Cancelled,
            _ => ClipSaveReason::SourceChanged,
        };
        return not_committed(source, false, reason);
    }
    let parameters = request
        .clip_parameters
        .as_ref()
        .expect("validated original Save");
    if clip_edit::approval_preflight(studio, &parameters.action).is_err() {
        return not_committed(source, false, ClipSaveReason::SourceChanged);
    }
    let (deadline, account) = {
        let bridge = studio.editor_mcp.borrow();
        let deadline = match bridge.writes.original_deadline(
            scope,
            &request.client_id,
            &request.client_token,
            Instant::now(),
        ) {
            Ok(Some(deadline)) => deadline.min(permit.expires_at),
            Ok(None) => permit.expires_at,
            Err(_) => return not_committed(source, false, ClipSaveReason::Cancelled),
        };
        let account = match capture_origin(&bridge) {
            Ok(account) => account,
            Err(()) => return not_committed(source, false, ClipSaveReason::SourceChanged),
        };
        (deadline, account)
    };
    let Some(lane) = studio.mcp_clip_save_lane() else {
        return not_committed(source, false, ClipSaveReason::SourceChanged);
    };
    let current_generation = studio.mcp_clip_state().0;
    let Some(session) = studio.session.as_mut() else {
        return not_committed(source, false, ClipSaveReason::SourceChanged);
    };
    let (snapshot, revoker) =
        match prepare_snapshot(&source, session, &lane, current_generation, deadline) {
            Ok(prepared) => prepared,
            Err(error) => return error,
        };
    let registration = crate::clip_save_fence::register(permit.id, revoker);
    let epoch = crate::clip_save_fence::epoch();
    let pending = response(source.clone(), ClipSaveProof::Pending { version: 1 });
    let worker_lane = lane.clone();
    let request = request.clone();
    let scope = scope.clone();
    let permit = permit.clone();
    ensure_completion_drain();
    crate::host::spawn_detached(move || {
        let (response, ticket) = completed(source, worker_lane.write_agent_save(snapshot));
        completions()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(Completion {
                registration,
                epoch,
                account,
                request,
                scope,
                permit,
                generation,
                lane,
                response,
                ticket,
            });
        let _ = slint::invoke_from_event_loop(drain_completions);
    });
    pending
}

pub(super) struct ReadJob {
    request: Request,
    scope: Scope,
    generation: u64,
    revision: u64,
    account: OutputOriginCapture,
    source_epoch: u64,
    lane: Arc<SaveLane>,
    snapshot: PersistedClipSnapshot,
}

pub(super) fn prepare_read(
    bridge: &BridgeUi,
    request: &Request,
    studio: &Studio,
    app: &App,
    kind: &str,
    ids: &DocumentIds,
) -> UiReply {
    let error = |status| UiReply::Response(Response::error(status));
    if kind != "clip" {
        return error("unsupportedWorkspace");
    }
    if modal_busy(studio, app, true) || app.get_app_read_blocked() || studio.mcp_clip_state().3 {
        return error("busy");
    }
    if !bridge.authorized(request, &ids.project, false) {
        return error("notAuthorized");
    }
    let Some(scope) = request_scope(request) else {
        return error("invalidInput");
    };
    if scope != bridge.scope(ids) {
        return error("wrongProject");
    }
    let Some(lane) = studio.mcp_clip_save_lane() else {
        return error("wrongProject");
    };
    let Some(session) = studio.session.as_ref() else {
        return error("wrongProject");
    };
    let snapshot = match session.prepare_persisted_read(&lane) {
        Ok(snapshot) => snapshot,
        Err(_) => return error("ownershipChanged"),
    };
    let account = match capture_origin(bridge) {
        Ok(account) => account,
        Err(()) => return error("busy"),
    };
    UiReply::PersistedClip(Box::new(ReadJob {
        request: request.clone(),
        scope,
        generation: studio.mcp_clip_state().0,
        revision: studio.mcp_clip_state().1,
        account,
        source_epoch: crate::clip_save_fence::epoch(),
        lane,
        snapshot,
    }))
}

pub(super) fn dispatch_read(job: ReadJob) -> Response {
    let ReadJob {
        request,
        scope,
        generation,
        revision,
        account,
        source_epoch,
        lane,
        snapshot,
    } = job;
    let proof = match lane.read_persisted_clip(snapshot) {
        Ok(proof) => proof,
        Err(_) => return Response::error("ioFailure"),
    };
    let name = match manifest_name(proof.manifest_name()) {
        Ok(name) => name,
        Err(status) => return Response::error(status),
    };
    let document_state_id = match clip_edit::persisted_document_state_id(proof.project()) {
        Ok(digest) => digest,
        Err(error) => return Response::error(error.status()),
    };
    let data = ClipPersistedState {
        clip_persistence_protocol_version: 1,
        source: "freshManifest".into(),
        app_instance_id: scope.instance.clone(),
        client_id: request.client_id.clone(),
        project_id: scope.project.clone(),
        document_session_id: scope.session.clone(),
        generation,
        observed_revision: revision,
        manifest_name: name,
        byte_length: proof.bytes(),
        manifest_sha256: proof.manifest_sha256().to_owned(),
        document_state_id,
        document_version: proof.document_version(),
    };
    if data.validate().is_err() {
        return Response::error("ioFailure");
    }
    let stable = on_ui(move |shell, app| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return Err(Response::error("busy"));
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return Err(Response::error("busy"));
        };
        let Some((kind, ids)) = bridge.active_ids(&studio, &app) else {
            return Err(Response::error("wrongProject"));
        };
        if kind != "clip"
            || bridge.scope(&ids) != scope
            || studio.mcp_clip_state().0 != generation
            || studio.mcp_clip_state().1 != revision
            || crate::clip_save_fence::epoch() != source_epoch
        {
            return Err(Response::error("staleContext"));
        }
        if !bridge.authorized(&request, &ids.project, false)
            || capture_origin(&bridge).as_ref() != Ok(&account)
        {
            return Err(Response::error("notAuthorized"));
        }
        if modal_busy(&studio, &app, true)
            || app.get_app_read_blocked()
            || studio.mcp_clip_state().3
        {
            return Err(Response::error("busy"));
        }
        Ok(())
    });
    match stable {
        Ok(Ok(())) => match serde_json::to_value(data) {
            Ok(data) => Response::ok(data),
            Err(_) => Response::error("ioFailure"),
        },
        Ok(Err(error)) | Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_host::projects;

    struct Fixture {
        session: concat_host::session::Session,
        lane: Arc<SaveLane>,
        _dir: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let session = projects::create_owned(
                &dir.path().to_string_lossy(),
                "Save proof",
                1280,
                720,
                30,
                1,
            )
            .unwrap()
            .into_session()
            .unwrap();
            Self {
                session,
                lane: Arc::new(SaveLane::default()),
                _dir: dir,
            }
        }
        fn source(&self) -> ClipSaveData {
            ClipSaveData {
                app_instance_id: "instance".into(),
                client_id: "client".into(),
                project_id: "project".into(),
                document_session_id: "session".into(),
                operation_id: concat_editor_mcp::operation_id("client", "session", 1).unwrap(),
                client_sequence: 1,
                tool: "clip_edit".into(),
                edit: concat_editor_mcp::ClipEdit::Save {},
                generation: 3,
                source_revision: 7,
                source_selection_revision: 2,
                source_timeline_id: format!(
                    "timeline:{}",
                    self.session.project().active_timeline_id
                ),
                source_edit_state_id: format!("clip-edit-v1-{}", "a".repeat(64)),
                source_document_state_id: clip_edit::persisted_document_state_id(
                    self.session.project(),
                )
                .unwrap(),
                save_proof: ClipSaveProof::Pending { version: 1 },
            }
        }
        fn snapshot(&mut self) -> (projects::AgentSaveSnapshot, projects::SaveRevoker) {
            self.session
                .prepare_agent_save(&self.lane, Instant::now() + Duration::from_secs(60))
                .unwrap()
        }
    }

    #[test]
    fn actual_host_save_receipt_matches_independent_disk_parse() {
        let mut fixture = Fixture::new();
        fixture
            .session
            .apply(concat_project::Command::AddTrack)
            .unwrap();
        let source = fixture.source();
        let (snapshot, _) = fixture.snapshot();
        let (response, ticket) = completed(source.clone(), fixture.lane.write_agent_save(snapshot));
        assert_eq!(response.status, "ok");
        assert!(ticket.is_some());
        let receipt: ClipSaveData = serde_json::from_value(response.data.unwrap()).unwrap();
        receipt.validate_for_status("ok").unwrap();
        let read = fixture
            .session
            .prepare_persisted_read(&fixture.lane)
            .unwrap();
        let disk = fixture.lane.read_persisted_clip(read).unwrap();
        let ClipSaveProof::Verified {
            manifest_sha256,
            persisted_document_state_id,
            ..
        } = receipt.save_proof
        else {
            panic!("actual verified result required")
        };
        assert_eq!(manifest_sha256, disk.manifest_sha256());
        assert_eq!(
            persisted_document_state_id,
            clip_edit::persisted_document_state_id(disk.project()).unwrap()
        );
        assert_eq!(persisted_document_state_id, source.source_document_state_id);
    }

    #[test]
    fn selection_write_revokes_original_worker_before_file_mutation() {
        let mut fixture = Fixture::new();
        let original =
            std::fs::read(std::path::Path::new(fixture.session.path()).join("concat.json"))
                .unwrap();
        fixture
            .session
            .apply(concat_project::Command::AddTrack)
            .unwrap();
        let source = fixture.source();
        let (snapshot, revoker) = fixture.snapshot();
        let registration = crate::clip_save_fence::register(41, revoker);
        let mut selection = crate::clip_save_fence::Selection::<Vec<String>>::default();
        selection.push("clip:changed".into());
        let (response, ticket) = completed(source, fixture.lane.write_agent_save(snapshot));
        assert_eq!(response.status, "saveNotCommitted");
        assert!(ticket.is_none());
        let receipt: ClipSaveData = serde_json::from_value(response.data.unwrap()).unwrap();
        assert!(matches!(
            receipt.save_proof,
            ClipSaveProof::NotCommitted {
                commit_claimed: false,
                rename_attempted: false,
                reason: ClipSaveReason::SourceChanged,
                ..
            }
        ));
        assert_eq!(
            original,
            std::fs::read(std::path::Path::new(fixture.session.path()).join("concat.json"))
                .unwrap()
        );
        crate::clip_save_fence::finish(registration);
    }

    #[test]
    fn actual_host_revocation_and_late_revocation_have_distinct_receipts() {
        for before in [true, false] {
            let mut fixture = Fixture::new();
            let source = fixture.source();
            let (snapshot, revoker) = fixture.snapshot();
            let registration = crate::clip_save_fence::register(42, revoker);
            if before {
                crate::clip_save_fence::revoke_permit(42);
            }
            let result = fixture.lane.write_agent_save(snapshot);
            if !before {
                crate::clip_save_fence::invalidate(SaveInvalidation::AccountChanged);
            }
            let (response, ticket) = completed(source, result);
            assert_eq!(
                response.status,
                if before { "saveNotCommitted" } else { "ok" }
            );
            assert_eq!(ticket.is_some(), !before);
            crate::clip_save_fence::finish(registration);
        }
    }

    #[test]
    fn verified_host_result_cannot_prove_a_different_original_document() {
        let mut fixture = Fixture::new();
        let mut source = fixture.source();
        source.source_document_state_id = format!("clip-v1-{}", "f".repeat(64));
        let (snapshot, _) = fixture.snapshot();
        let (response, ticket) = completed(source, fixture.lane.write_agent_save(snapshot));
        assert_eq!(response.status, "outcomeUnknown");
        assert!(ticket.is_none());
        let receipt: ClipSaveData = serde_json::from_value(response.data.unwrap()).unwrap();
        assert!(matches!(
            receipt.save_proof,
            ClipSaveProof::OutcomeUnknown {
                rename_attempted: true,
                rename_succeeded: true,
                stage: ClipSaveStage::EngineParse,
                ..
            }
        ));
    }

    #[test]
    fn original_expiry_is_not_extended_by_native_registration() {
        let mut fixture = Fixture::new();
        let source = fixture.source();
        let (snapshot, revoker) = fixture
            .session
            .prepare_agent_save(&fixture.lane, Instant::now() - Duration::from_secs(1))
            .unwrap();
        let registration = crate::clip_save_fence::register(43, revoker);
        let (response, ticket) = completed(source, fixture.lane.write_agent_save(snapshot));
        assert_eq!(response.status, "saveNotCommitted");
        assert!(ticket.is_none());
        let receipt: ClipSaveData = serde_json::from_value(response.data.unwrap()).unwrap();
        assert!(matches!(
            receipt.save_proof,
            ClipSaveProof::NotCommitted {
                rename_attempted: false,
                reason: ClipSaveReason::Expired,
                ..
            }
        ));
        crate::clip_save_fence::finish(registration);
    }

    #[test]
    fn busy_native_borrows_keep_completion_and_late_scope_keeps_new_document_dirty() {
        let mut fixture = Fixture::new();
        let source = fixture.source();
        let request: Request = serde_json::from_value(json!({
            "instanceId":source.app_instance_id,"clientId":source.client_id,"clientToken":"token",
            "method":"clipEdit","projectId":source.project_id,"documentSessionId":source.document_session_id,
            "revision":source.source_revision,
            "clipParameters":{"operationId":source.operation_id,"clientSequence":source.client_sequence,
                "selectionRevision":source.source_selection_revision,
                "action":{"timelineId":source.source_timeline_id,"expectedEditStateId":source.source_edit_state_id,
                    "edit":{"kind":"save"}}}
        })).unwrap();
        concat_editor_mcp::validate_request_shape(&request).unwrap();
        let scope = request_scope(&request).unwrap();
        let parameters = request.clip_parameters.as_ref().unwrap();
        let mut bridge = BridgeUi::new();
        bridge.writes.bind_scope(Some(scope.clone()));
        let intent = Intent::from_request(&request).unwrap();
        let permit_id = bridge
            .canvas_permits
            .grant(
                scope.clone(),
                request.client_id.clone(),
                intent.clone(),
                Instant::now(),
            )
            .unwrap();
        let permit = bridge
            .canvas_permits
            .claim(&scope, &request.client_id, &intent, Instant::now())
            .unwrap();
        assert!(bridge.canvas_receipts.reserve(
            scope.clone(),
            request.client_id.clone(),
            parameters,
            Instant::now()
        ));
        assert!(bridge.canvas_receipts.finish(
            &scope,
            &request.client_id,
            parameters,
            response(source.clone(), ClipSaveProof::Pending { version: 1 })
        ));
        let query = concat_editor_mcp::OperationQuery {
            operation_id: parameters.operation_id.clone(),
            client_sequence: parameters.client_sequence,
        };
        let (snapshot, revoker) = fixture.snapshot();
        let registration = crate::clip_save_fence::register(permit_id, revoker);
        let epoch = crate::clip_save_fence::epoch();
        let (response, ticket) = completed(source, fixture.lane.write_agent_save(snapshot));
        assert_eq!(response.status, "ok");
        let original_response = response.clone();
        let client = request.client_id.clone();
        let queue = Mutex::new(VecDeque::from([Completion {
            registration,
            epoch,
            account: OutputOriginCapture {
                account_scope: "local".into(),
                assistant_session: None,
            },
            request,
            scope: scope.clone(),
            permit,
            generation: 3,
            lane: fixture.lane.clone(),
            response,
            ticket,
        }]));
        let bridge = std::rc::Rc::new(std::cell::RefCell::new(bridge));
        let state = std::cell::RefCell::new(bridge.clone());

        let held_state = state.borrow();
        assert!(take_ready_completions(&queue, &state, |bridge| bridge.as_ref()).is_none());
        assert_eq!(queue.lock().unwrap().len(), 1);
        drop(held_state);
        let held_bridge = bridge.borrow_mut();
        assert!(take_ready_completions(&queue, &state, |bridge| bridge.as_ref()).is_none());
        assert_eq!(queue.lock().unwrap().len(), 1);
        drop(held_bridge);
        assert_eq!(
            bridge
                .borrow()
                .canvas_receipts
                .query_family(&scope, &client, &query, Instant::now(), true)
                .status,
            "pending"
        );

        // This is the real native scope-change entry, after the disk result.
        bridge.borrow_mut().writes.bind_scope(Some(Scope {
            session: "new-document".into(),
            ..scope.clone()
        }));
        let new_document_dirty = std::cell::Cell::new(true);
        let (ready, batch) =
            take_ready_completions(&queue, &state, |bridge| bridge.as_ref()).unwrap();
        assert_eq!(batch.len(), 1);
        for done in batch {
            done.apply_to_original_source(|_| new_document_dirty.set(false));
            assert!(done.store_original(&mut ready.as_ref().borrow_mut()));
        }
        drop(ready);
        assert!(new_document_dirty.get());
        let stored = bridge.borrow().canvas_receipts.query_family(
            &scope,
            &client,
            &query,
            Instant::now(),
            true,
        );
        assert_eq!(stored.status, original_response.status);
        assert_eq!(stored.data, original_response.data);
        let (_, repeated) =
            take_ready_completions(&queue, &state, |bridge| bridge.as_ref()).unwrap();
        assert!(repeated.is_empty());
    }
    fn save_request(data: &ClipSaveData) -> Request {
        let request: Request = serde_json::from_value(json!({
            "instanceId":data.app_instance_id,"clientId":data.client_id,"clientToken":"token",
            "method":"clipEdit","projectId":data.project_id,"documentSessionId":data.document_session_id,
            "revision":data.source_revision,
            "clipParameters":{"operationId":data.operation_id,"clientSequence":data.client_sequence,
                "selectionRevision":data.source_selection_revision,
                "action":{"timelineId":data.source_timeline_id,"expectedEditStateId":data.source_edit_state_id,
                    "edit":{"kind":"save"}}}
        })).unwrap();
        concat_editor_mcp::validate_request_shape(&request).unwrap();
        request
    }

    #[test]
    fn approved_source_survives_same_session_edit_and_repeated_receipt() {
        let mut fixture = Fixture::new();
        let original = fixture.source();
        let manifest = std::path::Path::new(fixture.session.path()).join("concat.json");
        let original_bytes = std::fs::read(&manifest).unwrap();
        let request = save_request(&original);
        let scope = request_scope(&request).unwrap();
        let intent = Intent::from_request(&request).unwrap();
        let parameters = request.clip_parameters.as_ref().unwrap();
        let mut bridge = BridgeUi::new();
        let origin =
            ApprovedSource::capture(fixture.session.project(), original.generation).unwrap();
        bridge
            .canvas_permits
            .grant_with_clip_save_origin(
                scope.clone(),
                request.client_id.clone(),
                intent.clone(),
                Instant::now(),
                Some(origin),
            )
            .unwrap();
        // Real edit after trusted approval, before arrival of the old raw request.
        fixture
            .session
            .apply(concat_project::Command::AddTrack)
            .unwrap();
        assert_ne!(
            original.source_document_state_id,
            clip_edit::persisted_document_state_id(fixture.session.project()).unwrap()
        );
        let permit = bridge
            .canvas_permits
            .claim(&scope, &request.client_id, &intent, Instant::now())
            .unwrap();
        let frozen = source(&request, &scope, &permit).unwrap();
        assert_eq!(
            frozen.source_document_state_id,
            original.source_document_state_id
        );
        assert_eq!(frozen.generation, original.generation);
        assert!(fixture.lane.is_latest(0));
        let refused = match prepare_snapshot(
            &frozen,
            &mut fixture.session,
            &fixture.lane,
            original.generation,
            Instant::now() + Duration::from_secs(60),
        ) {
            Err(response) => response,
            Ok(_) => panic!("stale approved Project must not allocate a snapshot"),
        };
        assert_eq!(refused.status, "saveNotCommitted");
        let data: ClipSaveData = serde_json::from_value(refused.data.clone().unwrap()).unwrap();
        data.validate_for_status("saveNotCommitted").unwrap();
        assert_eq!(
            data.source_document_state_id,
            original.source_document_state_id
        );
        assert_eq!(data.generation, original.generation);
        assert_eq!(data.source_revision, original.source_revision);
        assert_eq!(data.source_edit_state_id, original.source_edit_state_id);
        assert!(matches!(
            data.save_proof,
            ClipSaveProof::NotCommitted {
                commit_claimed: false,
                rename_attempted: false,
                reason: ClipSaveReason::SourceChanged,
                ..
            }
        ));
        assert!(
            fixture.lane.is_latest(0),
            "host preparation did not allocate a ticket"
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), original_bytes);
        assert!(bridge.canvas_receipts.reserve(
            scope.clone(),
            request.client_id.clone(),
            parameters,
            Instant::now()
        ));
        assert!(bridge.canvas_receipts.finish(
            &scope,
            &request.client_id,
            parameters,
            refused.clone()
        ));
        let query = concat_editor_mcp::OperationQuery {
            operation_id: parameters.operation_id.clone(),
            client_sequence: parameters.client_sequence,
        };
        for _ in 0..2 {
            let same = bridge.canvas_receipts.query_family(
                &scope,
                &request.client_id,
                &query,
                Instant::now(),
                true,
            );
            assert_eq!(same.status, refused.status);
            assert_eq!(same.data, refused.data);
        }
    }

    #[test]
    fn approved_source_prepares_only_original_generation_and_document() {
        for changed_generation in [false, true] {
            let mut fixture = Fixture::new();
            let original = fixture.source();
            let request = save_request(&original);
            let scope = request_scope(&request).unwrap();
            let intent = Intent::from_request(&request).unwrap();
            let mut bridge = BridgeUi::new();
            let origin =
                ApprovedSource::capture(fixture.session.project(), original.generation).unwrap();
            bridge
                .canvas_permits
                .grant_with_clip_save_origin(
                    scope.clone(),
                    request.client_id.clone(),
                    intent.clone(),
                    Instant::now(),
                    Some(origin),
                )
                .unwrap();
            let permit = bridge
                .canvas_permits
                .claim(&scope, &request.client_id, &intent, Instant::now())
                .unwrap();
            let frozen = source(&request, &scope, &permit).unwrap();
            let prepared = prepare_snapshot(
                &frozen,
                &mut fixture.session,
                &fixture.lane,
                original.generation + u64::from(changed_generation),
                Instant::now() + Duration::from_secs(60),
            );
            if changed_generation {
                let rejected = match prepared {
                    Err(response) => response,
                    Ok(_) => panic!("changed generation"),
                };
                assert_eq!(rejected.status, "saveNotCommitted");
                let data: ClipSaveData = serde_json::from_value(rejected.data.unwrap()).unwrap();
                assert_eq!(data.generation, original.generation);
                assert!(fixture.lane.is_latest(0));
            } else {
                let (snapshot, _) = prepared.unwrap();
                let (saved, ticket) = completed(frozen, fixture.lane.write_agent_save(snapshot));
                assert_eq!(saved.status, "ok");
                assert!(ticket.is_some());
                let data: ClipSaveData = serde_json::from_value(saved.data.unwrap()).unwrap();
                data.validate_for_status("ok").unwrap();
                assert_eq!(
                    data.source_document_state_id,
                    original.source_document_state_id
                );
                assert_eq!(data.generation, original.generation);
            }
        }
    }

    #[test]
    fn save_without_trusted_approval_source_is_refused() {
        let fixture = Fixture::new();
        let request = save_request(&fixture.source());
        let scope = request_scope(&request).unwrap();
        let intent = Intent::from_request(&request).unwrap();
        let mut bridge = BridgeUi::new();
        bridge
            .canvas_permits
            .grant(
                scope.clone(),
                request.client_id.clone(),
                intent.clone(),
                Instant::now(),
            )
            .unwrap();
        let permit = bridge
            .canvas_permits
            .claim(&scope, &request.client_id, &intent, Instant::now())
            .unwrap();
        let rejected = source(&request, &scope, &permit).unwrap_err();
        assert_eq!(rejected.status, "notAuthorized");
        assert!(rejected.data.is_none());
        assert!(fixture.lane.is_latest(0));
    }
}
