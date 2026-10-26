//! Synthetic metadata plus a real generated PNG; no GUI sampling evidence.
use super::*;

const FIXTURE: &str = r#"{"kind":"adoptedDocumentComposite","adoptionStage":"uiPaintSubmitted","snapshotId":"snapshot-v1-2b503d396b74c39ce5b9a677bfff79d3d8d8eae821a8d63fbe934870b04fac2b","requestId":"capture-1","capturedFrame":18446744073709551605,"capturedAfterUiEnqueueMs":1,"timeBasis":{"captureDelay":"uiEnqueue","captureAge":"adoptedFrameCapture"},"returnedHistoricalFrame":true,"captureAgeAtReplyMs":2,"viewportId":18446744073709551611,"canvasId":18446744073709551610,"identity":{"hostInstanceId":"8e88a522-e405-40cd-9ef0-56b530ec0c21","docId":18446744073709551615,"documentGeneration":18446744073709551614,"revision":18446744073709551613},"document":{"name":"SENSITIVE_DOCUMENT_NAME","size":[1,1],"layers":[{"id":18446744073709551612,"name":"SENSITIVE_LAYER_NAME","kind":"raster","depth":0,"visible":true,"opacity":1.0,"fillOpacity":1.0,"mask":false}],"layersOrder":"documentBottomToTop","activeLayer":18446744073709551612,"selectedLayers":[18446744073709551612],"selectedLayersOrder":"documentBottomToTop","dirty":true,"canUndo":true,"canRedo":false},"selection":{"present":false,"domain":[0,0,1,1],"bounds":null,"boundsFormat":"xyxyExclusive","coverageHash":"ed26a630fdc5f445cd61196386cd39c4c63f2f08e98fd45444c5437f7df9c402","selectionStateId":"selection-v1-e1a54e8d7a65facef3d1f62d0fc188f35c1dc7bfb743b7ba8d7cb5c54192bf6a"},"view":{"geometry":{"rectPoints":[0.0,0.0,8.0,8.0],"pixelsPerPoint":1.0,"zoom":8.0,"center":[0.5,0.5],"flip":false,"rotation":0.0,"aspect":1.0,"displayCamera":[8.0,0.5,0.5],"targetCamera":[8.0,0.5,0.5],"visibleDocumentPolygon":[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0]]},"visibleDocumentAabb":[0.0,0.0,1.0,1.0],"clippedVisibleDocumentAabb":[0.0,0.0,1.0,1.0],"aabbFormat":"xyxyExclusive","viewStateId":"view-v1-240f7bc9538c74e547bab1ae94877fd6edd44edabb43c18c1c7932a5d0c5fb73"},"presentation":{"source":{"renderer":"cpu","displayKey":18446744073709551609,"rawPreviewKey":0,"complete":true,"displayIdentity":true,"sourceIsSrgb":true,"proof":false,"hdr":false,"channelComposite":true},"colorSpace":"sRGB IEC61966-2.1","profileHash":"d07634c10e65ca175d77d32886e90bd2c566b40ad6aa82046ccf38df858dc03a","pixelSemantics":"documentCompositeNotMonitorPixels"},"pixels":{"kind":"documentComposite","transparent":true,"mime":"image/png","width":1,"height":1,"documentRect":[0,0,1,1],"documentPixelsPerOutputPixel":[1.0,1.0],"pixelHash":"e8fede75a5544b49a7509938cad7d7a9848292bd80ad3aaaf36de57606e797e0","pngHash":"b8c2b8ee9f8608d727aeee696d15e534681e899e9bbf8a73b23f989e5ca06182","base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAAXNSR0IArs4c6QAAAA1JREFUeJxj+N/A0AAABgECAA08jykAAAAASUVORK5CYII="}}"#;
const SECOND_SNAPSHOT_ID: &str =
    "snapshot-v1-7de9b13ef49a759101662cc04b2af93709fb672a4928b4701d2b0d60f34b26ce";
// GENERATED FIXTURE END

