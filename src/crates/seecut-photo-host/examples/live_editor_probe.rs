//! Candidate for an explicitly isolated Linux Xvfb run. This is a direct host probe,
//! not a model invocation or the parent SeeCut MCP/Task integration acceptance.
use seecut_photo_host::{
    CloseOutcome, HostError, InstanceStatus, LaunchOptions, NativeHost, Uuid, VerifiedBuild,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Failure {
    stage: &'static str,
    class: &'static str,
}
type ProbeResult<T> = Result<T, Failure>;
fn require(ok: bool, stage: &'static str) -> ProbeResult<()> {
    if ok {
        Ok(())
    } else {
        Err(Failure {
            stage,
            class: "assertionFailed",
        })
    }
}
fn io<T>(result: std::io::Result<T>, stage: &'static str) -> ProbeResult<T> {
    result.map_err(|_| Failure {
        stage,
        class: "localIoFailed",
    })
}
fn host_class(error: &HostError) -> &'static str {
    match error {
        HostError::Unknown(_) => "unknownCompletionNeverResend",
        HostError::InvalidBuild(_) => "invalidBuildReceipt",
        HostError::StaleInstance => "staleInstance",
        HostError::NotReady => "notReady",
        HostError::NotSent(_) => "notSent",
        HostError::ReadFailed(_) => "readFailed",
        _ => "hostFailure",
    }
}
fn decode(path: &Path, stage: &'static str) -> ProbeResult<Value> {
    let bytes = io(fs::read(path), stage)?;
    require(bytes.len() <= 8 << 20, stage)?;
    serde_json::from_slice(&bytes).map_err(|_| Failure {
        stage,
        class: "invalidJson",
    })
}
fn private_new_dir(path: &Path) -> ProbeResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        io(
            fs::DirBuilder::new().mode(0o700).create(path),
            "createFreshPrivateDirectory",
        )?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(Failure {
            stage: "platform",
            class: "linuxOnly",
        })
    }
}
fn number(value: &Value, pointer: &str) -> ProbeResult<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or(Failure {
            stage: "stateShape",
            class: "missingInteger",
        })
}
fn history(state: &Value) -> ProbeResult<u64> {
    number(state, "/document/historyEntries")
}
fn identity(state: &Value) -> ProbeResult<Value> {
    let value = state
        .pointer("/document/identity")
        .cloned()
        .ok_or(Failure {
            stage: "documentIdentity",
            class: "noDocument",
        })?;
    require(
        number(&value, "/docId")? > 0
            && number(&value, "/documentGeneration")? > 0
            && number(&value, "/revision")? > 0,
        "documentIdentity",
    )?;
    Ok(value)
}
fn opacity(state: &Value, layer: u64) -> ProbeResult<f64> {
    state
        .pointer("/document/layers")
        .and_then(Value::as_array)
        .and_then(|layers| {
            layers
                .iter()
                .find(|item| item["id"].as_u64() == Some(layer))
        })
        .and_then(|item| item["opacity"].as_f64())
        .ok_or(Failure {
            stage: "layerState",
            class: "missingTargetLayer",
        })
}
fn same_document(a: &Value, b: &Value) -> bool {
    a.get("document") == b.get("document")
}
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.000_001
}

