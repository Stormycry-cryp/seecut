// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read only the native Monitor image already adopted on the UI thread.
//! No decoding, seek, playback change, write grant, named permit or operation.

use super::*;
use concat_editor_mcp::{
    ClipAdoptedFrame, ClipPreviewData, ClipPreviewFrame, clip_preview_dimensions,
};
use concat_host::preview::{PreviewPoll, PreviewRgba, PreviewTicket};
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct Capture {
    request: Request,
    ids: DocumentIds,
    origin: canvas_policy::OutputOriginCapture,
    revision: u64,
    context_revision: u64,
    selection_revision: u64,
    view_state_id: String,
    frame: ClipPreviewFrame,
    playhead_seconds: f64,
    captured_at_unix_ms: u64,
}

pub(super) struct Job {
    capture: Capture,
    pixels: mpsc::Receiver<Result<PreviewRgba, &'static str>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    _slot: CaptureSlot,
}

struct CaptureSlot(Arc<AtomicBool>);
impl CaptureSlot {
    fn claim(busy: &Arc<AtomicBool>) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Self(busy.clone()))
    }
}
impl Drop for CaptureSlot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

fn origin(bridge: &BridgeUi) -> Result<canvas_policy::OutputOriginCapture, ()> {
    Ok(canvas_policy::OutputOriginCapture {
        account_scope: crate::cloud::try_canvas_output_scope()?,
        assistant_session: bridge.assistant_session.clone(),
    })
}

fn view_id(
    bridge: &BridgeUi,
    studio: &Studio,
    ids: &DocumentIds,
    frame: &ClipAdoptedFrame,
) -> String {
    view_context::clip_snapshot(
        studio,
        &view_context::Binding {
            instance: &bridge.instance_id,
            project: &ids.project,
            session: &ids.session,
            revision: studio.mcp_clip_state().1,
            context_revision: studio.mcp_context_epoch(),
            selection_revision: studio.mcp_selection_epoch(),
        },
        frame,
    )["viewStateId"]
        .as_str()
        .expect("native view identity")
        .to_owned()
}

/// Runs inside the original trusted UI callback. Texture copying is submitted
/// here, before control can return to another Monitor/ring rendering callback.
pub(super) fn prepare(
    bridge: &BridgeUi,
    request: &Request,
    studio: &Studio,
    ids: DocumentIds,
) -> UiReply {
    let fail = |status| UiReply::Response(Response::error(status));
    if !bridge.authorized(request, &ids.project, true) {
        return fail("notAuthorized");
    }
    let (_, revision, _, busy) = studio.mcp_clip_state();
    if busy || studio.playing {
        return fail("busy");
    }
    if !concat_editor_mcp::preview_matches(
        request,
        &ids.project,
        &ids.session,
        revision,
        studio.mcp_context_epoch(),
    ) {
        return fail("stalePreview");
    }
    let frame = match studio.monitor.adopted_frame(studio) {
        ClipAdoptedFrame::Ready(frame) => frame,
        ClipAdoptedFrame::Unavailable(value) => {
            use concat_editor_mcp::ClipPreviewUnavailableReason as Reason;
            return fail(match value.reason {
                Reason::Playing | Reason::Busy => "busy",
                Reason::Pending => "stalePreview",
                Reason::NoFrame | Reason::Unsupported => "unsupportedPreview",
            });
        }
    };
    if request
        .clip_preview_query
        .as_ref()
        .is_none_or(|query| query.frame_id != frame.frame_id)
    {
        return fail("stalePreview");
    }
    let max_edge = request.max_edge.unwrap_or(256);
    if clip_preview_dimensions(frame.source_width, frame.source_height, max_edge).is_none() {
        return fail("unsupportedPreview");
    }
    let Ok(captured_at_unix_ms) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
    else {
        return fail("ioFailure");
    };
    let Ok(origin) = origin(bridge) else {
        return fail("busy");
    };
    let capture = Capture {
        request: request.clone(),
        origin,
        revision,
        context_revision: studio.mcp_context_epoch(),
        selection_revision: studio.mcp_selection_epoch(),
        view_state_id: view_id(
            bridge,
            studio,
            &ids,
            &ClipAdoptedFrame::Ready(frame.clone()),
        ),
        ids,
        frame,
        playhead_seconds: f64::from(studio.playhead),
        captured_at_unix_ms,
    };
    let Some(slot) = CaptureSlot::claim(&studio.host.clip_preview_busy) else {
        return fail("busy");
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let cancelled = Arc::new(AtomicBool::new(false));
    let (sender, pixels) = mpsc::sync_channel(1);
    if let Some(cpu) = studio.monitor.cached_cpu() {
        // Snapshot already held pixels only; never Slint's GPU render_to_buffer.
        let _ = sender.send(Ok(PreviewRgba {
            width: cpu.width(),
            height: cpu.height(),
            pixels: cpu.as_bytes().to_vec(),
        }));
    } else if let Some(texture) = studio.monitor.image.to_wgpu_29_texture() {
        let Ok(mut slot) = studio.host.clip_preview_reader.try_borrow_mut() else {
            return fail("busy");
        };
        let Some(reader) = slot.as_mut() else {
            return fail("unsupportedPreview");
        };
        let ticket = match reader.capture(&texture, max_edge, deadline) {
            Ok(ticket) => ticket,
            Err(concat_host::preview::PreviewError::Busy) => return fail("busy"),
            Err(_) => return fail("unsupportedPreview"),
        };
        let cancelled = cancelled.clone();
        slint::Timer::single_shot(Duration::from_millis(8), move || {
            poll(ticket, sender, cancelled, deadline)
        });
    } else {
        return fail("unsupportedPreview");
    }
    UiReply::ClipPreview(Box::new(Job {
        capture,
        pixels,
        cancelled,
        deadline,
        _slot: slot,
    }))
}

/// Timer callbacks remain on the same device/UI thread even while paused.
/// No PNG work or blocking GPU wait is performed here.
fn poll(
    ticket: PreviewTicket,
    sender: mpsc::SyncSender<Result<PreviewRgba, &'static str>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
) {
    let mut result = Some(Err("appUnavailable"));
    Shell::with(|shell, _| {
        let Ok(studio) = shell.studio.try_borrow() else {
            result = None;
            return;
        };
        let Ok(mut slot) = studio.host.clip_preview_reader.try_borrow_mut() else {
            result = None;
            return;
        };
        let Some(reader) = slot.as_mut() else {
            return;
        };
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = reader.cancel(&ticket);
            reader.maintain();
            result = Some(Err("unsupportedPreview"));
            return;
        }
        result = match reader.poll(&ticket) {
            PreviewPoll::Pending => None,
            PreviewPoll::Ready(pixels) => Some(Ok(pixels)),
            PreviewPoll::Failed(_) => Some(Err("unsupportedPreview")),
        };
    });
    if let Some(result) = result {
        let _ = sender.send(result);
    } else if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
        // Dropping the ticket cancels it. The long-lived reader retains the
        // physical slot until queue_done; the next capture advances cleanup.
        let _ = sender.send(Err("unsupportedPreview"));
    } else {
        slint::Timer::single_shot(Duration::from_millis(8), move || {
            poll(ticket, sender, cancelled, deadline)
        });
    }
}

