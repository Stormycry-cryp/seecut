//! Candidate for an explicitly isolated Linux Xvfb run. This is a direct host probe,
//! not a model invocation or the parent SeeCut MCP/Task integration acceptance.
use seecut_photo_host::{
    CloseOutcome, HostError, InstanceStatus, LaunchOptions, NativeHost, PhotoBackingBinding,
    PhotoEditAction, PhotoEditStatus, PhotoIdentity, PhotoReceiptLookup, PhotoSnapshot,
    PhotoSnapshotLayerKind, PhotoSnapshotRenderer, Uuid, VerifiedBuild,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const SYNTHETIC_BACKING_NAME: &str = "SeeCut disposable orange A.pcraft";
// Non-binary fractions intentionally exercise native f32 JSON round trips.
const FRACTIONAL_ZOOM: f32 = 1.3;
const FRACTIONAL_CENTER: [f32; 2] = [8.1, 7.7];
const FRACTIONAL_ROTATION: f32 = 13.1;

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

fn fractional_target_matches(state: &Value) -> bool {
    let exact_f32 = |pointer: &str, expected: f32| {
        state
            .pointer(pointer)
            .and_then(Value::as_f64)
            .is_some_and(|value| value.is_finite() && value as f32 == expected)
    };
    exact_f32("/view/zoom", FRACTIONAL_ZOOM)
        && exact_f32("/view/center/0", FRACTIONAL_CENTER[0])
        && exact_f32("/view/center/1", FRACTIONAL_CENTER[1])
        && exact_f32("/view/rotation", FRACTIONAL_ROTATION)
        && state["view"]["fit_pending"] == false
        && state["view"]["fill_pending"] == false
}

// Deliberate evidence allowlist from already checked typed getters. This is not
// a raw wire reply, a general serialization API or model-delivery permission.
fn snapshot_evidence(snapshot: &PhotoSnapshot) -> Value {
    let identity = snapshot.identity();
    let document = snapshot.document();
    let selection = snapshot.selection();
    let view = snapshot.view();
    let geometry = &view.geometry;
    let presentation = snapshot.presentation();
    let pixels = snapshot.pixels();
    json!({
        "kind":"adoptedDocumentComposite","adoptionStage":"uiPaintSubmitted",
        "requestId":snapshot.request_id(),"snapshotId":snapshot.snapshot_id(),
        "identity":{"hostInstanceId":identity.host_instance_id.to_string(),"docId":identity.doc_id,
            "documentGeneration":identity.document_generation,"revision":identity.revision},
        "capturedFrame":snapshot.captured_frame(),"viewportId":snapshot.viewport_id(),"canvasId":snapshot.canvas_id(),
        "capturedAfterUiEnqueueMs":snapshot.timing().captured_after_ui_enqueue_ms,
        "captureAgeAtReplyMs":snapshot.timing().capture_age_at_reply_ms,
        "timeBasis":{"captureDelay":"uiEnqueue","captureAge":"adoptedFrameCapture"},
        "returnedHistoricalFrame":true,
        "document":{"size":document.size,"activeLayer":document.active_layer,
            "selectedLayers":document.selected_layers,"dirty":document.dirty,
            "canUndo":document.can_undo,"canRedo":document.can_redo},
        "selection":{"present":selection.present,"domain":selection.domain,"bounds":selection.bounds,
            "boundsFormat":"xyxyExclusive","coverageHash":selection.coverage_hash,
            "selectionStateId":selection.selection_state_id},
        "view":{"geometry":{"rectPoints":geometry.rect_points,"pixelsPerPoint":geometry.pixels_per_point,
            "zoom":geometry.zoom,"center":geometry.center,"flip":geometry.flip,"rotation":geometry.rotation,
            "aspect":geometry.aspect,"displayCamera":geometry.display_camera,"targetCamera":geometry.target_camera,
            "visibleDocumentPolygon":geometry.visible_document_polygon},
            "visibleDocumentAabb":view.visible_document_aabb,"clippedVisibleDocumentAabb":view.clipped_visible_document_aabb,
            "aabbFormat":"xyxyExclusive","viewStateId":view.view_state_id},
        "presentation":{"source":{"renderer":match presentation.renderer {
            PhotoSnapshotRenderer::Cpu => "cpu", PhotoSnapshotRenderer::Gpu => "gpu"},
            "displayKey":presentation.display_key,"displayIdentity":presentation.display_identity},
            "colorSpace":"sRGB IEC61966-2.1","profileHash":presentation.profile_hash,
            "pixelSemantics":"documentCompositeNotMonitorPixels"},
        "pixels":{"kind":"documentComposite","mime":"image/png","transparent":true,
            "width":pixels.size[0],"height":pixels.size[1],"documentRect":pixels.document_rect,
            "documentPixelsPerOutputPixel":pixels.document_pixels_per_output_pixel,
            "pngHash":pixels.png_hash,"pixelHash":pixels.pixel_hash}
    })
}
fn save_private_adopted_png(root: &Path, bytes: &[u8]) -> ProbeResult<()> {
    let temporary = root.join("orange-adopted-opacity-040.png.part");
    let final_path = root.join("orange-adopted-opacity-040.png");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = io(options.open(&temporary), "createPrivateAdoptedPng")?;
    io(file.write_all(bytes), "writeExactAdoptedPngBytes")?;
    io(file.sync_all(), "syncPrivateAdoptedPng")?;
    drop(file);
    // Publish a complete inode without replacing an existing destination. Both
    // fixed paths belong to this newly created disposable private directory.
    io(
        fs::hard_link(&temporary, &final_path),
        "publishPrivateAdoptedPng",
    )?;
    io(fs::remove_file(&temporary), "removePrivatePngTemporaryLink")?;
    Ok(())
}

struct Probe {
    primary: NativeHost,
    contender: NativeHost,
    instance: Option<Uuid>,
    deadline: Instant,
    observations: Vec<Value>,
    snapshot: Value,
}
impl Probe {
    fn new() -> Self {
        Self {
            primary: NativeHost::new(),
            contender: NativeHost::new(),
            instance: None,
            deadline: Instant::now() + Duration::from_secs(180),
            observations: Vec::new(),
            snapshot: json!({"status":"notRequested"}),
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
        // app.save uses the existing directory capability. A flat PNG does not
        // become this layered document's own file or mark its edits saved.
        self.call(
            "app.save",
            json!({"path":target,"layers":false,"wait":true}),
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
    fn checked_backing(
        &mut self,
        state: &Value,
        stage: &'static str,
    ) -> ProbeResult<PhotoBackingBinding> {
        self.check_time()?;
        let instance = self.instance.ok_or(Failure {
            stage,
            class: "noInstance",
        })?;
        // This real closed API performs its own two state reads around filesystem
        // verification. Binding/key/file observations remain local to this probe.
        let binding = self
            .primary
            .photo_backing_after_gate(instance)
            .map_err(|error| Failure {
                stage,
                class: host_class(&error),
            })?;
        let native = binding.identity();
        require(
            native.host_instance_id == instance
                && native.doc_id == number(state, "/document/identity/docId")?
                && native.document_generation
                    == number(state, "/document/identity/documentGeneration")?
                && native.revision == number(state, "/document/identity/revision")?,
            stage,
        )?;
        self.check_time()?;
        Ok(binding)
    }
    fn saved_backing_contract(&mut self, write_root: &Path, layer_id: u64) -> ProbeResult<()> {
        let first_dir = write_root.join("backing-a");
        let second_dir = write_root.join("backing-b");
        private_new_dir(&first_dir)?;
        private_new_dir(&second_dir)?;
        let first_path = first_dir.join(SYNTHETIC_BACKING_NAME);
        let second_path = second_dir.join(SYNTHETIC_BACKING_NAME);
        require(
            !first_path.exists() && !second_path.exists(),
            "freshSyntheticBackingTargets",
        )?;
        let initial = self.idle_state()?;
        // Explicit target is the existing capability-scoped Save As semantic,
        // writing a genuine native .pcraft through the desktop's actual saver.
        self.call(
            "app.save",
            json!({"path":first_path,"wait":true}),
            "saveFirstSyntheticNativeBacking",
        )?;
        let first = self.idle_state()?;
        require(
            first["document"]["backing"]["nativePath"] == json!(first_path)
                && first["document"]["backing"]["pathStatus"] == "recorded"
                && first["document"]["backing"]["sourceReadOnly"] == false
                && number(&first, "/document/backing/savedRevision")?
                    == number(&first, "/document/identity/revision")?
                && first["document"]["dirty"] == false
                && number(&first, "/document/identity/docId")?
                    == number(&initial, "/document/identity/docId")?
                && number(&first, "/document/identity/revision")?
                    == number(&initial, "/document/identity/revision")?
                && number(&first, "/document/identity/documentGeneration")?
                    != number(&initial, "/document/identity/documentGeneration")?
                && history(&first)? == history(&initial)?
                && near(opacity(&first, layer_id)?, 1.0),
            "realFirstSaveOwnsBackingWithoutEditStep",
        )?;
        let first_binding = self.checked_backing(&first, "firstClosedBackingRead")?;
        // No target: actual normal Save of this document's existing own file.
        self.call(
            "app.save",
            json!({"wait":true}),
            "normalSaveSameSyntheticBacking",
        )?;
        let same = self.idle_state()?;
        require(
            same_document(&first, &same),
            "normalSaveKeepsNativeGenerationAndHistory",
        )?;
        let same_binding = self.checked_backing(&same, "samePathClosedBackingRead")?;
        require(
            first_binding.identity() == same_binding.identity()
                && first_binding.history_key() == same_binding.history_key(),
            "normalSaveKeepsTrustedHistoryKey",
        )?;
        let file_observation_changed =
            first_binding.file_identity() != same_binding.file_identity();
        let stale_expected = identity(&same)?;
        self.call(
            "app.save",
            json!({"path":second_path,"wait":true}),
            "saveSameNameAtDifferentSyntheticDirectory",
        )?;
        let moved = self.idle_state()?;
        require(
            moved["document"]["backing"]["nativePath"] == json!(second_path)
                && moved["document"]["name"] == same["document"]["name"]
                && moved["document"]["name"] == SYNTHETIC_BACKING_NAME
                && number(&moved, "/document/identity/docId")?
                    == number(&same, "/document/identity/docId")?
                && number(&moved, "/document/identity/revision")?
                    == number(&same, "/document/identity/revision")?
                && number(&moved, "/document/identity/documentGeneration")?
                    != number(&same, "/document/identity/documentGeneration")?
                && history(&moved)? == history(&same)?
                && moved["document"]["dirty"] == false,
            "sameNameSaveAsRebindsWithoutHistoryStep",
        )?;
        let moved_binding = self.checked_backing(&moved, "freshRetargetedClosedBackingRead")?;
        require(
            moved_binding.history_key() != same_binding.history_key(),
            "sameNameDifferentDirectoryChangesHistoryKey",
        )?;
        let rejected = self.call(
            "seecut.execute",
            json!({"operationId":"probe-pre-save-as-refusal","expected":stale_expected,
            "action":{"kind":"layerSetOpacity","layerId":layer_id,"opacity":0.9}}),
            "rejectExpectedFromPreviousBacking",
        )?;
        let unchanged = self.state()?;
        require(
            rejected["status"] == "rejected"
                && rejected["errorCode"] == "wrongDocument"
                && rejected["before"] == rejected["after"]
                && rejected["safeToResend"] == false
                && same_document(&moved, &unchanged)
                && history(&unchanged)? == history(&moved)?
                && near(opacity(&unchanged, layer_id)?, 1.0),
            "oldBackingExpectedRefusedWithoutEditOrHistory",
        )?;
        // Deliberately publish no path, history key, file identity, raw state or
        // binding Debug. These are observations, never new Agent authorization.
        self.note("savedNativeBackingContract", json!({"firstSaveConfirmed":true,"closedApiBindingMatched":true,
            "normalSaveSameGeneration":true,"normalSaveSameHistoryKey":true,"normalSaveFileObservationChanged":file_observation_changed,
            "sameNameSaveAsChangedGeneration":true,"sameNameSaveAsChangedHistoryKey":true,
            "sameDocIdRevisionAndHistory":true,"oldExpectedStatus":"rejected","oldExpectedError":"wrongDocument",
            "oldExpectedBeforeAfterEqual":true,"oldExpectedNoHistoryOrOpacityEffect":true,
            "threeFreshClosedBindings":true,"pathsKeysAndFileIdsPublished":false,"saveRetried":false}));
        Ok(())
    }
    fn dirty_owned_synthetic_for_close(
        &mut self,
        expected_doc: u64,
        layer_id: u64,
    ) -> ProbeResult<()> {
        let before = self.idle_state()?;
        require(
            number(&before, "/document/identity/docId")? == expected_doc
                && near(opacity(&before, layer_id)?, 1.0),
            "finalDirtyEditTargetsRestoredOwnedSynthetic",
        )?;
        let operation_id = "probe-final-synthetic-dirty-for-close";
        let id = self.instance.ok_or(Failure {
            stage: "applyIndependentSyntheticDirtyEditForClose",
            class: "noInstance",
        })?;
        let before_identity = identity(&before)?;
        require(
            before_identity["hostInstanceId"] == id.to_string(),
            "finalDirtyOwnedHostIdentity",
        )?;
        let expected = PhotoIdentity {
            host_instance_id: id,
            doc_id: number(&before_identity, "/docId")?,
            document_generation: number(&before_identity, "/documentGeneration")?,
            revision: number(&before_identity, "/revision")?,
        };
        self.check_time()?;
        // One original write through the closed action API; no retry on Unknown.
        let receipt = self
            .primary
            .photo_execute_after_gate(
                id,
                operation_id,
                &expected,
                PhotoEditAction::LayerSetOpacity {
                    layer_id,
                    opacity: 0.6,
                },
            )
            .map_err(|error| Failure {
                stage: "applyIndependentSyntheticDirtyEditForClose",
                class: host_class(&error),
            })?;
        self.check_time()?;
        // Query the original fixed ID once. Unknown/mismatch fails; never execute again.
        let lookup = self
            .primary
            .photo_receipt_after_gate(id, operation_id)
            .map_err(|error| Failure {
                stage: "queryOriginalFinalSyntheticDirtyReceipt",
                class: host_class(&error),
            })?;
        let PhotoReceiptLookup::Known(queried) = lookup else {
            return Err(Failure {
                stage: "queryOriginalFinalSyntheticDirtyReceipt",
                class: "receiptNotKnownNeverResend",
            });
        };
        require(
            queried.as_ref() == &receipt
                && queried.operation_id() == operation_id
                && receipt.operation_id() == operation_id
                && !queried.safe_to_resend()
                && !receipt.safe_to_resend(),
            "sameOriginalTypedFinalReceiptWithoutResend",
        )?;
        let after = self.state()?;
        let after_identity = identity(&after)?;
        require(
            after_identity["hostInstanceId"] == id.to_string(),
            "finalDirtyAfterOwnedHostIdentity",
        )?;
        let actual_after = PhotoIdentity {
            host_instance_id: id,
            doc_id: number(&after_identity, "/docId")?,
            document_generation: number(&after_identity, "/documentGeneration")?,
            revision: number(&after_identity, "/revision")?,
        };
        require(
            receipt.status == PhotoEditStatus::Applied
                && !receipt.safe_to_resend()
                && receipt
                    .before
                    .as_ref()
                    .is_some_and(|state| state.identity == expected)
                && receipt
                    .after
                    .as_ref()
                    .is_some_and(|state| state.identity == actual_after)
                && number(&after, "/document/identity/docId")? == expected_doc
                && number(&after, "/document/identity/documentGeneration")?
                    == number(&before, "/document/identity/documentGeneration")?
                && history(&after)? == history(&before)? + 1
                && near(opacity(&after, layer_id)?, 0.6)
                && after["document"]["dirty"] == true
                && after["document"]["canUndo"] == true,
            "finalSyntheticEditIsOneReversibleDirtyHistoryStep",
        )?;
        // Earlier 3+1 PNG evidence remains tied to its original capture/export.
        // This separate final reversible edit and original receipt exercise the
        // closed APIs and dirty close; they do not reuse earlier pixel evidence.
        self.note("independentSyntheticDirtyEditForClose", json!({"status":"applied","freshExpected":true,
            "oneHistoryStep":true,"opacityReadbackMatched":true,"dirtyReadback":true,"canUndo":true,
            "sameOwnedDocument":true,"saveRetried":false,"reexported":false,"priorPixelEvidenceReusedForThisEdit":false,
            "executeMethod":"photo_execute_after_gate","receiptMethod":"photo_receipt_after_gate",
            "executeRequestCount":1,"receiptRequestCount":1,"knownOriginalReceipt":true,
            "sameTypedReceipt":true,"sameOperationId":true,"safeToResend":false,"reexecuted":false,
            "typedReceiptPublished":false,"model":false,"parentSeeCutMcp":false}));
        Ok(())
    }
    fn prepare_fractional_snapshot_view(&mut self, layer_id: u64) -> ProbeResult<()> {
        let before = self.idle_state()?;
        require(
            near(opacity(&before, layer_id)?, 0.4),
            "fractionalViewOnlyOwnedOrangeDocument",
        )?;
        // actual control.rs: a view write turns zoom_animating off and clears its
        // anchor; zoom/center also clear fit/fill. canvas::animated_view then uses
        // the direct target, clearing any prior transition in its nonanimated branch.
        self.call(
            "ui.set",
            json!({"zoom":f64::from(FRACTIONAL_ZOOM),
            "center":FRACTIONAL_CENTER.map(f64::from),"rotation":f64::from(FRACTIONAL_ROTATION)}),
            "setNonIntegerViewDirectlyOnce",
        )?;
        let applied = self.state()?;
        let first_frame = number(&applied, "/frame")?;
        let after_two_frames = first_frame.checked_add(2).ok_or(Failure {
            stage: "waitDirectViewFrames",
            class: "frameCounterOverflow",
        })?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let settled_frame = loop {
            self.check_time()?;
            require(Instant::now() < deadline, "waitDirectViewFrames")?;
            let current = self.state()?;
            require(
                same_document(&before, &current)
                    && before["receiptCache"] == current["receiptCache"]
                    && current["busy"].is_null()
                    && fractional_target_matches(&current),
                "nonIntegerViewHasStableTargetAndNoDocumentSideEffects",
            )?;
            let frame = number(&current, "/frame")?;
            if frame >= after_two_frames {
                break frame;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        // These reads observe target and frame progression, not display_camera:
        // transient presentation fields are serde(skip). The sole following typed
        // snapshot must prove actual geometry, then same-ID postInputFrame validation.
        self.note("directNonIntegerViewBeforeOnlySnapshot", json!({"status":"targetAndFrameWaitPassed",
            "method":"ui.set","requestCount":1,"retried":false,
            "zoom":f64::from(FRACTIONAL_ZOOM),"center":FRACTIONAL_CENTER.map(f64::from),
            "rotation":f64::from(FRACTIONAL_ROTATION),"animationMode":"directNonAnimatedControlSource",
            "firstReadFrame":first_frame,"settledReadFrame":settled_frame,"minimumFrameAdvance":2,
            "documentHistoryUnchanged":true,"readback":"targetOnlyUntilTypedSnapshot",
            "actualDisplayCameraClaimedByTheseReads":false}));
        Ok(())
    }
    fn adopted_orange_snapshot(&mut self, layer_id: u64, root: &Path) -> ProbeResult<()> {
        self.prepare_fractional_snapshot_view(layer_id)?;
        let session_before = self.inspect()?["session"].clone();
        let before = self.idle_state()?;
        require(
            near(opacity(&before, layer_id)?, 0.4),
            "snapshotAtOpacity040",
        )?;
        let expected = identity(&before)?;
        let id = self.instance.ok_or(Failure {
            stage: "requestAdoptedSnapshotOnce",
            class: "noInstance",
        })?;
        require(
            expected["hostInstanceId"] == id.to_string(),
            "snapshotOwnedHostIdentity",
        )?;
        let typed_expected = PhotoIdentity {
            host_instance_id: id,
            doc_id: number(&expected, "/docId")?,
            document_generation: number(&expected, "/documentGeneration")?,
            revision: number(&expected, "/revision")?,
        };
        let request_id = "probe-adopted-orange-opacity-040-once";
        let validation_request_id = "probe-validate-same-orange-opacity-040-once";
        let request = json!({"requestId":request_id,"expected":expected,"maxEdge":16});
        self.snapshot = json!({"status":"captureRequested","request":request,
            "requestCount":1,"validationRequestCount":0,"retried":false,
            "captureMethod":"photo_snapshot_after_gate","validationMethod":"validate_photo_snapshot_after_gate"});
        self.check_time()?;
        // Exactly one closed read-only capture, followed immediately by validation
        // of that same object. Any error/expiry fails; no second capture or resend.
        let snapshot = self
            .primary
            .photo_snapshot_after_gate(id, request_id, typed_expected.clone(), 16)
            .map_err(|error| Failure {
                stage: "typedAdoptedSnapshotOnce",
                class: host_class(&error),
            })?;
        self.snapshot["status"] = json!("validationRequested");
        self.snapshot["typedSnapshotParserPassed"] = json!(true);
        self.snapshot["validationRequestCount"] = json!(1);
        self.snapshot["validationRequest"] =
            json!({"requestId":validation_request_id,"snapshotId":snapshot.snapshot_id()});
        self.check_time()?;
        let validation = self
            .primary
            .validate_photo_snapshot_after_gate(id, validation_request_id, &snapshot)
            .map_err(|error| Failure {
                stage: "validateSameTypedSnapshotOnce",
                class: host_class(&error),
            })?;
        let after = self.state()?;
        let session_after = self.inspect()?["session"].clone();
        require(
            same_document(&before, &after)
                && before["receiptCache"] == after["receiptCache"]
                && after["busy"].is_null()
                && session_before == session_after,
            "snapshotAndValidationLeaveSessionDocumentSelectionAndHistoryUnchanged",
        )?;
        require(
            snapshot.identity() == &typed_expected
                && identity(&after)? == expected
                && validation.identity() == snapshot.identity()
                && validation.snapshot_id() == snapshot.snapshot_id()
                && validation.view_state_id() == snapshot.view().view_state_id
                && validation.selection_state_id() == snapshot.selection().selection_state_id,
            "sameOriginalTypedIdentityViewAndSelectionValidated",
        )?;
        require(
            snapshot.captured_frame() >= number(&before, "/frame")?
                && snapshot.captured_frame() < validation.validated_frame()
                && validation.validated_frame() <= number(&after, "/frame")?
                && snapshot.timing().captured_after_ui_enqueue_ms < 2_000
                && snapshot.timing().capture_age_at_reply_ms < 5_000
                && snapshot.timing().captured_after_ui_enqueue_ms
                    + snapshot.timing().capture_age_at_reply_ms
                    <= 5_000
                && validation.capture_age_ms() >= snapshot.timing().capture_age_at_reply_ms
                && validation.capture_age_ms() < 5_000
                && validation.capture_age_ms() <= validation.capture_age_upper_bound_ms(),
            "actualNewPostInputFrameAndCaptureAgeBounds",
        )?;
        let document = snapshot.document();
        require(
            document.size == [16, 16]
                && before["document"]["name"].as_str() == Some(document.name.as_str())
                && document.active_layer == Some(layer_id)
                && json!(document.selected_layers) == before["document"]["selectedLayers"]
                && document.dirty == before["document"]["dirty"]
                && document.can_undo == before["document"]["canUndo"]
                && document.can_redo == before["document"]["canRedo"]
                && document.layers.len()
                    == before["document"]["layers"].as_array().map_or(0, Vec::len)
                && document.layers.iter().any(|layer| {
                    layer.id == layer_id
                        && layer.kind == PhotoSnapshotLayerKind::Raster
                        && near(layer.opacity, 0.4)
                }),
            "typedAdoptedDocumentAndLayerSelection",
        )?;
        require(
            before["document"]["hasPixelSelection"] == false
                && !snapshot.selection().present
                && snapshot.selection().bounds.is_none()
                && snapshot.selection().domain == [0, 0, 16, 16]
                && snapshot.view().geometry.display_camera
                    == snapshot.view().geometry.target_camera
                && snapshot.pixels().size == [16, 16]
                && snapshot.pixels().document_rect == [0, 0, 16, 16]
                && snapshot.pixels().document_pixels_per_output_pixel == [1.0, 1.0],
            "typedOriginalSelectionCameraAndPngEnvelope",
        )?;
        let geometry = &snapshot.view().geometry;
        require(
            fractional_target_matches(&before)
                && fractional_target_matches(&after)
                && geometry.center == FRACTIONAL_CENTER
                && geometry.rotation == FRACTIONAL_ROTATION
                && geometry.display_camera
                    == [FRACTIONAL_ZOOM, FRACTIONAL_CENTER[0], FRACTIONAL_CENTER[1]]
                && geometry.target_camera == geometry.display_camera
                && geometry.zoom == FRACTIONAL_ZOOM / geometry.pixels_per_point,
            "actualTypedNonIntegerGeometryMatchesDirectView",
        )?;
        // Save only this parser-accepted original PNG; Python independently decodes
        // these exact bytes for all pixels, embedded ICC and native pixel hash.
        save_private_adopted_png(root, snapshot.png_bytes())?;
        let response = snapshot_evidence(&snapshot);
        let validation_evidence = json!({"kind":"adoptedSnapshotValidation",
            "adoptionStage":"uiPaintSubmitted","validationStage":"postInputFrame",
            "requestId":validation.request_id(),"snapshotId":validation.snapshot_id(),
            "identity":response["identity"],"viewStateId":validation.view_state_id(),
            "selectionStateId":validation.selection_state_id(),"validatedFrame":validation.validated_frame(),
            "captureAgeMs":validation.capture_age_ms()});
        self.snapshot = json!({"status":"pendingPythonPngReadback","request":request,"response":response,
            "responseProjection":"typedSnapshotAllowlist","typedSnapshotParserPassed":true,
            "validationRequest":{"requestId":validation_request_id,"snapshotId":snapshot.snapshot_id()},
            "validation":validation_evidence,"validationProjection":"typedValidationAllowlist",
            "typedValidationParserPassed":true,"validationCaptureAgeUpperBoundMs":validation.capture_age_upper_bound_ms(),
            "requestCount":1,"validationRequestCount":1,"retried":false,"sameFrozenIds":true,"newPaint":true,
            "actualNonIntegerGeometryVerified":true,"viewSetup":"ui.setDirectNonAnimated",
            "captureMethod":"photo_snapshot_after_gate","validationMethod":"validate_photo_snapshot_after_gate",
            "sessionUnchanged":true,"beforeIdentity":before["document"]["identity"],"afterIdentity":after["document"]["identity"],
            "beforeFrame":number(&before,"/frame")?,"afterFrame":number(&after,"/frame")?,
            "beforeHistoryEntries":history(&before)?,"afterHistoryEntries":history(&after)?,
            "pngSource":"typedOriginalPngBytes","pngFile":"orange-adopted-opacity-040.png",
            "pngBytes":snapshot.png_bytes().len(),
            "screenCapture":false,"presentVerified":false,"model":false,"parentSeeCutMcp":false});
        Ok(())
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
        self.saved_backing_contract(&root.join("write"), layer_id)?;
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

        self.adopted_orange_snapshot(layer_id, root)?;

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
        // Metadata now includes a raw native backing path. Keep full-state
        // assertions above, but exclude backing from the public report projection.
        let mut restored_evidence = restored["document"].clone();
        if let Some(document) = restored_evidence.as_object_mut() {
            document.remove("backing");
        }
        self.note(
            "independentUndo",
            json!({"receipt":undone,"restored":restored_evidence}),
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

        self.dirty_owned_synthetic_for_close(doc_a, layer_id)?;
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

        let before_discard = self.state()?;
        let inspected = self.inspect()?;
        require(
            number(&before_discard, "/document/identity/docId")? == doc_a
                && before_discard["document"]["dirty"] == true
                && inspected["session"]["active"] == 0
                && inspected["session"]["documents"]
                    .as_array()
                    .is_some_and(|docs| {
                        docs.len() == 1
                            && docs[0]["name"] == SYNTHETIC_BACKING_NAME
                            && docs[0]["revision"]
                                == before_discard["document"]["identity"]["revision"]
                    })
                && inspected["dialogs"]
                    .as_array()
                    .is_some_and(|dialogs| dialogs.iter().all(|dialog| dialog["id"] != "discard")),
            "onlyOwnedSyntheticAExistsAfterCancelBeforeSingleDocumentDiscard",
        )?;
        // Do not reopen file.exit after Cancel. file.close is allowed by the
        // existing automation policy and uses the real single-document prompt.
        self.call(
            "ui.menu.invoke",
            json!({"command":"file.close","params":{}}),
            "normalCloseFinalOwnedSyntheticDocument",
        )?;
        self.discard_active_synthetic(doc_a, "file.close")?;
        let empty = self.inspect()?;
        require(
            empty["session"]["documents"]
                .as_array()
                .is_some_and(Vec::is_empty)
                && empty["session"]["active"].is_null()
                && empty["dialogs"]
                    .as_array()
                    .is_some_and(|dialogs| dialogs.iter().all(|dialog| dialog["id"] != "discard")),
            "singleDocumentDiscardLeavesEmptySessionBeforeCleanExit",
        )?;
        self.note(
            "ownedSyntheticSingleDocumentDiscard",
            json!({"command":"file.close",
            "choice":"dontSave","onlyOwnedSyntheticDocument":true,"sessionEmptyReadback":true,
            "discardPromptCleared":true,"fileExitDontSaveAutomaticallyConfirmed":false,
            "writeNotResent":true}),
        );
        self.check_time()?;
        // Request clean exit once. Unknown does not identify a lost reply or
        // absence of effects: reconcile only by actual owned Child exit below.
        let clean_close_outcome = match self.primary.close(id, Duration::from_millis(300)) {
            Ok(CloseOutcome::Exited) => "exitedDuringCloseWait",
            Ok(CloseOutcome::NeedsUser { .. }) => "pendingActualChildExitObservation",
            Err(HostError::Unknown(_)) => {
                self.note(
                    "cleanExitCompletionUnknownNeverResent",
                    json!({
                    "completion":"unknown","writeNotResent":true,
                    "requiresActualChildExitObservation":true}),
                );
                "completionUnknownNeverResent"
            }
            Err(error) => {
                return Err(Failure {
                    stage: "requestCleanApplicationExitOnce",
                    class: host_class(&error),
                });
            }
        };
        Self::await_exit(&mut self.primary, Duration::from_secs(5))?;
        require(lock.try_lock().is_ok(), "osLockReleasedAfterEditorExit")?;
        self.note(
            "singleDocumentDiscardAndCleanApplicationExit",
            json!({
            "dirtyExitProtectionAndCancelVerified":true,"onlySyntheticDocsDiscarded":true,
            "discardCommand":"file.close","sessionEmptyBeforeCleanExit":true,
            "cleanCloseMethod":"seecut.close","cleanCloseRequestCount":1,
            "cleanCloseOutcome":clean_close_outcome,"writeNotResent":true,
            "actualChildExit":true,"profileLockReleased":true,
            "fileExitDontSaveAutomaticallyConfirmed":false,"model":false,"parentSeeCutMcp":false}),
        );
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
        "cleanupOwnedChildren":cleanup,"snapshot":probe.snapshot,
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
