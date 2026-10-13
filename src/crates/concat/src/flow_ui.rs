// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native Flow host with durable graph commands and background shared image execution.
use crate::{
    flow_controller::{Controller, Effect, Intent},
    ui::{App, Flow, FlowEdgeView, FlowNodeView, FlowProjectRow, SeeCut},
};
use seecut_flow_core::{
    Result,
    catalog::{self, Project},
    commands::Command,
    graph::{Edge, Node},
};
use serde_json::json;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, rc::Rc};
mod editor;
mod image_execution;

/// Preserve delegates, focus and drag state when background receipts do not change a row.
fn sync_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (index, row) in rows.into_iter().enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
}

enum Naming {
    New(PathBuf),
    Rename(Project),
    Node(String),
}
struct NameDialog {
    token: String,
    session: Option<String>,
    mode: Naming,
}
struct Host {
    editor: editor::Editor,
    images: image_execution::Images,
    browsing: bool,
    projects: Vec<Project>,
    project_rows: Rc<VecModel<FlowProjectRow>>,
    search: String,
    name_sort: bool,
    dialog: Option<NameDialog>,
    controller: Controller,
    root: PathBuf,
    nodes: Rc<VecModel<FlowNodeView>>,
    edges: Rc<VecModel<FlowEdgeView>>,
    preview: BTreeMap<String, [f64; 2]>,
    view: [f64; 3],
    connecting: String,
    message: String,
    error: bool,
    recovery: bool,
    recovery_available: bool,
}
impl Host {
    fn fail(&mut self, error: String) {
        self.recovery_available |= self
            .controller
            .session
            .as_ref()
            .is_some_and(|s| s.store.is_poisoned());
        self.error = true;
        self.message = error;
    }
    fn request(&mut self, intent: Intent) -> Result<Option<Effect>> {
        if self.recovery {
            return Err("请先处理恢复对话框".into());
        }
        let project_change = matches!(
            &intent,
            Intent::NewNamed(_, _)
                | Intent::Open(_)
                | Intent::OpenProject(_, _, _)
                | Intent::RenameProject(_)
        );
        if matches!(
            intent,
            Intent::Select(_) | Intent::Edit(_) | Intent::Undo | Intent::Redo
        ) {
            self.controller.save_draft()?;
        }
        let previous = self.controller.session.as_ref().map(|s| s.token.clone());
        let before_nodes: std::collections::BTreeSet<_> = self
            .controller
            .session
            .as_ref()
            .map(|s| s.graph().nodes.keys().cloned().collect())
            .unwrap_or_default();
        let effect = self.controller.request(intent)?;
        if let Some(s) = self.controller.session.as_ref() {
            let created: std::collections::BTreeSet<_> = s
                .graph()
                .nodes
                .keys()
                .filter(|id| !before_nodes.contains(*id))
                .cloned()
                .collect();
            if !created.is_empty() {
                self.editor.selected = created;
            }
        }
        if previous != self.controller.session.as_ref().map(|s| s.token.clone()) {
            self.recovery_available = false;
        }
        if effect.is_some() {
            self.editor.edge = None;
            self.preview.clear();
            self.connecting.clear();
        }
        self.error = false;
        self.message.clear();
        if project_change {
            self.browsing = false;
            if effect.is_some() {
                self.remember_current();
                self.refresh_projects();
            }
        }
        if self.controller.session.is_none() {
            self.browsing = true;
        }
        Ok(effect)
    }
    fn publish(&mut self, app: &App) {
        let ui = app.global::<Flow>();
        ui.set_open(self.controller.session.is_some());
        ui.set_browsing(self.browsing);
        if self.browsing {
            ui.set_input_focused(false);
        }
        ui.set_project_dialog(self.dialog.is_some());
        ui.set_dialog_token(self.dialog.as_ref().map_or("", |d| d.token.as_str()).into());
        ui.set_read_only(
            self.controller
                .session
                .as_ref()
                .is_some_and(|s| s.store.is_read_only()),
        );
        ui.set_confirm(self.controller.pending() && !self.recovery);
        ui.set_recovery(self.recovery);
        ui.set_recovery_available(self.recovery_available);
        ui.set_dirty(self.controller.dirty());
        ui.set_error(self.error);
        ui.set_message(self.message.clone().into());
        ui.set_draft_token(self.controller.draft_token().into());
        ui.set_prompt(self.controller.prompt().into());
        ui.set_connecting_from(self.connecting.clone().into());
        if let Some(s) = self.controller.session.as_ref() {
            ui.set_token(s.token.clone().into());
            ui.set_title(
                s.store
                    .document()
                    .title
                    .clone()
                    .unwrap_or_else(|| {
                        s.path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .into(),
            );
            ui.set_selected_id(s.selected.clone().into());
            ui.set_can_undo(s.can_undo());
            ui.set_can_redo(s.can_redo());
            let node = s.graph().nodes.get(&s.selected);
            ui.set_selected_title(node.map_or("", |n| n.title.as_str()).into());
            ui.set_selected_kind(node.map_or("", |n| n.kind.as_str()).into());
            let generation = node
                .and_then(|n| {
                    s.graph()
                        .edges
                        .iter()
                        .find(|e| e.to == n.id && e.input == "prompt")
                })
                .and_then(|e| s.graph().nodes.get(&e.from))
                .and_then(|n| n.params["text"].as_str())
                .unwrap_or("");
            ui.set_generation_prompt(generation.into());
            ui.set_source_label(
                node.filter(|n| n.kind == "asset")
                    .map(|n| {
                        n.params["asset_name"]
                            .as_str()
                            .unwrap_or("来自资产库 · 固定版本")
                    })
                    .unwrap_or("")
                    .into(),
            );
            let upstream = node
                .and_then(|n| {
                    s.graph()
                        .edges
                        .iter()
                        .find(|e| e.to == n.id && e.input == "prompt")
                })
                .and_then(|e| s.graph().nodes.get(&e.from));
            ui.set_prompt_source_id(upstream.map(|n| n.id.as_str()).unwrap_or("").into());
            ui.set_prompt_source_title(upstream.map(|n| n.title.as_str()).unwrap_or("").into());
            let selected_preview =
                node.and_then(|n| self.images.node_preview(&s.store, s.graph(), n));
            ui.set_selected_preview(selected_preview.unwrap_or_default());
            self.view = self.controller.viewport().unwrap_or(s.graph().viewport);
            ui.set_pan_x(self.view[0] as f32);
            ui.set_pan_y(self.view[1] as f32);
            ui.set_zoom(self.view[2] as f32);
            let rows = s
                .graph()
                .nodes
                .values()
                .enumerate()
                .map(|(index, n)| {
                    let attempt = self.images.latest(&s.store, &s.graph_id, &n.id);
                    let preview = self.images.node_preview(&s.store, s.graph(), n);
                    let has_preview = preview.is_some();
                    let state = match n.kind.as_str() {
                        "prompt" => "",
                        "asset" if has_preview => "",
                        "asset" => "素材不可用",
                        _ => attempt
                            .map(image_execution::attempt_status)
                            .unwrap_or("未执行"),
                    };
                    let summary = if n.kind == "prompt" {
                        n.params["text"].as_str().unwrap_or("").to_owned()
                    } else {
                        match n.kind.as_str() {
                            "image_generate" => {
                                format!("{}\n{}", n.params["prompt"].as_str().unwrap_or(""), state)
                            }
                            "asset" if !has_preview => "素材不可用".into(),
                            "asset" => n.title.clone(),
                            "select" => "选择上游图片结果".into(),
                            _ => format!("{}\n尚未接通执行", kind_label(&n.kind)),
                        }
                    };
                    let p = self.preview.get(&n.id).unwrap_or(&n.position);
                    FlowNodeView {
                        id: n.id.clone().into(),
                        title: n.title.clone().into(),
                        kind: n.kind.clone().into(),
                        summary: summary.into(),
                        state: state.into(),
                        x: p[0] as f32,
                        y: p[1] as f32,
                        preview: preview.unwrap_or_default(),
                        has_preview,
                        selected: self.editor.selected.contains(&n.id) || n.id == s.selected,
                        inputs: self
                            .nodes
                            .row_data(index)
                            .filter(|old| old.id == n.id && old.kind == n.kind)
                            .map(|old| old.inputs)
                            .unwrap_or_else(|| {
                                ModelRc::new(VecModel::from(
                                    n.ports()
                                        .map(|p| {
                                            p.0.into_iter()
                                                .map(|p| SharedString::from(p.name))
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default(),
                                ))
                            }),
                    }
                })
                .collect::<Vec<_>>();
            sync_rows(&self.nodes, rows);
        } else {
            ui.set_token("".into());
            ui.set_selected_id("".into());
            ui.set_selected_kind("".into());
            ui.set_can_undo(false);
            ui.set_can_redo(false);
            sync_rows(&self.nodes, vec![]);
        }
        self.publish_editor(app);
        self.publish_edges();
        let s = self.controller.session.as_ref();
        self.images.publish(
            app,
            s.map(|s| &s.store),
            s.map(|s| s.graph()),
            s.map_or("", |s| s.selected.as_str()),
        );
    }
    fn publish_edges(&self) {
        let Some(s) = self.controller.session.as_ref() else {
            sync_rows(&self.edges, vec![]);
            return;
        };
        let [px, py, z] = self.view;
        let edges = s
            .graph()
            .edges
            .iter()
            .filter_map(|e| {
                let from = s.graph().nodes.get(&e.from)?;
                let to = s.graph().nodes.get(&e.to)?;
                let a = self.preview.get(&from.id).unwrap_or(&from.position);
                let b = self.preview.get(&to.id).unwrap_or(&to.position);
                let index = to.ports().ok()?.0.iter().position(|p| p.name == e.input)? as f64;
                let (x1, y1, x2, y2) = (
                    (a[0] + 228.) * z + px,
                    (a[1] + 100.) * z + py,
                    b[0] * z + px,
                    (b[1] + 82. + 36. * index) * z + py,
                );
                let bend = (x2 - x1).abs().max(80.) * 0.45;
                Some(FlowEdgeView {
                    path: format!(
                        "M {x1} {y1} C {} {y1} {} {y2} {x2} {y2}",
                        x1 + bend,
                        x2 - bend
                    )
                    .into(),
                    selected: self.editor.edge.as_ref() == Some(e),
                    related: self.editor.selected.contains(&e.from)
                        || self.editor.selected.contains(&e.to)
                        || e.from == s.selected
                        || e.to == s.selected,
                })
            })
            .collect::<Vec<_>>();
        sync_rows(&self.edges, edges);
    }
    fn remember_current(&mut self) {
        if let Some(session) = &self.controller.session {
            let result = catalog::inspect(&session.path).and_then(|p| {
                if p.document_id != session.store.document().id
                    || p.project_id != session.store.document().project_id
                {
                    return Err("磁盘工程身份已改变，未更新列表记录".into());
                }
                catalog::remember(&self.root, &p)
            });
            if let Err(e) = result {
                self.message = format!("工程已保留；列表记录写入失败：{e}");
                self.error = true;
            }
        }
    }
    fn refresh_projects(&mut self) {
        let listing = catalog::list(&self.root);
        self.projects = listing.projects;
        if let Some(warning) = listing.warnings.first() {
            self.message = format!("部分工程暂不可用，原文件已保留：{warning}");
            self.error = true;
        }
        self.filter_projects();
    }
    fn filter_projects(&self) {
        let search = self.search.to_lowercase();
        let mut projects = self
            .projects
            .iter()
            .filter(|p| p.title.to_lowercase().contains(&search))
            .collect::<Vec<_>>();
        if self.name_sort {
            projects.sort_by(|a, b| a.title.cmp(&b.title));
        }
        self.project_rows.set_vec(
            projects
                .into_iter()
                .map(|p| FlowProjectRow {
                    id: p.key().into(),
                    name: p.title.clone().into(),
                    read_only: p.read_only,
                    detail: format!(
                        "{} 个节点{}",
                        p.nodes,
                        if p.read_only { " · 只读" } else { "" }
                    )
                    .into(),
                    date: chrono::DateTime::from_timestamp(p.modified as i64, 0)
                        .map(|v| {
                            v.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_default()
                        .into(),
                })
                .collect::<Vec<_>>(),
        );
    }
    fn start_naming(&mut self, mode: Naming) {
        self.dialog = Some(NameDialog {
            token: seecut_flow_core::id(),
            session: self.controller.session.as_ref().map(|s| s.token.clone()),
            mode,
        });
        self.error = false;
    }
    fn submit_name(&mut self, token: &str, name: &str) -> Result<Option<Effect>> {
        let Some(dialog) = self.dialog.as_ref().filter(|d| d.token == token) else {
            return Ok(None);
        };
        if dialog.session != self.controller.session.as_ref().map(|s| s.token.clone()) {
            return Err("工程已切换，忽略旧名称操作".into());
        }
        let name = if matches!(dialog.mode, Naming::Node(_)) {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 256 {
                return Err("节点名称需为 1–256 个字符".into());
            }
            name.to_owned()
        } else {
            seecut_flow_core::store::project_name(name)?
        };
        // Consume the fixed UUID and UI token once, including a pending draft switch.
        let dialog = self.dialog.take().expect("validated dialog");
        match dialog.mode {
            Naming::Node(node) => self.request(Intent::Edit(Box::new(Command::Rename {
                node,
                title: name,
            }))),
            Naming::New(path) => {
                std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
                self.request(Intent::NewNamed(path, name))
            }
            Naming::Rename(project) => {
                if self.controller.session.as_ref().is_some_and(|s| {
                    s.store.document().id == project.document_id
                        && s.store.document().project_id == project.project_id
                        && s.path.canonicalize().ok().as_ref() == Some(&project.path)
                }) {
                    self.request(Intent::RenameProject(name))
                } else {
                    let mut store = seecut_flow_core::store::Store::open_project(
                        &project.path,
                        &project.document_id,
                        &project.project_id,
                    )?;
                    store.rename_project(&name)?;
                    let updated = catalog::inspect(&project.path)?;
                    if let Err(e) = catalog::remember(&self.root, &updated) {
                        self.message = format!("名称已保存；列表记录写入失败：{e}");
                        self.error = true;
                    }
                    self.refresh_projects();
                    Ok(Some(Effect::None))
                }
            }
        }
    }
    fn action(&mut self, action: &str) -> Result<Option<Effect>> {
        self.controller.flush_viewport()?;
        match action {
            "projects" => {
                self.browsing = true;
                self.refresh_projects();
                Ok(Some(Effect::None))
            }
            "refresh" => {
                self.error = false;
                self.refresh_projects();
                Ok(Some(Effect::None))
            }
            "rename-node" => {
                let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
                if self.editor.selected.len() != 1 || !s.graph().nodes.contains_key(&s.selected) {
                    return Err("请选择一个节点".into());
                }
                self.start_naming(Naming::Node(s.selected.clone()));
                Ok(Some(Effect::None))
            }
            "rename" => {
                let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
                let project = catalog::inspect(&s.path)?;
                self.start_naming(Naming::Rename(project));
                Ok(Some(Effect::None))
            }
            "close" => self.request(Intent::Close),
            "save" => {
                self.controller.save_draft()?;
                self.error = false;
                self.message = "工程与提示词已保存".into();
                self.remember_current();
                self.refresh_projects();
                Ok(Some(Effect::None))
            }
            "undo" => self.request(Intent::Undo),
            "redo" => self.request(Intent::Redo),
            "arrange" => self.request(Intent::Edit(Box::new(Command::Arrange))),
            "duplicate" | "copy-node" | "remove" => {
                let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
                if s.selected.is_empty() && self.editor.edge.is_none() {
                    return Err("请先选择节点".into());
                }
                if action == "remove"
                    && let Some(edge) = self.editor.edge.take()
                {
                    return self.request(Intent::Edit(Box::new(Command::Disconnect { edge })));
                }
                let nodes = if self.editor.selected.is_empty() {
                    vec![s.selected.clone()]
                } else {
                    self.editor.selected.iter().cloned().collect()
                };
                self.request(Intent::Edit(Box::new(if action == "remove" {
                    Command::Remove { nodes }
                } else {
                    Command::Duplicate {
                        nodes,
                        incoming: action == "duplicate",
                    }
                })))
            }
            "fit" | "focus" => {
                self.frame_selection(action == "fit")?;
                Ok(Some(Effect::None))
            }
            "select-all" => {
                let ids = self
                    .controller
                    .session
                    .as_ref()
                    .ok_or("请先打开工程")?
                    .graph()
                    .nodes
                    .keys()
                    .cloned()
                    .collect();
                self.select_nodes(ids, false)?;
                Ok(Some(Effect::None))
            }
            "cancel-connect" => {
                self.connecting.clear();
                self.editor.wire = None;
                self.preview.clear();
                Ok(Some(Effect::None))
            }
            a if a.starts_with("add_") => {
                let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
                let kind = &a[4..];
                let params = match kind {
                    "prompt" => json!({"text":""}),
                    "image_generate" | "video_generate" => {
                        json!({"model":"unconfigured","count":1,"prompt":""})
                    }
                    "select" => json!({"media_kind":"Image"}),
                    "select_video" => json!({"media_kind":"Video"}),
                    _ => return Err("此节点尚未接通".into()),
                };
                let mut node = Node::new(
                    if kind == "select_video" {
                        "select"
                    } else {
                        kind
                    },
                    params,
                );
                node.title = format!("{} {}", kind_label(kind), s.graph().nodes.len() + 1);
                let count = s.graph().nodes.len() as f64;
                node.position = [40. + (count % 3.) * 300., 40. + (count / 3.).floor() * 250.];
                let result = self.request(Intent::Edit(Box::new(Command::Add { node })))?;
                self.frame_selection(false)?;
                Ok(result)
            }
            _ => Err("该功能尚未接通".into()),
        }
    }
}
fn kind_label(kind: &str) -> &str {
    match kind {
        "prompt" => "提示词",
        "asset" => "参考素材",
        "image_generate" => "图像生成",
        "video_generate" => "视频生成",
        "select" => "采用图片",
        "select_video" => "采用视频",
        "deliver" => "交付",
        _ => "未知节点",
    }
}
fn finish(app: &App, host: &Rc<RefCell<Host>>, result: Result<Option<Effect>>) {
    // AccessKit can call us while rebuilding its tree. Publishing a modal can
    // move focus, so leave that callback before updating the window.
    let weak = app.as_weak();
    let host = host.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let was_confirm = app.global::<Flow>().get_confirm()
            || app.global::<Flow>().get_recovery()
            || app.global::<Flow>().get_project_dialog();
        let effect = match result {
            Ok(effect) => effect,
            Err(error) => {
                host.borrow_mut().fail(error);
                None
            }
        };
        host.borrow_mut().publish(&app);
        let ui = app.global::<Flow>();
        if was_confirm && !ui.get_confirm() && !ui.get_recovery() && !ui.get_project_dialog() {
            ui.set_focus_token(ui.get_focus_token() + 1);
        }
        match effect {
            Some(Effect::Navigate(page)) => app
                .global::<SeeCut>()
                .invoke_action("navigate-resume".into(), page.to_string().into()),
            Some(Effect::Quit) => app.invoke_titlebar_close(),
            _ => {}
        }
    });
}

/// Native sheets must not run a nested modal loop inside a Slint timer callback.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn pick_path(app: &App, host: &Rc<RefCell<Host>>, export: bool) {
    if app.global::<Flow>().get_picker_open() {
        return;
    }
    if !app.window().is_visible() {
        finish(app, host, Err("请先打开主窗口再选择文件".into()));
        return;
    }
    app.global::<Flow>().set_picker_open(true);
    let weak = app.as_weak();
    let pending_host = host.clone();
    let dir = host.borrow().root.clone();
    let original_session = host
        .borrow()
        .controller
        .session
        .as_ref()
        .map(|s| s.token.clone());
    let started = slint::spawn_local(async move {
        let path = if export {
            rfd::AsyncFileDialog::new()
                .set_title("导出 Flow 内存恢复备份")
                .set_file_name(format!(
                    "Flow-recovery-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ))
                .add_filter("JSON 恢复备份", &["json"])
                .save_file()
                .await
        } else {
            rfd::AsyncFileDialog::new()
                .set_title("打开 Flow 工程文件夹")
                .set_directory(dir)
                .pick_folder()
                .await
        };
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.global::<Flow>().set_picker_open(false);
        if !app.window().is_visible() {
            return;
        }
        let current_session = pending_host
            .borrow()
            .controller
            .session
            .as_ref()
            .map(|s| s.token.clone());
        if current_session != original_session || (export && !pending_host.borrow().recovery) {
            finish(
                &app,
                &pending_host,
                Err("工程或恢复操作已切换，已忽略文件选择结果".into()),
            );
            return;
        }
        let result = if let Some(file) = path {
            let path = file.path().to_owned();
            let mut h = pending_host.borrow_mut();
            if export {
                h.controller.export_recovery(&path).map(|()| {
                    h.error = false;
                    h.message = format!(
                        "已导出内存恢复备份：{}。可关闭后重新打开原工程。",
                        path.display()
                    );
                    Some(Effect::None)
                })
            } else {
                h.request(Intent::Open(path))
            }
        } else {
            Ok(Some(Effect::None))
        };
        finish(&app, &pending_host, result);
    });
    if let Err(error) = started {
        app.global::<Flow>().set_picker_open(false);
        finish(app, host, Err(format!("无法打开文件选择器：{error}")));
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn pick_path(app: &App, host: &Rc<RefCell<Host>>, export: bool) {
    app.global::<Flow>().set_picker_open(false);
    {
        let mut h = host.borrow_mut();
        h.error = true;
        h.message = if export {
            "移动端暂不支持导出 Flow 内存恢复备份"
        } else {
            "移动端暂不支持选择 Flow 工程文件夹"
        }
        .into();
    }
    finish(app, host, Ok(None));
}

pub(crate) fn bind(app: &App, root: PathBuf, account: crate::cloud::AccountAccess) {
    let host = Rc::new(RefCell::new(Host {
        editor: editor::Editor::default(),
        images: image_execution::Images::new(account, root.clone()),
        browsing: true,
        projects: Vec::new(),
        project_rows: Rc::new(VecModel::default()),
        search: String::new(),
        name_sort: false,
        dialog: None,
        controller: Controller::default(),
        root,
        nodes: Rc::new(VecModel::default()),
        edges: Rc::new(VecModel::default()),
        preview: BTreeMap::new(),
        view: [60., 64., 0.8],
        connecting: String::new(),
        message: "新建或打开 Flow 工程".into(),
        error: false,
        recovery: false,
        recovery_available: false,
    }));
    let ui = app.global::<Flow>();
    ui.set_projects(ModelRc::from(host.borrow().project_rows.clone()));
    host.borrow_mut().refresh_projects();
    ui.set_nodes(ModelRc::from(host.borrow().nodes.clone()));
    ui.set_edges(ModelRc::from(host.borrow().edges.clone()));
    ui.on_action({
        let weak = app.as_weak();
        let host = host.clone();
        move |action| {
            let weak = weak.clone();
            let host = host.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                if app.global::<Flow>().get_picker_open() || host.borrow().dialog.is_some() {
                    return;
                }
                if action == "recover" && host.borrow().controller.session.is_some() {
                    host.borrow_mut().recovery = true;
                    finish(&app, &host, Ok(Some(Effect::None)));
                    return;
                }
                if host.borrow().controller.pending()
                    || host.borrow().dialog.is_some()
                    || host.borrow().recovery
                    || app.global::<Flow>().get_recovery()
                    || app.global::<Flow>().get_picker_open()
                {
                    return;
                }
                if action == "flow-login" {
                    app.global::<SeeCut>()
                        .invoke_action("auth-open".into(), "".into());
                } else if action == "new" {
                    let mut h = host.borrow_mut();
                    let path = h.root.join(format!("Flow-{}", seecut_flow_core::id()));
                    h.start_naming(Naming::New(path));
                    app.global::<Flow>().set_dialog_name("未命名工程".into());
                    app.global::<Flow>()
                        .set_dialog_title("新建 Flow 工程".into());
                    drop(h);
                    finish(&app, &host, Ok(Some(Effect::None)));
                } else if action == "rename-node" {
                    let title = app.global::<Flow>().get_selected_title();
                    let result = host.borrow_mut().action("rename-node");
                    app.global::<Flow>().set_dialog_name(title);
                    app.global::<Flow>().set_dialog_title("重命名节点".into());
                    finish(&app, &host, result);
                } else if action == "rename" {
                    let result = host.borrow_mut().action("rename");
                    app.global::<Flow>()
                        .set_dialog_name(app.global::<Flow>().get_title());
                    app.global::<Flow>().set_dialog_title("重命名工程".into());
                    finish(&app, &host, result);
                } else if action == "add_asset" {
                    let result = image_execution::pick_new(&app, &host);
                    finish(&app, &host, result.map(|()| None));
                } else if action == "open" {
                    pick_path(&app, &host, false);
                } else {
                    let result = {
                        let mut h = host.borrow_mut();
                        match image_execution::action(&app, &mut h, action.as_str()) {
                            Ok(true) => Ok(Some(Effect::None)),
                            Ok(false) => h.action(action.as_str()),
                            Err(error) => Err(error),
                        }
                    };
                    finish(&app, &host, result);
                }
            });
        }
    });
    ui.on_project_action({
        let weak = app.as_weak();
        let host = host.clone();
        move |action, key| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let result = (|| {
                let mut h = host.borrow_mut();
                if h.dialog.is_some()
                    || h.controller.pending()
                    || h.recovery
                    || app.global::<Flow>().get_picker_open()
                {
                    return Ok(None);
                }
                if action == "search" {
                    h.search = key.to_string();
                    h.filter_projects();
                    return Ok(None);
                }
                if action == "sort" {
                    h.name_sort = !h.name_sort;
                    h.filter_projects();
                    return Ok(None);
                }
                let p = h
                    .projects
                    .iter()
                    .find(|p| p.key() == key.as_str())
                    .cloned()
                    .ok_or("工程列表已更新，请刷新")?;
                match action.as_str() {
                    "open" => h.request(Intent::OpenProject(p.path, p.document_id, p.project_id)),
                    "rename" => {
                        if p.read_only {
                            return Err("此工程为只读".into());
                        }
                        app.global::<Flow>().set_dialog_name(p.title.clone().into());
                        app.global::<Flow>().set_dialog_title("重命名工程".into());
                        h.start_naming(Naming::Rename(p));
                        Ok(Some(Effect::None))
                    }
                    _ => Err("未知工程操作".into()),
                }
            })();
            finish(&app, &host, result);
        }
    });
    ui.on_name_submit({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, name| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let result = host.borrow_mut().submit_name(token.as_str(), name.as_str());
            finish(&app, &host, result);
        }
    });
    ui.on_name_cancel({
        let weak = app.as_weak();
        let host = host.clone();
        move |token| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut h = host.borrow_mut();
            if h.dialog.as_ref().is_some_and(|d| d.token == token.as_str()) {
                h.dialog = None;
                h.error = false;
            }
            drop(h);
            finish(&app, &host, Ok(Some(Effect::None)));
        }
    });
    ui.on_recover({
        let weak = app.as_weak();
        let host = host.clone();
        move |choice| {
            let weak = weak.clone();
            let host = host.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                if !host.borrow().recovery || app.global::<Flow>().get_picker_open() {
                    return;
                }
                let result = match choice {
                    0 => {
                        host.borrow_mut().recovery = false;
                        Ok(Some(Effect::None))
                    }
                    1 => {
                        pick_path(&app, &host, true);
                        return;
                    }
                    2 => {
                        let mut h = host.borrow_mut();
                        h.controller.discard_and_close();
                        h.browsing = true;
                        h.refresh_projects();
                        h.preview.clear();
                        h.connecting.clear();
                        h.recovery = false;
                        h.recovery_available = false;
                        h.error = false;
                        h.message = "已放弃未保存内容并关闭。可重新打开原工程检查磁盘内容。".into();
                        Ok(Some(Effect::None))
                    }
                    _ => Err("无效的恢复选项".into()),
                };
                finish(&app, &host, result);
            });
        }
    });
    ui.on_selected({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, id| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            if !host.borrow().controller.token_matches(token.as_str()) {
                return;
            }
            let result = host
                .borrow_mut()
                .select_nodes(
                    if id.is_empty() {
                        Default::default()
                    } else {
                        std::collections::BTreeSet::from([id.to_string()])
                    },
                    !id.is_empty(),
                )
                .map(|()| Some(Effect::None));
            finish(&app, &host, result);
        }
    });
    ui.on_draft_changed({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, text| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let mut h = host.borrow_mut();
            if let Err(e) = h.controller.change_draft(token.as_str(), text.to_string()) {
                h.fail(e);
                app.global::<Flow>()
                    .set_prompt(h.controller.prompt().into());
            }
            let ui = app.global::<Flow>();
            ui.set_dirty(h.controller.dirty());
            ui.set_error(h.error);
            ui.set_message(h.message.clone().into());
        }
    });
    ui.on_prompt_commit({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, text| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let result = (|| {
                let mut h = host.borrow_mut();
                h.controller
                    .change_draft(token.as_str(), text.to_string())?;
                h.controller.save_draft()?;
                h.error = false;
                h.message = "提示词已保存".into();
                Ok(Some(Effect::None))
            })();
            finish(&app, &host, result);
        }
    });
    ui.on_resolve({
        let weak = app.as_weak();
        let host = host.clone();
        move |choice| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let result = {
                let mut h = host.borrow_mut();
                let result = h.controller.resolve(choice).map(Some);
                if result.is_ok() {
                    h.error = false;
                    if choice != 0 {
                        h.remember_current();
                        h.refresh_projects();
                        h.preview.clear();
                        h.connecting.clear();
                    }
                }
                result
            };
            finish(&app, &host, result);
        }
    });
    ui.on_leave_requested({
        let weak = app.as_weak();
        let host = host.clone();
        move |page| {
            let Some(app) = weak.upgrade() else {
                return false;
            };
            if app.global::<SeeCut>().get_page() != 8 || page == 8 {
                return false;
            }
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return true;
            }
            let flush = host.borrow_mut().controller.flush_viewport();
            if let Err(e) = flush {
                finish(&app, &host, Err(e));
                return true;
            }
            if !host.borrow().controller.dirty() {
                return false;
            }
            let result = host.borrow_mut().request(Intent::Navigate(page));
            finish(&app, &host, result);
            true
        }
    });
    ui.on_close_requested({
        let weak = app.as_weak();
        let host = host.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return false;
            };
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return true;
            }
            let flush = host.borrow_mut().controller.flush_viewport();
            if let Err(e) = flush {
                finish(&app, &host, Err(e));
                return true;
            }
            if !host.borrow().controller.dirty() {
                return false;
            }
            let result = host.borrow_mut().request(Intent::Quit);
            finish(&app, &host, result);
            true
        }
    });
    ui.on_viewport({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, x, y, z, commit| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut h = host.borrow_mut();
            if !h.controller.token_matches(token.as_str())
                || h.browsing
                || h.dialog.is_some()
                || h.controller.pending()
                || h.recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let view = [f64::from(x), f64::from(y), f64::from(z)];
            let result = h
                .controller
                .preview_viewport(token.as_str(), view)
                .and_then(|_| {
                    if commit {
                        h.controller.flush_viewport()
                    } else {
                        Ok(())
                    }
                });
            h.view = view;
            h.publish_edges();
            if let Err(e) = result {
                h.fail(e);
                app.global::<Flow>()
                    .set_recovery_available(h.recovery_available);
                app.global::<Flow>().set_error(true);
                app.global::<Flow>().set_message(h.message.clone().into());
            }
        }
    });
    ui.on_move_node({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, id, x, y, commit| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut h = host.borrow_mut();
            if !h.controller.token_matches(token.as_str())
                || h.browsing
                || h.dialog.is_some()
                || h.controller
                    .session
                    .as_ref()
                    .is_some_and(|s| s.store.is_read_only())
                || h.controller.pending()
                || h.recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let position = [f64::from(x), f64::from(y)];
            let Some(s) = h.controller.session.as_ref() else {
                return;
            };
            let Some(original) = s.graph().nodes.get(id.as_str()) else {
                return;
            };
            let delta = [
                position[0] - original.position[0],
                position[1] - original.position[1],
            ];
            let ids = if h.editor.selected.contains(id.as_str()) {
                h.editor.selected.clone()
            } else {
                std::collections::BTreeSet::from([id.to_string()])
            };
            let positions: BTreeMap<_, _> = ids
                .into_iter()
                .filter_map(|key| {
                    s.graph()
                        .nodes
                        .get(&key)
                        .map(|n| (key, [n.position[0] + delta[0], n.position[1] + delta[1]]))
                })
                .collect();
            if commit {
                let result = h
                    .controller
                    .session
                    .as_mut()
                    .ok_or_else(|| "工程已关闭".into())
                    .and_then(|s| s.edit(Command::Move { positions }));
                h.preview.clear();
                if let Err(e) = result {
                    h.fail(e);
                }
                h.publish(&app);
            } else {
                h.preview = positions;
                for row in 0..h.nodes.row_count() {
                    if let Some(mut item) = h.nodes.row_data(row)
                        && let Some(p) = h.preview.get(item.id.as_str())
                    {
                        item.x = p[0] as f32;
                        item.y = p[1] as f32;
                        h.nodes.set_row_data(row, item);
                    }
                }
                h.publish_editor(&app);
                h.publish_edges();
            }
        }
    });
    ui.on_output_port({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, id| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut h = host.borrow_mut();
            if !h.controller.token_matches(token.as_str())
                || h.browsing
                || h.dialog.is_some()
                || h.controller
                    .session
                    .as_ref()
                    .is_some_and(|s| s.store.is_read_only())
                || h.controller.pending()
                || h.recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            h.connecting = if h.connecting == id.as_str() {
                String::new()
            } else {
                id.to_string()
            };
            app.global::<Flow>()
                .set_connecting_from(h.connecting.clone().into());
        }
    });
    ui.on_input_port({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, id, port| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if host.borrow().dialog.is_some()
                || host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let result = (|| {
                let mut h = host.borrow_mut();
                if !h.controller.token_matches(token.as_str()) {
                    return Err("工程已切换".into());
                }
                let s = h.controller.session.as_ref().ok_or("请先打开工程")?;
                let command = if h.connecting.is_empty() {
                    let edge = s
                        .graph()
                        .edges
                        .iter()
                        .find(|e| e.to == id.as_str() && e.input == port.as_str())
                        .ok_or("先点击输出端口，再点击此输入端口")?
                        .clone();
                    h.editor.edge = Some(edge);
                    return Ok(Some(Effect::None));
                } else {
                    let order = s
                        .graph()
                        .edges
                        .iter()
                        .filter(|e| e.to == id.as_str() && e.input == port.as_str())
                        .map(|e| e.order)
                        .max()
                        .map_or(Some(0), |x| x.checked_add(1))
                        .ok_or("连线顺序已耗尽")?;
                    Command::Connect {
                        edge: Edge {
                            from: h.connecting.clone(),
                            output: "out".into(),
                            to: id.to_string(),
                            input: port.to_string(),
                            order,
                        },
                    }
                };
                h.request(Intent::Edit(Box::new(command)))
            })();
            finish(&app, &host, result);
        }
    });
    editor::bind(app, &host);
    image_execution::bind(app, &host);
    host.borrow_mut().publish(app);
}
