use super::*;
use serde_json::{Value, json};

fn action() -> Value {
    json!({"kind":"exportVideo","name":"成片","codec":"h264","shortSide":720,"fps":30,"quality":"balanced","tenBit":false})
}
fn expected() -> Value {
    json!({"width":1280,"height":720,"rateNum":30,"rateDen":1,"frames":30,"codec":"h264","tenBit":false,"audioExpected":false})
}
fn summary() -> Value {
    json!({"byteLength":1234,"sha256":"a".repeat(64),"codec":"h264","bitDepth":8,"width":1280,"height":720,"rateNum":30,"rateDen":1,"videoFrames":30,"firstVideoPositionSeconds":0.0,"lastVideoPositionSeconds":29.0/30.0,"videoSeconds":1.0,"audioStreams":[]})
}
const OUTPUT: &str = "12345678-1234-4234-8234-123456789abc";
const REGISTRATION: &str = "23456789-2345-4345-8345-23456789abcd";
fn asset() -> Value {
    json!({"assetId":"fixed-asset","version":{"sha256":"a".repeat(64),"registration_id":format!("operation:{REGISTRATION}")}})
}
fn verified() -> Value {
    json!({"version":1,"state":"verified","inputManifestSha256":"b".repeat(64),"expected":expected(),"summary":summary(),"durability":"confirmed","asset":asset()})
}
fn data(proof: Value) -> Value {
    json!({"appInstanceId":"app","clientId":"client","projectId":"project","documentSessionId":"session","operationId":crate::operation_id("client","session",7).unwrap(),"clientSequence":7,"tool":"clip_edit","edit":action(),"generation":1,"sourceRevision":2,"sourceSelectionRevision":3,"sourceTimelineId":"timeline:original","sourceEditStateId":format!("clip-edit-v1-{}","c".repeat(64)),"sourceDocumentStateId":format!("clip-v1-{}","d".repeat(64)),"outputId":OUTPUT,"registrationId":REGISTRATION,"exportProof":proof})
}
fn query(method: &str) -> Value {
    json!({"instanceId":"app","clientId":"client","clientToken":"credential","method":method,"projectId":"project","documentSessionId":"session","operationQuery":{"operationId":crate::operation_id("client","session",7).unwrap(),"clientSequence":7}})
}
fn receipt_ok(value: Value, status: &str) -> bool {
    serde_json::from_value::<ClipExportData>(value)
        .is_ok_and(|r| r.validate_for_status(status).is_ok())
}

#[test]
fn export_action_requires_all_fields_and_closed_values_without_paths() {
    let edit: ClipEdit = serde_json::from_value(action()).unwrap();
    assert!(edit.validate().is_ok());
    for key in ["name", "codec", "shortSide", "fps", "quality", "tenBit"] {
        let mut value = action();
        value.as_object_mut().unwrap().remove(key);
        assert!(
            serde_json::from_value::<ClipEdit>(value).is_err(),
            "missing {key}"
        );
    }
    for (key, bad) in [
        ("name", json!(" leading")),
        ("name", json!("trailing ")),
        ("name", json!("")),
        ("name", json!("a/b")),
        ("name", json!("a\\b")),
        ("name", json!("a\nline")),
        ("name", json!("a\u{0}b")),
        ("name", json!("汉".repeat(86))),
        ("shortSide", json!(721)),
        ("fps", json!(29)),
        ("codec", json!("vp9")),
        ("quality", json!("veryfast")),
        ("tenBit", json!("false")),
    ] {
        let mut value = action();
        value[key] = bad;
        assert!(
            match serde_json::from_value::<ClipEdit>(value) {
                Err(_) => true,
                Ok(e) => e.validate().is_err(),
            },
            "{key}"
        );
    }
    for key in [
        "path",
        "outputId",
        "asset",
        "project",
        "filter",
        "exportProof",
    ] {
        let mut value = action();
        value[key] = json!("forged");
        assert!(serde_json::from_value::<ClipEdit>(value).is_err());
    }
    for codec in ["h264", "hevc", "av1"] {
        for side in [720, 1080, 1440, 2160] {
            for fps in [24, 30, 60] {
                for quality in ["high", "balanced", "small"] {
                    let mut value = action();
                    value["codec"] = json!(codec);
                    value["shortSide"] = json!(side);
                    value["fps"] = json!(fps);
                    value["quality"] = json!(quality);
                    assert!(
                        serde_json::from_value::<ClipEdit>(value)
                            .unwrap()
                            .validate()
                            .is_ok()
                    );
                }
            }
        }
    }
    let save: ClipEdit = serde_json::from_value(json!({"kind":"save"})).unwrap();
    assert!(save.validate().is_ok());
    assert!(serde_json::from_value::<ClipEdit>(json!({"kind":"save","codec":"h264"})).is_err());
}

