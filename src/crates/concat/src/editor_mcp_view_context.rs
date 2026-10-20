// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only native view state. Coordinates belong to the bound document,
//! never the desktop. Unknown Slint-local geometry stays explicitly unknown.

use concat_canvas::{CanvasViewport, Mask};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::studio::Studio;

pub(super) struct Binding<'a> {
    pub instance: &'a str,
    pub project: &'a str,
    pub session: &'a str,
    pub revision: u64,
    pub context_revision: u64,
    pub selection_revision: u64,
}

fn size(width: f64, height: f64) -> Value {
    json!({"width":width,"height":height})
}

fn rect((x, y, width, height): (f64, f64, f64, f64)) -> Value {
    json!({"x":x,"y":y,"width":width,"height":height})
}

/// None means the viewport has not been measured. A zero-sized rectangle
/// means a measured viewport currently shows none of the document.
fn visible_document_rect(viewport: &CanvasViewport, document: (f64, f64)) -> Option<Value> {
    let (width, height) = viewport.view_size();
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let (left, top) = viewport.document_point((0.0, 0.0), document);
    let (right, bottom) = viewport.document_point((width, height), document);
    let x = left.clamp(0.0, document.0);
    let y = top.clamp(0.0, document.1);
    Some(rect((
        x,
        y,
        (right.clamp(0.0, document.0) - x).max(0.0),
        (bottom.clamp(0.0, document.1) - y).max(0.0),
    )))
}

fn canvas_data(
    viewport: &CanvasViewport,
    document: (f64, f64),
    selected_layer: Option<String>,
    selection: Option<&Mask>,
    painting_layer_mask: bool,
) -> Value {
    let (width, height) = viewport.view_size();
    let (x, y) = viewport.pan();
    json!({
        "viewport": if width > 0.0 && height > 0.0 {Some(size(width,height))}else{None},
        "zoom":{"percent":viewport.zoom()*100.0,"logicalPixelsPerDocumentPixel":viewport.points_per_pixel()},
        "pan":{"x":x,"y":y},
        "stageRect":if width > 0.0 && height > 0.0 {Some(rect(viewport.document_rect(document)))}else{None},
        "visibleDocumentRect":visible_document_rect(viewport,document),
        "documentSize":size(document.0,document.1),
        "selectedLayerId":selected_layer,
        "pixelSelectionBounds":selection.and_then(Mask::bounds).map(|(x,y,w,h)|rect((x as f64,y as f64,w as f64,h as f64))),
        // Bounds alone cannot distinguish masks with different holes/coverage.
        "pixelSelectionStateId":selection.map(mask_state_id),
        "paintingLayerMask":painting_layer_mask,
    })
}

fn mask_state_id(mask: &Mask) -> String {
    let mut digest = Sha256::new();
    digest.update(mask.width.to_le_bytes());
    digest.update(mask.height.to_le_bytes());
    digest.update(&mask.bytes);
    format!("mask-v1-{:x}", digest.finalize())
}

fn finish(kind: &str, binding: &Binding<'_>, data: Value, unavailable: &[&str]) -> Value {
    // The conservative publish epoch can change without the view changing.
    // Bind the fingerprint to the actual state and document identity/revision,
    // rather than making every incidental publish look like a view change.
    let identity = json!({
        "appInstanceId":binding.instance,"projectId":binding.project,
        "documentSessionId":binding.session,"revision":binding.revision,
        "kind":kind,"state":data,
    });
    let state_id = format!(
        "view-v1-{:x}",
        Sha256::digest(serde_json::to_vec(&identity).expect("native view values serialize"))
    );
    let mut result = json!({
        "version":1,"kind":kind,"viewStateId":state_id,
        "binding":{
            "appInstanceId":binding.instance,"projectId":binding.project,
            "documentSessionId":binding.session,"revision":binding.revision,
            "contextRevision":binding.context_revision,"selectionRevision":binding.selection_revision,
        },
        "coordinateSpaces":{
            "viewport":"slintLogicalPixelsTopLeftYDown",
            "document":"documentPixelsTopLeftYDown",
            "timeline":"secondsFromTimelineStart",
        },
        "unavailable":unavailable,
    });
    result[kind] = data;
    result
}

pub(super) fn snapshot(studio: &Studio, kind: &str, binding: &Binding<'_>) -> Value {
    snapshot_with_frame(studio, kind, binding, None)
}

