// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
fn cube(swap: bool) -> String {
    let mut s = "TITLE \"Synthetic channel look\"\nLUT_3D_SIZE 2\n".to_owned();
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                let c = if swap { [b, 1 - r, g] } else { [r, g, b] };
                s.push_str(&format!("{} {} {}\n", c[0], c[1], c[2]));
            }
        }
    }
    s
}
fn lut(p: &CanvasPane, id: concat_canvas::LayerId) -> &CubeLut {
    let LayerNode::Adjustment(a) = p.document.as_ref().unwrap().find(id).unwrap() else {
        panic!("adjustment")
    };
    let Adjustment::ColorLookup { lut: Some(lut), .. } = &a.adjustment else {
        panic!("loaded LUT")
    };
    lut
}
fn pane() -> CanvasPane {
    let mut p = super::super::c02_tests::pane();
    p.agent_adjustment_add(10);
    p
}
#[test]
fn lookup_import_failure_cancel_stale_and_copy_isolation() {
    let mut p = pane();
    assert_eq!(p.lookup_name(), "未选择 LUT");
    let a = CubeLut::parse(&cube(true), "a").unwrap();
    let b = CubeLut::parse(&cube(false), "b").unwrap();
    let before = p.snapshot().unwrap();
    let undo = p.undo_stack.len();
    let cancelled = p.begin_lookup_import().unwrap(); // Cancel means no completion call.
    assert!(p.snapshot().unwrap().same_state(&before));
    let target = p.begin_lookup_import().unwrap();
    assert!(p.finish_lookup_import(cancelled, Ok(a.clone())).is_err());
    assert!(
        p.finish_lookup_import(target, Err("broken file".into()))
            .is_err()
    );
    assert!(p.snapshot().unwrap().same_state(&before));
    assert_eq!(p.undo_stack.len(), undo);
    let target = p.begin_lookup_import().unwrap();
    p.agent_adjustment_param(0, 0.6);
    let edited = p.snapshot().unwrap();
    assert!(p.finish_lookup_import(target, Ok(a.clone())).is_err());
    assert!(p.snapshot().unwrap().same_state(&edited));
    let target = p.begin_lookup_import().unwrap();
    p.agent_adjustment_param(0, 0.7);
    p.agent_undo(); // Same revision again must not revive an older import response.
    assert!(p.finish_lookup_import(target, Ok(a.clone())).is_err());
    let target = p.begin_lookup_import().unwrap();
    p.mcp_binding_epoch += 1;
    assert!(p.finish_lookup_import(target, Ok(a.clone())).is_err());
    let target = p.begin_lookup_import().unwrap();
    p.document_generation += 1;
    assert!(p.finish_lookup_import(target, Ok(a.clone())).is_err());
    let target = p.begin_lookup_import().unwrap();
    p.agent_layer_pick(1);
    assert!(p.finish_lookup_import(target, Ok(a.clone())).is_err());
    p.agent_layer_pick(0);
    let target = p.begin_lookup_import().unwrap();
    let original = p.active.unwrap();
    p.begin_parameter_gesture(ParameterTarget::Adjustment(original, 0));
    p.set_adjustment_param_for(original, 0, 0.3);
    assert!(p.finish_lookup_import(target, Ok(a.clone())).is_err());
    p.finish_parameter_gesture(false);
    let target = p.begin_lookup_import().unwrap();
    p.finish_lookup_import(target, Ok(a.clone())).unwrap();
    assert_eq!(
        p.undo_stack.last().unwrap().retained_bytes,
        8 * 3 * 4,
        "LUT storage is budgeted"
    );
    let loaded = p.snapshot().unwrap();
    p.agent_undo();
    assert_eq!(p.lookup_name(), "未选择 LUT");
    p.agent_redo();
    assert!(p.snapshot().unwrap().same_state(&loaded));
    p.history_edit(|p| p.duplicate_row(0));
    let copy = p.active.unwrap();
    assert_ne!(copy, original);
    assert_eq!(
        lut(&p, original).values().as_ptr(),
        lut(&p, copy).values().as_ptr()
    );
    p.agent_adjustment_param(0, 0.2);
    assert_eq!(
        p.undo_stack.last().unwrap().retained_bytes,
        0,
        "parameter edits share the immutable table"
    );
    let copied = p.snapshot().unwrap();
    let target = p.begin_lookup_import().unwrap();
    p.finish_lookup_import(target, Ok(b.clone())).unwrap();
    assert_eq!(lut(&p, original).digest(), a.digest());
    assert_eq!(lut(&p, copy).digest(), b.digest());
    p.agent_undo();
    assert!(p.snapshot().unwrap().same_state(&copied));
    p.agent_redo();
    assert_eq!(lut(&p, copy).digest(), b.digest());
    p.agent_layer_pick(1);
    assert_eq!(p.agent_adjustment_state().1[0].1, 0.6);
}

