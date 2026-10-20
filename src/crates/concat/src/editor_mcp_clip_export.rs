//! Clip 导出保持原审批来源、原操作回执和实际成片证明。
use super::*;
use canvas_policy::OutputOriginCapture;
use concat_editor_mcp::ClipEdit;
use concat_editor_mcp::clip_export::*;
use concat_host::export_publication::{PublishRevocation, PublishRevoker, new_publish_gate};
use std::collections::VecDeque;
use std::path::PathBuf;

#[path = "editor_mcp_clip_export_worker.rs"]
pub(super) mod worker;

#[derive(Clone, PartialEq)]
pub(super) struct ApprovedSource {
    generation: u64,
    document_state_id: String,
    action: concat_editor_mcp::ClipAction,
}
impl ApprovedSource {
    pub(super) fn capture(
        project: &concat_project::Project,
        generation: u64,
        action: &concat_editor_mcp::ClipAction,
    ) -> Result<Self, Response> {
        validate_action(project, action)?;
        Ok(Self {
            generation,
            document_state_id: clip_edit::persisted_document_state_id(project)
                .map_err(|e| Response::error(e.status()))?,
            action: action.clone(),
        })
    }
}

pub(super) fn validate_action(
    project: &concat_project::Project,
    action: &concat_editor_mcp::ClipAction,
) -> Result<(), Response> {
    let ClipEdit::ExportVideo {
        name,
        short_side,
        fps,
        ..
    } = &action.edit
    else {
        return Err(Response::error("invalidInput"));
    };
    validate_export_name(name).map_err(Response::error)?;
    if ![720, 1080, 1440, 2160].contains(short_side)
        || ![24, 30, 60].contains(fps)
        || concat_editor_mcp::clip_edit::clip_native_id(&action.timeline_id, "timeline").ok()
            != Some(project.active_timeline_id.as_str())
    {
        return Err(Response::error("invalidInput"));
    }
    let timeline = project.active();
    if timeline.clips.is_empty() {
        return Err(Response::error("invalidInput"));
    }
    let (width, height) = crate::panes::export::normalized_size(
        timeline.video.width,
        timeline.video.height,
        *short_side,
    )
    .ok_or_else(|| Response::error("resourceLimit"))?;
    if width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > 16_777_216 {
        return Err(Response::error("resourceLimit"));
    }
    Ok(())
}

fn capture_origin(bridge: &BridgeUi) -> Result<OutputOriginCapture, ()> {
    Ok(OutputOriginCapture {
        account_scope: crate::cloud::try_canvas_output_scope()?,
        assistant_session: bridge.assistant_session.clone(),
    })
}
fn pending_proof() -> ClipExportProof {
    ClipExportProof::Pending {
        version: 1,
        phase: ClipExportPhase::Preparing,
        frames_done: 0,
        frames_total: 0,
    }
}
fn response(mut source: ClipExportData, proof: ClipExportProof) -> Response {
    let status = match &proof {
        ClipExportProof::Pending { .. } => "pending",
        ClipExportProof::NotPublished { .. } => "exportNotPublished",
        ClipExportProof::RegistrationPending { .. } => "partial",
        ClipExportProof::Verified { .. } => "ok",
        ClipExportProof::OutcomeUnknown { .. } => "outcomeUnknown",
    };
    source.export_proof = proof;
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
fn not_published(source: ClipExportData, claimed: bool, reason: ClipExportReason) -> Response {
    response(
        source,
        ClipExportProof::NotPublished {
            version: 1,
            publish_claimed: claimed,
            publish_attempted: false,
            reason,
        },
    )
}
fn source(request: &Request, scope: &Scope, permit: &Permit) -> Result<ClipExportData, Response> {
    let p = request
        .clip_parameters
        .as_ref()
        .ok_or_else(|| Response::error("invalidInput"))?;
    let original = permit
        .clip_export_origin
        .as_ref()
        .ok_or_else(|| Response::error("notAuthorized"))?;
    if original.action != p.action {
        return Err(Response::error("notAuthorized"));
    }
    let source = ClipExportData {
        app_instance_id: scope.instance.clone(),
        client_id: request.client_id.clone(),
        project_id: scope.project.clone(),
        document_session_id: scope.session.clone(),
        operation_id: p.operation_id.clone(),
        client_sequence: p.client_sequence,
        tool: "clip_edit".into(),
        edit: original.action.edit.clone(),
        generation: original.generation,
        source_revision: request
            .revision
            .ok_or_else(|| Response::error("invalidInput"))?,
        source_selection_revision: p.selection_revision,
        source_timeline_id: original.action.timeline_id.clone(),
        source_edit_state_id: original.action.expected_edit_state_id.clone(),
        source_document_state_id: original.document_state_id.clone(),
        output_id: uuid::Uuid::new_v4().to_string(),
        registration_id: uuid::Uuid::new_v4().to_string(),
        export_proof: pending_proof(),
    };
    source
        .validate_for_status("pending")
        .map_err(|_| Response::error("invalidInput"))?;
    Ok(source)
}

/// 唯一结果附件归原 Receipts 的容量和 24 小时有效期；不另建操作集合。
#[derive(Clone)]
pub(super) struct Attachment {
    source: ClipExportData,
    origin: OutputOriginCapture,
    revoker: PublishRevoker,
    progress: Arc<Mutex<ClipExportProof>>,
    material: Option<worker::VerifiedMaterial>,
    retry_running: bool,
}
impl Attachment {
    pub(super) fn output_id(&self) -> &str {
        &self.source.output_id
    }
    pub(super) fn complete(&mut self, material: Option<worker::VerifiedMaterial>) {
        self.material = material;
        self.retry_running = false;
    }
    pub(super) fn observed(&self, stored: &Response) -> Response {
        if stored.status == "pending" && self.material.is_none() {
            response(
                self.source.clone(),
                self.progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )
        } else {
            stored.clone()
        }
    }
    pub(super) fn origin_matches(&self, bridge: &BridgeUi) -> bool {
        capture_origin(bridge).as_ref() == Ok(&self.origin)
    }
}

/// 原导出查询只读取本客户端的既有回执，不要求仍存在模型回合或批准卡。
pub(super) fn owned_read_attachment<'a>(
    bridge: &'a BridgeUi,
    request: &Request,
    current: &Scope,
) -> Result<&'a Attachment, Response> {
    owned_read_with_origin(bridge, request, current, capture_origin(bridge))
}
fn owned_read_with_origin<'a>(
    bridge: &'a BridgeUi,
    request: &Request,
    current: &Scope,
    origin: Result<OutputOriginCapture, ()>,
) -> Result<&'a Attachment, Response> {
    if !matches!(
        request.method,
        Method::ClipOperation | Method::ClipExportOutput
    ) || request.instance_id != bridge.instance_id
        || request_scope(request).as_ref() != Some(current)
        || bridge.current_scope().as_ref() != Some(current)
        || !bridge.active_clip
        || !bridge.authorized(request, &current.project, true)
    {
        return Err(Response::error("notAuthorized"));
    }
    let query = request
        .operation_query
        .as_ref()
        .ok_or_else(|| Response::error("invalidInput"))?;
    let attachment = bridge
        .canvas_receipts
        .clip_export(current, &request.client_id, query, Instant::now())
        .ok_or_else(|| Response::error("notAuthorized"))?;
    if origin.as_ref() != Ok(&attachment.origin)
        || attachment
            .source
            .validate_for_request("pending", request)
            .is_err()
    {
        return Err(Response::error("notAuthorized"));
    }
    Ok(attachment)
}