#[derive(Clone)]
struct Observation {
    instance: String,
    ids: DocumentIds,
    origin: canvas_policy::OutputOriginCapture,
    authorized: bool,
    playing: bool,
    busy: bool,
    revision: u64,
    context_revision: u64,
    selection_revision: u64,
    frame: ClipAdoptedFrame,
    view_state_id: String,
}

fn matches_capture(capture: &Capture, current: &Observation) -> bool {
    current.instance == capture.request.instance_id
        && current.origin == capture.origin
        && current.authorized
        && current.ids.project == capture.ids.project
        && current.ids.session == capture.ids.session
        && !current.playing
        && !current.busy
        && current.revision == capture.revision
        && current.context_revision == capture.context_revision
        && current.selection_revision == capture.selection_revision
        && current.frame == ClipAdoptedFrame::Ready(capture.frame.clone())
        && current.view_state_id == capture.view_state_id
}

fn current(capture: &Capture, bridge: &BridgeUi, studio: &Studio, ids: &DocumentIds) -> bool {
    let Ok(origin) = origin(bridge) else {
        return false;
    };
    let frame = studio.monitor.adopted_frame(studio);
    let view_state_id = view_id(bridge, studio, ids, &frame);
    matches_capture(
        capture,
        &Observation {
            instance: bridge.instance_id.clone(),
            ids: ids.clone(),
            origin,
            authorized: bridge.authorized(&capture.request, &ids.project, true),
            playing: studio.playing,
            busy: studio.mcp_clip_state().3 || studio.monitor_inputs_busy(),
            revision: studio.mcp_clip_state().1,
            context_revision: studio.mcp_context_epoch(),
            selection_revision: studio.mcp_selection_epoch(),
            frame,
            view_state_id,
        },
    )
}

