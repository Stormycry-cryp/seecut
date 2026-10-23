// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native canvas adapters: exact permits, the existing history/save/export
//! paths, and receipts owned by the original invocation despite late callbacks.
use super::*;
use crate::panes::canvas::{CanvasImport, CanvasJobCheckedError, CanvasPane};

struct Origin {
    request: Request,
    scope: Scope,
    permit: Permit,
    generation: u64,
    binding_epoch: u64,
}
impl Origin {
    fn parameters(&self) -> &CanvasParameters {
        self.request
            .canvas_parameters
            .as_ref()
            .expect("typed write")
    }
    fn current(&self, studio: &Studio, app: &App) -> Result<(), Response> {
        let mut bridge = studio.editor_mcp.borrow_mut();
        let active = bridge.active_ids(studio, app);
        if !active.is_some_and(|(kind, ids)| kind == "canvas" && bridge.scope(&ids) == self.scope)
            || studio.canvas.mcp_state().0 != self.generation
            || studio.canvas.mcp_binding_epoch() != self.binding_epoch
        {
            return Err(Response::error("wrongProject"));
        }
        bridge
            .writes
            .authorize(
                &self.scope,
                &self.scope,
                &self.request.client_id,
                &self.request.client_token,
                Instant::now(),
            )
            .map_err(|e| Response::error(registry_status(e)))?;
        if !bridge.canvas_permits.valid(&self.permit, Instant::now()) {
            return Err(Response::error("notAuthorized"));
        }
        if modal_busy(studio, app, true) || studio.canvas.mcp_state().2 {
            return Err(Response::error("busy"));
        }
        if self.request.revision != Some(studio.canvas.mcp_state().1) {
            return Err(Response::error("staleRevision"));
        }
        if self.parameters().selection_revision != studio.mcp_selection_epoch() {
            return Err(Response::error("selectionChanged"));
        }
        Ok(())
    }
    fn final_check(&self, bridge: &BridgeUi, canvas: &CanvasPane) -> Result<(), Response> {
        bridge
            .writes
            .authorize(
                &self.scope,
                &self.scope,
                &self.request.client_id,
                &self.request.client_token,
                Instant::now(),
            )
            .map_err(|e| Response::error(registry_status(e)))?;
        if bridge.current_scope().as_ref() != Some(&self.scope)
            || canvas.mcp_state().0 != self.generation
            || canvas.mcp_binding_epoch() != self.binding_epoch
        {
            return Err(Response::error("wrongProject"));
        }
        if !bridge.canvas_permits.valid(&self.permit, Instant::now()) {
            return Err(Response::error("notAuthorized"));
        }
        if self.request.revision != Some(canvas.mcp_state().1) {
            return Err(Response::error("staleRevision"));
        }
        Ok(())
    }
    fn response(&self, status: &str, evidence: Value) -> Response {
        let mut data = evidence.as_object().cloned().unwrap_or_default();
        data.insert("appInstanceId".into(), json!(self.scope.instance));
        data.insert("clientId".into(), json!(self.request.client_id));
        data.insert("projectId".into(), json!(self.scope.project));
        data.insert("documentSessionId".into(), json!(self.scope.session));
        data.insert("operationId".into(), json!(self.parameters().operation_id));
        data.insert(
            "clientSequence".into(),
            json!(self.parameters().client_sequence),
        );
        data.insert("tool".into(), json!(self.request.method.tool_name()));
        Response {
            status: status.into(),
            data: Some(Value::Object(data)),
        }
    }
    fn finish(&self, studio: &Studio, response: Response) {
        let mut bridge = studio.editor_mcp.borrow_mut();
        let response = self.response(&response.status, response.data.unwrap_or_else(|| json!({})));
        bridge.canvas_receipts.finish(
            &self.scope,
            &self.request.client_id,
            self.parameters(),
            response,
        );
    }
}
fn checked_error(error: CanvasMoveCheckedError<Response>) -> Response {
    match error {
        CanvasMoveCheckedError::Canvas(e) => Response::error(canvas_status(&e)),
        CanvasMoveCheckedError::Commit(r) => r,
    }
}
fn job_error(error: CanvasJobCheckedError<Response>) -> Response {
    match error {
        CanvasJobCheckedError::Canvas(e) => Response::error(canvas_status(&e)),
        CanvasJobCheckedError::Commit(r) => r,
        CanvasJobCheckedError::Io(error) => {
            log::warn!("canvas job preparation: {error}");
            Response::error("ioFailure")
        }
    }
}