struct Completion {
    registration: Option<u64>,
    scope: Scope,
    client: String,
    query: concat_editor_mcp::OperationQuery,
    output_id: String,
    result: worker::Outcome,
}
fn completions() -> &'static Mutex<VecDeque<Completion>> {
    static QUEUE: OnceLock<Mutex<VecDeque<Completion>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}
thread_local! {
    static TIMER: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
}
fn ensure_completion_drain() {
    TIMER.with(|slot| {
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
fn drain_completions() {
    if completions()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        return;
    }
    Shell::with(|shell, _app| {
        // 先取得两个真实 UI 借用，再移走任何结果。模态事件循环忙时保留整批。
        let Ok(studio) = shell.studio.try_borrow() else {
            return;
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return;
        };
        let batch: Vec<_> = {
            let mut queue = completions().lock().unwrap_or_else(|e| e.into_inner());
            let count = queue.len().min(16);
            queue.drain(..count).collect()
        };
        for done in batch {
            if let Some(id) = done.registration {
                crate::clip_export_fence::finish(id);
            }
            // 不检查当前文档来丢弃旧完成。只更新同一原生输出的原回执。
            bridge.canvas_receipts.complete_clip_export(
                &done.scope,
                &done.client,
                &done.query,
                &done.output_id,
                done.result.response,
                done.result.material,
            );
        }
    });
}

pub(super) fn queue_export(
    request: &Request,
    scope: &Scope,
    permit: &Permit,
    studio: &mut Studio,
    app: &App,
) -> Response {
    let source = match source(request, scope, permit) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if let Err(error) =
        clip_execute::final_check_no_io(studio, app, request, scope, permit, source.generation)
    {
        let reason = match error.status.as_str() {
            "leaseExpired" => ClipExportReason::LeaseExpired,
            "notAuthorized" => ClipExportReason::Revoked,
            _ => ClipExportReason::SourceChanged,
        };
        return not_published(source, false, reason);
    }
    let p = request
        .clip_parameters
        .as_ref()
        .expect("original export parameters");
    if clip_edit::approval_preflight(studio, &p.action).is_err()
        || clip_edit::persisted_document_state_id(studio.project()).as_ref()
            != Ok(&source.source_document_state_id)
    {
        return not_published(source, false, ClipExportReason::SourceChanged);
    }
    let (deadline, origin) = {
        let bridge = studio.editor_mcp.borrow();
        let deadline = match bridge.writes.original_deadline(
            scope,
            &request.client_id,
            &request.client_token,
            Instant::now(),
        ) {
            Ok(deadline) => deadline,
            Err(_) => return not_published(source, false, ClipExportReason::Revoked),
        };
        let origin = match capture_origin(&bridge) {
            Ok(origin) => origin,
            Err(()) => return not_published(source, false, ClipExportReason::SourceChanged),
        };
        (deadline, origin)
    };
    let provenance = match provenance(
        studio.project(),
        &source,
        studio.session.as_ref().map(|s| PathBuf::from(s.path())),
        &origin,
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let project = match snapshot_project(studio.project(), studio.mcp_clip_state().0, &source) {
        Ok(project) => project,
        Err(error) => return error,
    };
    let job = match studio.host.exporter.begin() {
        Ok(job) => job,
        Err(_) => return not_published(source, false, ClipExportReason::ResourceLimit),
    };
    // 原有限 write lease 才影响发布。请求超时和 300 秒批准卡不缩短后台作业。
    let (gate, revoker) = new_publish_gate(job.cancel_handle(), deadline);
    let Some(registration) = crate::clip_export_fence::register(
        permit.id,
        &request.client_id,
        origin.assistant_session.as_deref(),
        revoker.clone(),
    ) else {
        return not_published(source, false, ClipExportReason::ResourceLimit);
    };
    let progress = Arc::new(Mutex::new(pending_proof()));
    let attached = studio
        .editor_mcp
        .borrow_mut()
        .canvas_receipts
        .attach_clip_export(
            scope,
            &request.client_id,
            p,
            Attachment {
                source: source.clone(),
                origin,
                revoker: revoker.clone(),
                progress: Arc::clone(&progress),
                material: None,
                retry_running: false,
            },
        );
    if !attached {
        crate::clip_export_fence::finish(registration);
        return not_published(source, false, ClipExportReason::ResourceLimit);
    }
    let work = worker::Work {
        project,
        project_dir: studio.session.as_ref().map(|s| PathBuf::from(s.path())),
        titles: Arc::clone(&studio.host.titles),
        catalogue: concat_effects::Catalogue::builtin().clone(),
        staging_parent: studio.host.dirs.data.join("cache/clip-export-staging"),
        output: studio
            .host
            .dirs
            .data
            .join("clip-exports")
            .join(format!("{}.mp4", source.output_id)),
        library_root: studio.host.dirs.data.join("personal-library"),
        source: source.clone(),
        provenance,
        job,
        gate,
        revoker,
    };
    let completion = Completion {
        registration: Some(registration),
        scope: scope.clone(),
        client: request.client_id.clone(),
        query: concat_editor_mcp::OperationQuery {
            operation_id: source.operation_id.clone(),
            client_sequence: source.client_sequence,
        },
        output_id: source.output_id.clone(),
        result: worker::Outcome {
            response: Response::error("outcomeUnknown"),
            material: None,
        },
    };
    ensure_completion_drain();
    crate::host::spawn_detached(move || {
        let result = worker::run(work, |phase, frames_done, frames_total| {
            *progress.lock().unwrap_or_else(|e| e.into_inner()) = ClipExportProof::Pending {
                version: 1,
                phase,
                frames_done,
                frames_total,
            };
        });
        completions()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(Completion {
                result,
                ..completion
            });
        let _ = slint::invoke_from_event_loop(drain_completions);
    });
    response(source, pending_proof())
}

fn provenance(
    project: &concat_project::Project,
    source: &ClipExportData,
    project_path: Option<PathBuf>,
    origin: &OutputOriginCapture,
) -> Result<crate::personal_library::OutputProvenance, Response> {
    let mut inputs = Vec::new();
    for item in &project.media {
        if !project
            .active()
            .clips
            .iter()
            .any(|clip| clip.media_id == item.id)
        {
            continue;
        }
        if let Some(raw) = item.extra.get("seecutLibraryReference") {
            let value = clip_edit::library_reference(item)
                .ok_or_else(|| Response::error("invalidInput"))?;
            // Decode the trusted native form after the public fixed-version check.
            let reference: crate::personal_library::AssetReference =
                serde_json::from_value(raw.clone()).map_err(|_| Response::error("invalidInput"))?;
            if value.is_null() {
                return Err(Response::error("invalidInput"));
            }
            if !inputs.contains(&reference) {
                inputs.push(reference);
            }
        }
    }
    Ok(crate::personal_library::OutputProvenance {
        kind: "clip_export".into(),
        project_path,
        document_sha256: source
            .source_document_state_id
            .strip_prefix("clip-v1-")
            .ok_or_else(|| Response::error("invalidInput"))?
            .to_owned(),
        inputs,
        parameters: json!({"format":"mp4", "edit":source.edit, "timelineId":source.source_timeline_id,
            "generation":source.generation, "revision":source.source_revision,
            "selectionRevision":source.source_selection_revision, "editStateId":source.source_edit_state_id,
            "accountScope":origin.account_scope, "assistantSession":origin.assistant_session}),
    })
}

fn control_response(
    request: &Request,
    action: ClipExportControlAction,
    result: ClipExportControlResult,
    original: Response,
) -> Response {
    let Some(query) = request.operation_query.as_ref() else {
        return Response::error("invalidInput");
    };
    let data = ClipExportControlData {
        clip_export_control_protocol_version: 1,
        action,
        operation_id: query.operation_id.clone(),
        client_sequence: query.client_sequence,
        result,
        original,
    };
    if data.validate_for_request(request).is_err() {
        return Response::error("outcomeUnknown");
    }
    match serde_json::to_value(data) {
        Ok(data) => Response {
            status: "ok".into(),
            data: Some(data),
        },
        Err(_) => Response::error("outcomeUnknown"),
    }
}

pub(super) fn execute_control(
    request: &Request,
    studio: &mut Studio,
    app: &App,
) -> (Response, bool) {
    (control(request, studio, app).unwrap_or_else(|e| e), false)
}
fn control(request: &Request, studio: &Studio, app: &App) -> Result<Response, Response> {
    let scope = request_scope(request).ok_or_else(|| Response::error("invalidInput"))?;
    let query = request
        .operation_query
        .as_ref()
        .ok_or_else(|| Response::error("invalidInput"))?;
    let action = request
        .clip_export_control_action
        .ok_or_else(|| Response::error("invalidInput"))?;
    let intent = Intent::from_request(request).ok_or_else(|| Response::error("invalidInput"))?;
    studio
        .mcp_clip_permission_ready()
        .map_err(|e| Response::error(canvas_status(&e)))?;
    if modal_busy(studio, app, true) || app.get_app_read_blocked() {
        return Err(Response::error("busy"));
    }
    let mut bridge = studio.editor_mcp.borrow_mut();
    let (kind, ids) = bridge
        .active_ids(studio, app)
        .ok_or_else(|| Response::error("wrongProject"))?;
    let current = bridge.scope(&ids);
    if kind != "clip" || current != scope || request.instance_id != bridge.instance_id {
        return Err(Response::error("wrongProject"));
    }
    bridge
        .writes
        .authorize(
            &current,
            &scope,
            &request.client_id,
            &request.client_token,
            Instant::now(),
        )
        .map_err(|e| Response::error(registry_status(e)))?;
    let attachment = bridge
        .canvas_receipts
        .clip_export(&scope, &request.client_id, query, Instant::now())
        .filter(|a| a.origin_matches(&bridge))
        .cloned()
        .ok_or_else(|| Response::error("notAuthorized"))?;
    let permit = bridge
        .canvas_permits
        .claim(&scope, &request.client_id, &intent, Instant::now())
        .ok_or_else(|| Response::error("notAuthorized"))?;
    let original = bridge.canvas_receipts.query_family(
        &scope,
        &request.client_id,
        query,
        Instant::now(),
        true,
    );
    let original_data: ClipExportData = serde_json::from_value(
        original
            .data
            .clone()
            .ok_or_else(|| Response::error("outcomeUnknown"))?,
    )
    .map_err(|_| Response::error("outcomeUnknown"))?;
    original_data
        .validate_for_request(&original.status, request)
        .map_err(|_| Response::error("outcomeUnknown"))?;
    match action {
        ClipExportControlAction::Cancel => {
            let result = if original.status != "pending" {
                ClipExportControlResult::AlreadyTerminal
            } else {
                match attachment.revoker.revoke(PublishRevocation::Stopped) {
                    concat_host::export_publication::PublishRevokeResult::AlreadyPublishing => {
                        ClipExportControlResult::AlreadyPublishing
                    }
                    _ => ClipExportControlResult::CancelAccepted,
                }
            };
            Ok(control_response(request, action, result, original))
        }
        ClipExportControlAction::RetryRegistration => {
            if !matches!(
                original_data.export_proof,
                ClipExportProof::RegistrationPending { .. }
            ) {
                return Err(Response::error("invalidInput"));
            }
            if attachment.retry_running {
                return Err(Response::error("busy"));
            }
            let mut material = attachment
                .material
                .ok_or_else(|| Response::error("outcomeUnknown"))?;
            if material.asset.is_some() || material.registration_id != original_data.registration_id
            {
                return Err(Response::error("outcomeUnknown"));
            }
            let deadline = bridge
                .writes
                .original_deadline(
                    &scope,
                    &request.client_id,
                    &request.client_token,
                    Instant::now(),
                )
                .map_err(|e| Response::error(registry_status(e)))?;
            let cancel = Arc::new(AtomicBool::new(false));
            let (gate, revoker) = new_publish_gate(Arc::clone(&cancel), deadline);
            let registration = crate::clip_export_fence::register(
                permit.id,
                &request.client_id,
                attachment.origin.assistant_session.as_deref(),
                revoker,
            )
            .ok_or_else(|| Response::error("resourceLimit"))?;
            bridge
                .canvas_receipts
                .clip_export_mut(&scope, &request.client_id, query, Instant::now())
                .ok_or_else(|| Response::error("outcomeUnknown"))?
                .retry_running = true;
            let completion = Completion {
                registration: Some(registration),
                scope,
                client: request.client_id.clone(),
                query: query.clone(),
                output_id: original_data.output_id.clone(),
                result: worker::Outcome {
                    response: Response::error("outcomeUnknown"),
                    material: None,
                },
            };
            ensure_completion_drain();
            crate::host::spawn_detached(move || {
                // 发布事实已确认；重试只恢复原登记，不再次请求发布或渲染。
                worker::register_with_gate(&mut material, &cancel, &gate);
                let response = worker::material_response(original_data, &material);
                completions()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(Completion {
                        result: worker::Outcome {
                            response,
                            material: Some(material),
                        },
                        ..completion
                    });
                let _ = slint::invoke_from_event_loop(drain_completions);
            });
            Ok(control_response(
                request,
                action,
                ClipExportControlResult::RegistrationAttempted,
                original,
            ))
        }
    }
}

pub(super) struct ReadJob {
    request: Request,
    scope: Scope,
    original: ClipExportData,
    origin: OutputOriginCapture,
    material: worker::VerifiedMaterial,
}

pub(super) fn prepare_read(
    bridge: &mut BridgeUi,
    request: &Request,
    studio: &Studio,
    app: &App,
    kind: &str,
    ids: &DocumentIds,
) -> UiReply {
    prepare_read_inner(bridge, request, studio, app, kind, ids).unwrap_or_else(UiReply::Response)
}
fn prepare_read_inner(
    bridge: &mut BridgeUi,
    request: &Request,
    studio: &Studio,
    app: &App,
    kind: &str,
    ids: &DocumentIds,
) -> Result<UiReply, Response> {
    if kind != "clip" {
        return Err(Response::error("unsupportedWorkspace"));
    }
    if modal_busy(studio, app, true) || app.get_app_read_blocked() {
        return Err(Response::error("busy"));
    }
    if !bridge.authorized(request, &ids.project, true) {
        return Err(Response::error("notAuthorized"));
    }
    let scope = request_scope(request).ok_or_else(|| Response::error("invalidInput"))?;
    if scope != bridge.scope(ids) {
        return Err(Response::error("wrongProject"));
    }
    let query = request
        .operation_query
        .as_ref()
        .ok_or_else(|| Response::error("invalidInput"))?;
    let attachment = owned_read_attachment(bridge, request, &scope)?.clone();
    let original = bridge.canvas_receipts.query_family(
        &scope,
        &request.client_id,
        query,
        Instant::now(),
        true,
    );
    let data: ClipExportData = serde_json::from_value(
        original
            .data
            .clone()
            .ok_or_else(|| Response::error("outcomeUnknown"))?,
    )
    .map_err(|_| Response::error("outcomeUnknown"))?;
    data.validate_for_request(&original.status, request)
        .map_err(|_| Response::error("outcomeUnknown"))?;
    if !matches!(data.export_proof, ClipExportProof::Verified { .. }) {
        // partial/pending 保留原回执，不附 freshExport 或 checkedAt。
        return Ok(UiReply::Response(original));
    }
    let material = attachment
        .material
        .filter(|m| m.asset.is_some())
        .ok_or_else(|| Response::error("outcomeUnknown"))?;
    Ok(UiReply::ClipExport(Box::new(ReadJob {
        request: request.clone(),
        scope,
        original: data,
        origin: attachment.origin,
        material,
    })))
}

fn checked_output(job: &ReadJob) -> Result<ClipExportOutputData, Response> {
    let material = &job.material;
    let asset = material
        .asset
        .as_ref()
        .ok_or_else(|| Response::error("outcomeUnknown"))?;
    let cancel = AtomicBool::new(false);
    material
        .proof
        .recheck_same_content(&material.output, &cancel)
        .map_err(|_| Response::error("outputChanged"))?;
    let checked = crate::personal_library::with_library_write_lock(|| {
        let library = crate::personal_library::Library::load_for_relink(&material.library_root)?;
        let stored = library.get(&asset.asset_id).ok_or("原登记素材不存在")?;
        let version = stored.version.as_ref().ok_or("原登记素材缺少版本")?;
        if version.sha256 != asset.version.sha256
            || version.registration_id != asset.version.registration_id
            || version.previous_id != asset.version.previous_id
        {
            return Err("原登记素材版本已变化".to_owned());
        }
        let path =
            crate::personal_library::checked_managed_version_path(&material.library_root, stored)?;
        material.proof.recheck_same_content(path, &cancel)
    })
    .map_err(|_| Response::error("outputChanged"))?;
    let checked_at_unix_ms = u64::try_from(
        checked
            .checked_at()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Response::error("ioFailure"))?
            .as_millis(),
    )
    .map_err(|_| Response::error("ioFailure"))?;
    let source = &job.original;
    let data = ClipExportOutputData {
        clip_export_output_protocol_version: 1,
        source: ClipExportOutputSource::FreshExport,
        app_instance_id: source.app_instance_id.clone(),
        client_id: source.client_id.clone(),
        project_id: source.project_id.clone(),
        document_session_id: source.document_session_id.clone(),
        operation_id: source.operation_id.clone(),
        client_sequence: source.client_sequence,
        output_id: source.output_id.clone(),
        registration_id: source.registration_id.clone(),
        input_manifest_sha256: material.manifest_sha256.clone(),
        asset: asset.clone(),
        summary: worker::summary(&checked).map_err(|_| Response::error("ioFailure"))?,
        checked_at_unix_ms,
    };
    data.validate_for_request(&job.request)
        .and_then(|()| data.matches_original(&job.original))
        .map_err(|_| Response::error("outcomeUnknown"))?;
    Ok(data)
}

pub(super) fn dispatch_read(job: ReadJob) -> Response {
    let data = match checked_output(&job) {
        Ok(data) => data,
        Err(error) => return error,
    };
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
        if kind != "clip" || bridge.scope(&ids) != job.scope {
            return Err(Response::error("wrongProject"));
        }
        let attachment = owned_read_attachment(&bridge, &job.request, &job.scope)?;
        if attachment.origin != job.origin {
            return Err(Response::error("notAuthorized"));
        }
        if attachment.output_id() != job.original.output_id
            || attachment.material.as_ref().and_then(|m| m.asset.as_ref())
                != job.material.asset.as_ref()
        {
            return Err(Response::error("outcomeUnknown"));
        }
        Ok(())
    });
    match stable {
        Ok(Ok(())) => match serde_json::to_value(data) {
            Ok(data) => Response {
                status: "ok".into(),
                data: Some(data),
            },
            Err(_) => Response::error("outcomeUnknown"),
        },
        Ok(Err(error)) | Err(error) => error,
    }
}