fn encode(
    pixels: PreviewRgba,
    source: &ClipPreviewFrame,
    max_edge: u32,
) -> Result<(Vec<u8>, u32, u32), &'static str> {
    let (width, height) =
        clip_preview_dimensions(source.source_width, source.source_height, max_edge)
            .ok_or("unsupportedPreview")?;
    let source_size = (source.source_width, source.source_height);
    if (pixels.width, pixels.height) != source_size
        && (pixels.width, pixels.height) != (width, height)
    {
        return Err("ioFailure");
    }
    if u64::from(pixels.width) * u64::from(pixels.height) > 4_194_304 {
        return Err("unsupportedPreview");
    }
    if pixels.pixels.len() != pixels.width as usize * pixels.height as usize * 4 {
        return Err("ioFailure");
    }
    let image = image::RgbaImage::from_raw(pixels.width, pixels.height, pixels.pixels)
        .ok_or("ioFailure")?;
    let thumb = if image.dimensions() == (width, height) {
        image
    } else {
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
    };
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| "ioFailure")?;
        writer
            .write_image_data(thumb.as_raw())
            .map_err(|_| "ioFailure")?;
        writer.finish().map_err(|_| "ioFailure")?;
    }
    if bytes.is_empty() || bytes.len() > 524_288 {
        return Err("resourceLimit");
    }
    Ok((bytes, width, height))
}