pub(super) fn execute_canvas(
    request: &Request,
    studio: &mut Studio,
    app: &App,
) -> (Response, bool) {
    let Some(scope) = request_scope(request) else {
        return (Response::error("invalidInput"), false);
    };
    let Some(parameters) = request.canvas_parameters.as_ref() else {
        return (Response::error("invalidInput"), false);
    };
    let Some(revision) = request.revision else {
        return (Response::error("invalidInput"), false);
    };
    let key = OperationKey {
        client: request.client_id.clone(),
        document_session: scope.session.clone(),
        client_sequence: parameters.client_sequence,
    };
    let fingerprint = EditFingerprint::Canvas {
        revision,
        parameters: parameters.clone(),
    };
    let permit = {
        let mut bridge = studio.editor_mcp.borrow_mut();
        if request.instance_id != bridge.instance_id {
            return (Response::error("appUnavailable"), false);
        }
        let Some((kind, ids)) = bridge.active_ids(studio, app) else {
            return (Response::error("wrongProject"), false);
        };
        let current = bridge.scope(&ids);
        if kind != "canvas" {
            return (Response::error("wrongProject"), false);
        }
        match bridge.writes.cached(
            &current,
            &scope,
            &key,
            &request.client_token,
            &fingerprint,
            Instant::now(),
        ) {
            Ok(Some(response)) => return (response, false),
            Err(e) => return (Response::error(registry_status(e)), false),
            Ok(None) => {}
        }
        let Some(intent) = Intent::from_request(request) else {
            return (Response::error("invalidInput"), false);
        };
        let Some(permit) =
            bridge
                .canvas_permits
                .claim(&scope, &request.client_id, &intent, Instant::now())
        else {
            return (Response::error("notAuthorized"), false);
        };
        match bridge.writes.begin(
            &current,
            &scope,
            &key,
            &request.client_token,
            &fingerprint,
            Instant::now(),
        ) {
            Ok(Begin::New) => {}
            Ok(Begin::Cached(response)) => return (response, false),
            Err(e) => return (Response::error(registry_status(e)), false),
        }
        if !bridge.canvas_receipts.reserve(
            scope.clone(),
            request.client_id.clone(),
            parameters,
            Instant::now(),
        ) {
            let response = Response::error("resourceLimit");
            let _ = bridge
                .writes
                .finish(&scope, &key, response.clone(), Instant::now());
            return (response, false);
        }
        permit
    };
    let origin = Origin {
        request: request.clone(),
        scope: scope.clone(),
        permit,
        generation: studio.canvas.mcp_state().0,
        binding_epoch: studio.canvas.mcp_binding_epoch(),
    };
    if matches!(&parameters.action, CanvasAction::Export) {
        let capture = library_assets::output_origin(&studio.editor_mcp.borrow());
        let accepted = studio
            .editor_mcp
            .borrow_mut()
            .canvas_receipts
            .capture_export_origin(&scope, &request.client_id, parameters, capture);
        if !accepted {
            let response = origin.response("resourceLimit", json!({}));
            origin.finish(studio, response.clone());
            let _ = studio.editor_mcp.borrow_mut().writes.finish(
                &scope,
                &key,
                response.clone(),
                Instant::now(),
            );
            return (response, false);
        }
    }
    let (response, changed) = match origin.current(studio, app) {
        Err(response) => (response, false),
        Ok(()) => match &parameters.action {
            CanvasAction::Edit {
                edit,
                expected_edit_state_id,
            } => {
                let previous_revision = studio.canvas.mcp_state().1;
                let check = studio
                    .canvas
                    .mcp_writer_ready()
                    .map_err(|error| Response::error(canvas_status(&error)))
                    .and_then(|()| origin.final_check(&studio.editor_mcp.borrow(), &studio.canvas));
                match check {
                    Err(error) => (error, false),
                    Ok(()) => {
                        let cell = &studio.editor_mcp;
                        match canvas_edit::apply_checked(
                            &mut studio.canvas,
                            edit,
                            expected_edit_state_id,
                            |canvas| {
                                canvas
                                    .mcp_writer_ready()
                                    .map_err(CanvasMoveCheckedError::Canvas)?;
                                origin
                                    .final_check(&cell.borrow(), canvas)
                                    .map_err(CanvasMoveCheckedError::Commit)
                            },
                        ) {
                            Err(error) => (
                                checked_error(error),
                                studio.canvas.mcp_state().1 != previous_revision,
                            ),
                            Ok(mut evidence) => {
                                studio.sync_mcp_selection();
                                evidence["selectionRevision"] = json!(studio.mcp_selection_epoch());
                                let changed = evidence["changed"] == true;
                                (
                                    origin.response(
                                        if changed { "ok" } else { "unchanged" },
                                        evidence,
                                    ),
                                    changed || studio.canvas.mcp_state().1 != previous_revision,
                                )
                            }
                        }
                    }
                }
            }
            CanvasAction::Transform {
                object_id,
                x,
                y,
                scale,
            } => {
                let id = concat_editor_mcp::parse_layer_id(object_id)
                    .ok()
                    .and_then(|id| serde_json::from_value::<concat_canvas::LayerId>(json!(id)).ok())
                    .expect("typed layer identity");
                let cell = &studio.editor_mcp;
                match studio.canvas.mcp_transform_selected_checked(id,*x,*y,*scale,|canvas|origin.final_check(&cell.borrow(),canvas)) {
                    Ok(result)=>(origin.response(if result.changed{"ok"}else{"unchanged"},json!({"objectId":object_id,"revision":studio.canvas.mcp_state().1,
                        "selectionRevision":studio.mcp_selection_epoch(),"transform":result.transform,"changed":result.changed})),result.changed),
                    Err(error)=>(checked_error(error),false),
                }
            }
            CanvasAction::Insert { asset_id, version } => {
                let texture_limit = studio.canvas.mcp_texture_limit().or_else(|| {
                    studio
                        .host
                        .gpu_device
                        .as_ref()
                        .map(|d| d.limits().max_texture_dimension_2d)
                });
                let asset_id = asset_id.clone();
                let version = version.clone();
                let pending = origin.response("pending", json!({"sourceRevision":revision}));
                crate::host::spawn(
                    move || prepare_import(&asset_id, &version, texture_limit),
                    move |studio, app, models, result| {
                        let mut changed = false;
                        let response = match origin.current(studio, app) {
                            Err(response) => response,
                            Ok(()) => match result {
                                Err(error) => {
                                    log::warn!("canvas library import: {error}");
                                    Response::error("assetUnavailable")
                                }
                                Ok((item, frame)) => {
                                    let cell = &studio.editor_mcp;
                                    match studio.canvas.mcp_insert_library_checked(
                                        &item,
                                        frame,
                                        |canvas| origin.final_check(&cell.borrow(), canvas),
                                    ) {
                                        Err(error) => checked_error(error),
                                        Ok(id) => {
                                            changed = true;
                                            studio.sync_mcp_selection();
                                            origin.response("ok",json!({"createdLayerId":format!("layer:{}",id.as_u64()),"createdLayer":studio.canvas.document.as_ref().and_then(|doc|doc.find(id)).and_then(|node|if let concat_canvas::LayerNode::Layer(layer)=node {Some(canvas_image_value(layer,None))}else{None}),
                                        "revision":studio.canvas.mcp_state().1,"selectionRevision":studio.mcp_selection_epoch(),
                                        "libraryReference":item.library_reference.map(|r|json!({"assetId":r.asset_id,"version":r.version}))}))
                                        }
                                    }
                                }
                            },
                        };
                        origin.finish(studio, response);
                        if changed {
                            let mut canvas = std::mem::take(&mut studio.canvas);
                            canvas.mcp_move_committed(studio);
                            studio.canvas = canvas;
                            studio.publish(app, models);
                        }
                    },
                );
                (pending, false)
            }
            CanvasAction::Save => {
                let cell = &studio.editor_mcp;
                match studio
                    .canvas
                    .mcp_prepare_save_checked(|canvas| origin.final_check(&cell.borrow(), canvas))
                {
                    Err(error) => (job_error(error), false),
                    Ok(None) => (
                        origin.response("awaitingSelection", json!({"sourceRevision":revision})),
                        false,
                    ),
                    Ok(Some(job)) => {
                        let pending =
                            origin.response("pending", json!({"sourceRevision":job.revision}));
                        crate::host::spawn(
                            move || {
                                let result = job.run();
                                (job, result)
                            },
                            move |studio, app, models, (job, result)| {
                                let written = result.as_ref().is_ok_and(|r| r.is_some());
                                let response = match result {
                                    Ok(Some(receipt)) => origin.response("ok", json!(receipt)),
                                    Ok(None) => origin.response(
                                        "superseded",
                                        json!({"sourceRevision":job.revision}),
                                    ),
                                    Err(error) => {
                                        log::warn!("canvas save job: {error}");
                                        origin.response(
                                            "ioFailure",
                                            json!({"sourceRevision":job.revision}),
                                        )
                                    }
                                };
                                origin.finish(studio, response);
                                if studio.canvas.mcp_complete_save(&job, written).is_some() {
                                    studio.publish(app, models);
                                }
                            },
                        );
                        (pending, false)
                    }
                }
            }
            CanvasAction::Export => {
                let cell = &studio.editor_mcp;
                match studio
                    .canvas
                    .mcp_prepare_export_checked(|canvas| origin.final_check(&cell.borrow(), canvas))
                {
                    Err(error) => (job_error(error), false),
                    Ok(job) => {
                        let pending=origin.response("pending",json!({"sourceRevision":job.revision,"registrationId":job.registration_id}));
                        crate::host::spawn(
                            move || {
                                let result = job.run(None);
                                (job, result)
                            },
                            move |studio, app, models, (job, result)| {
                                let response = match result {
                                    Ok(receipt) => {
                                        if let Some(recovery) = receipt.recovery.clone() {
                                            let mut bridge = studio.editor_mcp.borrow_mut();
                                            if bridge.canvas_export_recoveries.len() < 1024 {
                                                bridge.canvas_export_recoveries.insert(
                                                    (
                                                        origin.scope.clone(),
                                                        origin.request.client_id.clone(),
                                                        origin.parameters().client_sequence,
                                                    ),
                                                    recovery,
                                                );
                                            }
                                        }
                                        origin.response(if receipt.asset.is_some(){"ok"}else{"partial"},json!({"sourceRevision":receipt.source_revision,"pngSha256":receipt.sha256,
                                    "width":receipt.width,"height":receipt.height,"registrationId":receipt.registration_id,
                                    "asset":receipt.asset.map(|r|json!({"assetId":r.asset_id,"version":r.version})),"registrationPending":receipt.registration_error.is_some()}))
                                    }
                                    Err(error) => {
                                        log::warn!("canvas export job: {error}");
                                        origin.response("ioFailure",json!({"sourceRevision":job.revision,"registrationId":job.registration_id}))
                                    }
                                };
                                origin.finish(studio, response);
                                if studio.canvas.mcp_complete_export(&job) {
                                    studio.publish(app, models);
                                }
                            },
                        );
                        (pending, false)
                    }
                }
            }
        },
    };
    let mut response = response;
    let data = response.data.get_or_insert_with(|| json!({}));
    if let Some(data) = data.as_object_mut() {
        data.insert("appInstanceId".into(), json!(scope.instance));
        data.insert("clientId".into(), json!(request.client_id));
        data.insert("projectId".into(), json!(scope.project));
        data.insert("documentSessionId".into(), json!(scope.session));
        data.insert("operationId".into(), json!(parameters.operation_id));
        data.insert("clientSequence".into(), json!(parameters.client_sequence));
        data.insert("tool".into(), json!(request.method.tool_name()));
    }
    // Async closures were queued, not executed. Store the accepted original
    // sequence before returning; duplicate writes can never start another job.
    let mut bridge = studio.editor_mcp.borrow_mut();
    bridge
        .canvas_receipts
        .finish(&scope, &request.client_id, parameters, response.clone());
    if bridge
        .writes
        .finish(&scope, &key, response.clone(), Instant::now())
        .is_err()
    {
        return (Response::error("outcomeUnknown"), changed);
    }
    (response, changed)
}

