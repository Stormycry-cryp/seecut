//! Background parsing with a single atomic, document/session-bound commit.
use super::*;
use concat_canvas::psd::{ImportError, Imported};
use std::sync::atomic::AtomicBool;

pub(super) struct ImportTicket {
    id: String,
    generation: u64,
    clock: u64,
    revision: u64,
    epoch: u64,
    project: Option<PathBuf>,
    session: u64,
    cancel: Arc<AtomicBool>,
}
fn read(path: &Path, limit: u32, cancel: &AtomicBool) -> Result<Imported, ImportError> {
    use std::io::Read;
    static PARSER: Mutex<()> = Mutex::new(());
    let _lane = PARSER
        .lock()
        .map_err(|_| ImportError("PSD 读取队列不可用".into()))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(ImportError("导入已取消".into()));
    }
    let file = std::fs::File::open(path).map_err(|e| ImportError(format!("无法读取 PSD：{e}")))?;
    if file
        .metadata()
        .map_err(|e| ImportError(e.to_string()))?
        .len()
        > concat_canvas::psd::FILE_LIMIT as u64
    {
        return Err(ImportError("PSD 文件超过 64 MiB".into()));
    }
    let mut bytes = Vec::new();
    file.take(concat_canvas::psd::FILE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ImportError(e.to_string()))?;
    concat_canvas::psd::parse(&bytes, limit, cancel)
}
impl CanvasPane {
    pub(super) fn cancel_psd(&mut self) {
        if let Some(ticket) = self.psd_import.take() {
            ticket.cancel.store(true, Ordering::Relaxed);
            self.psd_status = "PSD 导入已取消".into();
            self.psd_details.clear();
        }
    }
    fn prepare_psd(&mut self, session: u64) -> Result<(String, Arc<AtomicBool>), String> {
        if self.has_uncommitted_edit() {
            return Err("请先完成当前编辑，再导入 PSD".into());
        }
        self.cancel_psd();
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        self.psd_import = Some(ImportTicket {
            id: id.clone(),
            generation: self.document_generation,
            clock: self.revision_clock,
            revision: self.revision,
            epoch: self.mcp_binding_epoch,
            project: self.project_path.clone(),
            session,
            cancel: cancel.clone(),
        });
        self.psd_status = "正在导入分层 PSD…".into();
        self.psd_details.clear();
        Ok((id, cancel))
    }
    pub(super) fn start_psd(&mut self, path: &Path, limit: u32, studio: &mut Studio) {
        let (id, cancel) = match self.prepare_psd(studio.session_generation) {
            Ok(v) => v,
            Err(e) => {
                studio.notify(&e, true);
                return;
            }
        };
        let source = path.to_owned();
        let output = source.clone();
        crate::host::spawn(
            move || read(&source, limit, &cancel),
            move |studio, _, _, result| {
                let mut canvas = std::mem::take(&mut studio.canvas);
                if canvas.finish_psd(&id, studio.session_generation, &output, result) {
                    canvas.render(studio);
                }
                studio.canvas = canvas;
            },
        );
    }
    fn finish_psd(
        &mut self,
        id: &str,
        session: u64,
        path: &Path,
        result: Result<Imported, ImportError>,
    ) -> bool {
        let Some(t) = self.psd_import.as_ref().filter(|t| t.id == id) else {
            return false;
        };
        let current = !t.cancel.load(Ordering::Relaxed)
            && t.session == session
            && t.generation == self.document_generation
            && t.clock == self.revision_clock
            && t.revision == self.revision
            && t.epoch == self.mcp_binding_epoch
            && t.project == self.project_path
            && !self.has_uncommitted_edit()
            && self.pending_open.is_none();
        self.psd_import = None;
        if !current {
            self.psd_status = "画布或项目已变化，PSD 导入未应用".into();
            return false;
        }
        match result {
            Err(e) => {
                self.psd_status = if e.0.contains("图层") || e.0.contains("记录") {
                    "PSD 含未支持或损坏的图层，未导入".into()
                } else {
                    format!("未导入：{}", e.0.chars().take(60).collect::<String>())
                };
                self.psd_details = e.to_string();
                false
            }
            Ok(imported) => {
                let details = imported.warnings.join("\n");
                let count = imported.document.walk().len();
                self.replace_imported_document(path, imported.document, imported.store);
                self.psd_status = format!("已导入 {count} 个图层与组");
                self.psd_details = format!(
                    "{details}\n导入的位图蒙版保留边界、默认覆盖率与随层移动；可绘制、启停或移除。保存为 .comp 可继续编辑；导出 PNG 会加入资产库。"
                );
                true
            }
        }
    }
    pub(crate) fn publish_psd_controls(&self, app: &crate::App) {
        let controls = app.global::<crate::CanvasControls>();
        controls.set_psd_importing(self.psd_import.is_some());
        controls.set_psd_status(self.psd_status.clone().into());
        controls.set_psd_details(self.psd_details.clone().into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Result<Imported, ImportError> {
        concat_canvas::psd::parse(
            include_bytes!("../../../../concat-canvas/tests/fixtures/psd/2layers.psd"),
            8192,
            &AtomicBool::new(false),
        )
    }
    #[test]
    fn psd_atomic_fences_cover_cancel_edit_undo_project_session_and_error() {
        for mode in 0..8 {
            let mut pane = CanvasPane::default();
            let (id, flag) = pane.prepare_psd(3).unwrap();
            match mode {
                0 => pane.cancel_psd(),
                1 => pane.revision_clock += 1,
                2 => pane.document_generation += 1,
                3 => pane.mcp_binding_epoch += 1,
                4 => pane.project_path = Some(PathBuf::from("other.comp")),
                5 => {}
                6 => {}
                _ => {
                    pane.prepare_psd(3).unwrap();
                }
            }
            let result = if mode == 6 {
                Err(ImportError("bad PSD".into()))
            } else {
                sample()
            };
            assert!(!pane.finish_psd(
                &id,
                if mode == 5 { 4 } else { 3 },
                Path::new("sample.psd"),
                result
            ));
            assert!(pane.document.is_none());
            if mode == 0 || mode == 7 {
                assert!(flag.load(Ordering::Relaxed));
            }
        }
        let mut p = CanvasPane::default();
        let (id, _) = p.prepare_psd(4).unwrap();
        assert!(p.finish_psd(&id, 4, Path::new("sample.psd"), sample()));
        assert_eq!(p.document.as_ref().unwrap().root.children.len(), 2);
        assert!(p.is_modified());
        assert!(p.project_path.is_none());
    }
    fn synthetic() -> Imported {
        concat_canvas::psd::parse(
            include_bytes!("../../../../concat-canvas/tests/fixtures/psd/seecut-layered.psd"),
            8192,
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    #[test]
    fn psd_mask_paint_mapping_and_v7_save_reopen_are_lossless() {
        let root = std::env::temp_dir().join(format!("seecut-psd-{}", uuid::Uuid::new_v4()));
        let path = root.join("imported.comp");
        std::fs::create_dir_all(&root).unwrap();
        let imported = synthetic();
        let mut p = CanvasPane::default();
        p.replace_imported_document(Path::new("source.psd"), imported.document, imported.store);
        let id = p
            .document
            .as_ref()
            .unwrap()
            .walk()
            .into_iter()
            .find(|n| n.name() == "中文·负坐标")
            .unwrap()
            .id();
        p.active = Some(id);
        p.paint_mask = true;
        let target = p.paint_target().unwrap();
        let before = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
        p.history_edit(|p| {
            p.document
                .as_mut()
                .unwrap()
                .layer_mut(id)
                .unwrap()
                .transform
                .x += 3.0;
        });
        let mapping = p.mapping_for_target(target).unwrap();
        let local = mapping.to_target((1.5, 1.5)).unwrap();
        assert_eq!(local, (1.5, 3.5));
        assert_eq!(mapping.to_document(local), Some((1.5, 1.5)));
        let moved = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
        assert_ne!(moved.pixels(), before.pixels());
        p.undo();
        assert_eq!(
            concat_canvas::compose(p.document.as_ref().unwrap(), &p.store).pixels(),
            before.pixels()
        );
        p.redo();
        p.brush.color = [0, 0, 0];
        p.brush.diameter = 2.0;
        p.brush.hardness = 1.0;
        p.brush.opacity = 1.0;
        p.brush_press_document(1.5, 1.5);
        p.brush_release();
        assert!(
            p.store
                .get(p.paint_target().unwrap())
                .unwrap()
                .pixel(1, 3)
                .unwrap()[0]
                < 255
        );
        p.undo();
        assert_eq!(
            concat_canvas::compose(p.document.as_ref().unwrap(), &p.store).pixels(),
            moved.pixels()
        );
        let doc = p.document.clone().unwrap();
        p.agent_save_comp(&path).unwrap();
        drop(p);
        let mut reopened = CanvasPane::default();
        reopened.load_comp(&path, None).unwrap();
        assert_eq!(reopened.document.as_ref(), Some(&doc));
        assert_eq!(
            concat_canvas::compose(reopened.document.as_ref().unwrap(), &reopened.store).pixels(),
            moved.pixels()
        );
        drop(reopened);
        let manifest = path.join("manifest.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(v["concat-project"], 7);
        v["concat-project"] = 6.into();
        std::fs::write(&manifest, serde_json::to_vec(&v).unwrap()).unwrap();
        let mut old = CanvasPane::default();
        assert!(old.load_comp(&path, None).is_err());
        assert!(old.document.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires isolated portable executable and SEECUT_PSD_FIXTURE_DIR"]
    fn psd_complete_edit_save_gpu_png_global_library_chain() {
        let dir =
            std::fs::canonicalize(std::env::var_os("SEECUT_PSD_FIXTURE_DIR").expect("fixture"))
                .unwrap();
        let data = concat_host::AppDirs::locate().unwrap().data;
        assert_eq!(data, dir.join("portable"), "never touch user assets");
        let mut gpu = CanvasGpu::new().expect("actual GPU required");
        let mut results = Vec::new();
        for (name, imported) in [("synthetic", synthetic()), ("external", sample().unwrap())] {
            let mut p = CanvasPane::default();
            let (id, _) = p.prepare_psd(1).unwrap();
            assert!(p.finish_psd(&id, 1, Path::new("source.psd"), Ok(imported)));
            let original = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
            std::fs::write(
                dir.join(format!("{name}-original.png")),
                encode_png(&original).unwrap(),
            )
            .unwrap();
            let id = p.active.unwrap();
            p.history_edit(|p| {
                let layer = p.document.as_mut().unwrap().layer_mut(id).unwrap();
                layer.transform.x += 1.0;
                layer.name.push_str("·可编辑");
            });
            let after = p.snapshot().unwrap();
            p.undo();
            p.redo();
            assert!(p.snapshot().unwrap().same_state(&after));
            let expected = concat_canvas::compose(p.document.as_ref().unwrap(), &p.store);
            let actual = gpu.compose_frame(p.document.as_ref().unwrap(), &p.store);
            for (a, b) in actual.pixels().iter().zip(expected.pixels()) {
                assert!((*a as i16 - *b as i16).abs() <= 2);
            }
            let project = dir.join(format!("{name}-editable.comp"));
            p.agent_save_comp(&project).unwrap();
            drop(p);
            let mut p = CanvasPane::default();
            p.load_comp(&project, None).unwrap();
            assert_eq!(
                concat_canvas::compose(p.document.as_ref().unwrap(), &p.store).pixels(),
                expected.pixels()
            );
            let job = CanvasExportJob {
                account_scope: "psd-fixture".into(),
                snapshot: p.save_data().unwrap(),
                project_path: Some(project),
                path: data.join(format!("canvas-exports/{name}.png")),
                export_stem: format!("PSD {name}"),
                generation: p.document_generation,
                binding_epoch: p.mcp_binding_epoch,
                revision: p.revision,
                registration_id: format!("psd-{name}"),
            };
            let receipt = job.run(None).unwrap();
            assert!(receipt.registration_error.is_none());
            let reference = receipt.asset.clone().unwrap();
            assert_eq!(job.run(None).unwrap().asset, Some(reference.clone()));
            let root = data.join("personal-library");
            let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
            let asset = library.get(&reference.asset_id).unwrap();
            let file = crate::personal_library::checked_managed_version_path(&root, asset).unwrap();
            crate::personal_library::verify_asset_reference(&file, Some(&reference)).unwrap();
            let (_, _, rgba) = decode_png(&std::fs::read(&file).unwrap(), None).unwrap();
            assert_eq!(rgba, expected.pixels());
            std::fs::copy(file, dir.join(format!("{name}-render.png"))).unwrap();
            results.push(serde_json::json!({"name":name,"layers_and_groups":p.document.as_ref().unwrap().walk().len(),"width":expected.width(),"height":expected.height(),"sha256":receipt.sha256}));
        }
        let library =
            crate::personal_library::Library::load_for_relink(data.join("personal-library"))
                .unwrap();
        assert_eq!(library.items().len(), 2);
        std::fs::write(dir.join("result.json"),serde_json::to_vec_pretty(&serde_json::json!({"samples":results,"library_items":2,"gpu_required":true,"ui_verified":false})).unwrap()).unwrap();
    }
}
