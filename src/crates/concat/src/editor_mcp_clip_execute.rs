//! Native timeline writes using the existing exact permit, lease and operation ledger.
use super::*;

pub(super) fn response(
    request: &Request,
    scope: &Scope,
    status: &str,
    evidence: Value,
) -> Response {
    let p = request
        .clip_parameters
        .as_ref()
        .expect("validated clip edit");
    let mut data = evidence.as_object().cloned().unwrap_or_default();
    for (key, value) in [
        ("appInstanceId", json!(scope.instance)),
        ("clientId", json!(request.client_id)),
        ("projectId", json!(scope.project)),
        ("documentSessionId", json!(scope.session)),
        ("operationId", json!(p.operation_id)),
        ("clientSequence", json!(p.client_sequence)),
        ("tool", json!("clip_edit")),
    ] {
        data.insert(key.into(), value);
    }
    Response {
        status: status.into(),
        data: Some(Value::Object(data)),
    }
}

pub(super) fn final_check(
    studio: &Studio,
    app: &App,
    request: &Request,
    scope: &Scope,
    permit: &Permit,
    generation: u64,
) -> Result<(), Response> {
    studio
        .mcp_clip_writer_ready()
        .map_err(|e| Response::error(canvas_status(&e)))?;
    final_check_no_io(studio, app, request, scope, permit, generation)
}

/// Save worker validates the original writer on disk. UI preparation only reads
/// native memory and the original authorization records.
pub(super) fn final_check_no_io(
    studio: &Studio,
    app: &App,
    request: &Request,
    scope: &Scope,
    permit: &Permit,
    generation: u64,
) -> Result<(), Response> {
    studio
        .mcp_clip_permission_ready()
        .map_err(|e| Response::error(canvas_status(&e)))?;
    let bridge = studio.editor_mcp.borrow();
    if bridge.current_scope().as_ref() != Some(scope) || studio.mcp_clip_state().0 != generation {
        return Err(Response::error("wrongProject"));
    }
    bridge
        .writes
        .authorize(
            scope,
            scope,
            &request.client_id,
            &request.client_token,
            Instant::now(),
        )
        .map_err(|e| Response::error(registry_status(e)))?;
    if !bridge.canvas_permits.valid(permit, Instant::now()) {
        return Err(Response::error("notAuthorized"));
    }
    if modal_busy(studio, app, true) || app.get_app_read_blocked() || studio.mcp_clip_state().3 {
        return Err(Response::error("busy"));
    }
    if request.revision != Some(studio.mcp_clip_state().1) {
        return Err(Response::error("staleRevision"));
    }
    if request
        .clip_parameters
        .as_ref()
        .is_none_or(|p| p.selection_revision != studio.mcp_selection_epoch())
    {
        return Err(Response::error("selectionChanged"));
    }
    Ok(())
}

