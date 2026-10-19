// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use concat_canvas::BlendMode;

pub(super) fn pane() -> CanvasPane {
    let mut p = CanvasPane::default();
    let mut frame = Frame::transparent(640, 360);
    let colors = [
        [140, 110, 110],
        [240, 10, 10],
        [40, 180, 150],
        [128, 128, 128],
        [50, 80, 210],
    ];
    for y in 0..360 {
        for x in 0..640 {
            let c = colors[(x / 128) as usize];
            frame.set_pixel(x, y, [c[0], c[1], c[2], if y < 300 { 255 } else { 128 }]);
        }
    }
    let pixels = p.store.put(frame);
    let mut doc = ImageDocument::new(640, 360);
    p.active = Some(doc.new_layer("Synthetic colour strips", pixels));
    p.layer = Some(pixels);
    p.document = Some(doc);
    p.nav.set_document(Some((640.0, 360.0)));
    p.nav
        .viewport_mut()
        .resize((640.0, 360.0), 1.0, Some((640.0, 360.0)));
    p
}

#[test]
fn c02_parameters_defaults_history_cancel_and_pinned_target() {
    let mut p = pane();
    assert_eq!(p.agent_adjustment_kinds().len(), 10);
    for kind in 1..=10 {
        let a = default_adjustment(kind).unwrap();
        assert_eq!(adjustment_kind(&a), kind);
    }
    p.agent_adjustment_add(8);
    let vibrance = p.active.unwrap();
    assert_eq!(p.agent_adjustment_state().1[0].1, 0.0);
    p.agent_adjustment_add(9);
    let bw = p.active.unwrap();
    let params = p.agent_adjustment_state().1;
    assert_eq!(
        params.iter().map(|p| p.1).collect::<Vec<_>>(),
        vec![0.299, 0.587, 0.114]
    );
    for (i, _, lo, hi) in params.iter().enumerate().map(|(i, p)| (i, p.1, p.2, p.3)) {
        p.agent_adjustment_param(i as i32, 5.0);
        assert_eq!(p.agent_adjustment_state().1[i].1, hi);
        p.agent_adjustment_param(i as i32, -5.0);
        assert_eq!(p.agent_adjustment_state().1[i].1, lo);
    }
    let before = p.snapshot().unwrap();
    p.agent_adjustment_param(0, f32::NAN);
    p.agent_adjustment_param(1, f32::INFINITY);
    assert!(p.snapshot().unwrap().same_state(&before));
    let count = p.undo_stack.len();
    p.begin_parameter_gesture(ParameterTarget::Adjustment(bw, 0));
    for v in [0.1, 0.2, 0.3, 0.8] {
        p.set_adjustment_param_for(bw, 0, v);
    }
    p.finish_parameter_gesture(true);
    assert_eq!(p.undo_stack.len(), count + 1);
    let changed = p.snapshot().unwrap();
    p.agent_undo();
    assert!(p.snapshot().unwrap().same_state(&before));
    p.agent_redo();
    assert!(p.snapshot().unwrap().same_state(&changed));
    p.begin_parameter_gesture(ParameterTarget::Adjustment(bw, 0));
    p.set_adjustment_param_for(bw, 0, 1.8);
    p.finish_parameter_gesture(false);
    assert!(p.snapshot().unwrap().same_state(&changed));
    // The gesture owns its original layer ID, independent of a changed pick.
    p.begin_parameter_gesture(ParameterTarget::Adjustment(bw, 0));
    p.active = Some(vibrance);
    if let Some(ParameterGesture {
        target: ParameterTarget::Adjustment(id, index),
        ..
    }) = p.parameter_gesture
    {
        p.set_adjustment_param_for(id, index, 0.4);
    } else {
        panic!("pinned target");
    }
    assert_eq!(p.agent_adjustment_state().1[0].1, 0.0);
    p.finish_parameter_gesture(false);
    assert!(p.snapshot().unwrap().same_state(&changed));
    // Existing public pick boundary cancels the open adjustment gesture.
    p.begin_parameter_gesture(ParameterTarget::Adjustment(bw, 0));
    p.set_adjustment_param_for(bw, 0, 1.8);
    p.agent_layer_pick(1);
    assert!(p.parameter_gesture.is_none());
    assert_eq!(p.active, Some(vibrance));
    assert!(matches!(&p.document.as_ref().unwrap().find(bw).unwrap(),
        LayerNode::Adjustment(a) if matches!(a.adjustment,Adjustment::BlackWhite { red, .. } if red == 0.8)));
}