/// Capture already validated in the same UI callback; reuse it so identity
/// construction does not hash the native render inputs a second time.
pub(super) fn clip_snapshot(
    studio: &Studio,
    binding: &Binding<'_>,
    frame: &concat_editor_mcp::ClipAdoptedFrame,
) -> Value {
    snapshot_with_frame(studio, "clip", binding, Some(frame))
}

fn snapshot_with_frame(
    studio: &Studio,
    kind: &str,
    binding: &Binding<'_>,
    frame: Option<&concat_editor_mcp::ClipAdoptedFrame>,
) -> Value {
    if kind == "canvas" {
        let Some(document) = studio.canvas.agent_document_size() else {
            return finish(kind, binding, Value::Null, &["canvas.document"]);
        };
        let data = canvas_data(
            studio.canvas.nav.viewport(),
            document,
            studio
                .canvas
                .active
                .map(|id| format!("layer:{}", id.as_u64())),
            studio.canvas.selection.as_ref(),
            studio.canvas.agent_paint_mask(),
        );
        let unavailable = if data["viewport"].is_null() {
            vec![
                "canvas.viewport",
                "canvas.stageRect",
                "canvas.visibleDocumentRect",
            ]
        } else {
            vec![]
        };
        return finish(kind, binding, data, &unavailable);
    }

    let timeline = studio.timeline();
    let mut selected_clips = studio
        .selection
        .iter()
        .map(|id| format!("clip:{id}"))
        .collect::<Vec<_>>();
    selected_clips.sort();
    let mut selected_media = studio
        .media
        .selected
        .iter()
        .map(|id| format!("media:{id}"))
        .collect::<Vec<_>>();
    selected_media.sort();
    let (width, height) = studio.output_size();
    let rendered = studio.monitor.image.size();
    let data = json!({
        "timelineId":format!("timeline:{}",timeline.id),
        "playheadSeconds":studio.playhead,"playing":studio.playing,
        "timeline":{
            "visibleStartSeconds":studio.scroll_left,
            "secondsPerLogicalPixel":studio.seconds_per_pixel,
            "visibleEndSeconds":null,"viewport":null,
        },
        "preview":{
            "adoptedFrame": frame.cloned().unwrap_or_else(|| studio.monitor.adopted_frame(studio)),
            "outputSize":size(width as f64,height as f64),
            // This image can lag the playhead while a frame is rendering.
            "renderedImageSize":if rendered.width>0 && rendered.height>0 {Some(size(rendered.width as f64,rendered.height as f64))}else{None},
            "viewport":null,"visibleDocumentRect":null,
        },
        "selectedClipIds":selected_clips,"selectedMediaIds":selected_media,
    });
    // Preview zoom/pan and timeline dimensions/vertical scroll currently live
    // only inside Slint components. Dock outer boxes cannot establish them.
    let mut unavailable = vec![
        "clip.timeline.viewport",
        "clip.timeline.visibleEndSeconds",
        "clip.timeline.verticalScroll",
        "clip.preview.viewport",
        "clip.preview.visibleDocumentRect",
        "clip.preview.zoom",
        "clip.preview.pan",
    ];
    if data["preview"]["renderedImageSize"].is_null() {
        unavailable.push("clip.preview.renderedImageSize");
    }
    finish(kind, binding, data, &unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding<'static> {
        Binding {
            instance: "app",
            project: "project",
            session: "session",
            revision: 3,
            context_revision: 8,
            selection_revision: 2,
        }
    }

    fn viewport() -> CanvasViewport {
        let mut view = CanvasViewport::default();
        view.resize((200.0, 100.0), 1.0, None);
        view.set_zoom(1.0, view.center(), (400.0, 200.0));
        view
    }

    #[test]
    fn native_view_context_uses_document_intersection_and_logical_pixel_scale() {
        let mut view = viewport();
        assert_eq!(
            visible_document_rect(&view, (400.0, 200.0)),
            Some(rect((100.0, 50.0, 200.0, 100.0)))
        );
        view.translate((50.0, -25.0));
        assert_eq!(
            visible_document_rect(&view, (400.0, 200.0)),
            Some(rect((50.0, 75.0, 200.0, 100.0)))
        );
        // A 2x backing scale: zoom 2 means one document pixel per logical pixel.
        view.resize((200.0, 100.0), 2.0, None);
        view.set_zoom(2.0, view.center(), (400.0, 200.0));
        assert_eq!(view.points_per_pixel(), 1.0);
        let data = canvas_data(&view, (400.0, 200.0), None, None, false);
        assert_eq!(data["stageRect"]["width"], 400.0);
        assert_eq!(data["zoom"]["percent"], 200.0);
    }

    #[test]
    fn native_view_context_distinguishes_unmeasured_and_offscreen() {
        assert!(visible_document_rect(&CanvasViewport::default(), (400.0, 200.0)).is_none());
        let mut view = viewport();
        view.translate((1000.0, 0.0));
        assert_eq!(
            visible_document_rect(&view, (400.0, 200.0)),
            Some(rect((0.0, 50.0, 0.0, 100.0)))
        );
    }

    #[test]
    fn native_view_context_fingerprint_tracks_view_selection_and_binding() {
        let mut view = viewport();
        let data = canvas_data(&view, (400.0, 200.0), Some("layer:1".into()), None, false);
        let original = finish("canvas", &binding(), data.clone(), &[]);
        assert_eq!(original, finish("canvas", &binding(), data.clone(), &[]));
        let mut bound = binding();
        bound.context_revision += 1;
        bound.selection_revision += 1;
        assert_eq!(
            original["viewStateId"],
            finish("canvas", &bound, data.clone(), &[])["viewStateId"]
        );
        bound.session = "other";
        assert_ne!(
            original["viewStateId"],
            finish("canvas", &bound, data.clone(), &[])["viewStateId"]
        );
        bound = binding();
        bound.revision += 1;
        assert_ne!(
            original["viewStateId"],
            finish("canvas", &bound, data.clone(), &[])["viewStateId"]
        );
        for changed in [
            {
                view.translate((5.0, 0.0));
                canvas_data(&view, (400.0, 200.0), Some("layer:1".into()), None, false)
            },
            {
                view.set_zoom(2.0, view.center(), (400.0, 200.0));
                canvas_data(&view, (400.0, 200.0), Some("layer:1".into()), None, false)
            },
            canvas_data(
                &viewport(),
                (400.0, 200.0),
                Some("layer:2".into()),
                None,
                false,
            ),
        ] {
            assert_ne!(
                original["viewStateId"],
                finish("canvas", &binding(), changed, &[])["viewStateId"]
            );
        }
    }

    #[test]
    fn native_view_context_distinguishes_masks_with_equal_bounds() {
        let all = Mask::all(3, 3);
        let mut hole = all.clone();
        hole.bytes[4] = 0;
        assert_eq!(all.bounds(), hole.bounds());
        let a = canvas_data(&viewport(), (400.0, 200.0), None, Some(&all), false);
        let b = canvas_data(&viewport(), (400.0, 200.0), None, Some(&hole), false);
        assert_ne!(
            finish("canvas", &binding(), a, &[])["viewStateId"],
            finish("canvas", &binding(), b, &[])["viewStateId"]
        );
    }

    #[test]
    fn native_view_context_fingerprint_tracks_transport_and_media_selection() {
        let data = json!({"playheadSeconds":1.0,"playing":false,"selectedClipIds":["clip:a"],"selectedMediaIds":["media:a"]});
        let original = finish("clip", &binding(), data.clone(), &[]);
        for (key, value) in [
            ("playheadSeconds", json!(2.0)),
            ("playing", json!(true)),
            ("selectedClipIds", json!(["clip:b"])),
            ("selectedMediaIds", json!(["media:b"])),
        ] {
            let mut changed = data.clone();
            changed[key] = value;
            assert_ne!(
                original["viewStateId"],
                finish("clip", &binding(), changed, &[])["viewStateId"]
            );
        }
    }
    #[test]
    fn adopted_frame_identity_and_unavailable_transition_change_native_view_id() {
        let data = json!({"preview":{"renderedImageSize":{"width":2,"height":2},"adoptedFrame":{"state":"ready","frameId":"first","renderedFrameTimeSeconds":1.0}}});
        let first = finish("clip", &binding(), data.clone(), &[]);
        let mut changed = data.clone();
        changed["preview"]["adoptedFrame"]["frameId"] = json!("second");
        assert_ne!(
            first["viewStateId"],
            finish("clip", &binding(), changed, &[])["viewStateId"]
        );
        let mut changed = data;
        changed["preview"]["adoptedFrame"] =
            json!({"state":"unavailable","version":1,"reason":"pending"});
        assert_ne!(
            first["viewStateId"],
            finish("clip", &binding(), changed, &[])["viewStateId"]
        );
    }
}
