//! App navigation negotiation, original identity, receipt and stdio schema regressions.
use concat_editor_mcp::*;
use serde_json::{Value, json};
const TOKEN: &str = "a3883456-096e-4f11-b093-ec58c66d867d";
fn page() -> Value {
    json!({"pageId":"generation","pageEpoch":1,"pageStateId":format!("app-page-v1-{}", "b".repeat(64))})
}
fn scope() -> Value {
    json!({"accountScope":"a".repeat(64),"appSessionId":"app","appControlProtocolVersion":1})
}
fn navigate() -> Value {
    json!({"appSessionId":"app","operationId":app_operation_id("client","coto",1).unwrap(),"sessionId":"coto","clientSequence":1,"before":page(),"target":"personalLibrary"})
}
fn query() -> Value {
    json!({"operationId":app_operation_id("client","coto",1).unwrap(),"sessionId":"coto","clientSequence":1})
}
fn raw(method: Method) -> Value {
    json!({"instanceId":"instance","clientId":"client","clientToken":"read-token","method":method,"appScope":scope()})
}
fn nav_request() -> Request {
    let mut v = raw(Method::AppNavigate);
    v["appNavigate"] = navigate();
    v["appControlToken"] = json!(TOKEN);
    serde_json::from_value(v).unwrap()
}
fn operation_request() -> Request {
    let mut v = raw(Method::AppOperation);
    v["appOperationQuery"] = query();
    serde_json::from_value(v).unwrap()
}
fn receipt() -> AppNavigationReceipt {
    serde_json::from_value(json!({"appControlProtocolVersion":1,"operationId":query()["operationId"],"sessionId":"coto","clientSequence":1,
        "origin":{"appInstanceId":"instance","clientId":"client","appSessionId":"app"},"before":page(),"target":"personalLibrary",
        "recordedStatus":"running","status":"running","durability":"confirmed","completion":null,"humanGate":null,"currentObservation":null})).unwrap()
}
#[test]
fn legacy_scope_serialization_and_response_are_unchanged() {
    let v = json!({"accountScope":"a".repeat(64),"appSessionId":"app"});
    let s: AppReadScope = serde_json::from_value(v.clone()).unwrap();
    assert!(s.valid());
    assert_eq!(serde_json::to_value(s).unwrap(), v);
    let response = Response::ok(
        json!({"scopeKind":"app","tools":["capabilities","app_context","page_items","page_preview"],"readOnly":true}),
    );
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        json!({"status":"ok","data":{"scopeKind":"app","tools":["capabilities","app_context","page_items","page_preview"],"readOnly":true}})
    );
}
#[test]
fn version_is_exact_or_omitted_never_null() {
    for v in [
        Value::Null,
        json!(0),
        json!(2),
        json!("1"),
        json!(true),
        json!(-1),
        json!(1.5),
    ] {
        let mut s = scope();
        s["appControlProtocolVersion"] = v;
        assert!(serde_json::from_value::<AppReadScope>(s).is_err());
    }
    let mut s = scope();
    s["protocolVersion"] = json!(1);
    assert!(serde_json::from_value::<AppReadScope>(s).is_err());
    assert!(
        serde_json::from_value::<AppReadScope>(scope())
            .unwrap()
            .valid()
    );
}
#[test]
fn trusted_environment_requires_independent_exact_negotiation() {
    for version in ["", "01", "2", " 1", "1 "] {
        let mut s = Some(serde_json::from_value(scope()).unwrap());
        assert!(app_control_environment(&mut s, Some(version), None).is_err());
    }
    let mut s: Option<AppReadScope> = None;
    assert!(app_control_environment(&mut s, Some("1"), Some(TOKEN)).is_err());
    let mut s = Some(serde_json::from_value(scope()).unwrap());
    assert!(app_control_environment(&mut s, None, Some(TOKEN)).is_err());
    for token in [
        "",
        "secret",
        "00000000-0000-0000-0000-000000000000",
        "A3883456-096E-4F11-B093-EC58C66D867D",
    ] {
        assert!(app_control_environment(&mut s, Some("1"), Some(token)).is_err());
    }
}
#[test]
fn negotiation_without_control_token_does_not_grant_navigation() {
    let mut s = Some(serde_json::from_value(scope()).unwrap());
    assert_eq!(
        app_control_environment(&mut s, Some("1"), None).unwrap(),
        None
    );
    let mut r = nav_request();
    r.app_control_token = None;
    assert!(validate_request_shape(&r).is_err());
    assert!(validate_request_shape(&operation_request()).is_ok());
}
#[test]
fn valid_navigation_is_write_and_original_query_is_read() {
    let r = nav_request();
    assert!(validate_request_shape(&r).is_ok());
    assert!(r.method.is_write());
    assert_eq!(r.method.tool_name(), "app_navigate");
    let q = operation_request();
    assert!(validate_request_shape(&q).is_ok());
    assert!(!q.method.is_write());
    assert_eq!(q.method.tool_name(), "app_operation");
}
#[test]
fn canonical_ids_use_utf8_lengths_and_safe_sequences() {
    assert_eq!(
        app_operation_id("中:c", "会:s", 9).unwrap(),
        "sca1:1:5:中:c:5:会:s:9"
    );
    for s in [0, 9_007_199_254_740_992, u64::MAX] {
        assert!(app_operation_id("c", "s", s).is_err());
    }
    for s in ["", "a\n", &"a".repeat(65)] {
        assert!(app_operation_id(s, "s", 1).is_err());
        assert!(app_operation_id("c", s, 1).is_err());
    }
    assert_ne!(
        app_operation_id("é", "s", 1).unwrap(),
        app_operation_id("e\u{301}", "s", 1).unwrap()
    );
}
#[test]
fn unknown_payloads_paths_openproject_and_action_are_refused() {
    for (key, value) in [
        ("path", json!("/tmp/a")),
        ("action", json!("navigate-resume")),
        ("accountScope", json!("forged")),
        ("projectEntryId", json!("app-item-forged")),
    ] {
        let mut v = navigate();
        v[key] = value;
        assert!(serde_json::from_value::<AppNavigateParameters>(v).is_err());
    }
    for target in ["openProject", "navigate-resume", "canvas", "/tmp/a"] {
        let mut v = navigate();
        v["target"] = json!(target);
        assert!(serde_json::from_value::<AppNavigateParameters>(v).is_err());
    }
}
#[test]
fn all_eight_named_page_targets_are_supported() {
    for target in [
        "generation",
        "personalLibrary",
        "teamLibrary",
        "templates",
        "account",
        "credits",
        "settings",
        "projects",
    ] {
        let mut r = nav_request();
        let mut v = navigate();
        v["target"] = json!(target);
        r.app_navigate = Some(serde_json::from_value(v).unwrap());
        assert!(validate_request_shape(&r).is_ok());
    }
}
#[test]
fn mismatched_identity_source_or_method_is_refused() {
    let mut r = nav_request();
    r.app_navigate.as_mut().unwrap().operation_id.push('0');
    assert!(validate_request_shape(&r).is_err());
    let mut r = nav_request();
    r.app_navigate.as_mut().unwrap().app_session_id = "old".into();
    assert!(validate_request_shape(&r).is_err());
    let mut r = nav_request();
    r.app_navigate.as_mut().unwrap().before.page_epoch = 0;
    assert!(validate_request_shape(&r).is_err());
    let mut r = nav_request();
    r.app_navigate.as_mut().unwrap().before.page_state_id = "fake".into();
    assert!(validate_request_shape(&r).is_err());
    let mut r = nav_request();
    r.method = Method::AppOperation;
    assert!(validate_request_shape(&r).is_err());
}
#[test]
fn original_query_accepts_current_app_session_without_old_control_or_page() {
    let mut r = operation_request();
    r.instance_id = "new-instance".into();
    r.app_scope.as_mut().unwrap().app_session_id = "new-app-session".into();
    assert!(validate_request_shape(&r).is_ok());
    r.app_control_token = Some(TOKEN.into());
    assert!(validate_request_shape(&r).is_err());
    r.app_control_token = None;
    r.app_page = Some(AppPageQuery::Context(AppContextQuery {
        app_session_id: "new-app-session".into(),
    }));
    assert!(validate_request_shape(&r).is_err());
    let mut v = query();
    v["before"] = page();
    assert!(serde_json::from_value::<AppOperationQuery>(v).is_err());
}
#[test]
fn any_app_control_field_on_doc_and_doc_field_on_app_is_refused() {
    for key in [
        "projectId",
        "documentSessionId",
        "revision",
        "contextRevision",
        "offset",
        "limit",
        "maxEdge",
        "objectId",
        "objectIds",
        "operationQuery",
        "flow",
        "flowWorkspace",
    ] {
        let mut v = serde_json::to_value(nav_request()).unwrap();
        v[key] = match key {
            "revision" | "contextRevision" | "offset" | "limit" | "maxEdge" => json!(1),
            "objectIds" => json!(["a"]),
            "operationQuery" => json!({"operationId":"a","clientSequence":1}),
            "flow" | "flowWorkspace" => json!({}),
            _ => json!("a"),
        };
        if let Ok(r) = serde_json::from_value::<Request>(v) {
            assert!(validate_request_shape(&r).is_err(), "{key}");
        }
    }
    let mut r = operation_request();
    r.app_scope = None;
    r.method = Method::Context;
    assert!(validate_request_shape(&r).is_err());
    let mut r = nav_request();
    r.app_scope.as_mut().unwrap().app_control_protocol_version = None;
    assert!(validate_request_shape(&r).is_err());
}
#[test]
fn dispatch_is_running_and_completed_requires_real_completion_and_durability() {
    let mut r = receipt();
    assert!(r.valid());
    r.status = AppOperationStatus::Completed;
    assert!(!r.valid());
    r.recorded_status = AppOperationStatus::Completed;
    assert!(!r.valid());
    let mut actual: AppViewBinding = serde_json::from_value(page()).unwrap();
    actual.page_id = AppPageId::PersonalLibrary;
    r.completion = Some(AppNavigationCompletion { page: actual });
    assert!(r.valid());
    r.durability = AppOperationDurability::Unconfirmed;
    assert!(!r.valid());
    r.status = AppOperationStatus::OutcomeUnknown;
    assert!(r.valid());
    r.completion.as_mut().unwrap().page.page_id = AppPageId::Projects;
    assert!(!r.valid());
}
#[test]
fn recovered_running_and_human_gate_never_imply_completion() {
    let mut r = receipt();
    r.status = AppOperationStatus::OutcomeUnknown;
    r.human_gate = Some(AppNavigationHumanGate::DocumentLeaveConfirmation);
    assert!(r.valid());
    r.status = AppOperationStatus::Completed;
    assert!(!r.valid());
    let mut v = serde_json::to_value(receipt()).unwrap();
    v["humanGate"] = json!("navigateResume");
    assert!(serde_json::from_value::<AppNavigationReceipt>(v).is_err());
}
#[test]
fn public_receipt_is_closed_and_does_not_expose_account_token_or_path() {
    for key in [
        "token",
        "appControlToken",
        "account",
        "accountScope",
        "path",
        "authority",
    ] {
        let mut v = serde_json::to_value(receipt()).unwrap();
        v[key] = json!("secret");
        assert!(serde_json::from_value::<AppNavigationReceipt>(v).is_err());
    }
    let v = serde_json::to_string(&receipt()).unwrap();
    assert!(!v.contains(TOKEN));
    assert!(!v.contains("account"));
}
#[test]
fn original_completion_and_current_observation_remain_separate() {
    let mut r = receipt();
    r.current_observation = Some(AppCurrentObservation {
        app_instance_id: "new-instance".into(),
        app_session_id: "new-session".into(),
        page: serde_json::from_value(page()).unwrap(),
    });
    assert!(r.valid());
    let mut request = operation_request();
    request.instance_id = "new-instance".into();
    request.app_scope.as_mut().unwrap().app_session_id = "new-session".into();
    let response = app_control::checked_app_navigation_response(
        &request,
        Response::ok(serde_json::to_value(r).unwrap()),
    );
    assert_eq!(response.status, "ok");
}
#[test]
fn wrong_or_extra_success_receipt_is_ambiguous_without_leaking_payload() {
    let request = nav_request();
    let mut wrong = receipt();
    wrong.target = AppPageId::Projects;
    let result = app_control::checked_app_navigation_response(
        &request,
        Response::ok(serde_json::to_value(wrong).unwrap()),
    );
    assert_eq!(result.status, "outcomeUnknown");
    assert!(result.data.is_none());
    let result = app_control::checked_app_navigation_response(
        &request,
        Response::ok(json!({"dispatched":true,"account":"secret"})),
    );
    assert_eq!(result.status, "outcomeUnknown");
    assert!(result.data.is_none());
    let result = app_control::checked_app_navigation_response(
        &operation_request(),
        Response::ok(json!({"account":"secret"})),
    );
    assert_eq!(result.status, "ioFailure");
    assert!(result.data.is_none());
}