/// Runs only from a copied portable executable; never touches user assets.
#[test]
#[ignore = "requires isolated portable executable and SEECUT_C02_FIXTURE_DIR"]
fn c02_editable_project_export_asset_and_reimport() {
    use concat_canvas::vector::{ShapeKind, VectorShape};
    let dir = std::fs::canonicalize(
        std::env::var_os("SEECUT_C02_FIXTURE_DIR").expect("fixture directory"),
    )
    .unwrap();
    let data = concat_host::AppDirs::locate().unwrap().data;
    assert_eq!(data, dir.join("portable"), "must be isolated portable data");
    let mut p = pane();
    // Read/write legacy versions as well as the new feature-gated v4.
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&canvas_manifest(&p.save_data().unwrap()))
            .unwrap()["concat-project"],
        1
    );
    let old = dir.join("legacy-v1.comp");
    p.agent_save_comp(&old).unwrap();
    drop(p);
    let mut p = CanvasPane::default();
    p.load_comp(&old, None).unwrap();
    let shape = VectorShape::new(ShapeKind::Ellipse, [50.0, 50.0], [180.0, 160.0]);
    let pixels = p.store.put(shape.rasterize(640, 360).unwrap());
    let doc = p.document.as_mut().unwrap();
    let id = doc.new_layer("Editable shape", pixels);
    doc.layer_mut(id).unwrap().vector = Some(shape);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&canvas_manifest(&p.save_data().unwrap()))
            .unwrap()["concat-project"],
        2
    );
    p.active = Some(id);
    p.text_start(0, 0.0, 0.0).unwrap();
    p.text_change(0, "自然饱和度 · Black & White", 0.0).unwrap();
    p.text_change(2, "", 28.0).unwrap();
    p.text_change(3, "#ffffff", 0.0).unwrap();
    p.text_finish(true);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&canvas_manifest(&p.save_data().unwrap()))
            .unwrap()["concat-project"],
        3
    );
    p.agent_adjustment_add(8);
    p.agent_adjustment_param(0, 0.8);
    p.agent_adjustment_add(9);
    let bw = p.active.unwrap();
    p.agent_adjustment_param(0, 0.4);
    p.agent_layer_mask_add(0);
    let mask = p
        .document
        .as_ref()
        .unwrap()
        .find(bw)
        .unwrap()
        .mask()
        .unwrap()
        .pixels;
    p.brush.color = [0, 0, 0];
    p.brush.diameter = 180.0;
    p.agent_paint_stroke(&[(120.0, 180.0), (220.0, 180.0)]);
    assert!(
        p.store
            .get(mask)
            .unwrap()
            .pixels()
            .chunks_exact(4)
            .any(|v| v[0] < 128)
    );
    p.agent_layer_opacity(0, 0.8);
    p.history_edit(|p| {
        let a = p.document.as_mut().unwrap().adjustment_mut(bw).unwrap();
        a.name = "局部黑白".into();
        a.blend = BlendMode::Luminosity;
    });
    let before = p.snapshot().unwrap();
    p.agent_layer_move(0, 1);
    let moved = p.snapshot().unwrap();
    assert!(!moved.same_state(&before));
    p.agent_undo();
    assert!(p.snapshot().unwrap().same_state(&before));
    p.agent_redo();
    assert!(p.snapshot().unwrap().same_state(&moved));
    let row = p.rows().iter().position(|(n, _)| n.id() == bw).unwrap() as i32;
    p.agent_layer_mask_toggle(row);
    p.agent_undo();
    p.agent_layer_mask_remove(row);
    p.agent_undo();
    p.agent_layer_toggle_visibility(row);
    p.agent_undo();
    assert!(p.snapshot().unwrap().same_state(&moved));
    p.agent_layer_pick(row);
    let project = dir.join("adjustments-editable.comp");
    p.agent_save_comp(&project).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["concat-project"], 4);
    let expected = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
    let mut gpu = CanvasGpu::new().expect("hardware GPU required");
    let preview = gpu.compose_frame(p.document.as_ref().unwrap(), &p.store);
    for (a, b) in preview.pixels().iter().zip(expected.pixels()) {
        assert!((*a as i16 - *b as i16).abs() <= 1);
    }
    let job = CanvasExportJob {
        snapshot: p.save_data().unwrap(),
        project_path: Some(project.clone()),
        path: data.join("canvas-exports/adjustments.png"),
        export_stem: "C02 adjustments".into(),
        generation: p.document_generation,
        binding_epoch: p.mcp_binding_epoch,
        revision: p.revision,
        registration_id: "c02-review-export".into(),
    };
    let receipt = job.run(None).unwrap();
    assert!(
        receipt.registration_error.is_none(),
        "{:?}",
        receipt.registration_error
    );
    let reference = receipt.asset.unwrap();
    let retry = job.run(None).unwrap();
    assert!(retry.registration_error.is_none());
    assert_eq!(retry.asset.as_ref(), Some(&reference));
    let library_root = data.join("personal-library");
    let library = crate::personal_library::Library::load_for_relink(&library_root).unwrap();
    assert_eq!(library.items().len(), 1);
    let asset = library.get(&reference.asset_id).unwrap();
    let path = crate::personal_library::checked_managed_version_path(&library_root, asset).unwrap();
    crate::personal_library::verify_asset_reference(&path, Some(&reference)).unwrap();
    let (w, h, rgba) = decode_png(&std::fs::read(&path).unwrap(), None).unwrap();
    assert_eq!(rgba, expected.pixels());
    std::fs::copy(&path, dir.join("adjustments.png")).unwrap();
    drop(p);
    let mut reopened = CanvasPane::default();
    reopened.load_comp(&project, None).unwrap();
    assert_eq!(
        concat_canvas::compose(reopened.document.as_ref().unwrap(), &reopened.store).pixels(),
        expected.pixels()
    );
    assert_eq!(reopened.agent_adjustment_state().0, 9);
    assert_eq!(reopened.agent_adjustment_state().1[0].1, 0.4);
    reopened.agent_adjustment_param(0, 0.65);
    assert_eq!(reopened.agent_adjustment_state().1[0].1, 0.65);
    let mut destination = pane();
    let import = CanvasImport {
        path,
        display_name: Some(asset.name.clone()),
        library_reference: Some(reference.clone()),
    };
    destination
        .mcp_insert_library_checked(&import, Frame::from_rgba(w, h, rgba).unwrap(), |_| {
            Ok::<(), ()>(())
        })
        .ok()
        .unwrap();
    assert_eq!(
        crate::personal_library::Library::load_for_relink(&library_root)
            .unwrap()
            .items()
            .len(),
        1
    );
    std::fs::write(dir.join("export-receipt.json"),serde_json::to_vec_pretty(&serde_json::json!({
        "asset":reference,"sha256":receipt.sha256,"registration_id":receipt.registration_id,
        "library_count":1,"editable_reopen":true,"canvas_reimport":true,
        "gpu_cpu_png_max_byte_delta":1,"clip_ui_reuse":"not verified", "actual_ui":"not verified"
    })).unwrap()).unwrap();
}
