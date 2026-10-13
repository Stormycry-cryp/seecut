//! Flow uses the same native lease and exact per-call approval as canvas tools.
use super::*;
pub(super) fn read(
    bridge: &mut BridgeUi,
    request: &Request,
    studio: &Studio,
    _app: &App,
    ids: &DocumentIds,
) -> Response {
    let Some(flow) = studio.flow.as_ref() else {
        return Response::error("wrongProject");
    };
    let Some(snapshot) = flow.snapshot() else {
        return Response::error("wrongProject");
    };
    if request.method == Method::Capabilities {
        return Response::ok(
            json!({"appInstanceId":bridge.instance_id,"tools":["context","capabilities","flow_read","flow_write"],"namedToolApprovalRequired":["flow_write"],"writeAuthorization":"trustedUI","writeLeaseSeconds":300,"writeWorkspace":"flow","maxPageSize":50}),
        );
    }
    if request.method == Method::Context {
        return Response::ok(
            json!({"appInstanceId":bridge.instance_id,"activeWorkspace":"flow","documents":[{
                "kind":"flow","projectId":ids.project,"documentSessionId":ids.session,"revision":snapshot.revision,
                "contextRevision":snapshot.revision,"selectionRevision":snapshot.selection_revision,"selectedObjectIds":snapshot.selected,
                "dirty":snapshot.busy,"busy":if snapshot.busy{Some("editing")}else{None},
                "flow":{"graphId":snapshot.graph_id,"accountScope":snapshot.account_scope,"documentId":snapshot.document,"projectId":snapshot.project}
            }]}),
        );
    }
    if request.project_id.as_deref() != Some(&ids.project)
        || request.document_session_id.as_deref() != Some(&ids.session)
    {
        return Response::error("wrongProject");
    }
    if request.method != Method::FlowRead {
        return Response::error("unsupportedWorkspace");
    }
    let Some(p) = request.flow.as_ref() else {
        return Response::error("invalidInput");
    };
    match flow.read(p, request.revision) {
        Ok(data) if p.action == "operation" => {
            if data["clientId"] != request.client_id
                || data["documentSessionId"] != ids.session
                || data["operationId"] != p.operation_id.as_deref().unwrap_or("")
                || data["clientSequence"] != p.client_sequence.unwrap_or(0)
            {
                return Response::error("notAuthorized");
            }
            Response::ok(data)
        }
        Ok(data) => Response::ok(
            json!({"projectId":ids.project,"documentSessionId":ids.session,"graphId":p.graph_id,"accountScope":p.account_scope,"revision":snapshot.revision,"selectionRevision":snapshot.selection_revision,"data":data}),
        ),
        Err(status) => Response::error(&status),
    }
}
pub(super) fn write(request: &Request, studio: &mut Studio, app: &App) -> (Response, bool) {
    let Some(scope) = request_scope(request) else {
        return (Response::error("invalidInput"), false);
    };
    let (Some(p), Some(revision)) = (request.flow.as_ref(), request.revision) else {
        return (Response::error("invalidInput"), false);
    };
    let key = OperationKey {
        client: request.client_id.clone(),
        document_session: scope.session.clone(),
        client_sequence: p.client_sequence.unwrap_or(0),
    };
    let fingerprint = EditFingerprint::Flow {
        revision,
        parameters: p.clone(),
    };
    {
        let mut bridge = studio.editor_mcp.borrow_mut();
        if request.instance_id != bridge.instance_id {
            return (Response::error("appUnavailable"), false);
        }
        let Some((kind, ids)) = bridge.active_ids(studio, app) else {
            return (Response::error("wrongProject"), false);
        };
        if kind != "flow" {
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
        if modal_busy(studio, app, true) {
            return (Response::error("busy"), false);
        }
        let Some(flow) = studio.flow.as_ref() else {
            return (Response::error("wrongProject"), false);
        };
        if let Err(status) = flow.check(p, Some(revision)) {
            return (Response::error(&status), false);
        }
        if !bridge.canvas_permits.valid(&permit, Instant::now()) {
            return (Response::error("notAuthorized"), false);
        }
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
    }
    // No async work or event-loop yield lies between the authority check and this
    // commit. Release BridgeUi before Flow publishes or inspects the shared UI.
    let envelope = json!({"appInstanceId":request.instance_id,"clientId":request.client_id,"projectId":scope.project,"documentSessionId":scope.session,"operationId":p.operation_id,"clientSequence":p.client_sequence,"tool":"flow_write"});
    let result = studio
        .flow
        .as_ref()
        .ok_or("wrongProject".to_owned())
        .and_then(|f| f.write(p, revision, envelope));
    let changed = result.is_ok();
    let response = match result {
        Ok(data) => Response::ok(data),
        Err(status) => Response::error(&status),
    };
    if studio
        .editor_mcp
        .borrow_mut()
        .writes
        .finish(&scope, &key, response.clone(), Instant::now())
        .is_err()
    {
        return (Response::error("outcomeUnknown"), changed);
    }
    (response, changed)
}