fn fixture() -> (Value, PhotoIdentity) {
    let value: Value = serde_json::from_str(FIXTURE).unwrap();
    let i = &value["identity"];
    let identity = PhotoIdentity {
        host_instance_id: Uuid::parse_str(i["hostInstanceId"].as_str().unwrap()).unwrap(),
        doc_id: i["docId"].as_u64().unwrap(),
        document_generation: i["documentGeneration"].as_u64().unwrap(),
        revision: i["revision"].as_u64().unwrap(),
    };
    (value, identity)
}
fn parsed(
    v: &Value,
    request: &str,
    identity: &PhotoIdentity,
    max_edge: u32,
) -> Result<PhotoSnapshot> {
    let received = Instant::now();
    parse_snapshot(
        v,
        request,
        identity,
        max_edge,
        received - Duration::from_millis(10),
        received,
    )
}
fn parsed_validation(
    v: &Value,
    request: &str,
    snapshot: &PhotoSnapshot,
) -> Result<PhotoSnapshotValidation> {
    let started = snapshot.received_at + Duration::from_millis(1);
    parse_validation(v, request, snapshot, started, started)
}
fn snapshot() -> PhotoSnapshot {
    let (v, i) = fixture();
    parsed(&v, "capture-1", &i, 256).unwrap()
}
fn validation(s: &PhotoSnapshot) -> Value {
    json!({"kind":"adoptedSnapshotValidation","adoptionStage":"uiPaintSubmitted","validationStage":"postInputFrame",
        "requestId":"validate-1","snapshotId":s.snapshot_id(),"identity":{
            "hostInstanceId":s.identity.host_instance_id.to_string(),"docId":s.identity.doc_id,
            "documentGeneration":s.identity.document_generation,"revision":s.identity.revision},
        "viewStateId":s.view.view_state_id,"selectionStateId":s.selection.selection_state_id,
        "validatedFrame":s.captured_frame+1,"captureAgeMs":3})
}

