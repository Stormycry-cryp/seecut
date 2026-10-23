//! Synthetic IPC only. Tests stdio wire conversion, never actual native pixels.
use super::*;
use serde_json::{Value, json};
fn data() -> Value {
    json!({"version":1,"tool":"clip_preview","source":"adoptedMonitorFrame","appInstanceId":"instance","clientId":"client","projectId":"p","documentSessionId":"s","revision":2,"contextRevision":3,"selectionRevision":4,"viewStateId":format!("view-v1-{}","b".repeat(64)),"frame":{"state":"ready","version":1,"source":"adoptedMonitorFrame","frameId":format!("clip-frame-v1-{}","a".repeat(64)),"generation":7,"revision":2,"timelineId":"timeline:1","renderedFrameTimeSeconds":1.96,"sourceWidth":3,"sourceHeight":2},"playheadSeconds":2.0,"width":3,"height":2,"mimeType":"image/png","byteLength":71,"pngSha256":"6d365bf71967774da0b019284bbc187da65b2cbd2475ac120cce657dc2550c6e","capturedAtUnixMs":1000,"viewportCapture":false,"includesOverlays":false,"base64":"iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAADklEQVR4nGOoRwIMyBwAmuIL6QDbxXQAAAAASUVORK5CYII="})
}
fn args() -> ClipPreviewArgs {
    serde_json::from_value(json!({"projectId":"p","documentSessionId":"s","revision":2,"contextRevision":3,"frameId":format!("clip-frame-v1-{}","a".repeat(64))})).unwrap()
}
fn bridge() -> Bridge {
    Bridge {
        instance: "instance".into(),
        client: "client".into(),
        token: "read-only".into(),
        write_token: None,
        app_scope: None,
        app_control_token: None,
        socket: PathBuf::from("unused"),
    }
}
#[test]
fn clip_preview_schema_is_closed_and_read_only() {
    let tools = Bridge::tool_router().list_all();
    let tool = tools.iter().find(|t| t.name == "clip_preview").unwrap();
    assert_eq!(
        tool.annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );
    let schema = serde_json::to_value(&tool.input_schema).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"].as_object().unwrap().len(), 6);
    assert_eq!(schema["required"].as_array().unwrap().len(), 5);
    for key in ["path", "time", "source", "operationId", "clientSequence"] {
        let mut a=serde_json::to_value(json!({"projectId":"p","documentSessionId":"s","revision":2,"contextRevision":3,"frameId":format!("clip-frame-v1-{}","a".repeat(64))})).unwrap();
        a[key] = json!("forged");
        assert!(serde_json::from_value::<ClipPreviewArgs>(a).is_err());
    }
    let request = bridge().request(Method::ClipPreview);
    assert_eq!(request.client_token, "read-only");
    assert!(!request.method.is_write());
    assert!(request.clip_parameters.is_none());
    let mut invalid = args();
    invalid.max_edge = Some(257);
    assert_eq!(
        serde_json::to_value(bridge().clip_preview(Parameters(invalid))).unwrap()["isError"],
        true
    );
}
#[cfg(unix)]
fn converted(response: Response) -> Value {
    use std::os::unix::net::UnixListener;
    let path = endpoint(&uuid::Uuid::new_v4().to_string()).unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let r: Request = concat_editor_mcp::read_message(&mut stream).unwrap();
        assert_eq!(r.method, Method::ClipPreview);
        assert_eq!(r.client_token, "read-only");
        assert!(concat_editor_mcp::validate_request_shape(&r).is_ok());
        concat_editor_mcp::write_message(&mut stream, &response).unwrap();
    });
    let mut b = bridge();
    b.socket = path.clone();
    let result = b.clip_preview(Parameters(args()));
    worker.join().unwrap();
    std::fs::remove_file(path).unwrap();
    serde_json::to_value(result).unwrap()
}
#[test]
#[cfg(unix)]
fn stdio_preserves_response_envelope_and_emits_one_image_without_text_base64() {
    let result = converted(Response::ok(data()));
    assert_ne!(result["isError"], true);
    assert_eq!(
        result["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["type"] == "image")
            .count(),
        1
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    let envelope: Value = serde_json::from_str(text).unwrap();
    assert_eq!(envelope["status"], "ok");
    assert!(envelope["data"].get("base64").is_none());
    assert_eq!(result["structuredContent"], envelope);
    assert!(!text.contains("iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAADklEQVR4nGOoRwIMyBwAmuIL6QDbxXQAAAAASUVORK5CYII="));
}
#[test]
#[cfg(unix)]
fn stale_or_malformed_native_results_cannot_emit_images_or_error_bytes() {
    let result = converted(Response {
        status: "stalePreview".into(),
        data: Some(data()),
    });
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"].as_array().unwrap().len(), 1);
    assert!(!result.to_string().contains("iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAADklEQVR4nGOoRwIMyBwAmuIL6QDbxXQAAAAASUVORK5CYII="));
    for key in [
        "source",
        "frame",
        "pngSha256",
        "byteLength",
        "width",
        "clientId",
    ] {
        let mut v = data();
        v[key] = json!("forged");
        let result = converted(Response::ok(v));
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        assert!(!result.to_string().contains("iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAADklEQVR4nGOoRwIMyBwAmuIL6QDbxXQAAAAASUVORK5CYII="));
    }
}