/// 核对原审批后才创建派生输出设置，原 Project 永不修改。
fn snapshot_project(
    project: &concat_project::Project,
    generation: u64,
    source: &ClipExportData,
) -> Result<concat_project::Project, Response> {
    if generation != source.generation
        || clip_edit::persisted_document_state_id(project).as_ref()
            != Ok(&source.source_document_state_id)
    {
        return Err(not_published(
            source.clone(),
            false,
            ClipExportReason::SourceChanged,
        ));
    }
    let action = concat_editor_mcp::ClipAction {
        timeline_id: source.source_timeline_id.clone(),
        expected_edit_state_id: source.source_edit_state_id.clone(),
        edit: source.edit.clone(),
    };
    validate_action(project, &action)?;
    let ClipEdit::ExportVideo {
        short_side, fps, ..
    } = &source.edit
    else {
        return Err(Response::error("invalidInput"));
    };
    let mut copy = project.clone();
    let video = &mut copy.active_mut().video;
    let (width, height) =
        crate::panes::export::normalized_size(video.width, video.height, *short_side)
            .ok_or_else(|| not_published(source.clone(), false, ClipExportReason::ResourceLimit))?;
    video.width = width;
    video.height = height;
    video.rate_num = i64::from(*fps);
    video.rate_den = 1;
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_host::export::Exporter;
    use concat_host::export_publication::{PublishRefusal, PublishRevokeResult, PublishState};
    use concat_project::commands::{Command, NewMedia};
    use concat_project::model::{MediaKind, VideoSettings};
    use std::fs;

    struct Fixture {
        root: PathBuf,
        project: concat_project::Project,
        titles: Arc<concat_host::Titles>,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("seecut-native-export-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            let image = root.join("input.png");
            image::RgbaImage::from_pixel(64, 64, image::Rgba([40, 180, 90, 255]))
                .save(&image)
                .unwrap();
            let dirs = concat_host::AppDirs {
                config: root.join("config"),
                data: root.join("data"),
            };
            let mut editor = concat_project::Editor::with_video(VideoSettings {
                width: 64,
                height: 64,
                rate_num: 30,
                rate_den: 1,
            });
            let media = editor
                .apply(Command::AddMedia {
                    item: NewMedia {
                        path: image.to_string_lossy().into_owned(),
                        name: "actual fixture".into(),
                        duration: Some(0.1),
                        kind: MediaKind::Image,
                        width: Some(64),
                        height: Some(64),
                        frame_rate: None,
                        frame_rate_fraction: None,
                        video_codec: None,
                        audio_codec: None,
                        has_audio: false,
                        audio_tracks: vec![],
                        extra: Default::default(),
                    },
                })
                .unwrap()
                .created_id
                .unwrap();
            let track = editor.project().active().tracks[0].id.clone();
            editor
                .apply(Command::AddClip {
                    media_id: media,
                    track_id: track,
                    start: 0.0,
                })
                .unwrap();
            let mut project = editor.project().clone();
            project.active_mut().clip_at_mut(0).duration = 0.1;
            Self {
                root,
                project,
                titles: Arc::new(concat_host::Titles::new(&dirs)),
            }
        }
        fn source(&self) -> ClipExportData {
            ClipExportData {
                app_instance_id: "instance".into(),
                client_id: "client".into(),
                project_id: "project".into(),
                document_session_id: "session".into(),
                operation_id: concat_editor_mcp::operation_id("client", "session", 1).unwrap(),
                client_sequence: 1,
                tool: "clip_edit".into(),
                edit: ClipEdit::ExportVideo {
                    name: "test export".into(),
                    codec: ClipExportCodec::H264,
                    short_side: 720,
                    fps: 30,
                    quality: ClipExportQuality::Balanced,
                    ten_bit: false,
                },
                generation: 3,
                source_revision: 7,
                source_selection_revision: 2,
                source_timeline_id: format!("timeline:{}", self.project.active_timeline_id),
                source_edit_state_id: format!("clip-edit-v1-{}", "a".repeat(64)),
                source_document_state_id: clip_edit::persisted_document_state_id(&self.project)
                    .unwrap(),
                output_id: uuid::Uuid::new_v4().to_string(),
                registration_id: uuid::Uuid::new_v4().to_string(),
                export_proof: pending_proof(),
            }
        }
        fn work(&self, exporter: &Exporter, deadline: Option<Instant>) -> worker::Work {
            let source = self.source();
            source.validate_for_status("pending").unwrap();
            let origin = OutputOriginCapture {
                account_scope: "account".into(),
                assistant_session: Some("session".into()),
            };
            let provenance = provenance(
                &self.project,
                &source,
                Some(self.root.join("original-project")),
                &origin,
            )
            .unwrap();
            let project = snapshot_project(&self.project, source.generation, &source).unwrap();
            let job = exporter.begin().unwrap();
            let (gate, revoker) = new_publish_gate(job.cancel_handle(), deadline);
            worker::Work {
                source,
                project,
                project_dir: Some(self.root.join("original-project")),
                titles: Arc::clone(&self.titles),
                catalogue: concat_effects::Catalogue::builtin().clone(),
                staging_parent: self.root.join("staging"),
                output: self.root.join("output.mp4"),
                library_root: self.root.join("library"),
                provenance,
                job,
                gate,
                revoker,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn parsed(response: &Response) -> ClipExportData {
        let data: ClipExportData =
            serde_json::from_value(response.data.clone().expect("original source retained"))
                .unwrap();
        data.validate_for_status(&response.status).unwrap();
        data
    }
    fn query_request(source: &ClipExportData, method: Method) -> Request {
        serde_json::from_value(json!({
            "instanceId":source.app_instance_id, "clientId":source.client_id, "clientToken":"read",
            "method":method, "projectId":source.project_id, "documentSessionId":source.document_session_id,
            "operationQuery":{"operationId":source.operation_id,"clientSequence":source.client_sequence}
        })).unwrap()
    }
    fn read_job(source: ClipExportData, material: worker::VerifiedMaterial) -> ReadJob {
        let request = query_request(&source, Method::ClipExportOutput);
        ReadJob {
            scope: request_scope(&request).unwrap(),
            request,
            original: source,
            origin: OutputOriginCapture {
                account_scope: "account".into(),
                assistant_session: Some("session".into()),
            },
            material,
        }
    }

    fn owned_bridge(
        source: &ClipExportData,
        at: Instant,
    ) -> (BridgeUi, Request, Scope, OutputOriginCapture) {
        let request = query_request(source, Method::ClipOperation);
        let scope = request_scope(&request).unwrap();
        let origin = OutputOriginCapture {
            account_scope: "account".into(),
            assistant_session: Some("session".into()),
        };
        let mut bridge = BridgeUi::new();
        bridge.instance_id = scope.instance.clone();
        bridge.active_clip = true;
        bridge.clip_ids = Some((
            "generation".into(),
            DocumentIds {
                project: scope.project.clone(),
                session: scope.session.clone(),
            },
        ));
        bridge.grants.insert(
            source.client_id.clone(),
            Grant {
                token: "read".into(),
                project: scope.project.clone(),
                media: true,
            },
        );
        let parameters = concat_editor_mcp::ClipParameters {
            operation_id: source.operation_id.clone(),
            client_sequence: source.client_sequence,
            selection_revision: source.source_selection_revision,
            action: concat_editor_mcp::ClipAction {
                timeline_id: source.source_timeline_id.clone(),
                expected_edit_state_id: source.source_edit_state_id.clone(),
                edit: source.edit.clone(),
            },
        };
        assert!(bridge.canvas_receipts.reserve(
            scope.clone(),
            source.client_id.clone(),
            &parameters,
            at
        ));
        let (_, revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        assert!(bridge.canvas_receipts.attach_clip_export(
            &scope,
            &source.client_id,
            &parameters,
            Attachment {
                source: source.clone(),
                origin: origin.clone(),
                revoker,
                progress: Arc::new(Mutex::new(pending_proof())),
                material: None,
                retry_running: false,
            }
        ));
        (bridge, request, scope, origin)
    }

    #[test]
    fn owned_background_read_needs_current_grant_and_original_receipt_without_a_turn_or_permit() {
        let f = Fixture::new();
        let (mut bridge, mut request, scope, origin) = owned_bridge(&f.source(), Instant::now());
        for method in [Method::ClipOperation, Method::ClipExportOutput] {
            request.method = method;
            assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin.clone())).is_ok());
        }
        request.method = Method::ClipExportControl;
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin.clone())).is_err());
        request.method = Method::ClipOperation;
        let intent = Intent::from_request(&request).unwrap();
        assert!(
            bridge
                .canvas_permits
                .claim(&scope, &request.client_id, &intent, Instant::now())
                .is_none()
        );
        bridge.grants.get_mut(&request.client_id).unwrap().media = false;
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin.clone())).is_err());
        bridge.grants.get_mut(&request.client_id).unwrap().media = true;
        request.client_token = "wrong".into();
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin.clone())).is_err());
        request.client_token = "read".into();
        bridge.grants.clear();
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin)).is_err());
    }

    #[test]
    fn owned_background_read_rejects_changed_identity_origin_and_expired_receipt() {
        let f = Fixture::new();
        let source = f.source();
        let (mut bridge, request, scope, origin) = owned_bridge(&source, Instant::now());
        let mut changed = request.clone();
        changed.client_id = "foreign-client".into();
        bridge.grants.insert(
            changed.client_id.clone(),
            Grant {
                token: "read".into(),
                project: scope.project.clone(),
                media: true,
            },
        );
        assert!(owned_read_with_origin(&bridge, &changed, &scope, Ok(origin.clone())).is_err());
        let mut changed = request.clone();
        changed.operation_query.as_mut().unwrap().operation_id = uuid::Uuid::new_v4().to_string();
        assert!(owned_read_with_origin(&bridge, &changed, &scope, Ok(origin.clone())).is_err());
        changed = request.clone();
        changed.operation_query.as_mut().unwrap().client_sequence += 1;
        assert!(owned_read_with_origin(&bridge, &changed, &scope, Ok(origin.clone())).is_err());
        changed = request.clone();
        changed.document_session_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(owned_read_with_origin(&bridge, &changed, &scope, Ok(origin.clone())).is_err());
        changed = request.clone();
        changed.instance_id = uuid::Uuid::new_v4().to_string();
        assert!(owned_read_with_origin(&bridge, &changed, &scope, Ok(origin.clone())).is_err());
        let mut changed_origin = origin.clone();
        changed_origin.account_scope = "other-account".into();
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(changed_origin)).is_err());
        changed_origin = origin.clone();
        changed_origin.assistant_session = Some("other-session".into());
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(changed_origin)).is_err());
        assert!(owned_read_with_origin(&bridge, &request, &scope, Err(())).is_err());
        bridge.active_clip = false;
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin.clone())).is_err());
        let (bridge, request, scope, origin) =
            owned_bridge(&source, Instant::now() - Duration::from_secs(86401));
        assert!(owned_read_with_origin(&bridge, &request, &scope, Ok(origin)).is_err());
    }

    fn partial_material(f: &Fixture) -> (ClipExportData, worker::VerifiedMaterial) {
        let work = f.work(&Exporter::new(), None);
        fs::write(&work.library_root, b"temporary obstruction").unwrap();
        let outcome = worker::run(work, |_, _, _| {});
        assert_eq!(outcome.response.status, "partial");
        let source = parsed(&outcome.response);
        let material = outcome.material.unwrap();
        fs::remove_file(&material.library_root).unwrap();
        (source, material)
    }

    #[test]
    fn registration_retry_revocation_before_claim_keeps_partial_and_writes_no_journal_or_index() {
        let f = Fixture::new();
        let (source, material) = partial_material(&f);
        let cancel = Arc::new(AtomicBool::new(false));
        let (gate, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        let result = crate::personal_library::with_library_write_lock(|| {
            crate::registration_retry::register_verified_video_output_with_commit_check(
                &material.library_root,
                &material.output,
                Some(&material.name),
                &material.registration_id,
                &material.proof,
                Some(material.provenance.clone()),
                &cancel,
                || {
                    revoker.revoke(PublishRevocation::SessionChanged);
                    gate.try_claim().map(|_| ()).map_err(|e| format!("{e:?}"))
                },
            )
        });
        assert!(result.is_err());
        assert!(
            !material
                .library_root
                .join("pending-generated.json")
                .exists()
        );
        assert!(
            !material
                .library_root
                .join(crate::personal_library::MANIFEST_NAME)
                .exists()
        );
        assert_eq!(
            worker::material_response(source, &material).status,
            "partial"
        );
    }

    #[test]
    fn registration_retry_claim_before_revocation_keeps_actual_registration() {
        let f = Fixture::new();
        let (_, material) = partial_material(&f);
        let cancel = Arc::new(AtomicBool::new(false));
        let (gate, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        let result = crate::personal_library::with_library_write_lock(|| {
            crate::registration_retry::register_verified_video_output_with_commit_check(
                &material.library_root,
                &material.output,
                Some(&material.name),
                &material.registration_id,
                &material.proof,
                Some(material.provenance.clone()),
                &cancel,
                || {
                    gate.try_claim().map_err(|e| format!("{e:?}"))?;
                    assert_eq!(
                        revoker.revoke(PublishRevocation::AccountChanged),
                        PublishRevokeResult::AlreadyPublishing
                    );
                    Ok(())
                },
            )
        })
        .unwrap();
        assert!(!cancel.load(Ordering::Acquire));
        assert_eq!(
            result.version.unwrap().sha256,
            material.proof.summary().sha256
        );
        let library =
            crate::personal_library::Library::load_for_relink(&material.library_root).unwrap();
        assert_eq!(library.items().len(), 1);
    }

    #[test]
    fn registration_retry_expired_original_lease_does_not_use_the_old_publication_claim() {
        let f = Fixture::new();
        let (source, mut material) = partial_material(&f);
        let cancel = Arc::new(AtomicBool::new(false));
        let (gate, _) = new_publish_gate(Arc::clone(&cancel), Some(Instant::now()));
        worker::register_with_gate(&mut material, &cancel, &gate);
        assert!(material.asset.is_none());
        assert!(
            !material
                .library_root
                .join("pending-generated.json")
                .exists()
        );
        assert!(
            !material
                .library_root
                .join(crate::personal_library::MANIFEST_NAME)
                .exists()
        );
        assert_eq!(
            worker::material_response(source, &material).status,
            "partial"
        );
    }

    #[test]
    fn approved_source_is_checked_before_derivation_and_keeps_original_document() {
        let f = Fixture::new();
        let source = f.source();
        let original = f.project.clone();
        let derived = snapshot_project(&f.project, 3, &source).unwrap();
        assert_eq!(f.project, original);
        assert_eq!(derived.active().video.width, 720);
        assert_eq!(
            source.source_document_state_id,
            clip_edit::persisted_document_state_id(&original).unwrap()
        );
        assert_ne!(
            source.source_document_state_id,
            clip_edit::persisted_document_state_id(&derived).unwrap()
        );
        assert!(snapshot_project(&f.project, 4, &source).is_err());
        let mut changed = f.project.clone();
        changed.active_mut().video.rate_num = 24;
        assert!(snapshot_project(&changed, 3, &source).is_err());
    }

    #[test]
    fn real_worker_freezes_renders_registers_and_independently_reads_both_files() {
        let f = Fixture::new();
        let exporter = Exporter::new();
        let work = f.work(&exporter, None);
        let original = work.source.clone();
        let mut phases = vec![];
        let result = worker::run(work, |phase, _, _| {
            assert!(exporter.is_busy());
            phases.push(phase);
        });
        assert!(!exporter.is_busy());
        assert_eq!(result.response.status, "ok", "{:?}", result.response);
        let final_source = parsed(&result.response);
        assert_eq!(
            final_source.source_document_state_id,
            original.source_document_state_id
        );
        assert_eq!(final_source.output_id, original.output_id);
        let material = result.material.unwrap();
        assert_eq!(material.proof.summary().video_frames, 3);
        assert!(phases.contains(&ClipExportPhase::Verifying));
        assert!(phases.contains(&ClipExportPhase::Registering));
        let library =
            crate::personal_library::Library::load_for_relink(&material.library_root).unwrap();
        let asset = library
            .get(&material.asset.as_ref().unwrap().asset_id)
            .unwrap();
        assert_eq!(asset.provenance.as_ref(), Some(&material.provenance));
        assert_eq!(
            material.provenance.document_sha256,
            original
                .source_document_state_id
                .strip_prefix("clip-v1-")
                .unwrap()
        );
        let job = read_job(final_source, material);
        let fresh = checked_output(&job).unwrap();
        fresh.matches_original(&job.original).unwrap();
        assert_eq!(fresh.source, ClipExportOutputSource::FreshExport);
        assert!(fresh.checked_at_unix_ms > 0);
        fs::write(&job.material.output, b"changed").unwrap();
        assert!(checked_output(&job).is_err());
    }

    #[test]
    fn cancellation_during_full_output_verification_prevents_publication() {
        let f = Fixture::new();
        let exporter = Exporter::new();
        let work = f.work(&exporter, None);
        let revoker = work.revoker.clone();
        let output = work.output.clone();
        let result = worker::run(work, |phase, _, _| {
            if phase == ClipExportPhase::Verifying {
                assert_eq!(
                    revoker.revoke(PublishRevocation::Stopped),
                    PublishRevokeResult::Revoked
                );
            }
        });
        assert!(matches!(
            parsed(&result.response).export_proof,
            ClipExportProof::NotPublished {
                publish_claimed: false,
                publish_attempted: false,
                reason: ClipExportReason::Stopped,
                ..
            }
        ));
        assert!(!output.exists());
        assert!(!exporter.is_busy());
    }

    #[test]
    fn revocation_after_actual_publication_preserves_registration_and_truth() {
        let f = Fixture::new();
        let exporter = Exporter::new();
        let work = f.work(&exporter, None);
        let revoker = work.revoker.clone();
        let cancel = work.job.cancel_handle();
        let result = worker::run(work, |phase, _, _| {
            if phase == ClipExportPhase::Publishing {
                assert_eq!(
                    revoker.revoke(PublishRevocation::Stopped),
                    PublishRevokeResult::AlreadyPublishing
                );
            }
        });
        assert_eq!(result.response.status, "ok");
        assert!(!cancel.load(Ordering::Acquire));
        assert_eq!(revoker.state(), PublishState::Publishing);
    }

    #[test]
    fn original_expired_lease_refuses_claim_after_render_without_new_deadline() {
        let f = Fixture::new();
        let exporter = Exporter::new();
        let work = f.work(&exporter, Some(Instant::now()));
        let output = work.output.clone();
        let result = worker::run(work, |_, _, _| {});
        assert!(matches!(
            parsed(&result.response).export_proof,
            ClipExportProof::NotPublished {
                reason: ClipExportReason::LeaseExpired,
                publish_claimed: false,
                ..
            }
        ));
        assert!(!output.exists());
    }

    #[test]
    fn registration_failure_retains_exact_material_and_retry_keeps_one_identity() {
        let f = Fixture::new();
        let exporter = Exporter::new();
        let work = f.work(&exporter, None);
        fs::write(&work.library_root, b"temporary obstruction").unwrap();
        let result = worker::run(work, |_, _, _| {});
        assert_eq!(result.response.status, "partial");
        let partial = parsed(&result.response);
        let mut material = result.material.unwrap();
        let original_provenance = material.provenance.clone();
        assert!(material.output.is_file());
        assert!(material.asset.is_none());
        fs::remove_file(&material.library_root).unwrap();
        worker::register(&mut material, &AtomicBool::new(false));
        let successful = worker::material_response(partial.clone(), &material);
        assert_eq!(successful.status, "ok");
        let data = parsed(&successful);
        assert_eq!(data.registration_id, partial.registration_id);
        assert_eq!(data.output_id, partial.output_id);
        assert_eq!(material.provenance, original_provenance);
        let first = material.asset.clone();
        worker::register(&mut material, &AtomicBool::new(false));
        assert_eq!(material.asset, first);
        let library =
            crate::personal_library::Library::load_for_relink(&material.library_root).unwrap();
        assert_eq!(library.items().len(), 1);
        let fresh = checked_output(&read_job(data, material)).unwrap();
        assert!(fresh.checked_at_unix_ms > 0);
    }

    #[test]
    fn final_managed_copy_change_blocks_fresh_output_even_when_original_is_unchanged() {
        let f = Fixture::new();
        let result = worker::run(f.work(&Exporter::new(), None), |_, _, _| {});
        let data = parsed(&result.response);
        assert_eq!(result.response.status, "ok");
        let material = result.material.unwrap();
        let library =
            crate::personal_library::Library::load_for_relink(&material.library_root).unwrap();
        let asset = library
            .get(&material.asset.as_ref().unwrap().asset_id)
            .unwrap();
        let path =
            crate::personal_library::checked_managed_version_path(&material.library_root, asset)
                .unwrap();
        fs::write(path, b"tampered managed copy").unwrap();
        assert!(
            material
                .proof
                .recheck_same_content(&material.output, &AtomicBool::new(false))
                .is_ok()
        );
        assert!(checked_output(&read_job(data, material)).is_err());
    }

    #[test]
    fn discarded_approval_card_does_not_revoke_accepted_job() {
        let f = Fixture::new();
        let source = f.source();
        let request = query_request(&source, Method::ClipExportOutput);
        let scope = request_scope(&request).unwrap();
        let intent = Intent::from_request(&request).unwrap();
        let mut permits = Permits::default();
        let id = permits
            .grant(
                scope.clone(),
                source.client_id.clone(),
                intent.clone(),
                Instant::now(),
            )
            .unwrap();
        let _approved = permits
            .claim(&scope, &source.client_id, &intent, Instant::now())
            .unwrap();
        let (gate, revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let registration = crate::clip_export_fence::register(
            id,
            &source.client_id,
            Some("session"),
            revoker.clone(),
        )
        .unwrap();
        permits.forget(id);
        assert_eq!(revoker.state(), PublishState::Pending);
        let _claim = gate.try_claim().unwrap();
        crate::clip_export_fence::finish(registration);
    }

    #[test]
    fn explicit_permit_revocation_still_withdraws_original_job() {
        let f = Fixture::new();
        let source = f.source();
        let request = query_request(&source, Method::ClipExportOutput);
        let scope = request_scope(&request).unwrap();
        let intent = Intent::from_request(&request).unwrap();
        let mut permits = Permits::default();
        let id = permits
            .grant(scope, source.client_id.clone(), intent, Instant::now())
            .unwrap();
        let (gate, revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let registration =
            crate::clip_export_fence::register(id, &source.client_id, Some("session"), revoker)
                .unwrap();
        permits.revoke(id);
        assert_eq!(
            gate.try_claim().unwrap_err(),
            PublishRefusal::Revoked(PublishRevocation::Revoked)
        );
        crate::clip_export_fence::finish(registration);
    }

    #[test]
    fn completion_updates_only_original_receipt_and_does_not_create_another_ledger_entry() {
        let f = Fixture::new();
        let source = f.source();
        let request = query_request(&source, Method::ClipExportOutput);
        let scope = request_scope(&request).unwrap();
        let query = request.operation_query.clone().unwrap();
        let parameters = concat_editor_mcp::ClipParameters {
            operation_id: source.operation_id.clone(),
            client_sequence: source.client_sequence,
            selection_revision: source.source_selection_revision,
            action: concat_editor_mcp::ClipAction {
                timeline_id: source.source_timeline_id.clone(),
                expected_edit_state_id: source.source_edit_state_id.clone(),
                edit: source.edit.clone(),
            },
        };
        let (_gate, revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let mut receipts = Receipts::default();
        assert!(receipts.reserve(
            scope.clone(),
            source.client_id.clone(),
            &parameters,
            Instant::now()
        ));
        assert!(receipts.attach_clip_export(
            &scope,
            &source.client_id,
            &parameters,
            Attachment {
                source: source.clone(),
                origin: OutputOriginCapture {
                    account_scope: "original-account".into(),
                    assistant_session: Some("session".into())
                },
                revoker,
                progress: Arc::new(Mutex::new(pending_proof())),
                material: None,
                retry_running: false
            }
        ));
        let result = not_published(source.clone(), false, ClipExportReason::Stopped);
        let mut newer_scope = scope.clone();
        newer_scope.session = "new-document".into();
        assert!(!receipts.complete_clip_export(
            &newer_scope,
            &source.client_id,
            &query,
            &source.output_id,
            result.clone(),
            None
        ));
        assert!(!receipts.complete_clip_export(
            &scope,
            &source.client_id,
            &query,
            "other-output",
            result.clone(),
            None
        ));
        assert!(receipts.complete_clip_export(
            &scope,
            &source.client_id,
            &query,
            &source.output_id,
            result.clone(),
            None
        ));
        let actual = receipts.query_family(&scope, &source.client_id, &query, Instant::now(), true);
        assert_eq!(actual.status, result.status);
        assert_eq!(actual.data, result.data);
        assert!(
            receipts
                .clip_export(
                    &scope,
                    &source.client_id,
                    &query,
                    Instant::now() + Duration::from_secs(86401)
                )
                .is_none()
        );
    }
}