#[test]
fn real_png_and_full_u64_historical_metadata_are_preserved_without_debug_leaks() {
    let s = snapshot();
    assert_eq!(s.identity().doc_id, u64::MAX);
    assert_eq!(s.identity().document_generation, u64::MAX - 1);
    assert_eq!(s.identity().revision, u64::MAX - 2);
    assert_eq!(s.document().layers[0].id, u64::MAX - 3);
    assert_eq!(s.viewport_id(), u64::MAX - 4);
    assert_eq!(s.canvas_id(), u64::MAX - 5);
    assert_eq!(s.presentation().display_key, u64::MAX - 6);
    assert_eq!(s.pixels().size, [1, 1]);
    assert!(s.png_bytes().starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(digest(&[s.png_bytes()]), s.pixels().png_hash);
    let debug = format!("{s:?} {:?} {:?}", s.document(), s.document().layers[0]);
    for hidden in [
        "SENSITIVE_DOCUMENT_NAME",
        "SENSITIVE_LAYER_NAME",
        "iVBOR",
        "base64",
        "png:",
    ] {
        assert!(!debug.contains(hidden));
    }
    let proof = parsed_validation(&validation(&s), "validate-1", &s).unwrap();
    assert_eq!(proof.snapshot_id(), s.snapshot_id());
    assert_eq!(proof.validated_frame(), s.captured_frame() + 1);
}

#[test]
fn snapshot_closed_schema_rejects_wrong_ids_hashes_dimensions_and_source_claims() {
    let (base, i) = fixture();
    for (pointer, value) in [
        (
            "/identity/hostInstanceId",
            json!(Uuid::new_v4().to_string()),
        ),
        ("/identity/docId", json!(9007199254740992_f64)),
        ("/identity/documentGeneration", json!(1)),
        ("/identity/revision", json!(1)),
        ("/requestId", json!("other")),
        (
            "/snapshotId",
            json!(format!("snapshot-v2-{}", "a".repeat(64))),
        ),
        (
            "/snapshotId",
            json!(format!("snapshot-v1-{}", "a".repeat(64))),
        ),
        ("/viewportId", json!(1)),
        (
            "/view/viewStateId",
            json!(format!("view-v1-{}", "a".repeat(64))),
        ),
        (
            "/selection/selectionStateId",
            json!(format!("selection-v1-{}", "a".repeat(64))),
        ),
        ("/selection/coverageHash", json!("a".repeat(64))),
        ("/returnedHistoricalFrame", json!(false)),
        ("/adoptionStage", json!("presented")),
        ("/capturedAfterUiEnqueueMs", json!(2000)),
        ("/captureAgeAtReplyMs", json!(5000)),
        ("/timeBasis/captureDelay", json!("wireArrival")),
        ("/pixels/base64", json!("%%%malformed")),
        ("/pixels/pngHash", json!("a".repeat(64))),
        ("/pixels/pngHash", json!("a".repeat(63))),
        ("/pixels/width", json!(2)),
        ("/pixels/width", json!(257)),
        ("/pixels/transparent", json!(false)),
        ("/pixels/mime", json!("image/jpeg")),
        ("/presentation/colorSpace", json!("Display P3")),
        ("/presentation/source/proof", json!(true)),
        ("/presentation/source/rawPreviewKey", json!(1)),
        ("/presentation/source/complete", json!(false)),
        ("/presentation/source/sourceIsSrgb", json!(false)),
        ("/presentation/source/hdr", json!(true)),
        ("/presentation/source/channelComposite", json!(false)),
        ("/view/geometry/rectPoints/2", json!(0.0)),
        ("/view/geometry/targetCamera/0", json!(16.0)),
        ("/view/visibleDocumentAabb/2", json!(0.5)),
        ("/document/layers/0/mask", json!(true)),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            matches!(
                parsed(&changed, "capture-1", &i, 256),
                Err(HostError::ReadFailed(_))
            ),
            "{pointer}"
        );
    }
    for pointer in [
        "",
        "/identity",
        "/document",
        "/document/layers/0",
        "/selection",
        "/view",
        "/view/geometry",
        "/presentation",
        "/presentation/source",
        "/pixels",
        "/timeBasis",
    ] {
        let mut changed = base.clone();
        changed
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        assert!(parsed(&changed, "capture-1", &i, 256).is_err(), "{pointer}");
    }
}

#[test]
fn png_digest_container_crc_header_and_reply_budgets_are_checked() {
    let (base, i) = fixture();
    let original = base64::engine::general_purpose::STANDARD
        .decode(base["pixels"]["base64"].as_str().unwrap())
        .unwrap();
    for bytes in [
        vec![0; 32],
        original[..33].to_vec(),
        {
            let mut b = original.clone();
            let at = b.iter().position(|b| *b == b'x').unwrap_or(40);
            b[at] ^= 1;
            b
        },
        {
            let mut b = original.clone();
            b[16..20].copy_from_slice(&2u32.to_be_bytes());
            let crc = crc32(&b[12..29]);
            b[29..33].copy_from_slice(&crc.to_be_bytes());
            b
        },
        vec![0; MAX_PNG + 1],
    ] {
        let mut changed = base.clone();
        changed["pixels"]["base64"] =
            json!(base64::engine::general_purpose::STANDARD.encode(&bytes));
        changed["pixels"]["pngHash"] = json!(digest(&[&bytes]));
        assert!(parsed(&changed, "capture-1", &i, 256).is_err());
    }
    let mut over = base;
    over["document"]["name"] = json!("x".repeat(MAX_RESULT + 1));
    assert!(parsed(&over, "capture-1", &i, 256).is_err());
}