#[test]
fn both_native_capability_fields_are_required_and_quality_mapping_stays_native() {
    assert!(capabilities_supported(
        &json!({"clipExportProtocolVersion":1,"clipExportOutputProtocolVersion":1})
    ));
    for value in [
        json!({}),
        json!({"clipExportProtocolVersion":1}),
        json!({"clipExportOutputProtocolVersion":1}),
        json!({"clipExportProtocolVersion":1,"clipExportOutputProtocolVersion":2}),
        json!({"clipExportProtocolVersion":"1","clipExportOutputProtocolVersion":1}),
    ] {
        assert!(!capabilities_supported(&value));
    }
    assert_eq!(
        (
            ClipExportQuality::High.crf(),
            ClipExportQuality::Balanced.crf(),
            ClipExportQuality::Small.crf()
        ),
        (16, 20, 26)
    );
}

#[test]
fn control_and_output_queries_are_original_only_and_never_alias_edit_or_save() {
    let output: Request = serde_json::from_value(query("clipExportOutput")).unwrap();
    assert!(crate::validate_request_shape(&output).is_ok());
    assert!(!output.method.is_write());
    let mut control = query("clipExportControl");
    control["clipExportControlAction"] = json!("retryRegistration");
    let r: Request = serde_json::from_value(control.clone()).unwrap();
    assert!(crate::validate_request_shape(&r).is_ok());
    assert!(r.method.is_write());
    for action in ["cancel", "retryRegistration"] {
        control["clipExportControlAction"] = json!(action);
        assert!(
            crate::validate_request_shape(&serde_json::from_value(control.clone()).unwrap())
                .is_ok()
        );
    }
    assert!(
        crate::validate_request_shape(&serde_json::from_value(query("clipExportControl")).unwrap())
            .is_err()
    );
    for method in [
        "clipExportOutput",
        "clipOperation",
        "project",
        "clipEdit",
        "capabilities",
    ] {
        let mut bad = control.clone();
        bad["method"] = json!(method);
        assert!(crate::validate_request_shape(&serde_json::from_value(bad).unwrap()).is_err());
    }
    for key in ["revision", "contextRevision", "offset", "limit", "maxEdge"] {
        let mut bad = query("clipExportOutput");
        bad[key] = json!(1);
        assert!(crate::validate_request_shape(&serde_json::from_value(bad).unwrap()).is_err());
    }
    for key in ["path", "assetId", "version", "sha256", "expected", "action"] {
        let mut bad = query("clipExportOutput");
        bad[key] = json!("forged");
        assert!(serde_json::from_value::<Request>(bad).is_err());
    }
    for sequence in [0, SAFE + 1] {
        let mut bad = query("clipExportOutput");
        bad["operationQuery"]["clientSequence"] = json!(sequence);
        assert!(crate::validate_request_shape(&serde_json::from_value(bad).unwrap()).is_err());
    }
    let mut wrong = query("clipExportOutput");
    wrong["documentSessionId"] = json!("other");
    assert!(crate::validate_request_shape(&serde_json::from_value(wrong).unwrap()).is_err());
    let mut extra = query("clipExportOutput");
    extra["operationQuery"]["action"] = json!("cancel");
    assert!(serde_json::from_value::<Request>(extra).is_err());
}