fn prepare_import(
    asset_id: &str,
    version: &concat_editor_mcp::AssetVersion,
    texture_limit: Option<u32>,
) -> Result<(CanvasImport, concat_core::frame::Frame), String> {
    let root = concat_host::AppDirs::locate()?
        .data
        .join("personal-library");
    let library = crate::personal_library::Library::load_for_relink(&root)?;
    let asset = library
        .get(asset_id)
        .filter(|asset| asset.kind == crate::personal_library::AssetKind::Image && !asset.trashed)
        .ok_or("library image unavailable")?;
    let actual = asset
        .version
        .as_ref()
        .ok_or("library image not versioned")?;
    if actual.sha256 != version.sha256
        || actual.registration_id != version.registration_id
        || actual.previous_id != version.previous_id
    {
        return Err("library image version changed".into());
    }
    let path = crate::personal_library::checked_managed_version_path(&root, asset)?;
    let item = CanvasImport::from_library(
        &json!({"id":asset.id,"name":asset.name,"version":actual,"path":path}),
    );
    let frame = crate::panes::canvas::decode_import(&item, texture_limit)?;
    Ok((item, frame))
}

pub(super) fn dispatch_library(job: LibraryJob) -> Response {
    let query = job
        .request
        .library_query
        .as_ref()
        .expect("typed library query");
    let result = (|| {
        let root = concat_host::AppDirs::locate()?
            .data
            .join("personal-library");
        let library = crate::personal_library::Library::load_for_relink(&root)?;
        let search = query.query.to_lowercase();
        let mut total = 0usize;
        let mut rows = Vec::new();
        let mut size = 0usize;
        for asset in library.items().iter().filter(|a| {
            a.kind == crate::personal_library::AssetKind::Image
                && !a.trashed
                && a.version.is_some()
                && query.asset_id.as_ref().is_none_or(|id| id == &a.id)
                && a.name.to_lowercase().contains(&search)
                && match &job.authority {
                    LibraryAuthority::Exact(_) => true,
                    LibraryAuthority::OwnedOutput(proof) => library_assets::fixed_version(a)
                        .is_some_and(|version| proof.matches_asset(&a.id, &version)),
                }
        }) {
            let offset = total;
            total = bounded_object_count(total, 1).map_err(|_| "resourceLimit")?;
            if offset < query.offset || rows.len() >= query.limit || size >= 8500 {
                continue;
            }
            let version = asset.version.as_ref().expect("versioned asset");
            let row = json!({"assetId":asset.id,"name":bounded_name(&asset.name),"kind":"image","version":version,
                "available":crate::personal_library::checked_managed_version_path(&root,asset).is_ok()});
            let bytes = serde_json::to_vec(&row).map_err(|_| "ioFailure")?.len();
            if size + bytes > 8500 {
                size = 8500;
                continue;
            }
            size += bytes;
            rows.push(row);
        }
        Ok::<_, String>(
            json!({"projectId":job.request.project_id,"documentSessionId":job.request.document_session_id,
            "offset":query.offset,"total":total,"nextOffset":(query.offset+rows.len()<total).then_some(query.offset+rows.len()),"images":rows}),
        )
    })();
    let stable = on_ui(move |shell, app| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return false;
        };
        library_assets::library_job_current(&job, &studio, &app)
    });
    if !matches!(stable, Ok(true)) {
        return Response::error("notAuthorized");
    }
    match result {
        Ok(data) => Response::ok(data),
        Err(error) => {
            log::warn!("canvas library query: {error}");
            Response::error("libraryUnavailable")
        }
    }
}