/// IPC worker: only dedicated captured pixels cross here. The final serialized
/// UI check is after encoding; stale/revoked requests never return image bytes.
pub(super) fn dispatch(job: Job) -> Response {
    let result = job
        .pixels
        .recv_timeout(job.deadline.saturating_duration_since(Instant::now()));
    let pixels = match result {
        Ok(Ok(pixels)) => pixels,
        Ok(Err(status)) => return Response::error(status),
        Err(_) => return Response::error("unsupportedPreview"),
    };
    let (bytes, width, height) = match encode(
        pixels,
        &job.capture.frame,
        job.capture.request.max_edge.unwrap_or(256),
    ) {
        Ok(result) => result,
        Err(status) => return Response::error(status),
    };
    if Instant::now() >= job.deadline {
        return Response::error("unsupportedPreview");
    }
    let capture = &job.capture;
    let data = ClipPreviewData {
        version: 1,
        tool: "clip_preview".into(),
        source: "adoptedMonitorFrame".into(),
        app_instance_id: capture.request.instance_id.clone(),
        client_id: capture.request.client_id.clone(),
        project_id: capture.ids.project.clone(),
        document_session_id: capture.ids.session.clone(),
        revision: capture.revision,
        context_revision: capture.context_revision,
        selection_revision: capture.selection_revision,
        view_state_id: capture.view_state_id.clone(),
        frame: capture.frame.clone(),
        playhead_seconds: capture.playhead_seconds,
        width,
        height,
        mime_type: "image/png".into(),
        byte_length: bytes.len(),
        png_sha256: format!("{:x}", Sha256::digest(&bytes)),
        captured_at_unix_ms: capture.captured_at_unix_ms,
        viewport_capture: false,
        includes_overlays: false,
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    if data.validate_for_request(&capture.request).is_err() {
        return Response::error("ioFailure");
    }
    // Job must remain alive until the final callback returns, for cancellation.
    let capture = capture.clone();
    let cancelled = job.cancelled.clone();
    let deadline = job.deadline;
    match on_ui(move |shell, app| {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Response::error("unsupportedPreview");
        }
        let Ok(studio) = shell.studio.try_borrow() else {
            return Response::error("busy");
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return Response::error("busy");
        };
        let Some((kind, ids)) = bridge.active_ids(&studio, &app) else {
            return Response::error("wrongProject");
        };
        if kind != "clip" || !current(&capture, &bridge, &studio, &ids) {
            return Response::error("stalePreview");
        }
        Response::ok(serde_json::to_value(data).expect("typed clip preview"))
    }) {
        Ok(response) => response,
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> ClipPreviewFrame {
        ClipPreviewFrame {
            state: "ready".into(),
            version: 1,
            source: "adoptedMonitorFrame".into(),
            frame_id: format!("clip-frame-v1-{}", "a".repeat(64)),
            generation: 3,
            revision: 7,
            timeline_id: "timeline:main".into(),
            rendered_frame_time_seconds: 1.0,
            source_width: width,
            source_height: height,
        }
    }

    #[test]
    fn retained_rgba_is_encoded_as_real_full_frame_rgba8_png() {
        // Every corner is distinct: a top-left crop, vertical flip, or alpha
        // loss cannot pass merely by returning a plausible PNG header.
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 0,
        ];
        let (bytes, w, h) = encode(
            PreviewRgba {
                width: 2,
                height: 2,
                pixels: pixels.clone(),
            },
            &frame(2, 2),
            256,
        )
        .unwrap();
        assert_eq!((w, h), (2, 2));
        let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes))
            .read_info()
            .unwrap();
        assert_eq!(reader.info().color_type, png::ColorType::Rgba);
        assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
        assert!(!reader.info().interlaced);
        let mut output = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut output).unwrap();
        assert_eq!(&output[..info.buffer_size()], pixels.as_slice());
        assert!(bytes.len() <= 524_288);
    }

    #[test]
    fn source_budget_and_floor_dimensions_are_enforced_before_encoding() {
        let pixels = vec![40; 1000 * 333 * 4];
        let (bytes, w, h) = encode(
            PreviewRgba {
                width: 1000,
                height: 333,
                pixels,
            },
            &frame(1000, 333),
            256,
        )
        .unwrap();
        assert_eq!((w, h), (256, 85));
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (256, 85));
        assert_eq!(decoded.get_pixel(255, 84).0, [40; 4]);
        assert!(
            encode(
                PreviewRgba {
                    width: 4096,
                    height: 4096,
                    pixels: vec![]
                },
                &frame(4096, 4096),
                256
            )
            .is_err()
        );
        assert!(
            encode(
                PreviewRgba {
                    width: 3,
                    height: 2,
                    pixels: vec![0; 24]
                },
                &frame(4, 2),
                256
            )
            .is_err()
        );
    }

    #[test]
    fn native_capture_slot_serializes_cpu_and_gpu_and_releases_on_drop() {
        let busy = Arc::new(AtomicBool::new(false));
        let slot = CaptureSlot::claim(&busy).unwrap();
        assert!(CaptureSlot::claim(&busy).is_none());
        drop(slot);
        assert!(CaptureSlot::claim(&busy).is_some());
    }
    #[test]
    fn final_delivery_rejects_revoke_account_document_view_and_frame_changes() {
        let request:Request=serde_json::from_value(json!({"instanceId":"instance","clientId":"reader","clientToken":"read-token","method":"clipPreview","projectId":"project","documentSessionId":"opening","revision":7,"contextRevision":11,"clipPreviewQuery":{"frameId":frame(2,2).frame_id}})).unwrap();
        let capture = Capture {
            request: request.clone(),
            ids: DocumentIds {
                project: "project".into(),
                session: "opening".into(),
            },
            origin: canvas_policy::OutputOriginCapture {
                account_scope: "account-a".into(),
                assistant_session: Some("conversation-a".into()),
            },
            revision: 7,
            context_revision: 11,
            selection_revision: 4,
            view_state_id: format!("view-v1-{}", "b".repeat(64)),
            frame: frame(2, 2),
            playhead_seconds: 1.01,
            captured_at_unix_ms: 1,
        };
        let original = Observation {
            instance: request.instance_id.clone(),
            ids: capture.ids.clone(),
            origin: capture.origin.clone(),
            authorized: true,
            playing: false,
            busy: false,
            revision: 7,
            context_revision: 11,
            selection_revision: 4,
            frame: ClipAdoptedFrame::Ready(capture.frame.clone()),
            view_state_id: capture.view_state_id.clone(),
        };
        assert!(matches_capture(&capture, &original));
        let mut variants = Vec::new();
        macro_rules! change {
            ($field:ident,$value:expr) => {{
                let mut value = original.clone();
                value.$field = $value;
                variants.push(value);
            }};
        }
        change!(authorized, false);
        change!(playing, true);
        change!(busy, true);
        change!(revision, 8);
        change!(context_revision, 12);
        change!(selection_revision, 5);
        change!(instance, "another-instance".into());
        change!(view_state_id, "new-view".into());
        change!(
            ids,
            DocumentIds {
                project: "other-project".into(),
                session: "opening".into()
            }
        );
        change!(
            ids,
            DocumentIds {
                project: "project".into(),
                session: "reopened".into()
            }
        );
        change!(
            origin,
            canvas_policy::OutputOriginCapture {
                account_scope: "account-b".into(),
                assistant_session: capture.origin.assistant_session.clone()
            }
        );
        change!(
            origin,
            canvas_policy::OutputOriginCapture {
                account_scope: "account-a".into(),
                assistant_session: Some("conversation-b".into())
            }
        );
        let mut next_frame = capture.frame.clone();
        next_frame.frame_id = format!("clip-frame-v1-{}", "c".repeat(64));
        change!(frame, ClipAdoptedFrame::Ready(next_frame));
        change!(
            frame,
            ClipAdoptedFrame::Unavailable(concat_editor_mcp::ClipPreviewUnavailable {
                state: "unavailable".into(),
                version: 1,
                reason: concat_editor_mcp::ClipPreviewUnavailableReason::Pending
            })
        );
        for value in variants {
            assert!(!matches_capture(&capture, &value));
        }
        // Exercise the real native read/media-grant predicate used by current.
        let mut bridge = BridgeUi::new();
        bridge.grants.insert(
            "reader".into(),
            Grant {
                token: "read-token".into(),
                project: "project".into(),
                media: true,
            },
        );
        assert!(bridge.authorized(&request, "project", true));
        bridge.grants.get_mut("reader").unwrap().media = false;
        assert!(!bridge.authorized(&request, "project", true));
        bridge.grants.remove("reader");
        assert!(!bridge.authorized(&request, "project", true));
    }
}