#[test]
fn proof_union_preserves_publication_status_and_rejects_forged_fields() {
    assert!(receipt_ok(data(verified()), "ok"));
    let pending =
        json!({"version":1,"state":"pending","phase":"preparing","framesDone":0,"framesTotal":0});
    assert!(receipt_ok(data(pending.clone()), "pending"));
    let mut bad = pending;
    bad["framesDone"] = json!(1);
    assert!(!receipt_ok(data(bad), "pending"));
    let not = json!({"version":1,"state":"notPublished","publishClaimed":true,"publishAttempted":false,"reason":"leaseExpired"});
    assert!(receipt_ok(data(not.clone()), "exportNotPublished"));
    let mut bad = not;
    bad["publishAttempted"] = json!(true);
    assert!(!receipt_ok(data(bad), "exportNotPublished"));
    for publication in ["confirmed", "unconfirmed"] {
        let proof = json!({"version":1,"state":"outcomeUnknown","publishClaimed":true,"publication":publication,"stage":"completion"});
        assert!(receipt_ok(data(proof), "outcomeUnknown"));
    }
    for status in ["pending", "partial", "outcomeUnknown", "exportNotPublished"] {
        assert!(!receipt_ok(data(verified()), status));
    }
    let mut partial = verified();
    partial["state"] = json!("registrationPending");
    partial.as_object_mut().unwrap().remove("asset");
    assert!(receipt_ok(data(partial), "partial"));
    for key in ["path", "saveProof", "persistedRevision"] {
        let mut bad = data(verified());
        bad[key] = json!("forged");
        assert!(serde_json::from_value::<ClipExportData>(bad).is_err());
    }
    for key in ["path", "checked", "nativeVerified"] {
        let mut bad = verified();
        bad[key] = json!(true);
        assert!(serde_json::from_value::<ClipExportProof>(bad).is_err());
    }
}

#[test]
fn immutable_source_expected_summary_and_asset_are_cross_checked() {
    for (key, bad) in [
        ("outputId", json!("path")),
        ("registrationId", json!(REGISTRATION.to_uppercase())),
        ("clientSequence", json!(SAFE + 1)),
        ("sourceRevision", json!(SAFE + 1)),
        ("sourceDocumentStateId", json!("model")),
        ("tool", json!("clip_export_output")),
        ("operationId", json!("other")),
    ] {
        let mut v = data(verified());
        v[key] = bad;
        assert!(!receipt_ok(v, "ok"), "{key}");
    }
    for (key, bad) in [
        ("bitDepth", json!(10)),
        ("width", json!(1278)),
        ("videoFrames", json!(29)),
        ("rateNum", json!(25)),
        ("byteLength", json!(0)),
        ("sha256", json!("A".repeat(64))),
        ("firstVideoPositionSeconds", json!(2.0)),
        ("lastVideoPositionSeconds", json!(400.0)),
        ("videoSeconds", json!(2.0)),
    ] {
        let mut v = verified();
        v["summary"][key] = bad;
        assert!(!receipt_ok(data(v), "ok"), "{key}");
    }
    let mut v = verified();
    v["asset"]["version"]["registration_id"] = json!("operation:other");
    assert!(!receipt_ok(data(v), "ok"));
    let mut v = verified();
    v["expected"]["width"] = json!(1282);
    assert!(!receipt_ok(data(v), "ok"));
    let mut v = data(verified());
    v["edit"]["fps"] = json!(24);
    assert!(!receipt_ok(v, "ok"));
    let mut v = data(verified());
    v["edit"]["shortSide"] = json!(1080);
    assert!(!receipt_ok(v, "ok"));
}

#[test]
fn normalized_audio_is_bounded_finite_and_linked_to_frozen_duration() {
    let mut proof = verified();
    proof["expected"]["audioExpected"] = json!(true);
    let audio = json!({"index":1,"codec":"aac","sourceSampleRate":48000,"sourceChannels":2,"sampleFrames":48000,"seconds":1.0,"peak":0.0,"rms":0.0});
    proof["summary"]["audioStreams"] = json!([audio]);
    assert!(
        receipt_ok(data(proof.clone()), "ok"),
        "normal silence is valid"
    );
    for (key, bad) in [
        ("sampleFrames", json!(96000)),
        ("seconds", json!(2.0)),
        ("sourceChannels", json!(0)),
        ("peak", json!(-1.0)),
        ("rms", json!(1.0)),
    ] {
        let mut v = proof.clone();
        v["summary"]["audioStreams"][0][key] = bad;
        assert!(!receipt_ok(data(v), "ok"), "{key}");
    }
    let mut v = proof.clone();
    let audio = v["summary"]["audioStreams"][0].clone();
    v["summary"]["audioStreams"] = json!(vec![audio; 9]);
    assert!(!receipt_ok(data(v), "ok"));
    let mut summary: ClipExportSummary = serde_json::from_value(proof["summary"].clone()).unwrap();
    summary.audio_streams[0].peak = f64::NAN;
    assert!(summary.validate_shape().is_err());
}

