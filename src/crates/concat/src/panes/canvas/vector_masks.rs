//! Vector masks use the existing single mask slot and shape-edit transaction.
use super::*;
use concat_canvas::{
    vector::{ShapeKind, VectorShape},
    vector_mask::VectorMask,
};

impl CanvasPane {
    pub(super) fn editing_vector_mask(&self) -> bool {
        self.paint_mask
            && self
                .active
                .and_then(|id| self.document.as_ref()?.find(id))
                .and_then(|n| n.mask())
                .is_some_and(|m| m.vector.is_some())
    }

    fn vector_mask_size(&self) -> Option<(u32, u32)> {
        let doc = self.document.as_ref()?;
        let node = doc.find(self.active?)?;
        match node {
            LayerNode::Layer(layer) => {
                if !layer.transform.is_invertible() {
                    return None;
                }
                let frame = self.store.get(layer.pixels)?;
                Some((frame.width(), frame.height()))
            }
            _ => Some((doc.width, doc.height)),
        }
    }

    pub(super) fn add_vector_mask(&mut self, kind: i32) {
        let Some((width, height)) = self.vector_mask_size() else {
            return;
        };
        let Some(node) = self.active.and_then(|id| self.document.as_ref()?.find(id)) else {
            return;
        };
        if node.mask().is_some() || self.shape_session.is_some() {
            return;
        }
        let kind = match kind {
            0 => ShapeKind::Rectangle,
            1 => ShapeKind::Ellipse,
            2 => ShapeKind::Path,
            _ => return,
        };
        let anchor = match node {
            LayerNode::Layer(l) => Some(l.transform),
            _ => None,
        };
        let mut shape = VectorShape::new(
            if kind == ShapeKind::Path {
                ShapeKind::Rectangle
            } else {
                kind
            },
            [width as f32 * 0.125, height as f32 * 0.125],
            [width as f32 * 0.875, height as f32 * 0.875],
        );
        if kind == ShapeKind::Path {
            shape.kind = kind;
            let points: Vec<_> = shape.knots.iter().map(|k| k.anchor).collect();
            for (i, k) in shape.knots.iter_mut().enumerate() {
                for (axis, anchor) in k.anchor.into_iter().enumerate() {
                    k.incoming[axis] = anchor + (points[(i + 3) % 4][axis] - anchor) / 3.0;
                    k.outgoing[axis] = anchor + (points[(i + 1) % 4][axis] - anchor) / 3.0;
                }
            }
        }
        let Some(source) = VectorMask::new(shape, width, height) else {
            return;
        };
        let Some(frame) = source.rasterize() else {
            return;
        };
        let mut mask = LayerMask::new(self.store.put(frame));
        mask.vector = Some(source);
        mask.anchor = anchor;
        *self
            .document
            .as_mut()
            .unwrap()
            .find_mut(self.active.unwrap())
            .unwrap()
            .mask_slot_mut() = Some(mask);
        self.paint_mask = true;
        self.tool = 10;
        self.sync_view();
    }

    pub(super) fn refresh_vector_mask(&mut self) {
        let Some(mask) = self
            .active
            .and_then(|id| self.document.as_ref()?.find(id))
            .and_then(|n| n.mask())
        else {
            return;
        };
        let Some(frame) = mask.vector.as_ref().and_then(VectorMask::rasterize) else {
            return;
        };
        let pixels = mask.pixels;
        self.store.replace(pixels, frame);
        self.bump_thumbnail_revision(pixels);
        self.sync_view();
    }

    pub(super) fn toggle_vector_mask_link(&mut self) {
        let Some(doc) = self.document.as_mut() else {
            return;
        };
        let canvas = (doc.width as f32, doc.height as f32);
        let Some(LayerNode::Layer(layer)) = self.active.and_then(|id| doc.find_mut(id)) else {
            return;
        };
        let Some(mask) = layer.mask.as_mut().filter(|m| m.vector.is_some()) else {
            return;
        };
        let current = layer.transform;
        if !current.is_invertible() {
            return;
        }
        if !mask.linked {
            let old = mask.anchor.unwrap_or(current);
            let mut source = mask.vector.as_ref().unwrap().clone();
            let size = (source.width as f32, source.height as f32);
            for k in &mut source.geometry.knots {
                for p in [&mut k.anchor, &mut k.incoming, &mut k.outgoing] {
                    let Some(world) = old.from_bitmap((p[0], p[1]), size, canvas) else {
                        return;
                    };
                    let Some(local) = current.to_bitmap(world, size, canvas) else {
                        return;
                    };
                    *p = [local.0, local.1];
                }
            }
            if !source.validate() {
                return;
            }
            mask.vector = Some(source);
        }
        mask.anchor = Some(current);
        mask.linked = !mask.linked;
        self.refresh_vector_mask();
    }