#[test]
fn lookup_file_reader_rejects_wrong_extension_encoding_and_large_input() {
    let dir = std::env::temp_dir().join(format!("seecut-lut-input-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let wrong = dir.join("bad.3dl");
    std::fs::write(&wrong, cube(false)).unwrap();
    assert!(read_cube(&wrong).is_err());
    let path = dir.join("safe.CUBE");
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    assert!(read_cube(&path).is_err());
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_FILE_BYTES as u64 + 1).unwrap();
    drop(file);
    assert!(read_cube(&path).is_err());
    std::fs::write(&path, cube(true)).unwrap();
    assert_eq!(read_cube(&path).unwrap().title(), "Synthetic channel look");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated portable executable and SEECUT_LOOKUP_FIXTURE_DIR"]
fn lookup_project_owns_resource_after_source_deletion_and_export_deduplicates() {
    use concat_canvas::vector::{ShapeKind, VectorShape};
    let dir =
        std::fs::canonicalize(std::env::var_os("SEECUT_LOOKUP_FIXTURE_DIR").expect("fixture"))
            .unwrap();
    let data = concat_host::AppDirs::locate().unwrap().data;
    assert_eq!(data, dir.join("portable"), "never touch user data");
    let mut p = super::super::c02_tests::pane();
    let shape = VectorShape::new(ShapeKind::Ellipse, [50.0, 50.0], [180.0, 160.0]);
    let pixels = p.store.put(shape.rasterize(640, 360).unwrap());
    let doc = p.document.as_mut().unwrap();
    let id = doc.new_layer("Editable shape", pixels);
    doc.layer_mut(id).unwrap().vector = Some(shape);
    p.active = Some(id);
    p.text_start(0, 0.0, 0.0).unwrap();
    p.text_change(0, "Color Lookup · 可编辑", 0.0).unwrap();
    p.text_change(2, "", 28.0).unwrap();
    p.text_finish(true);
    p.agent_adjustment_add(8); // Prove v4 remains readable before adding Color Lookup.
    let old = dir.join("legacy-v4.comp");
    p.agent_save_comp(&old).unwrap();
    drop(p);
    let mut p = CanvasPane::default();
    p.load_comp(&old, None).unwrap();
    p.agent_adjustment_add(10);
    let original = p.active.unwrap();
    let source = dir.join("user-selected.cube");
    std::fs::write(&source, cube(true)).unwrap();
    let target = p.begin_lookup_import().unwrap();
    p.finish_lookup_import(target, Ok(read_cube(&source).unwrap()))
        .unwrap();
    let digest = lut(&p, original).digest().to_owned();
    p.agent_adjustment_param(0, 0.8);
    p.agent_layer_mask_add(0);
    p.brush.color = [0, 0, 0];
    p.brush.diameter = 180.0;
    p.agent_paint_stroke(&[(110.0, 180.0), (220.0, 180.0)]);
    p.agent_layer_opacity(0, 0.9);
    p.history_edit(|p| p.duplicate_row(0));
    let copy = p.active.unwrap();
    assert_eq!(
        lut(&p, copy).values().as_ptr(),
        lut(&p, original).values().as_ptr()
    );
    p.agent_adjustment_param(0, 0.25);
    let final_state = p.snapshot().unwrap();
    p.agent_adjustment_param(0, 0.6);
    p.agent_undo();
    assert!(p.snapshot().unwrap().same_state(&final_state));
    p.agent_redo();
    p.agent_undo();
    let project = dir.join("color-lookup-editable.comp");
    p.agent_save_comp(&project).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["concat-project"], 5);
    assert!(
        !std::fs::read_to_string(project.join("manifest.json"))
            .unwrap()
            .contains("user-selected.cube")
    );
    let expected = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
    let mut gpu = CanvasGpu::new().expect("real GPU required");
    let actual = gpu.compose_frame(p.document.as_ref().unwrap(), &p.store);
    for (a, b) in actual.pixels().iter().zip(expected.pixels()) {
        assert!((*a as i16 - *b as i16).abs() <= 1);
    }
    std::fs::remove_file(&source).unwrap();
    drop(p);
    let mut p = CanvasPane::default();
    p.load_comp(&project, None).unwrap();
    assert_eq!(lut(&p, original).digest(), digest);
    assert_eq!(p.agent_adjustment_state().0, 10);
    assert_eq!(
        concat_canvas::compose(p.document.as_ref().unwrap(), &p.store).pixels(),
        expected.pixels()
    );
    p.agent_adjustment_param(0, 0.5);
    p.agent_undo();
    let job = CanvasExportJob {
        snapshot: p.save_data().unwrap(),
        project_path: Some(project.clone()),
        path: data.join("canvas-exports/color-lookup.png"),
        export_stem: "Color Lookup synthetic".into(),
        generation: p.document_generation,
        binding_epoch: p.mcp_binding_epoch,
        revision: p.revision,
        registration_id: "color-lookup-review-export".into(),
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
    let root = data.join("personal-library");
    let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
    assert_eq!(library.items().len(), 1);
    let asset = library.get(&reference.asset_id).unwrap();
    let path = crate::personal_library::checked_managed_version_path(&root, asset).unwrap();
    crate::personal_library::verify_asset_reference(&path, Some(&reference)).unwrap();
    let (w, h, rgba) = decode_png(&std::fs::read(&path).unwrap(), None).unwrap();
    assert_eq!(rgba, expected.pixels());
    std::fs::copy(&path, dir.join("color-lookup.png")).unwrap();
    let mut destination = super::super::c02_tests::pane();
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
        crate::personal_library::Library::load_for_relink(&root)
            .unwrap()
            .items()
            .len(),
        1
    );
    std::fs::write(dir.join("export-receipt.json"),serde_json::to_vec_pretty(&serde_json::json!({"asset":reference,"sha256":receipt.sha256,"lut_sha256":digest,"library_count":1,"source_file_deleted":true,"editable_reopen":true,"canvas_reimport":true,"actual_ui":"not verified","clip_ui_reuse":"not verified"})).unwrap()).unwrap();
    // Invalid digest/version cannot partially replace the currently edited project.
    drop(p);
    let before = destination.snapshot().unwrap();
    let mut broken = manifest;
    broken["concat-project"] = serde_json::json!(4);
    std::fs::write(
        project.join("manifest.json"),
        serde_json::to_vec(&broken).unwrap(),
    )
    .unwrap();
    assert!(destination.load_comp(&project, None).is_err());
    assert!(destination.snapshot().unwrap().same_state(&before));
    broken["concat-project"] = serde_json::json!(5);
    std::fs::write(
        project.join("manifest.json"),
        serde_json::to_vec(&broken).unwrap(),
    )
    .unwrap();
}