#[test]
fn control_outer_success_preserves_and_binds_the_complete_original_response() {
    let original = data(verified());
    let control = json!({"clipExportControlProtocolVersion":1,"action":"cancel","operationId":original["operationId"],"clientSequence":7,"result":"alreadyTerminal","original":{"status":"ok","data":original}});
    let parsed: ClipExportControlData = serde_json::from_value(control.clone()).unwrap();
    assert!(parsed.validate().is_ok());
    let mut request = query("clipExportControl");
    request["clipExportControlAction"] = json!("cancel");
    let request: Request = serde_json::from_value(request).unwrap();
    assert!(parsed.validate_for_request(&request).is_ok());
    let mut bad = control.clone();
    bad["original"]["newSource"] = json!(true);
    assert!(serde_json::from_value::<ClipExportControlData>(bad).is_err());
    for (key, bad) in [
        ("clientSequence", json!(8)),
        ("action", json!("retryRegistration")),
        ("result", json!("registrationAttempted")),
    ] {
        let mut v = control.clone();
        v[key] = bad;
        assert!(
            serde_json::from_value::<ClipExportControlData>(v)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut scope = request;
    scope.project_id = Some("other".into());
    assert!(parsed.validate_for_request(&scope).is_err());
}

#[test]
fn successful_fresh_output_must_exactly_match_original_fixed_proof() {
    let original: ClipExportData = serde_json::from_value(data(verified())).unwrap();
    let mut output = data(verified());
    for key in [
        "tool",
        "edit",
        "generation",
        "sourceRevision",
        "sourceSelectionRevision",
        "sourceTimelineId",
        "sourceEditStateId",
        "sourceDocumentStateId",
        "exportProof",
    ] {
        output.as_object_mut().unwrap().remove(key);
    }
    output["clipExportOutputProtocolVersion"] = json!(1);
    output["source"] = json!("freshExport");
    output["inputManifestSha256"] = json!("b".repeat(64));
    output["asset"] = asset();
    output["summary"] = summary();
    output["checkedAtUnixMs"] = json!(1000);
    let read: ClipExportOutputData = serde_json::from_value(output.clone()).unwrap();
    assert!(read.matches_original(&original).is_ok());
    let mut changed = read.clone();
    changed.summary.sha256 = "f".repeat(64);
    assert!(changed.matches_original(&original).is_err());
    let mut changed = read.clone();
    changed.output_id = REGISTRATION.into();
    assert!(changed.matches_original(&original).is_err());
    let mut changed = read;
    changed.input_manifest_sha256 = "e".repeat(64);
    assert!(changed.matches_original(&original).is_err());
    for (key, bad) in [
        ("path", json!("/private")),
        ("source", json!("cachedReceipt")),
        ("clipExportOutputProtocolVersion", json!(2)),
        ("checkedAtUnixMs", json!(SAFE + 1)),
    ] {
        let mut v = output.clone();
        v[key] = bad;
        assert!(match serde_json::from_value::<ClipExportOutputData>(v) {
            Err(_) => true,
            Ok(r) => r.validate().is_err(),
        });
    }
}

#[test]
fn rust_serde_generates_validated_mock_wire_fixture_bundle() {
    let mut records: Vec<(&str, Response)> = Vec::new();
    let mut registration = verified();
    registration["state"] = json!("registrationPending");
    registration.as_object_mut().unwrap().remove("asset");
    let proofs = [
        (
            "pending-preparing",
            "pending",
            json!({"version":1,"state":"pending","phase":"preparing","framesDone":0,"framesTotal":0}),
        ),
        (
            "pending-rendering",
            "pending",
            json!({"version":1,"state":"pending","phase":"rendering","framesDone":5,"framesTotal":30}),
        ),
        (
            "not-published-after-claim",
            "exportNotPublished",
            json!({"version":1,"state":"notPublished","publishClaimed":true,"publishAttempted":false,"reason":"leaseExpired"}),
        ),
        ("registration-pending", "partial", registration.clone()),
        ("verified", "ok", verified()),
        (
            "outcome-unconfirmed",
            "outcomeUnknown",
            json!({"version":1,"state":"outcomeUnknown","publishClaimed":true,"publication":"unconfirmed","stage":"publish"}),
        ),
        (
            "outcome-confirmed",
            "outcomeUnknown",
            json!({"version":1,"state":"outcomeUnknown","publishClaimed":true,"publication":"confirmed","stage":"registration"}),
        ),
    ];
    for (name, status, proof) in proofs {
        let typed: ClipExportData = serde_json::from_value(data(proof)).unwrap();
        typed.validate_for_status(status).unwrap();
        let response = Response {
            status: status.into(),
            data: Some(serde_json::to_value(&typed).unwrap()),
        };
        let roundtrip: ClipExportData =
            serde_json::from_value(response.data.clone().unwrap()).unwrap();
        roundtrip.validate_for_status(&response.status).unwrap();
        records.push((name, response));
    }
    for (name, action, result, index) in [
        (
            "control-cancel-accepted",
            ClipExportControlAction::Cancel,
            ClipExportControlResult::CancelAccepted,
            0,
        ),
        (
            "control-already-publishing",
            ClipExportControlAction::Cancel,
            ClipExportControlResult::AlreadyPublishing,
            1,
        ),
        (
            "control-already-terminal",
            ClipExportControlAction::Cancel,
            ClipExportControlResult::AlreadyTerminal,
            4,
        ),
        (
            "control-registration-attempted",
            ClipExportControlAction::RetryRegistration,
            ClipExportControlResult::RegistrationAttempted,
            3,
        ),
    ] {
        let mut original = records[index].1.clone();
        if result == ClipExportControlResult::AlreadyPublishing {
            let mut typed_original: ClipExportData =
                serde_json::from_value(original.data.clone().unwrap()).unwrap();
            typed_original.export_proof = ClipExportProof::Pending {
                version: 1,
                phase: ClipExportPhase::Publishing,
                frames_done: 30,
                frames_total: 30,
            };
            typed_original.validate_for_status("pending").unwrap();
            original.data = Some(serde_json::to_value(typed_original).unwrap());
        }
        let typed = ClipExportControlData {
            clip_export_control_protocol_version: 1,
            action,
            operation_id: crate::operation_id("client", "session", 7).unwrap(),
            client_sequence: 7,
            result,
            original,
        };
        typed.validate().unwrap();
        let value = serde_json::to_value(&typed).unwrap();
        serde_json::from_value::<ClipExportControlData>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        records.push((name, Response::ok(value)));
    }
    let mut output = data(verified());
    for key in [
        "tool",
        "edit",
        "generation",
        "sourceRevision",
        "sourceSelectionRevision",
        "sourceTimelineId",
        "sourceEditStateId",
        "sourceDocumentStateId",
        "exportProof",
    ] {
        output.as_object_mut().unwrap().remove(key);
    }
    output["clipExportOutputProtocolVersion"] = json!(1);
    output["source"] = json!("freshExport");
    output["inputManifestSha256"] = json!("b".repeat(64));
    output["asset"] = asset();
    output["summary"] = summary();
    output["checkedAtUnixMs"] = json!(1000);
    let typed: ClipExportOutputData = serde_json::from_value(output).unwrap();
    typed
        .matches_original(&serde_json::from_value(data(verified())).unwrap())
        .unwrap();
    let value = serde_json::to_value(&typed).unwrap();
    serde_json::from_value::<ClipExportOutputData>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    records.push(("output-fresh", Response::ok(value)));
    // This is a synthetic schema fixture, never evidence of native rendering,
    // publication or IO. The Response payload itself retains its closed shape.
    let bundle = json!({"fixtureSetVersion":1,"mock":true,"description":"Synthetic fixed app/client/project/session, UUIDs and digests; Rust serde shape/cross-validation only; no real file or native operation.",
        "examples":records.into_iter().map(|(name,response)|json!({"name":name,"response":response})).collect::<Vec<_>>()});
    assert_eq!(bundle["examples"].as_array().unwrap().len(), 12);
    if let Some(directory) = std::env::var_os("CONCAT_CLIP_EXPORT_WIRE_FIXTURE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        assert!(
            directory.is_absolute(),
            "fixture output directory must be absolute; Cargo tests run from their package directory"
        );
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("clip-export-v1-mock.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
        eprintln!("CLIP_EXPORT_RUST_SERDE_FIXTURE {}", path.display());
    }
}

#[cfg(feature = "stdio")]
#[test]
fn export_schema_has_no_optional_defaults_and_closed_numeric_choices() {
    fn find(value: &Value) -> Option<&Value> {
        if value["properties"]["kind"]["const"] == "exportVideo" {
            return Some(value);
        }
        match value {
            Value::Object(object) => object.values().find_map(find),
            Value::Array(array) => array.iter().find_map(find),
            _ => None,
        }
    }
    let schema = serde_json::to_value(schemars::schema_for!(ClipEdit)).unwrap();
    let export = find(&schema).expect("typed exportVideo schema variant");
    assert_eq!(export["additionalProperties"], false);
    let required = export["required"].as_array().unwrap();
    assert_eq!(required.len(), 7);
    for name in [
        "kind",
        "name",
        "codec",
        "shortSide",
        "fps",
        "quality",
        "tenBit",
    ] {
        assert!(required.contains(&json!(name)));
    }
    assert_eq!(
        export["properties"]["shortSide"]["enum"],
        json!([720, 1080, 1440, 2160])
    );
    assert_eq!(export["properties"]["fps"]["enum"], json!([24, 30, 60]));
    assert_eq!(export["properties"]["tenBit"]["type"], "boolean");
}

#[test]
fn odd_dimensions_cannot_describe_a_native_normalized_output() {
    for (width, height) in [(1281, 720), (720, 1081)] {
        let mut facts = expected();
        facts["width"] = json!(width);
        facts["height"] = json!(height);
        let typed: ClipExportExpected = serde_json::from_value(facts.clone()).unwrap();
        assert!(typed.validate().is_err());
        let mut actual = summary();
        actual["width"] = json!(width);
        actual["height"] = json!(height);
        let typed: ClipExportSummary = serde_json::from_value(actual.clone()).unwrap();
        assert!(typed.validate_shape().is_err());
        let mut proof = verified();
        proof["expected"] = facts;
        proof["summary"] = actual;
        assert!(!receipt_ok(data(proof), "ok"));
    }
}

#[test]
fn cancel_accepted_refuses_known_claimed_or_published_originals_but_progress_may_lag() {
    let mut partial = verified();
    partial["state"] = json!("registrationPending");
    partial.as_object_mut().unwrap().remove("asset");
    let originals = [
        ("ok", verified()),
        ("partial", partial),
        (
            "exportNotPublished",
            json!({"version":1,"state":"notPublished","publishClaimed":true,"publishAttempted":false,"reason":"leaseExpired"}),
        ),
        (
            "outcomeUnknown",
            json!({"version":1,"state":"outcomeUnknown","publishClaimed":true,"publication":"unconfirmed","stage":"publish"}),
        ),
    ];
    for (status, proof) in originals {
        let original: ClipExportData = serde_json::from_value(data(proof)).unwrap();
        original.validate_for_status(status).unwrap();
        let control = ClipExportControlData {
            clip_export_control_protocol_version: 1,
            action: ClipExportControlAction::Cancel,
            operation_id: original.operation_id.clone(),
            client_sequence: 7,
            result: ClipExportControlResult::CancelAccepted,
            original: Response {
                status: status.into(),
                data: Some(serde_json::to_value(original).unwrap()),
            },
        };
        assert_eq!(control.validate(), Err("invalidClipExportCancellation"));
    }
    // Publication can have won after the latest rendering progress snapshot.
    let pending = data(
        json!({"version":1,"state":"pending","phase":"rendering","framesDone":5,"framesTotal":30}),
    );
    let control = ClipExportControlData {
        clip_export_control_protocol_version: 1,
        action: ClipExportControlAction::Cancel,
        operation_id: crate::operation_id("client", "session", 7).unwrap(),
        client_sequence: 7,
        result: ClipExportControlResult::AlreadyPublishing,
        original: Response {
            status: "pending".into(),
            data: Some(pending),
        },
    };
    assert!(control.validate().is_ok());
}