pub(super) fn execute_registration_retry(
    request: &Request,
    studio: &mut Studio,
    app: &App,
) -> (Response, bool) {
    let Some(scope) = request_scope(request) else {
        return (Response::error("invalidInput"), false);
    };
    let Some(query) = request.operation_query.as_ref() else {
        return (Response::error("invalidInput"), false);
    };
    let (mut response, recovery) = {
        let mut bridge = studio.editor_mcp.borrow_mut();
        let Some((kind, ids)) = bridge.active_ids(studio, app) else {
            return (Response::error("wrongProject"), false);
        };
        let current = bridge.scope(&ids);
        if kind != "canvas"
            || modal_busy(studio, app, true)
            || studio.canvas.mcp_writer_ready().is_err()
        {
            return (Response::error("busy"), false);
        }
        if let Err(error) = bridge.writes.authorize(
            &current,
            &current,
            &request.client_id,
            &request.client_token,
            Instant::now(),
        ) {
            return (Response::error(registry_status(error)), false);
        }
        if bridge
            .grants
            .get(&request.client_id)
            .is_none_or(|g| g.project != current.project || !g.media)
        {
            return (Response::error("notAuthorized"), false);
        }
        let Some(intent) = Intent::from_request(request) else {
            return (Response::error("invalidInput"), false);
        };
        if bridge
            .canvas_permits
            .claim(&scope, &request.client_id, &intent, Instant::now())
            .is_none()
        {
            return (Response::error("notAuthorized"), false);
        }
        let response =
            bridge
                .canvas_receipts
                .query(&scope, &request.client_id, query, Instant::now());
        if response.status != "partial" {
            return (response, false);
        }
        let recovery = bridge
            .canvas_export_recoveries
            .get(&(
                scope.clone(),
                request.client_id.clone(),
                query.client_sequence,
            ))
            .cloned();
        (response, recovery)
    };
    let Some(data) = response.data.as_ref() else {
        return (Response::error("invalidInput"), false);
    };
    if data["tool"].as_str() != Some("export_canvas_png") {
        return (Response::error("invalidInput"), false);
    }
    let (Some(registration), Some(sha256)) =
        (data["registrationId"].as_str(), data["pngSha256"].as_str())
    else {
        return (Response::error("invalidInput"), false);
    };
    let registration = registration.to_owned();
    let sha256 = sha256.to_owned();
    response.status = "pendingRegistration".into();
    studio.editor_mcp.borrow_mut().canvas_receipts.update(
        &scope,
        &request.client_id,
        query,
        response.clone(),
    );
    let pending = response.clone();
    let request = request.clone();
    crate::host::spawn(
        move || {
            crate::personal_library::with_library_write_lock(|| match recovery {
                Some(recovery) => recovery.retry(),
                None => concat_host::AppDirs::locate().and_then(|dirs| {
                    crate::registration_retry::retry_operation(
                        &dirs.data.join("personal-library"),
                        &registration,
                        &sha256,
                    )
                }),
            })
        },
        move |studio, _, _, result| {
            let query = request
                .operation_query
                .as_ref()
                .expect("original registration query");
            let succeeded = result.is_ok();
            match result {
                Ok(asset) => {
                    response.status = "ok".into();
                    if let Some(data) = response.data.as_mut() {
                        data["asset"] = json!({"assetId":asset.asset_id,"version":asset.version});
                        data["registrationPending"] = json!(false);
                    }
                }
                Err(error) => {
                    log::warn!("original PNG registration: {error}");
                    response.status = "partial".into();
                }
            }
            let mut bridge = studio.editor_mcp.borrow_mut();
            bridge
                .canvas_receipts
                .update(&scope, &request.client_id, query, response);
            if succeeded {
                bridge.canvas_export_recoveries.remove(&(
                    scope,
                    request.client_id,
                    query.client_sequence,
                ));
            }
        },
    );
    (pending, false)
}