#[test]
fn validation_requires_original_snapshot_identity_ids_and_post_input_boundary() {
    let s = snapshot();
    let base = validation(&s);
    for (pointer, value) in [
        ("/requestId", json!("old-validation")),
        ("/snapshotId", json!(SECOND_SNAPSHOT_ID)),
        (
            "/identity/hostInstanceId",
            json!(Uuid::new_v4().to_string()),
        ),
        ("/identity/revision", json!(1)),
        ("/viewStateId", json!(format!("view-v1-{}", "a".repeat(64)))),
        (
            "/selectionStateId",
            json!(format!("selection-v1-{}", "a".repeat(64))),
        ),
        ("/validationStage", json!("uiCallback")),
        ("/validatedFrame", json!(s.captured_frame())),
        ("/captureAgeMs", json!(1)),
        ("/captureAgeMs", json!(5000)),
        ("/captureAgeMs", json!(100)),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            parsed_validation(&changed, "validate-1", &s).is_err(),
            "{pointer}"
        );
    }
    let (mut other, i) = fixture();
    other["requestId"] = json!("capture-other");
    other["snapshotId"] = json!(SECOND_SNAPSHOT_ID);
    let other = parsed(&other, "capture-other", &i, 256).unwrap();
    assert!(
        parsed_validation(&base, "validate-1", &other).is_err(),
        "old validation cannot bind a new capture"
    );
    let mut extra = base;
    extra["validated"] = json!(true);
    assert!(parsed_validation(&extra, "validate-1", &s).is_err());
    let future_start = s.received_at + Duration::from_millis(2);
    assert!(
        parse_validation(
            &validation(&s),
            "validate-1",
            &s,
            future_start,
            future_start - Duration::from_millis(1)
        )
        .is_err()
    );
}

#[test]
fn requests_refuse_bad_host_id_request_identifier_and_edge_before_dispatch() {
    let (_, i) = fixture();
    for request in ["", "not valid", "line\nbreak"] {
        assert!(check_request(i.host_instance_id, request, &i, 1).is_err());
    }
    assert!(check_request(i.host_instance_id, &"x".repeat(129), &i, 1).is_err());
    for edge in [0, 257, u32::MAX] {
        assert!(check_request(i.host_instance_id, "good", &i, edge).is_err());
    }
    assert!(check_request(Uuid::new_v4(), "good", &i, 1).is_err());
}

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

enum WireReply {
    Ok(Value),
    Reject,
    WrongId,
    Disconnect,
    Timeout,
}
struct OwnedHost(NativeHost);
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if let Some(mut running) = self.0.running.take() {
            if running.child.try_wait().unwrap().is_none() {
                running.child.kill().unwrap();
            }
            running.child.wait().unwrap();
        }
    }
}
fn socket_host(
    replies: Vec<(&'static str, WireReply)>,
) -> (OwnedHost, std::thread::JoinHandle<usize>) {
    let (_, identity) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        if replies.is_empty() {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(150);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => panic!("locally expired snapshot must not connect or dispatch"),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("synthetic listener failed: {error}"),
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            return 0;
        }
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let auth: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(auth["method"], "auth");
        assert_eq!(auth["params"]["token"], "synthetic-token");
        writeln!(
            stream,
            "{}",
            json!({"id":auth["id"],"ok":true,"result":{"authenticated":true}})
        )
        .unwrap();
        let mut count = 0;
        for (method, reply) in replies {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            count += 1;
            assert_eq!(request["method"], method);
            if method == "seecut.snapshot" {
                assert_eq!(
                    request["params"]["expected"]["docId"].as_u64(),
                    Some(u64::MAX)
                );
                assert_eq!(request["params"]["requestId"], "capture-1");
            } else {
                assert_eq!(request["params"].as_object().unwrap().len(), 2);
                assert_eq!(request["params"]["requestId"], "validate-1");
                assert_eq!(request["params"]["snapshotId"], snapshot().snapshot_id());
            }
            let id = request["id"].as_u64().unwrap();
            match reply {
                WireReply::Ok(result) => {
                    std::thread::sleep(Duration::from_millis(5));
                    writeln!(stream, "{}", json!({"id":id,"ok":true,"result":result})).unwrap()
                }
                WireReply::Reject => writeln!(
                    stream,
                    "{}",
                    json!({"id":id,"ok":false,"error":"validationExpired"})
                )
                .unwrap(),
                WireReply::WrongId => {
                    writeln!(stream, "{}", json!({"id":id+1,"ok":true,"result":null})).unwrap()
                }
                WireReply::Disconnect => {
                    stream.shutdown(std::net::Shutdown::Both).unwrap();
                    break;
                }
                WireReply::Timeout => {
                    std::thread::sleep(Duration::from_millis(700));
                    break;
                }
            }
        }
        count
    });
    let child = Command::new("/bin/sleep")
        .arg("120")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.toonflow/photocraft-native-snapshot-tests");
    std::fs::create_dir_all(&parent).unwrap();
    let root = parent.join(Uuid::new_v4().to_string());
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let token_path = root.join("control.token");
    let mut token_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&token_path)
        .unwrap();
    writeln!(token_file, "synthetic-token").unwrap();
    drop(token_file);
    // This is a disposable synthetic lifecycle, not a verified editor build.
    let running = crate::Running {
        info: crate::InstanceInfo {
            id: identity.host_instance_id,
            pid: child.id(),
            build_attempt: "synthetic-socket".into(),
            ready: true,
            status: crate::InstanceStatus::Ready,
        },
        child,
        addr: Some(addr),
        token: "synthetic-token".into(),
        token_path,
        runtime_dir: root,
        conn: None,
        next_id: 1,
        timeout: Duration::from_millis(500),
    };
    (
        OwnedHost(NativeHost {
            running: Some(running),
        }),
        server,
    )
}