#[cfg(feature = "stdio")]
mod stdio_schema {
    use concat_editor_mcp::*;
    use serde_json::json;
    #[test]
    fn navigation_schema_has_only_closed_trusted_wrapper_parameters() {
        let schema = serde_json::to_value(schemars::schema_for!(AppNavigateParameters)).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        let keys = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "appSessionId",
                "before",
                "clientSequence",
                "operationId",
                "sessionId",
                "target"
            ]
        );
        assert_eq!(schema["required"].as_array().unwrap().len(), 6);
        assert_eq!(schema["properties"]["clientSequence"]["minimum"], 1);
        assert_eq!(
            schema["properties"]["clientSequence"]["maximum"],
            9_007_199_254_740_991u64
        );
        assert_eq!(
            serde_json::Value::Array(
                schema["$defs"]["AppPageId"]["oneOf"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["const"].clone())
                    .collect()
            ),
            json!([
                "generation",
                "personalLibrary",
                "teamLibrary",
                "templates",
                "account",
                "credits",
                "settings",
                "projects"
            ])
        );
        assert_eq!(
            schema["$defs"]["AppViewBinding"]["additionalProperties"],
            false
        );
        assert_eq!(
            schema["$defs"]["AppViewBinding"]["required"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
    #[test]
    fn original_query_schema_has_no_current_page_session_or_control_token() {
        let schema = serde_json::to_value(schemars::schema_for!(AppOperationQuery)).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        let keys = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["clientSequence", "operationId", "sessionId"]);
        assert_eq!(schema["required"].as_array().unwrap().len(), 3);
    }
}