pub(super) fn execute_clip(request: &Request, studio: &mut Studio, app: &App) -> (Response, bool) {
    let Some(scope) = request_scope(request) else {
        return (Response::error("invalidInput"), false);
    };
    let Some(p) = request.clip_parameters.as_ref() else {
        return (Response::error("invalidInput"), false);
    };
    let Some(revision) = request.revision else {
        return (Response::error("invalidInput"), false);
    };
    let key = OperationKey {
        client: request.client_id.clone(),
        document_session: scope.session.clone(),
        client_sequence: p.client_sequence,
    };
    let fingerprint = EditFingerprint::Clip {
        revision,
        parameters: p.clone(),
    };
    let permit = {
        let mut bridge = studio.editor_mcp.borrow_mut();
        if request.instance_id != bridge.instance_id {
            return (Response::error("appUnavailable"), false);
        }
        let Some((kind, ids)) = bridge.active_ids(studio, app) else {
            return (Response::error("wrongProject"), false);
        };
        if kind != "clip" {
            return (Response::error("wrongProject"), false);
        }
        let current = bridge.scope(&ids);
        match bridge.writes.cached(
            &current,
            &scope,
            &key,
            &request.client_token,
            &fingerprint,
            Instant::now(),
        ) {
            Ok(Some(r)) => return (r, false),
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
            Ok(Begin::Cached(r)) => return (r, false),
            Err(e) => return (Response::error(registry_status(e)), false),
        }
        if !bridge.canvas_receipts.reserve(
            scope.clone(),
            request.client_id.clone(),
            p,
            Instant::now(),
        ) {
            let r = Response::error("resourceLimit");
            let _ = bridge
                .writes
                .finish(&scope, &key, r.clone(), Instant::now());
            return (r, false);
        }
        permit
    };
    let generation = studio.mcp_clip_state().0;
    if matches!(&p.action.edit, concat_editor_mcp::ClipEdit::Save {}) {
        let response = clip_save::queue_save(request, &scope, &permit, studio, app);
        let mut bridge = studio.editor_mcp.borrow_mut();
        bridge
            .canvas_receipts
            .finish(&scope, &request.client_id, p, response.clone());
        let _ = bridge
            .writes
            .finish(&scope, &key, response.clone(), Instant::now());
        return (response, false);
    }
    if matches!(
        &p.action.edit,
        concat_editor_mcp::ClipEdit::ImportLibrary { .. }
    ) {
        let response = clip_import::queue_import(request, &scope, &permit, generation, studio, app);
        let mut bridge = studio.editor_mcp.borrow_mut();
        bridge
            .canvas_receipts
            .finish(&scope, &request.client_id, p, response.clone());
        let _ = bridge
            .writes
            .finish(&scope, &key, response.clone(), Instant::now());
        return (response, false);
    }
    let result = final_check(studio, app, request, &scope, &permit, generation).and_then(|()| {
        let expected = clip_edit::ClipBinding {
            project_id: &scope.project,
            document_session_id: &scope.session,
            generation,
            revision,
            selection_revision: p.selection_revision,
            timeline_id: &p.action.timeline_id,
            expected_edit_state_id: &p.action.expected_edit_state_id,
        };
        let actual = clip_edit::ClipIdentity {
            project_id: &scope.project,
            document_session_id: &scope.session,
        };
        let hooks = clip_edit::ClipNativeHooks {
            apply: Studio::mcp_clip_apply,
            peek_history: Some(|s, d| {
                s.mcp_clip_peek_history(matches!(d, clip_edit::ClipHistory::Redo))
            }),
            history: Some(|s, d| s.mcp_clip_history(matches!(d, clip_edit::ClipHistory::Redo))),
        };
        let mut refusal = None;
        let edit = clip_edit::execute(
            studio,
            &expected,
            &actual,
            &p.action.edit,
            &hooks,
            |prepared| match final_check(prepared, app, request, &scope, &permit, generation) {
                Ok(()) => Ok(()),
                Err(error) => {
                    refusal = Some(error);
                    Err(clip_edit::ClipEditError::CommitFailed)
                }
            },
        );
        match edit {
            Ok(receipt) => Ok(receipt),
            Err(error) => Err(refusal.unwrap_or_else(|| Response::error(error.status()))),
        }
    });
    let (r, changed) = match result {
        Ok(receipt) => {
            let changed = receipt.changed;
            (
                response(
                    request,
                    &scope,
                    if changed { "ok" } else { "unchanged" },
                    serde_json::to_value(receipt).expect("native clip receipt serializes"),
                ),
                changed,
            )
        }
        Err(error) => {
            let changed = studio.mcp_clip_state().1 != revision;
            (
                response(
                    request,
                    &scope,
                    if changed {
                        "outcomeUnknown"
                    } else {
                        &error.status
                    },
                    json!({}),
                ),
                changed,
            )
        }
    };
    let mut bridge = studio.editor_mcp.borrow_mut();
    bridge
        .canvas_receipts
        .finish(&scope, &request.client_id, p, r.clone());
    let _ = bridge
        .writes
        .finish(&scope, &key, r.clone(), Instant::now());
    (r, changed)
}