#[test]
fn actual_read_socket_snapshot_and_validation_use_closed_methods_and_native_u64() {
    let (wire, i) = fixture();
    let proof = validation(&snapshot());
    let (mut host, server) = socket_host(vec![
        ("seecut.snapshot", WireReply::Ok(wire)),
        ("seecut.snapshot.validate", WireReply::Ok(proof)),
    ]);
    let s = host
        .0
        .photo_snapshot_after_gate(i.host_instance_id, "capture-1", i.clone(), 256)
        .unwrap();
    let validation = host
        .0
        .validate_photo_snapshot_after_gate(i.host_instance_id, "validate-1", &s)
        .unwrap();
    assert_eq!(validation.identity(), &i);
    assert_eq!(server.join().unwrap(), 2);
}

#[test]
fn actual_read_errors_remain_read_only_and_are_never_resent() {
    for reply in [
        WireReply::Reject,
        WireReply::WrongId,
        WireReply::Disconnect,
        WireReply::Timeout,
    ] {
        let (_, i) = fixture();
        let (mut host, server) = socket_host(vec![("seecut.snapshot", reply)]);
        let result = host
            .0
            .photo_snapshot_after_gate(i.host_instance_id, "capture-1", i, 256);
        assert!(
            matches!(
                result,
                Err(HostError::ReadFailed(_) | HostError::RemoteRejected)
            ),
            "read must never become write Unknown"
        );
        assert_eq!(
            server.join().unwrap(),
            1,
            "exactly one dispatch and no automatic resend"
        );
    }
}

#[test]
fn actual_validation_expiry_never_falls_back_to_new_snapshot() {
    let s = snapshot();
    let id = s.identity.host_instance_id;
    let (mut host, server) = socket_host(vec![("seecut.snapshot.validate", WireReply::Reject)]);
    assert!(matches!(
        host.0
            .validate_photo_snapshot_after_gate(id, "validate-1", &s),
        Err(HostError::RemoteRejected)
    ));
    assert_eq!(server.join().unwrap(), 1);
}

#[test]
fn locally_expired_snapshot_sends_no_auth_or_validation_request() {
    let mut s = snapshot();
    s.received_at = Instant::now() - Duration::from_secs(5);
    s.request_started = s.received_at - Duration::from_millis(10);
    let id = s.identity.host_instance_id;
    let (mut host, server) = socket_host(Vec::new());
    assert!(matches!(
        host.0
            .validate_photo_snapshot_after_gate(id, "validate-1", &s),
        Err(HostError::ReadFailed(_))
    ));
    assert_eq!(server.join().unwrap(), 0);
}
