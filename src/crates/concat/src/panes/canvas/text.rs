// SPDX-License-Identifier: AGPL-3.0-or-later
//! Editable text uses the same snapshot transaction and derived pixel cache as shapes.
use super::*;
use concat_canvas::text::CanvasText;
use slint::Model;

fn text_layer_name(content: &str) -> String {
    let name: String = content
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(24)
        .collect();
    if name.is_empty() {
        "文字".into()
    } else {
        name
    }
}

impl CanvasPane {
    fn selected_text(&self) -> Option<&CanvasText> {
        let LayerNode::Layer(layer) = self.document.as_ref()?.find(self.active?)? else {
            return None;
        };
        layer.text.as_deref()
    }
    pub(super) fn text_start(&mut self, mode: i32, x: f64, y: f64) -> Result<(), String> {
        if self.text_session.is_some() {
            return Ok(());
        }
        // A double-click starts editing before its trailing pointer release.
        // Finish that object gesture so it cannot commit the text transaction.
        self.object_release(true);
        if mode == 2 {
            let Some(id) = self.hit_image(x, y) else {
                return Ok(());
            };
            self.select_object_after_transform(Some(id));
        }
        if mode == 0 {
            let Some(doc) = self.document.as_ref() else {
                return Ok(());
            };
            let text = CanvasText::new(doc.height);
            let frame = text.rasterize(doc.width, doc.height)?;
            self.begin_history();
            let pixels = self.store.put(frame);
            let doc = self.document.as_mut().expect("document");
            let id = doc.new_layer("文字", pixels);
            doc.layer_mut(id).expect("created").text = Some(Box::new(text));
            self.active = Some(id);
            self.layer = Some(pixels);
        } else if self.selected_text().is_some() {
            self.begin_history();
        } else {
            return Ok(());
        }
        self.tool = 0;
        self.paint_mask = false;
        self.text_session = self.active;
        self.sync_view();
        Ok(())
    }
    pub(super) fn text_change(
        &mut self,
        field: i32,
        value: &str,
        number: f32,
    ) -> Result<(), String> {
        let Some(mut text) = self.selected_text().cloned() else {
            return Ok(());
        };
        let previous_name = text_layer_name(&text.style.content);
        let height = self.document.as_ref().expect("document").height as f64;
        let s = &mut text.style;
        match field {
            0 => s.content = value.into(),
            1 => s.font_family = value.into(),
            2 => s.font_size = f64::from(number) / height,
            3 => s.color = value.into(),
            4 => s.font_weight = if number > 0.0 { 700.0 } else { 400.0 },
            5 => s.italic = number > 0.0,
            6 => {
                s.align = match number as i32 {
                    0 => concat_text::Align::Left,
                    2 => concat_text::Align::Right,
                    _ => concat_text::Align::Center,
                }
            }
            7 => s.stroke_width = f64::from(number) / height,
            8 => s.stroke_color = value.into(),
            9 => s.shadow = number > 0.0,
            10 => s.max_width = f64::from(number) / 100.0,
            11 => s.line_height = f64::from(number),
            12 => s.tracking = f64::from(number) / height,
            _ => return Ok(()),
        }
        let doc = self.document.as_mut().expect("document");
        let frame = text.rasterize(doc.width, doc.height)?;
        let layer = doc
            .layer_mut(self.active.expect("selected"))
            .expect("layer");
        if field == 0 && layer.name == previous_name {
            layer.name = text_layer_name(&text.style.content);
        }
        layer.text = Some(Box::new(text));
        let pixels = layer.pixels;
        self.store.replace(pixels, frame);
        self.bump_thumbnail_revision(pixels);
        self.sync_view();
        Ok(())
    }
    pub(super) fn text_finish(&mut self, commit: bool) {
        if self.text_session.take().is_none() {
            return;
        }
        self.pending_transform_boundary = None;
        let empty = self
            .selected_text()
            .is_none_or(|t| t.style.content.trim().is_empty());
        if commit && !empty {
            self.commit_history();
        } else if let Some((before, _)) = self.pending_history.take() {
            self.restore_snapshot(before);
        }
        self.sync_view();
    }
    pub(crate) fn publish_text_controls(&self, app: &crate::App) {
        let controls = app.global::<crate::CanvasControls>();
        let text = self.selected_text();
        controls.set_text_selected(text.is_some());
        controls.set_text_editing(self.text_session.is_some());
        let Some(text) = text else {
            return;
        };
        let s = &text.style;
        controls.set_text_content(s.content.clone().into());
        controls.set_text_family(s.font_family.clone().into());
        controls.set_text_size(
            (s.font_size * self.document.as_ref().expect("document").height as f64) as f32,
        );
        controls.set_text_color(s.color.clone().into());
        controls.set_text_bold(s.font_weight >= 600.0);
        controls.set_text_italic(s.italic);
        controls.set_text_align(match s.align {
            concat_text::Align::Left => 0,
            concat_text::Align::Center => 1,
            concat_text::Align::Right => 2,
        });
        controls.set_text_stroke(
            (s.stroke_width * self.document.as_ref().expect("document").height as f64) as f32,
        );
        controls.set_text_stroke_color(s.stroke_color.clone().into());
        controls.set_text_shadow(s.shadow);
        controls.set_text_width((s.max_width * 100.0) as f32);
        controls.set_text_line_height(s.line_height as f32);
        controls.set_text_tracking(
            (s.tracking * self.document.as_ref().expect("document").height as f64) as f32,
        );
        let fonts = concat_text::system_fonts();
        controls.set_text_font_warning(
            if fonts.has_family(&s.font_family) {
                ""
            } else {
                "字体不可用，已使用替代字体"
            }
            .into(),
        );
        if controls.get_text_families().row_count() == 0 {
            controls.set_text_families(slint::ModelRc::new(slint::VecModel::from(
                fonts
                    .families()
                    .into_iter()
                    .map(slint::SharedString::from)
                    .collect::<Vec<_>>(),
            )));
        }
        controls.set_text_family_index(
            controls
                .get_text_families()
                .iter()
                .position(|family| family.as_str() == s.font_family)
                .map_or(-1, |index| index as i32),
        );
        if let Some(doc) = self.document.as_ref()
            && let Some(LayerNode::Layer(layer)) = self.active.and_then(|id| doc.find(id))
        {
            let size = (doc.width as f32, doc.height as f32);
            if let Some(p) = layer
                .transform
                .from_bitmap((size.0 / 2.0, size.1 / 2.0), size, size)
            {
                let p = self
                    .nav
                    .viewport()
                    .view_point((p.0 as f64, p.1 as f64), (size.0 as f64, size.1 as f64));
                controls.set_text_editor_x(p.0 as f32);
                controls.set_text_editor_y(p.1 as f32);
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    fn pane() -> CanvasPane {
        let mut p = CanvasPane {
            document: Some(ImageDocument::new(640, 360)),
            ..CanvasPane::default()
        };
        p.nav.set_document(Some((640.0, 360.0)));
        p.nav
            .viewport_mut()
            .resize((640.0, 360.0), 1.0, Some((640.0, 360.0)));
        p
    }
    #[test]
    fn text_input_is_one_transaction_and_cancel_restores_source_and_pixels() {
        let mut p = pane();
        let before = p.snapshot().unwrap();
        p.text_start(0, 0.0, 0.0).unwrap();
        for content in ["中", "中文 English", "中文 English\n第二行"] {
            p.text_change(0, content, 0.0).unwrap();
        }
        assert!(p.save_data().is_err());
        assert_eq!(p.undo_stack.len(), 0);
        assert!(
            p.route_transform_boundary(CanvasMsg::LayerDelete(0))
                .is_none()
        );
        p.resolve_transform_boundary(0);
        p.text_finish(true);
        assert_eq!(
            p.document
                .as_ref()
                .unwrap()
                .find(p.active.unwrap())
                .unwrap()
                .name(),
            "中文 English"
        );
        assert_eq!(p.undo_stack.len(), 1);
        let created = p.snapshot().unwrap();
        p.undo();
        assert!(p.snapshot().unwrap().same_state(&before));
        p.redo();
        assert!(p.snapshot().unwrap().same_state(&created));
        assert!(p.paint_target().is_none());
        p.text_start(1, 0.0, 0.0).unwrap();
        p.text_change(0, "Cancelled", 0.0).unwrap();
        p.text_finish(false);
        assert!(p.snapshot().unwrap().same_state(&created));
        p.text_start(0, 0.0, 0.0).unwrap();
        p.text_change(0, "  ", 0.0).unwrap();
        p.text_finish(true);
        assert!(p.snapshot().unwrap().same_state(&created));
    }
    #[test]
    fn double_click_release_keeps_text_cancel_transaction() {
        let mut p = pane();
        p.text_start(0, 0.0, 0.0).unwrap();
        p.text_finish(true);
        let created = p.snapshot().unwrap();
        let frame = p.store.get(p.layer.unwrap()).unwrap();
        let index = frame
            .pixels()
            .chunks_exact(4)
            .position(|p| p[3] > 128)
            .unwrap();
        let x = (index % frame.width() as usize) as f64 + 0.5;
        let y = (index / frame.width() as usize) as f64 + 0.5;
        let (x, y) = p.nav.viewport().view_point((x, y), (640.0, 360.0));
        p.object_press(x, y);
        assert!(p.object_drag.is_some());
        p.text_start(2, x, y).unwrap();
        p.object_release(true);
        assert!(p.pending_history.is_some());
        p.text_change(0, "", 0.0).unwrap();
        p.text_finish(false);
        assert!(p.snapshot().unwrap().same_state(&created));
        assert!(p.active_geometry().is_some());
    }
    #[test]
    fn text_duplicate_cache_and_style_history_are_independent() {
        let mut p = pane();
        p.text_start(0, 0.0, 0.0).unwrap();
        p.text_finish(true);
        let first = p.active.unwrap();
        let pixels = p.layer.unwrap();
        let original = p.store.get(pixels).unwrap();
        p.begin_history();
        p.duplicate_row(0);
        p.commit_history();
        let second = p.active.unwrap();
        assert_ne!(first, second);
        assert_ne!(Some(pixels), p.layer);
        p.text_start(1, 0.0, 0.0).unwrap();
        p.text_change(0, "独立 Copy", 0.0).unwrap();
        p.text_finish(true);
        assert_eq!(
            match p.document.as_ref().unwrap().find(first).unwrap() {
                LayerNode::Layer(l) => &l.text.as_ref().unwrap().style.content,
                _ => unreachable!(),
            },
            "文字"
        );
        assert_eq!(p.store.get(pixels).unwrap().pixels(), original.pixels());
        p.document.as_mut().unwrap().layer_mut(second).unwrap().name = "自定义标题".into();
        let before = p.snapshot().unwrap();
        p.begin_history();
        p.text_change(1, "SeeCut Missing Font", 0.0).unwrap();
        p.text_change(2, "", 36.0).unwrap();
        p.text_change(3, "#E04825", 0.0).unwrap();
        p.text_change(6, "", 0.0).unwrap();
        p.text_change(7, "", 2.0).unwrap();
        p.text_change(9, "", 1.0).unwrap();
        p.commit_history();
        assert_eq!(
            p.document.as_ref().unwrap().find(second).unwrap().name(),
            "自定义标题"
        );
        assert_eq!(
            p.selected_text().unwrap().style.font_family,
            "SeeCut Missing Font"
        );
        let styled = p.snapshot().unwrap();
        p.undo();
        assert!(p.snapshot().unwrap().same_state(&before));
        p.redo();
        assert!(p.snapshot().unwrap().same_state(&styled));
        p.text_start(1, 0.0, 0.0).unwrap();
        p.text_change(0, "新的内容", 0.0).unwrap();
        p.text_finish(true);
        assert_eq!(
            p.document.as_ref().unwrap().find(second).unwrap().name(),
            "自定义标题"
        );
    }
    #[test]
    fn text_project_reopens_editable_source_and_png_matches_compositor() {
        let mut p = pane();
        p.text_start(0, 0.0, 0.0).unwrap();
        p.text_change(0, "中文 English\n第二行", 0.0).unwrap();
        p.text_finish(true);
        let expected = p.selected_text().unwrap().clone();
        let id = p.active.unwrap();
        let pixels = p.layer.unwrap();
        let dir =
            std::env::temp_dir().join(format!("seecut-text-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let path = dir.join("editable.comp");
        let composed = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
        let png = dir.join("text.png");
        p.agent_export_png(&png).unwrap();
        let (_, _, rgba) = decode_png(&std::fs::read(png).unwrap(), None).unwrap();
        assert_eq!(rgba, composed.pixels());
        let provenance = canvas_export_provenance(&p.save_data().unwrap(), None).unwrap();
        assert_eq!(
            provenance.parameters["layers"][0]["text"]["style"]["content"],
            expected.style.content
        );
        p.agent_save_comp(&path).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["concat-project"], 3);
        drop(p);
        let mut reopened = CanvasPane::default();
        reopened.load_comp(&path, None).unwrap();
        assert_eq!(reopened.active, Some(id));
        assert_eq!(reopened.selected_text(), Some(&expected));
        assert_eq!(
            reopened.store.get(pixels).unwrap().pixels(),
            expected.rasterize(640, 360).unwrap().pixels()
        );
        reopened.text_start(1, 0.0, 0.0).unwrap();
        reopened.text_change(0, "重开后仍可编辑", 0.0).unwrap();
        reopened.text_finish(true);
        assert_eq!(
            reopened.selected_text().unwrap().style.content,
            "重开后仍可编辑"
        );
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// Writes reviewable fixtures only when run from an isolated portable test executable.
    #[test]
    #[ignore = "requires an isolated portable executable and SEECUT_C03_FIXTURE_DIR"]
    fn mixed_text_export_registers_once_and_reimports_existing_asset() {
        use concat_canvas::vector::{ShapeKind, VectorShape};
        let dir = PathBuf::from(std::env::var_os("SEECUT_C03_FIXTURE_DIR").expect("fixture path"));
        let dir = std::fs::canonicalize(dir).unwrap();
        let data = concat_host::AppDirs::locate().unwrap().data;
        assert_eq!(data, dir.join("portable"), "never write to user data");
        let mut p = pane();
        let bitmap = Frame::from_rgba(640, 360, [232, 238, 246, 255].repeat(640 * 360)).unwrap();
        let pixels = p.store.put(bitmap);
        p.document
            .as_mut()
            .unwrap()
            .new_layer("Synthetic background", pixels);
        let shape = VectorShape::new(ShapeKind::Rectangle, [40.0, 40.0], [190.0, 100.0]);
        let pixels = p.store.put(shape.rasterize(640, 360).unwrap());
        let doc = p.document.as_mut().unwrap();
        let shape_id = doc.new_layer("C01 shape", pixels);
        doc.layer_mut(shape_id).unwrap().vector = Some(shape);
        p.text_start(0, 0.0, 0.0).unwrap();
        p.text_change(0, "中文 English\n第二行 · Editable", 0.0)
            .unwrap();
        p.text_finish(true);
        p.begin_history();
        p.text_change(2, "", 36.0).unwrap();
        p.text_change(3, "#244580", 0.0).unwrap();
        p.text_change(4, "", 1.0).unwrap();
        p.text_change(5, "", 1.0).unwrap();
        p.text_change(11, "", 1.4).unwrap();
        p.text_change(12, "", 0.5).unwrap();
        p.commit_history();
        let before = p.snapshot().unwrap();
        let id = p.active.unwrap();
        p.begin_history();
        let layer = p.document.as_mut().unwrap().layer_mut(id).unwrap();
        layer.transform.x = 0.1;
        layer.transform.rotation = 0.12;
        layer.transform.scale_x = 0.85;
        layer.transform.scale_y = 0.85;
        layer.opacity = 0.9;
        p.commit_history();
        let transformed = p.snapshot().unwrap();
        p.undo();
        assert!(p.snapshot().unwrap().same_state(&before));
        p.redo();
        assert!(p.snapshot().unwrap().same_state(&transformed));
        let project = dir.join("mixed-editable.comp");
        p.agent_save_comp(&project).unwrap();
        let job = CanvasExportJob {
            snapshot: p.save_data().unwrap(),
            project_path: Some(project.clone()),
            path: data.join("canvas-exports/mixed-text.png"),
            export_stem: "C03 mixed text".into(),
            generation: p.document_generation,
            binding_epoch: p.mcp_binding_epoch,
            revision: p.revision,
            registration_id: "c03-review-export".into(),
        };
        let receipt = job.run(None).unwrap();
        assert!(
            receipt.registration_error.is_none(),
            "{:?}",
            receipt.registration_error
        );
        let reference = receipt.asset.unwrap();
        let retry = job.run(None).unwrap();
        assert!(
            retry.registration_error.is_none(),
            "{:?}",
            retry.registration_error
        );
        assert_eq!(retry.asset.as_ref(), Some(&reference));
        let library_root = data.join("personal-library");
        let library = crate::personal_library::Library::load_for_relink(&library_root).unwrap();
        assert_eq!(library.items().len(), 1);
        let asset = library.get(&reference.asset_id).unwrap();
        let path =
            crate::personal_library::checked_managed_version_path(&library_root, asset).unwrap();
        crate::personal_library::verify_asset_reference(&path, Some(&reference)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let (w, h, rgba) = decode_png(&bytes, None).unwrap();
        let frame = Frame::from_rgba(w, h, rgba).unwrap();
        let expected = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
        assert_eq!(frame.pixels(), expected.pixels());
        drop(p);
        let mut reopened = CanvasPane::default();
        reopened.load_comp(&project, None).unwrap();
        assert_eq!(
            reopened.selected_text().unwrap().style.content,
            "中文 English\n第二行 · Editable"
        );
        assert_eq!(
            concat_canvas::compose(reopened.document.as_ref().unwrap(), &reopened.store).pixels(),
            expected.pixels()
        );
        reopened.text_start(1, 0.0, 0.0).unwrap();
        reopened.text_change(0, "重开后 Editable", 0.0).unwrap();
        reopened.text_finish(true);
        let mut destination = pane();
        let import = CanvasImport {
            path,
            display_name: Some(asset.name.clone()),
            library_reference: Some(reference.clone()),
        };
        destination
            .mcp_insert_library_checked(&import, frame, |_| Ok::<(), ()>(()))
            .ok()
            .unwrap();
        assert_eq!(
            crate::personal_library::Library::load_for_relink(&library_root)
                .unwrap()
                .items()
                .len(),
            1
        );
        std::fs::write(
            dir.join("export-receipt.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "asset": reference, "sha256": receipt.sha256, "width": w, "height": h,
                "registration_id": receipt.registration_id, "library_count": 1,
                "editable_reopen": true, "canvas_reimport": true,
                "clip_ui_reuse": "not verified; existing asset path and identity preserved",
                "actual_ime": "not verified; synthetic committed strings only"
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