/// A receipt query can observe registration completed by the existing library
/// UI's retry, without doing any registration write under a read permission.
pub(super) fn refresh_registration_receipt(job: RegistrationReadJob) -> Response {
    let mut response = job.response;
    let lookup = (|| {
        let d = response.data.as_ref().ok_or("receipt missing")?;
        let registration = d["registrationId"].as_str().ok_or("registration missing")?;
        let sha = d["pngSha256"].as_str().ok_or("PNG SHA missing")?;
        let root = concat_host::AppDirs::locate()?
            .data
            .join("personal-library");
        crate::registration_retry::registered_operation(
            &root,
            &format!("operation:{registration}"),
            sha,
        )
    })();
    if let Ok(Some(asset)) = lookup {
        response.status = "ok".into();
        if let Some(d) = response.data.as_mut() {
            d["asset"] = json!({"assetId":asset.asset_id,"version":asset.version});
            d["registrationPending"] = json!(false);
        }
    }
    on_ui(move |shell, app| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return Response::error("busy");
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return Response::error("busy");
        };
        let Some((kind, ids)) = bridge.active_ids(&studio, &app) else {
            return Response::error("notAuthorized");
        };
        if kind != "canvas"
            || !bridge.authorized(&job.request, &ids.project, true)
            || !bridge.canvas_permits.valid(&job.permit, Instant::now())
        {
            return Response::error("notAuthorized");
        }
        let scope = request_scope(&job.request).expect("typed original scope");
        let query = job
            .request
            .operation_query
            .as_ref()
            .expect("typed original query");
        if response.status == "ok" {
            bridge
                .canvas_receipts
                .update(&scope, &job.request.client_id, query, response.clone());
            bridge.canvas_export_recoveries.remove(&(
                scope,
                job.request.client_id,
                query.client_sequence,
            ));
        }
        response
    })
    .unwrap_or_else(|error| error)
}