struct Probe {
    primary: NativeHost,
    contender: NativeHost,
    instance: Option<Uuid>,
    deadline: Instant,
    observations: Vec<Value>,
}
impl Probe {
    fn new() -> Self {
        Self {
            primary: NativeHost::new(),
            contender: NativeHost::new(),
            instance: None,
            deadline: Instant::now() + Duration::from_secs(180),
            observations: Vec::new(),
        }
    }
    fn check_time(&self) -> ProbeResult<()> {
        require(Instant::now() < self.deadline, "absoluteProbeDeadline")
    }
    fn call(&mut self, method: &str, params: Value, stage: &'static str) -> ProbeResult<Value> {
        self.check_time()?;
        let id = self.instance.ok_or(Failure {
            stage,
            class: "noInstance",
        })?;
        self.primary
            .call_internal_after_gate(id, method, params)
            .map_err(|error| Failure {
                stage,
                class: host_class(&error),
            })
    }
    fn engine(&mut self, command: &str, params: Value, stage: &'static str) -> ProbeResult<Value> {
        self.call(
            "engine.execute",
            json!({"command":command,"params":params}),
            stage,
        )
    }
    fn state(&mut self) -> ProbeResult<Value> {
        let state = self.call("seecut.state", json!({}), "readBoundState")?;
        require(
            state["hostInstanceId"] == self.instance.unwrap().to_string(),
            "boundHostIdentity",
        )?;
        require(
            state["kind"] == "uiCallbackMetadata" && state["atomicPixels"] == false,
            "metadataContract",
        )?;
        Ok(state)
    }
    fn idle_state(&mut self) -> ProbeResult<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = self.state()?;
            if state["busy"].is_null() {
                return Ok(state);
            }
            require(Instant::now() < deadline, "editorIdleWithinFiveSeconds")?;
            std::thread::sleep(Duration::from_millis(30)); // Only known reads are repeated.
        }
    }
    fn inspect(&mut self) -> ProbeResult<Value> {
        self.primary
            .inspect(self.instance.unwrap())
            .map_err(|error| Failure {
                stage: "inspect",
                class: host_class(&error),
            })
    }
    fn note(&mut self, name: &'static str, value: Value) {
        self.observations
            .push(json!({"check":name,"evidence":value}));
    }
    fn export_copy(&mut self, write_root: &Path, filename: &str) -> ProbeResult<()> {
        let before = self.state()?;
        let target = write_root.join(filename);
        require(!target.exists(), "newExportDestination")?;
        self.engine(
            "file.saveACopy",
            json!({"path":target,"layers":false,"overwrite":false}),
            "exportSyntheticPngCopy",
        )?;
        let after = self.state()?;
        require(
            same_document(&before, &after),
            "exportDoesNotChangeDocumentOrHistory",
        )?;
        require(
            io(fs::metadata(target), "exportExists")?.is_file(),
            "exportRegularFile",
        )
    }
    fn discard_active_synthetic(
        &mut self,
        expected_doc: u64,
        command: &'static str,
    ) -> ProbeResult<()> {
        let state = self.state()?;
        require(
            number(&state, "/document/identity/docId")? == expected_doc,
            "discardOnlyOwnedSyntheticDocument",
        )?;
        let inspected = self.inspect()?;
        let prompt = inspected["dialogs"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == "discard"));
        require(
            prompt.is_some_and(|prompt| prompt["fields"]["command"] == command),
            "expectedRealDiscardPrompt",
        )?;
        self.call(
            "ui.dialog.confirm",
            json!({"dialog":"discard","choice":"dontSave"}),
            "discardOnlyThisSyntheticDocument",
        )?;
        Ok(())
    }
    fn await_exit(host: &mut NativeHost, wait: Duration) -> ProbeResult<()> {
        let deadline = Instant::now() + wait;
        loop {
            let state = host.current().map_err(|_| Failure {
                stage: "observeExit",
                class: "hostStatusFailed",
            })?;
            if state.is_some_and(|info| info.status == InstanceStatus::Exited) {
                return Ok(());
            }
            require(Instant::now() < deadline, "observeActualExit")?;
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    // Only called for this run's freshly created, empty, explicitly disposable profile.
    // NativeHost accepts an owned instance UUID; no outside PID or profile can be supplied.
    fn cleanup_disposable_owned_children(&mut self) -> bool {
        let mut ok = true;
        for host in [&mut self.contender, &mut self.primary] {
            match host.current() {
                Ok(Some(info)) if info.status != InstanceStatus::Exited => {
                    if host.terminate_confirmed(info.id).is_err() {
                        ok = false;
                    }
                }
                Err(_) => ok = false,
                _ => {}
            }
        }
        ok
    }
    fn run(&mut self, provenance: &Path, root: &Path, commands: &Value) -> ProbeResult<()> {
        VerifiedBuild::from_provenance(provenance).map_err(|_| Failure {
            stage: "buildIdentity",
            class: "invalidProvenanceOrBinaryHash",
        })?;
        for name in [
            "file.new",
            "layer.new.layer",
            "edit.fill",
            "file.saveACopy",
            "layer.setProps",
            "edit.undo",
        ] {
            require(
                commands
                    .as_array()
                    .is_some_and(|items| items.iter().any(|entry| entry["id"] == name)),
                "compiledCommandDiscovery",
            )?;
        }
        for name in ["read", "write", "state"] {
            private_new_dir(&root.join(name))?;
        }
        let mut options = LaunchOptions::new(provenance.to_path_buf(), root.join("state"));
        options.read_root = Some(root.join("read"));
        options.write_root = Some(root.join("write"));
        options.startup_timeout = Duration::from_secs(60);
        options.request_timeout = Duration::from_secs(10);
        let info = self
            .primary
            .start(options.clone())
            .map_err(|error| Failure {
                stage: "startActualDesktop",
                class: host_class(&error),
            })?;
        self.instance = Some(info.id);
        let initial = self.inspect()?;
        require(
            initial["session"]["documents"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "freshProfileHasNoDocuments",
        )?;
        self.note("authenticatedActualDesktopReady", json!({"instanceId":info.id.to_string(),"buildAttempt":info.build_attempt,"emptySession":true}));

        let lock_path = root.join("state/photocraft-profile/seecut-owned-editor.lock");
        let lock = io(File::open(&lock_path), "openOwnProfileLock")?;
        require(
            matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "desktopOwnsProfileOsLock",
        )?;
        let other = self.contender.start(options);
        require(other.is_err(), "sameProfileSecondHostRefused")?;
        Self::await_exit(&mut self.contender, Duration::from_secs(3))?;
        require(
            self.primary
                .current()
                .map_err(|_| Failure {
                    stage: "firstHostStillAlive",
                    class: "statusFailed",
                })?
                .is_some_and(|state| state.ready && state.id == info.id),
            "firstHostStillAlive",
        )?;
        require(
            matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "firstHostRetainsProfileLock",
        )?;
        self.state()?;
        self.note(
            "sameProfileContender",
            json!({"refusedBeforeReady":true,"contenderExited":true,"firstStillResponds":true,
            "osLockStillBusy":true,"rawStderrReadback":false,"exitCodeNotExposedByHostApi":true}),
        );

        self.engine("file.new", json!({"width":16,"height":16,"mode":"rgb","depth":8,"profile":"none","background":"transparent","name":"SeeCut disposable orange A"}), "createSyntheticRgb8")?;
        let layer = self.engine(
            "layer.new.layer",
            json!({"name":"SeeCut disposable orange pixels","opacity":1.0,"mode":"Normal"}),
            "createOrdinaryPixelLayer",
        )?;
        let layer_id = number(&layer, "/layer")?;
        self.engine(
            "edit.fill",
            json!({"contents":"color","color":"#ff8000","opacity":100,"target":"pixels"}),
            "fillSyntheticOrangePixels",
        )?;
        let inspected = self.inspect()?;
        require(
            inspected["document"]["mode"] == "Rgb" && inspected["document"]["depth"] == 8,
            "rgb8Inspect",
        )?;
        require(
            inspected["document"]["layers"]
                .as_array()
                .is_some_and(|layers| {
                    layers.iter().any(|layer| {
                        layer["id"].as_u64() == Some(layer_id) && layer["kind"] == "Pixel"
                    })
                }),
            "realPixelLayerInspect",
        )?;
        self.export_copy(&root.join("write"), "orange-full.png")?;
        let before = self.idle_state()?;
        let expected = identity(&before)?;
        let doc_a = number(&expected, "/docId")?;
        let original = json!({"operationId":"probe-opacity-once","expected":expected,
            "action":{"kind":"layerSetOpacity","layerId":layer_id,"opacity":0.4}});
        let receipt = self.call("seecut.execute", original.clone(), "applyOpacityOnce")?;
        require(
            receipt["status"] == "applied" && receipt["safeToResend"] == false,
            "appliedReceipt",
        )?;
        require(
            receipt["before"]["identity"] == before["document"]["identity"],
            "receiptBeforeIdentity",
        )?;
        let after = self.state()?;
        require(
            near(opacity(&after, layer_id)?, 0.4) && history(&after)? == history(&before)? + 1,
            "opacityIsOneRealHistoryStep",
        )?;
        require(
            receipt["after"]["identity"] == after["document"]["identity"],
            "receiptAfterIdentity",
        )?;
        let duplicate = self.call(
            "seecut.execute",
            original.clone(),
            "explicitExactDuplicateContractTest",
        )?;
        require(duplicate == receipt, "sameRequestReturnsOriginalReceipt")?;
        let queried = self.call(
            "seecut.receipt",
            json!({"operationId":"probe-opacity-once"}),
            "queryExistingReceipt",
        )?;
        require(
            queried == receipt && same_document(&after, &self.state()?),
            "duplicateAddsNoHistoryOrRevision",
        )?;
        self.export_copy(&root.join("write"), "orange-opacity-040.png")?;
        self.note(
            "opacityAndExactDuplicate",
            json!({"request":original,"receipt":receipt,"sameReceipt":true,"oneHistoryStep":true}),
        );

        let stale_before = self.state()?;
        let stale = self.call(
            "seecut.execute",
            json!({"operationId":"probe-stale-refusal","expected":before["document"]["identity"],
            "action":{"kind":"layerSetOpacity","layerId":layer_id,"opacity":0.9}}),
            "rejectStaleRevision",
        )?;
        require(
            stale["status"] == "rejected"
                && stale["errorCode"] == "staleRevision"
                && stale["before"] == stale["after"],
            "staleReceiptUnchanged",
        )?;
        require(
            same_document(&stale_before, &self.state()?),
            "staleHasNoSideEffects",
        )?;
        self.note("staleRefusal", stale);

        let undo_before = self.idle_state()?;
        let undone = self.call("seecut.execute", json!({"operationId":"probe-independent-undo","expected":identity(&undo_before)?,"action":{"kind":"undo"}}), "independentUndo")?;
        require(undone["status"] == "applied", "undoApplied")?;
        let restored = self.state()?;
        require(
            near(opacity(&restored, layer_id)?, 1.0)
                && history(&restored)? == history(&before)?
                && restored["document"]["canRedo"] == true,
            "undoRestoresOpacityAndHistory",
        )?;
        self.export_copy(&root.join("write"), "orange-after-undo.png")?;
        self.note(
            "independentUndo",
            json!({"receipt":undone,"restored":restored["document"]}),
        );

        self.engine("file.new", json!({"width":8,"height":8,"mode":"rgb","depth":8,"profile":"none","background":"transparent","name":"SeeCut disposable tab B"}), "createSecondSyntheticTab")?;
        // Make B definitely dirty, so its eventual close also exercises the real discard flow.
        self.engine(
            "layer.new.layer",
            json!({"name":"SeeCut disposable B"}),
            "dirtySecondSyntheticTab",
        )?;
        let tab_before = self.idle_state()?;
        let doc_b = number(&identity(&tab_before)?, "/docId")?;
        require(doc_a != doc_b, "newTabHasDifferentRealDocId")?;
        let tab_rejected = self.call(
            "seecut.execute",
            json!({"operationId":"probe-wrong-tab-refusal","expected":identity(&restored)?,
            "action":{"kind":"layerSetOpacity","layerId":layer_id,"opacity":0.2}}),
            "rejectOldTabIdentity",
        )?;
        require(
            tab_rejected["status"] == "rejected"
                && tab_rejected["errorCode"] == "wrongDocument"
                && tab_rejected["before"] == tab_rejected["after"],
            "wrongTabReceiptUnchanged",
        )?;
        require(
            same_document(&tab_before, &self.state()?),
            "tabRefusalHasNoSideEffects",
        )?;
        self.note("tabRefusal", tab_rejected);
        self.call(
            "ui.menu.invoke",
            json!({"command":"file.close","params":{}}),
            "normalCloseSyntheticTabB",
        )?;
        self.discard_active_synthetic(doc_b, "file.close")?;
        require(
            number(&self.state()?, "/document/identity/docId")? == doc_a,
            "returnsToOriginalSyntheticTab",
        )?;

        let id = self.instance.unwrap();
        let closed = self
            .primary
            .close(id, Duration::from_millis(300))
            .map_err(|error| Failure {
                stage: "normalDirtyClose",
                class: host_class(&error),
            })?;
        require(
            matches!(closed, CloseOutcome::NeedsUser { .. }),
            "dirtyCloseNeedsUser",
        )?;
        require(
            number(&self.state()?, "/document/identity/docId")? == doc_a,
            "dirtyDocumentRetained",
        )?;
        let prompt = self.inspect()?;
        require(
            prompt["dialogs"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["id"] == "discard" && item["fields"]["command"] == "file.exit")
            }),
            "dirtyExitPromptVisible",
        )?;
        self.call(
            "ui.dialog.cancel",
            json!({"dialog":"discard"}),
            "cancelNormalDirtyClose",
        )?;
        let cancelled = self.state()?;
        require(
            number(&cancelled, "/document/identity/docId")? == doc_a
                && cancelled["document"]["dirty"] == true,
            "cancelRetainsOwnedDirtyDocument",
        )?;
        require(
            self.primary
                .current()
                .map_err(|_| Failure {
                    stage: "cancelLeavesEditorAlive",
                    class: "statusFailed",
                })?
                .is_some_and(|info| info.ready),
            "cancelLeavesEditorAlive",
        )?;
        self.note(
            "dirtyCloseAndCancel",
            json!({"needsUser":true,"cancelKeptEditorAlive":true,"docId":doc_a}),
        );

        let inspected = self.inspect()?;
        require(
            inspected["session"]["documents"]
                .as_array()
                .is_some_and(|docs| {
                    docs.len() == 1 && docs[0]["name"] == "SeeCut disposable orange A"
                }),
            "onlyOwnedSyntheticAExistsBeforeDiscard",
        )?;
        let closed = self
            .primary
            .close(id, Duration::from_millis(100))
            .map_err(|error| Failure {
                stage: "requestFinalSyntheticExit",
                class: host_class(&error),
            })?;
        require(
            matches!(closed, CloseOutcome::NeedsUser { .. }),
            "finalDirtyPromptStillRequiresChoice",
        )?;
        if let Err(error) = self.discard_active_synthetic(doc_a, "file.exit") {
            // A closing process may finish before its last socket reply is delivered.
            // Do not resend the write: actual child exit below is the reconciliation.
            if error.class != "unknownCompletionNeverResend" {
                return Err(error);
            }
            self.note(
                "finalDiscardReplyLost",
                json!({"writeNotResent":true,"requiresActualExitObservation":true}),
            );
        }
        Self::await_exit(&mut self.primary, Duration::from_secs(5))?;
        require(lock.try_lock().is_ok(), "osLockReleasedAfterEditorExit")?;
        self.note("explicitSyntheticDiscardAndExit", json!({"onlySyntheticDocsDiscarded":true,"actualChildExit":true,"profileLockReleased":true}));
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    // Refuse every local macOS invocation, and require the private Linux runner's opt-in.
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if !cfg!(target_os = "linux")
        || arguments.len() != 3
        || std::env::var("SEECUT_LIVE_PROBE_ISOLATED").ok().as_deref() != Some("1")
        || !std::env::var("DISPLAY").ok().is_some_and(|value| {
            value.starts_with(':')
                && value[1..]
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        eprintln!(
            "live probe refused: requires isolated Linux Xvfb runner and three explicit paths"
        );
        return std::process::ExitCode::from(2);
    }
    let provenance = PathBuf::from(&arguments[0]);
    let root = PathBuf::from(&arguments[1]);
    let commands_path = PathBuf::from(&arguments[2]);
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || root.ancestors().skip(1).any(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
        || private_new_dir(&root).is_err()
    {
        eprintln!("live probe refused: disposable run directory must be new, absolute and private");
        return std::process::ExitCode::from(2);
    }
    let mut probe = Probe::new();
    let result = decode(&commands_path, "readCompiledCommands")
        .and_then(|commands| probe.run(&provenance, &root, &commands));
    let cleanup = probe.cleanup_disposable_owned_children();
    let mut report = json!({"schemaVersion":1,"status":if result.is_ok() && cleanup {"passed"} else {"failed"},
        "model":false,"parentSeeCutMcp":false,"scope":"actual complete desktop; direct NativeHost; synthetic disposable documents",
        "cleanupOwnedChildren":cleanup,"snapshot":{"status":"notRequested","reason":"new protocol pending; no guessed fields"},
        "pixelEvidence":{"status":"pendingPythonPngReadback","files":["orange-full.png","orange-opacity-040.png","orange-after-undo.png"]},
        "checks":probe.observations});
    if let Err(error) = &result {
        report["failure"] = json!({"stage":error.stage,"class":error.class});
    }
    if let Ok(receipt) = decode(&provenance, "readSanitizedBuildIdentity") {
        report["build"] = json!({"attemptId":receipt["attemptId"],"source":receipt["source"],"features":receipt["features"],
            "binaries":{"photocraft":{"sha256":receipt["binaries"]["photocraft"]["sha256"],"bytes":receipt["binaries"]["photocraft"]["bytes"]},
            "photocraft-cli":{"sha256":receipt["binaries"]["photocraft-cli"]["sha256"],"bytes":receipt["binaries"]["photocraft-cli"]["bytes"]}}});
    } else {
        report["status"] = json!("failed");
        report["failure"] =
            json!({"stage":"readSanitizedBuildIdentity","class":"missingBuildEvidence"});
    }
    if let Ok(bytes) = fs::read(&commands_path) {
        report["compiledCommandsSha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
    }
    let written = serde_json::to_vec_pretty(&report)
        .ok()
        .is_some_and(|bytes| fs::write(root.join("report.json"), bytes).is_ok());
    if result.is_ok() && cleanup && written && report["status"] == "passed" {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(1)
    }
}
