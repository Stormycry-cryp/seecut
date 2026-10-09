// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native Flow host. Graph commands are durable; generation and asset writes stay disabled.
use crate::{
    flow_controller::{Controller, Effect, Intent},
    ui::{App, Flow, FlowEdgeView, FlowNodeView, SeeCut},
};
use seecut_flow_core::{
    Result,
    commands::Command,
    graph::{Edge, Node},
};
use serde_json::json;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, rc::Rc};

struct Host {
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
        self.recovery_available |= self.controller.session.is_some();
        self.error = true;
        self.message = error;
    }
    fn request(&mut self, intent: Intent) -> Result<Option<Effect>> {
        if self.recovery {
            return Err("请先处理恢复对话框".into());
        }
        let previous = self.controller.session.as_ref().map(|s| s.token.clone());
        let effect = self.controller.request(intent)?;
        if previous != self.controller.session.as_ref().map(|s| s.token.clone()) {
            self.recovery_available = false;
        }
        if effect.is_some() {
            self.preview.clear();
            self.connecting.clear();
        }
        self.error = false;
        self.message = "生成、资产导入及交付尚未接通；节点编辑会自动保存".into();
        Ok(effect)
    }
    fn publish(&mut self, app: &App) {
        let ui = app.global::<Flow>();
        ui.set_open(self.controller.session.is_some());
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
                s.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
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
            self.view = self.controller.viewport().unwrap_or(s.graph().viewport);
            ui.set_pan_x(self.view[0] as f32);
            ui.set_pan_y(self.view[1] as f32);
            ui.set_zoom(self.view[2] as f32);
            let rows = s
                .graph()
                .nodes
                .values()
                .map(|n| {
                    let summary = if n.kind == "prompt" {
                        n.params["text"].as_str().unwrap_or("").to_owned()
                    } else {
                        format!("{}\n尚未接通执行", kind_label(&n.kind))
                    };
                    let p = self.preview.get(&n.id).unwrap_or(&n.position);
                    FlowNodeView {
                        id: n.id.clone().into(),
                        title: n.title.clone().into(),
                        kind: n.kind.clone().into(),
                        summary: summary.into(),
                        state: "未执行".into(),
                        x: p[0] as f32,
                        y: p[1] as f32,
                        preview: Default::default(),
                        has_preview: false,
                        inputs: ModelRc::new(VecModel::from(
                            n.ports()
                                .map(|p| {
                                    p.0.into_iter()
                                        .map(|p| SharedString::from(p.name))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default(),
                        )),
                    }
                })
                .collect::<Vec<_>>();
            self.nodes.set_vec(rows);
        } else {
            ui.set_token("".into());
            ui.set_selected_id("".into());
            ui.set_selected_kind("".into());
            ui.set_can_undo(false);
            ui.set_can_redo(false);
            self.nodes.set_vec(vec![]);
        }
        self.publish_edges();
    }
    fn publish_edges(&self) {
        let Some(s) = self.controller.session.as_ref() else {
            self.edges.set_vec(vec![]);
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
                    selected: from.id == s.selected || to.id == s.selected,
                })
            })
            .collect::<Vec<_>>();
        self.edges.set_vec(edges);
    }
    fn action(&mut self, action: &str) -> Result<Option<Effect>> {
        self.controller.flush_viewport()?;
        match action {
            "new" => {
                std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
                let name = format!("Flow-{}", chrono::Local::now().format("%Y%m%d-%H%M%S-%3f"));
                self.request(Intent::New(self.root.join(name)))
            }
            "close" => self.request(Intent::Close),
            "save" => {
                self.controller.save_draft()?;
                self.error = false;
                self.message = "工程与提示词已保存".into();
                Ok(Some(Effect::None))
            }
            "undo" => self.request(Intent::Undo),
            "redo" => self.request(Intent::Redo),
            "arrange" => self.request(Intent::Edit(Command::Arrange)),
            "duplicate" | "remove" => {
                let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
                if s.selected.is_empty() {
                    return Err("请先选择节点".into());
                }
                let nodes = vec![s.selected.clone()];
                self.request(Intent::Edit(if action == "remove" {
                    Command::Remove { nodes }
                } else {
                    Command::Duplicate {
                        nodes,
                        incoming: true,
                    }
                }))
            }
            "fit" => {
                let s = self.controller.session.as_mut().ok_or("请先打开工程")?;
                let x = s
                    .graph()
                    .nodes
                    .values()
                    .map(|n| n.position[0])
                    .reduce(f64::min)
                    .unwrap_or(0.);
                let y = s
                    .graph()
                    .nodes
                    .values()
                    .map(|n| n.position[1])
                    .reduce(f64::min)
                    .unwrap_or(0.);
                s.edit(Command::Viewport {
                    viewport: [40. - x * 0.75, 40. - y * 0.75, 0.75],
                })?;
                Ok(Some(Effect::None))
            }
            "cancel-connect" => {
                self.connecting.clear();
                Ok(Some(Effect::None))
            }
            a if a.starts_with("add_") => {
                let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
                let kind = &a[4..];
                let params = match kind {
                    "prompt" => json!({"text":""}),
                    "image_generate" | "video_generate" => {
                        json!({"model":"unconfigured","count":1})
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
                self.request(Intent::Edit(Command::Add { node }))
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
        let was_confirm = app.global::<Flow>().get_confirm() || app.global::<Flow>().get_recovery();
        let effect = match result {
            Ok(effect) => effect,
            Err(error) => {
                host.borrow_mut().fail(error);
                None
            }
        };
        host.borrow_mut().publish(&app);
        let ui = app.global::<Flow>();
        if was_confirm && !ui.get_confirm() && !ui.get_recovery() {
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

pub(crate) fn bind(app: &App, root: PathBuf) {
    let host = Rc::new(RefCell::new(Host {
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
                if app.global::<Flow>().get_picker_open() {
                    return;
                }
                if action == "recover" && host.borrow().controller.session.is_some() {
                    host.borrow_mut().recovery = true;
                    finish(&app, &host, Ok(Some(Effect::None)));
                    return;
                }
                if host.borrow().controller.pending()
                    || host.borrow().recovery
                    || app.global::<Flow>().get_recovery()
                    || app.global::<Flow>().get_picker_open()
                {
                    return;
                }
                if action == "open" {
                    pick_path(&app, &host, false);
                } else {
                    let result = host.borrow_mut().action(action.as_str());
                    finish(&app, &host, result);
                }
            });
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
            if host.borrow().recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            if !host.borrow().controller.token_matches(token.as_str()) {
                return;
            }
            let result = host.borrow_mut().request(Intent::Select(id.to_string()));
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
            if host.borrow().recovery
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
            if host.borrow().recovery
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
            if host.borrow().recovery
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
            if host.borrow().recovery
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
            if host.borrow().recovery
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
                || h.controller.pending()
                || h.recovery
                || app.global::<Flow>().get_recovery()
                || app.global::<Flow>().get_picker_open()
            {
                return;
            }
            let position = [f64::from(x), f64::from(y)];
            if commit {
                let result = h
                    .controller
                    .session
                    .as_mut()
                    .ok_or_else(|| "工程已关闭".into())
                    .and_then(|s| {
                        s.edit(Command::Move {
                            positions: BTreeMap::from([(id.to_string(), position)]),
                        })
                    });
                h.preview.clear();
                if let Err(e) = result {
                    h.fail(e);
                }
                h.publish(&app);
            } else {
                h.preview.insert(id.to_string(), position);
                for row in 0..h.nodes.row_count() {
                    if let Some(mut item) = h.nodes.row_data(row) {
                        if item.id == id {
                            item.x = x;
                            item.y = y;
                            h.nodes.set_row_data(row, item);
                            break;
                        }
                    }
                }
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
            if host.borrow().recovery
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
                    Command::Disconnect { edge }
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
                h.request(Intent::Edit(command))
            })();
            finish(&app, &host, result);
        }
    });
    host.borrow_mut().publish(app);
}