    pub(super) fn publish_vector_mask_controls(&self, app: &crate::App) {
        let controls = app.global::<crate::CanvasControls>();
        let node = self.active.and_then(|id| self.document.as_ref()?.find(id));
        let mask = node.and_then(|n| n.mask());
        let vector = mask.is_some_and(|m| m.vector.is_some());
        controls.set_vector_mask_selected(self.editing_vector_mask());
        controls.set_vector_mask_present(vector);
        controls.set_vector_mask_linked(mask.is_some_and(|m| m.linked));
        controls.set_vector_mask_linkable(vector && matches!(node, Some(LayerNode::Layer(_))));
        controls.set_vector_mask_can_add(
            node.is_some()
                && mask.is_none()
                && self.vector_mask_size().is_some()
                && !self.has_uncommitted_edit(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_canvas::LayerTransform;

    fn pane() -> CanvasPane {
        let mut p = CanvasPane {
            document: Some(ImageDocument::new(160, 120)),
            ..Default::default()
        };
        let mut frame = Frame::transparent(96, 64);
        for y in 0..64 {
            for x in 0..96 {
                frame.set_pixel(
                    x,
                    y,
                    [
                        30 + x as u8 * 2,
                        60 + y as u8 * 2,
                        180,
                        if x < 32 { 128 } else { 255 },
                    ],
                );
            }
        }
        let pixels = p.store.put(frame);
        let id = p
            .document
            .as_mut()
            .unwrap()
            .new_layer("synthetic image", pixels);
        p.active = Some(id);
        p.layer = Some(pixels);
        p.nav.set_document(Some((160.0, 120.0)));
        p.nav
            .viewport_mut()
            .resize((160.0, 120.0), 1.0, Some((160.0, 120.0)));
        p
    }
    fn mask(p: &CanvasPane) -> &LayerMask {
        p.document
            .as_ref()
            .unwrap()
            .find(p.active.unwrap())
            .unwrap()
            .mask()
            .unwrap()
    }
    fn compose(p: &CanvasPane) -> Frame {
        concat_canvas::compose(p.document.as_ref().unwrap(), &p.store)
    }
    fn edit_anchor(p: &mut CanvasPane) {
        let anchor = mask(p).vector.as_ref().unwrap().geometry.knots[0].anchor;
        let a = p.shape_screen_point(anchor).unwrap();
        let b = p
            .shape_screen_point([anchor[0] + 6.0, anchor[1] + 4.0])
            .unwrap();
        p.shape_pointer(0, a.0, a.1);
        p.shape_pointer(1, b.0, b.1);
        p.shape_pointer(2, b.0, b.1);
    }

    #[test]
    fn vector_mask_primitives_edit_in_intrinsic_space_with_history_and_no_brush_writes() {
        for kind in 0..3 {
            let mut p = pane();
            let id = p.active.unwrap();
            p.document
                .as_mut()
                .unwrap()
                .layer_mut(id)
                .unwrap()
                .transform = LayerTransform {
                x: 7.0,
                y: -3.0,
                rotation: 0.25,
                scale_x: 1.1,
                scale_y: 0.8,
                ..Default::default()
            };
            p.history_edit(|p| p.add_vector_mask(kind));
            let created = p.snapshot().unwrap();
            let pixels = mask(&p).pixels;
            assert_eq!(
                (
                    mask(&p).vector.as_ref().unwrap().width,
                    mask(&p).vector.as_ref().unwrap().height
                ),
                (96, 64)
            );
            assert!(p.editing_vector_mask());
            assert!(p.paint_target().is_none());
            let original = mask(&p).vector.as_ref().unwrap().geometry.clone();
            edit_anchor(&mut p);
            let changed = p.snapshot().unwrap();
            let actual = mask(&p).vector.as_ref().unwrap().geometry.knots[0].anchor;
            assert!((actual[0] - original.knots[0].anchor[0] - 6.0).abs() < 0.01);
            assert!((actual[1] - original.knots[0].anchor[1] - 4.0).abs() < 0.01);
            assert_ne!(
                p.store.get(pixels).unwrap().pixels(),
                created.store.get(pixels).unwrap().pixels()
            );
            p.undo();
            assert!(p.snapshot().unwrap().same_state(&created));
            p.redo();
            assert!(p.snapshot().unwrap().same_state(&changed));
            if kind > 0 {
                let knot = mask(&p).vector.as_ref().unwrap().geometry.knots[0];
                let a = p.shape_screen_point(knot.outgoing).unwrap();
                let b = p
                    .shape_screen_point([knot.outgoing[0] + 3.0, knot.outgoing[1] - 2.0])
                    .unwrap();
                p.shape_pointer(0, a.0, a.1);
                p.shape_pointer(1, b.0, b.1);
                p.shape_pointer(2, b.0, b.1);
                let after = mask(&p).vector.as_ref().unwrap().geometry.knots[0];
                assert_eq!(after.anchor, knot.anchor);
                assert_eq!(after.incoming, knot.incoming);
                assert!((after.outgoing[0] - knot.outgoing[0] - 3.0).abs() < 0.01);
                assert!((after.outgoing[1] - knot.outgoing[1] + 2.0).abs() < 0.01);
            }
            p.set_tool(3);
            assert_eq!(p.tool, 10);
            let old = p.store.get(pixels).unwrap();
            p.brush_press(80.0, 60.0);
            p.brush_release();
            assert_eq!(old.pixels(), p.store.get(pixels).unwrap().pixels());
            let geometry = mask(&p).vector.clone();
            p.history_edit(|p| p.add_vector_mask((kind + 1) % 3));
            assert_eq!(mask(&p).vector, geometry);
            p.history_edit(CanvasPane::remove_mask);
            p.undo();
            assert_eq!(mask(&p).vector, geometry);
            p.paint_mask = false;
            p.tool = 0;
            assert!(p.paint_target().is_some());
        }
        let mut p = pane();
        p.history_edit(CanvasPane::add_mask);
        let before = p.snapshot().unwrap();
        p.history_edit(|p| p.add_vector_mask(0));
        assert!(p.snapshot().unwrap().same_state(&before));
        assert!(mask(&p).vector.is_none());
    }

    #[test]
    fn vector_mask_link_toggle_copy_and_nonimage_targets_preserve_independent_sources() {
        let mut p = pane();
        p.history_edit(|p| p.add_vector_mask(1));
        let original = p.active.unwrap();
        let before = compose(&p);
        p.history_edit(CanvasPane::toggle_vector_mask_link);
        assert_eq!(compose(&p).pixels(), before.pixels());
        assert!(!mask(&p).linked);
        p.document
            .as_mut()
            .unwrap()
            .layer_mut(original)
            .unwrap()
            .transform
            .x += 8.0;
        let detached = compose(&p);
        p.history_edit(CanvasPane::toggle_vector_mask_link);
        assert_eq!(compose(&p).pixels(), detached.pixels());
        assert!(mask(&p).linked);
        let old_source = mask(&p).vector.clone();
        let old_pixels = mask(&p).pixels;
        p.history_edit(|p| p.duplicate_row(0));
        let copied = p.active.unwrap();
        assert_ne!(copied, original);
        p.mask_chip_click(0);
        assert!(p.editing_vector_mask());
        assert_ne!(mask(&p).pixels, old_pixels);
        edit_anchor(&mut p);
        assert_eq!(
            p.document
                .as_ref()
                .unwrap()
                .find(original)
                .unwrap()
                .mask()
                .unwrap()
                .vector,
            old_source
        );
        assert_ne!(mask(&p).vector, old_source);
        for group in [true, false] {
            let id = if group {
                p.document.as_mut().unwrap().new_group("group")
            } else {
                p.document
                    .as_mut()
                    .unwrap()
                    .new_adjustment("invert", Adjustment::Invert)
            };
            p.active = Some(id);
            p.layer = None;
            p.paint_mask = false;
            p.history_edit(|p| p.add_vector_mask(2));
            assert_eq!(
                (
                    mask(&p).vector.as_ref().unwrap().width,
                    mask(&p).vector.as_ref().unwrap().height
                ),
                (160, 120)
            );
            edit_anchor(&mut p);
            let source = mask(&p).vector.clone();
            p.toggle_vector_mask_link();
            assert_eq!(mask(&p).vector, source);
            assert!(p.paint_target().is_none());
        }
    }

    #[test]
    fn vector_mask_v6_rebuild_and_legacy_versions_reject_corruption_without_replacing_document() {
        let root =
            std::env::temp_dir().join(format!("seecut-vector-mask-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for version in 1..=5 {
            let path = root.join(format!("legacy-{version}.comp"));
            let mut p = pane();
            p.history_edit(CanvasPane::add_mask);
            p.agent_save_comp(&path).unwrap();
            drop(p);
            let file = path.join("manifest.json");
            let mut json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            json["concat-project"] = version.into();
            std::fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
            let mut reopened = CanvasPane::default();
            reopened.load_comp(&path, None).unwrap();
            assert!(
                reopened
                    .document
                    .as_ref()
                    .unwrap()
                    .masks()
                    .iter()
                    .all(|(_, m)| m.vector.is_none())
            );
        }
        let path = root.join("vector.comp");
        let mut p = pane();
        p.history_edit(|p| p.add_vector_mask(2));
        edit_anchor(&mut p);
        let pixels = mask(&p).pixels;
        let source = mask(&p).vector.clone();
        let expected = compose(&p);
        p.agent_save_comp(&path).unwrap();
        drop(p);
        assert!(
            !path
                .join("images")
                .join(format!("{}.png", pixels.as_u64()))
                .exists(),
            "derived coverage must not be a saved source"
        );
        let file = path.join("manifest.json");
        let good: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(good["concat-project"], 6);
        let mut reopened = CanvasPane::default();
        reopened.load_comp(&path, None).unwrap();
        reopened.mask_chip_click(0);
        assert_eq!(mask(&reopened).vector, source);
        assert_eq!(compose(&reopened).pixels(), expected.pixels());
        drop(reopened);
        for wrong_version in [true, false] {
            let mut broken = good.clone();
            if wrong_version {
                broken["concat-project"] = 5.into();
            } else {
                broken["document"]["root"]["children"][0]["Layer"]["mask"]["vector"]["geometry"]
                    ["closed"] = false.into();
            }
            std::fs::write(&file, serde_json::to_vec(&broken).unwrap()).unwrap();
            let mut existing = pane();
            let before = existing.snapshot().unwrap();
            assert!(existing.load_comp(&path, None).is_err());
            assert!(existing.snapshot().unwrap().same_state(&before));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires an isolated portable executable and SEECUT_VECTOR_MASK_FIXTURE_DIR"]
    fn vector_mask_complete_saved_gpu_png_global_library_chain() {
        let dir = std::fs::canonicalize(
            std::env::var_os("SEECUT_VECTOR_MASK_FIXTURE_DIR").expect("fixture"),
        )
        .unwrap();
        let data = concat_host::AppDirs::locate().unwrap().data;
        assert_eq!(data, dir.join("portable"), "must never touch user assets");
        let mut p = pane();
        for kind in 0..3 {
            if kind > 0 {
                p.history_edit(|p| p.duplicate_row(0));
                p.history_edit(CanvasPane::remove_mask);
            }
            p.history_edit(|p| p.add_vector_mask(kind));
            edit_anchor(&mut p);
            p.history_edit(CanvasPane::toggle_vector_mask_link);
            let id = p.active.unwrap();
            p.history_edit(|p| {
                p.document
                    .as_mut()
                    .unwrap()
                    .layer_mut(id)
                    .unwrap()
                    .transform
                    .x += 4.0
            });
            p.history_edit(CanvasPane::toggle_vector_mask_link);
            let final_state = p.snapshot().unwrap();
            p.undo();
            p.redo();
            assert!(p.snapshot().unwrap().same_state(&final_state));
        }
        let expected = compose(&p);
        let mut gpu = CanvasGpu::new().expect("actual GPU required");
        let actual = gpu.compose_frame(p.document.as_ref().unwrap(), &p.store);
        for (a, b) in actual.pixels().iter().zip(expected.pixels()) {
            assert!((*a as i16 - *b as i16).abs() <= 1);
        }
        let project = dir.join("vector-mask-editable.comp");
        p.agent_save_comp(&project).unwrap();
        drop(p);
        let mut p = CanvasPane::default();
        p.load_comp(&project, None).unwrap();
        assert_eq!(compose(&p).pixels(), expected.pixels());
        let job = CanvasExportJob {
            account_scope: "vector-mask-synthetic".into(),
            snapshot: p.save_data().unwrap(),
            project_path: Some(project),
            path: data.join("canvas-exports/vector-mask.png"),
            export_stem: "Editable vector masks".into(),
            generation: p.document_generation,
            binding_epoch: p.mcp_binding_epoch,
            revision: p.revision,
            registration_id: "vector-mask-export".into(),
        };
        let first = job.run(None).unwrap();
        assert!(
            first.registration_error.is_none(),
            "{:?}",
            first.registration_error
        );
        let reference = first.asset.unwrap();
        let second = job.run(None).unwrap();
        assert_eq!(second.asset.as_ref(), Some(&reference));
        let root = data.join("personal-library");
        let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
        assert_eq!(library.items().len(), 1);
        let asset = library.get(&reference.asset_id).unwrap();
        let file = crate::personal_library::checked_managed_version_path(&root, asset).unwrap();
        crate::personal_library::verify_asset_reference(&file, Some(&reference)).unwrap();
        let (_, _, rgba) = decode_png(&std::fs::read(&file).unwrap(), None).unwrap();
        assert_eq!(rgba, expected.pixels());
        std::fs::copy(file, dir.join("vector-mask-render.png")).unwrap();
        std::fs::write(dir.join("result.json"),serde_json::to_vec_pretty(&serde_json::json!({"width":160,"height":120,"editable_masks":p.document.as_ref().unwrap().masks().len(),"library_items":library.items().len(),"sha256":first.sha256,"gpu_required":true,"ui_verified":false})).unwrap()).unwrap();
    }
}
