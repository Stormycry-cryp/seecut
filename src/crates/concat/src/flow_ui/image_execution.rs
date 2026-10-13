//! UI ownership and background shared-client jobs, bound to the original project/attempt.
use super::{Host, finish, sync_rows};
use crate::{
    cloud::{AccountAccess, RegisteredImage, library_image},
    ui::{App, Flow, FlowAttachment, FlowCandidate, FlowParameter, SeeCut},
};
use seecut_flow_core::{
    Result,
    commands::Command,
    execution::{Execution, Phase, SubmissionFailure},
    host::{AssetPickerRequest, LibraryAsset},
    runner::{Asset, Scope},
    store::Store,
};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Origin {
    path: PathBuf,
    document: String,
    project: String,
}
struct Source {
    origin: Origin,
    execution: Execution,
    views: HashMap<String, Value>,
    prepared: HashMap<String, Value>,
    blocked: bool,
    open_token: Option<String>,
}
enum ValueReply {
    Catalog(Scope, Value),
    Discovered(Vec<(Origin, Execution)>),
    Prepared(Origin, String, Value),
    Task(Origin, String, Scope, Value),
    SubmissionFailed(Origin, String, Scope, SubmissionFailure),
    Registered(Origin, String, String, String, RegisteredImage),
    Picked(String, PathBuf, AssetPickerRequest, RegisteredImage),
    Preview(RegisteredImage),
    Deliver(String, String, Scope, String, String, RegisteredImage),
}
struct Reply {
    key: String,
    preparation: Option<(Origin, String)>,
    registration: Option<String>,
    result: std::result::Result<ValueReply, String>,
}
pub(super) struct Images {
    account: AccountAccess,
    sources: BTreeMap<PathBuf, Source>,
    catalog: Value,
    catalog_scope: Option<Scope>,
    running: HashSet<String>,
    cooldown: HashMap<String, Instant>,
    tx: mpsc::Sender<Reply>,
    rx: mpsc::Receiver<Reply>,
    previews: HashMap<String, slint::Image>,
    observed_scope: Option<Scope>,
    parameters: Rc<VecModel<FlowParameter>>,
    references: Rc<VecModel<FlowAttachment>>,
    candidates: Rc<VecModel<FlowCandidate>>,
}
impl Images {
    pub(super) fn new(account: AccountAccess, root: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut s = Self {
            account,
            sources: BTreeMap::new(),
            catalog: Value::Null,
            catalog_scope: None,
            running: HashSet::new(),
            cooldown: HashMap::new(),
            tx,
            rx,
            previews: HashMap::new(),
            observed_scope: None,
            parameters: Rc::new(VecModel::default()),
            references: Rc::new(VecModel::default()),
            candidates: Rc::new(VecModel::default()),
        };
        s.spawn("discover".into(), None, move || {
            let mut sources = Vec::new();
            for project in seecut_flow_core::catalog::list(&root).projects {
                if !project.path.join("execution.json").is_file() {
                    continue;
                }
                if let Ok(store) =
                    Store::open_project(&project.path, &project.document_id, &project.project_id)
                {
                    sources.push((
                        Origin {
                            path: project.path,
                            document: project.document_id,
                            project: project.project_id,
                        },
                        store.execution().clone(),
                    ));
                }
            }
            Ok(ValueReply::Discovered(sources))
        });
        s
    }
    fn spawn(
        &mut self,
        key: String,
        preparation: Option<(Origin, String)>,
        job: impl FnOnce() -> std::result::Result<ValueReply, String> + Send + 'static,
    ) {
        if self.running.contains(&key)
            || self
                .cooldown
                .get(&key)
                .is_some_and(|until| Instant::now() < *until)
        {
            return;
        }
        self.running.insert(key.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = job();
            let _ = tx.send(Reply {
                key,
                preparation,
                registration: None,
                result,
            });
        });
    }
    pub(super) fn track(&mut self, path: PathBuf, execution: Execution) {
        let path = path.canonicalize().unwrap_or(path);
        let origin = Origin {
            path: path.clone(),
            document: execution.document_id.clone(),
            project: execution.project_id.clone(),
        };
        if let Some(source) = self.sources.get_mut(&path) {
            source.execution = execution;
        } else {
            self.sources.insert(
                path,
                Source {
                    origin,
                    execution,
                    views: HashMap::new(),
                    prepared: HashMap::new(),
                    blocked: false,
                    open_token: None,
                },
            );
        }
    }
    fn preview(&mut self, image: &RegisteredImage) {
        let key = asset_key(
            &image.asset.id,
            &serde_json::to_string(&image.asset.version).unwrap_or_default(),
        );
        let preview = image
            .thumbnail
            .as_ref()
            .and_then(|p| slint::Image::load_from_path(p).ok())
            .unwrap_or_default();
        self.previews.insert(key, preview);
    }
    pub(super) fn latest<'a>(
        &self,
        store: &'a Store,
        graph: &str,
        node: &str,
    ) -> Option<&'a seecut_flow_core::execution::Attempt> {
        let scope = self.account.flow_scope();
        store.execution().attempts.iter().rev().find(|a| {
            a.draft.context.graph_id == graph
                && a.draft.context.node_id == node
                && a.draft.context.scope == scope
        })
    }
    pub(super) fn publish(
        &mut self,
        app: &App,
        store: Option<&Store>,
        graph: Option<&seecut_flow_core::graph::Graph>,
        node: &str,
    ) {
        let ui = app.global::<Flow>();
        let scope = self.account.flow_scope();
        ui.set_images_enabled(!store.is_some_and(Store::is_read_only));
        ui.set_execution_enabled(
            self.catalog_scope.as_ref() == Some(&scope)
                && self.catalog["models"].as_array().is_some_and(|v| {
                    v.iter()
                        .any(|m| m["kind"] == "image" && m["available"] == true)
                }),
        );
        ui.set_generation_parameters(ModelRc::from(self.parameters.clone()));
        ui.set_references(ModelRc::from(self.references.clone()));
        ui.set_candidates(ModelRc::from(self.candidates.clone()));
        ui.set_points("0".into());
        ui.set_paused(store.is_some_and(|s| s.execution().paused));
        ui.set_has_attempt(false);
        ui.set_selected_status("".into());
        ui.set_selected_output(false);
        let (Some(store), Some(graph)) = (store, graph) else {
            sync_rows(&self.parameters, vec![]);
            sync_rows(&self.references, vec![]);
            sync_rows(&self.candidates, vec![]);
            return;
        };
        let Some(selected) = graph.nodes.get(node) else {
            sync_rows(&self.parameters, vec![]);
            sync_rows(&self.references, vec![]);
            sync_rows(&self.candidates, vec![]);
            return;
        };
        if selected.kind == "image_generate" {
            let model = self
                .catalog_scope
                .as_ref()
                .filter(|current| *current == &scope)
                .and_then(|_| self.catalog["models"].as_array())
                .and_then(|v| v.iter().find(|m| m["id"] == selected.params["model"]));
            let label = model
                .and_then(|m| {
                    m["label"]
                        .as_str()
                        .or_else(|| m["name"].as_str())
                        .or_else(|| m["id"].as_str())
                })
                .unwrap_or(if self.account.image_client().is_none() {
                    "登录选择模型"
                } else {
                    "选择模型"
                });
            let mut parameters = vec![FlowParameter {
                id: "model".into(),
                label: label.into(),
            }];
            for field in ["size", "quality", "quantity"] {
                if model.is_some_and(|m| !m["parameters"][field].is_null()) {
                    let value =
                        selected.params.get(field).cloned().unwrap_or_else(|| {
                            model.unwrap()["parameters"][field]["default"].clone()
                        });
                    parameters.push(FlowParameter {
                        id: field.into(),
                        label: parameter_label(field, &value).into(),
                    });
                }
            }
            sync_rows(&self.parameters, parameters);
        } else {
            sync_rows(&self.parameters, vec![]);
        }
        let mut references = Vec::new();
        for edge in graph
            .edges
            .iter()
            .filter(|e| e.to == node && e.input == "references")
        {
            if let Some(source) = graph.nodes.get(&edge.from) {
                let preview = self.node_preview(store, graph, source).unwrap_or_default();
                references.push((
                    edge.order,
                    FlowAttachment {
                        id: source.id.clone().into(),
                        label: source.title.clone().into(),
                        preview,
                    },
                ));
            }
        }
        references.sort_by_key(|v| v.0);
        sync_rows(
            &self.references,
            references.into_iter().map(|v| v.1).collect::<Vec<_>>(),
        );
        let image_node = if selected.kind == "select" {
            graph
                .edges
                .iter()
                .find(|e| e.to == node && e.input == "candidates")
                .map(|e| e.from.as_str())
                .unwrap_or(node)
        } else {
            node
        };
        if let Some(attempt) = self.latest(store, &graph.id, image_node) {
            ui.set_selected_output(
                seecut_flow_core::host::node_choice(selected, attempt).is_some(),
            );
            ui.set_has_attempt(true);
            ui.set_selected_status(attempt_status(attempt).into());
            let mut rows = Vec::new();
            let mut credits = 0_u64;
            for task in attempt.tasks.values() {
                credits = credits.saturating_add(task.captured_credits);
                for output in task.outputs.values() {
                    if let Some(asset) = &output.asset {
                        self.load_preview(&asset.id, &asset.version);
                    }
                    let preview = output
                        .asset
                        .as_ref()
                        .and_then(|a| self.previews.get(&asset_key(&a.id, &a.version)))
                        .cloned()
                        .unwrap_or_default();
                    rows.push(FlowCandidate {
                        id: serde_json::to_string(&(&attempt.id, &task.id, &output.id))
                            .unwrap_or_default()
                            .into(),
                        label: format!("结果 {}", rows.len() + 1).into(),
                        selected: seecut_flow_core::host::node_choice(selected, attempt)
                            == Some((task.id.clone(), output.id.clone())),
                        ready: output.asset.is_some(),
                        preview,
                    });
                }
            }
            ui.set_points(credits.to_string().into());
            sync_rows(&self.candidates, rows);
        } else {
            sync_rows(&self.candidates, vec![]);
        }
    }
    pub(super) fn node_preview(
        &mut self,
        store: &Store,
        graph: &seecut_flow_core::graph::Graph,
        node: &seecut_flow_core::graph::Node,
    ) -> Option<slint::Image> {
        let asset = if node.kind == "asset" {
            Some((
                node.params["asset_id"].as_str()?.to_owned(),
                node.params["content_version"].as_str()?.to_owned(),
            ))
        } else {
            let id = if node.kind == "select" {
                graph
                    .edges
                    .iter()
                    .find(|e| e.to == node.id && e.input == "candidates")
                    .map(|e| e.from.as_str())
                    .unwrap_or(&node.id)
            } else {
                &node.id
            };
            self.latest(store, &graph.id, id)
                .and_then(|a| {
                    seecut_flow_core::host::node_choice(node, a)
                        .and_then(|(t, o)| a.tasks.get(&t)?.outputs.get(&o)?.asset.as_ref())
                        .or_else(|| {
                            a.tasks
                                .values()
                                .flat_map(|t| t.outputs.values())
                                .find_map(|o| o.asset.as_ref())
                        })
                })
                .map(|a| (a.id.clone(), a.version.clone()))
        }?;
        self.load_preview(&asset.0, &asset.1);
        self.previews.get(&asset_key(&asset.0, &asset.1)).cloned()
    }
    fn load_preview(&mut self, id: &str, version: &str) {
        if id.is_empty()
            || version.is_empty()
            || self.previews.contains_key(&asset_key(id, version))
        {
            return;
        }
        let id = id.to_owned();
        let version = version.to_owned();
        let key = format!("preview:{}", asset_key(&id, &version));
        self.spawn(key, None, move || {
            library_image(&id, Some(&version)).map(ValueReply::Preview)
        });
    }
}
pub(super) fn attempt_status(a: &seecut_flow_core::execution::Attempt) -> &'static str {
    if a.phase == Phase::Rejected {
        "未受理"
    } else if a.error.is_some() {
        "待核对"
    } else if a.tasks.values().any(|t| t.status == "pending_reconcile") {
        "待核账"
    } else {
        match a.phase {
            Phase::Prepared => "准备中",
            Phase::Intent => "核实提交",
            Phase::Accepted => "处理中",
            Phase::Rejected => "未受理",
            Phase::Finished => {
                if a.tasks
                    .values()
                    .any(|t| t.outputs.values().any(|o| o.asset.is_some()))
                {
                    "已入库"
                } else {
                    "未成功"
                }
            }
        }
    }
}
fn asset_key(id: &str, version: &str) -> String {
    seecut_flow_core::digest(&(id, version))
}
fn parameter_label(field: &str, value: &Value) -> String {
    let raw = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    match field {
        "quantity" => format!("{raw} 张"),
        "size" if raw == "auto" => "自动尺寸".into(),
        "quality" if raw == "high" => "高清".into(),
        _ => raw,
    }
}
fn save(h: &mut Host, origin: &Origin, baseline: &Execution, next: Execution) -> Result<()> {
    if h.images
        .sources
        .get(&origin.path)
        .is_some_and(|source| source.blocked)
    {
        return Err("执行记录写入已阻断，请重新打开工程恢复".into());
    }
    let active = h.controller.session.as_mut().filter(|s| {
        s.path.canonicalize().ok().as_ref() == Some(&origin.path)
            && s.store.document().id == origin.document
            && s.store.document().project_id == origin.project
    });
    let persisted = if let Some(s) = active {
        s.store.save_execution_from(baseline, next.clone())
    } else {
        Store::open_project(&origin.path, &origin.document, &origin.project)
            .and_then(|mut store| store.save_execution_from(baseline, next.clone()))
    };
    if let Err(error) = persisted {
        if let Some(source) = h.images.sources.get_mut(&origin.path) {
            source.blocked = true;
        }
        return Err(error);
    }
    h.images.track(origin.path.clone(), next);
    Ok(())
}
fn original(h: &Host, origin: &Origin) -> Result<Execution> {
    h.images
        .sources
        .get(&origin.path)
        .map(|s| s.execution.clone())
        .ok_or("原工程执行记录不可用".into())
}
pub(super) fn action(app: &App, h: &mut Host, action: &str) -> Result<bool> {
    match action {
        "run_node" | "run_to" | "retry" => {
            h.controller.save_draft()?;
            h.controller.flush_viewport()?;
            let client = h.images.account.image_client().ok_or("请先登录")?;
            let s = h.controller.session.as_mut().ok_or("请先打开工程")?;
            let draft = seecut_flow_core::host::image_draft(
                &s.store,
                &client.scope,
                &s.graph_id,
                &s.selected,
            )?;
            let mut next = s.store.execution().clone();
            let id = next.begin(draft, seecut_flow_core::digest(s.store.document()))?;
            next.attempt_mut(&id)?.error = None;
            s.store.save_execution(next.clone())?;
            h.images.track(s.path.clone(), next);
            h.message = "".into();
            h.error = false;
            Ok(true)
        }
        "cancel" | "resume" => {
            let s = h.controller.session.as_mut().ok_or("请先打开工程")?;
            let mut next = s.store.execution().clone();
            next.paused = action == "cancel";
            s.store.save_execution(next.clone())?;
            h.images.track(s.path.clone(), next);
            h.message = "".into();
            h.error = false;
            Ok(true)
        }
        "reconcile" => {
            h.images.cooldown.clear();
            h.message = "".into();
            h.error = false;
            Ok(true)
        }
        "flow-canvas" | "flow-clip" => {
            let s = h.controller.session.as_ref().ok_or("请先打开工程")?;
            let selected = s.graph().nodes.get(&s.selected).ok_or("节点已切换")?;
            let image_node = if selected.kind == "select" {
                s.graph()
                    .edges
                    .iter()
                    .find(|e| e.to == s.selected && e.input == "candidates")
                    .map(|e| e.from.as_str())
                    .unwrap_or(&s.selected)
            } else {
                &s.selected
            };
            let a = h
                .images
                .latest(&s.store, &s.graph_id, image_node)
                .ok_or("请先选用图片结果")?;
            let (task, out) =
                seecut_flow_core::host::node_choice(selected, a).ok_or("请先选用图片结果")?;
            let asset = a
                .tasks
                .get(&task)
                .and_then(|t| t.outputs.get(&out))
                .and_then(|o| o.asset.as_ref())
                .ok_or("图片尚未入库")?
                .clone();
            let kind = if action == "flow-canvas" {
                "canvas"
            } else {
                "clip"
            }
            .to_owned();
            let key = format!("deliver:{}", asset.id);
            let token = s.token.clone();
            let scope = h.images.account.flow_scope();
            let node = s.selected.clone();
            let attempt = a.id.clone();
            h.images.spawn(key, None, move || {
                library_image(&asset.id, Some(&asset.version))
                    .map(|image| ValueReply::Deliver(kind, token, scope, node, attempt, image))
            });
            Ok(true)
        }
        _ => {
            let _ = app;
            Ok(false)
        }
    }
}
pub(super) fn settings(app: &App, h: &mut Host, field: &str) -> Result<()> {
    h.controller.save_draft()?;
    let s = h.controller.session.as_ref().ok_or("请先打开工程")?;
    if s.store.is_read_only() {
        return Err("只读工程不能修改模型".into());
    }
    let scope = h.images.account.flow_scope();
    if h.images.catalog_scope.as_ref() != Some(&scope) {
        return Err("模型目录尚未就绪，请稍后重试".into());
    }
    let node = s
        .graph()
        .nodes
        .get(&s.selected)
        .filter(|n| n.kind == "image_generate")
        .ok_or("请先选择图片生成节点")?;
    let mut rows = Vec::new();
    if field == "model" {
        for model in h.images.catalog["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["kind"] == "image" && m["available"] == true)
        {
            rows.push(FlowParameter {
                id: serde_json::to_string(&("model", &model["id"]))
                    .unwrap_or_default()
                    .into(),
                label: model["label"]
                    .as_str()
                    .or_else(|| model["name"].as_str())
                    .unwrap_or_else(|| model["id"].as_str().unwrap_or_default())
                    .into(),
            });
        }
    } else {
        let model = h.images.catalog["models"]
            .as_array()
            .and_then(|v| v.iter().find(|m| m["id"] == node.params["model"]))
            .ok_or("请先选择模型")?;
        for value in model["parameters"][field]["values"]
            .as_array()
            .into_iter()
            .flatten()
        {
            rows.push(FlowParameter {
                id: serde_json::to_string(&(field, value))
                    .unwrap_or_default()
                    .into(),
                label: parameter_label(field, value).into(),
            });
        }
    }
    let ui = app.global::<Flow>();
    ui.set_setting_token(
        seecut_flow_core::digest(&(
            &s.token,
            &s.graph_id,
            &s.selected,
            s.graph().revision,
            &scope,
        ))
        .into(),
    );
    ui.set_setting_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_setting_title(
        match field {
            "size" => "画面尺寸",
            "quality" => "画质",
            "quantity" => "数量",
            _ => "图片模型",
        }
        .into(),
    );
    ui.set_settings_open(true);
    Ok(())
}
pub(super) fn set_setting(h: &mut Host, token: &str, value: &str) -> Result<()> {
    let s = h.controller.session.as_ref().ok_or("请先打开工程")?;
    if token
        != seecut_flow_core::digest(&(
            &s.token,
            &s.graph_id,
            &s.selected,
            s.graph().revision,
            h.images.account.flow_scope(),
        ))
    {
        return Err("工程、节点或参数已切换".into());
    }
    if h.images.catalog_scope.as_ref() != Some(&h.images.account.flow_scope()) {
        return Err("账户或服务已切换".into());
    }
    let (field, value): (String, Value) = serde_json::from_str(value).map_err(|e| e.to_string())?;
    let s = h.controller.session.as_mut().ok_or("请先打开工程")?;
    let node = s
        .graph()
        .nodes
        .get(&s.selected)
        .filter(|n| n.kind == "image_generate")
        .ok_or("节点已切换")?;
    let mut params = node.params.clone();
    if field == "model" {
        let model = h.images.catalog["models"]
            .as_array()
            .and_then(|v| {
                v.iter()
                    .find(|m| m["id"] == value && m["kind"] == "image" && m["available"] == true)
            })
            .ok_or("模型已不可用")?;
        let own_prompt = params["prompt"].as_str().unwrap_or("").to_owned();
        params = json!({"model":value,"capability_version":seecut_flow_core::digest(model),"count":1,"prompt":own_prompt});
        for key in ["size", "quality", "quantity"] {
            if !model["parameters"][key].is_null() {
                params[key] = model["parameters"][key]["default"].clone();
            }
        }
        params["count"] = params["quantity"].as_u64().map_or(json!(1), |n| json!(n));
        if let Some(effort) = h.images.catalog["efforts"].as_array().and_then(|v| {
            v.iter().find(|e| {
                e["available"] == true
                    && e["generation"]["image_model_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&value))
            })
        }) {
            params["effort_id"] = effort["id"].clone();
            params["policy_revision"] = effort["policy_revision"].clone();
        }
    } else {
        if !matches!(field.as_str(), "size" | "quality" | "quantity") {
            return Err("无效的生成参数".into());
        }
        let model = h.images.catalog["models"]
            .as_array()
            .and_then(|v| {
                v.iter()
                    .find(|m| m["id"] == params["model"] && m["available"] == true)
            })
            .ok_or("模型已不可用")?;
        if !model["parameters"][&field]["values"]
            .as_array()
            .is_some_and(|values| values.contains(&value))
        {
            return Err("模型参数已变化".into());
        }
        params[&field] = value.clone();
        if field == "quantity" {
            params["count"] = value;
        }
    }
    s.edit(Command::Parameters {
        node: s.selected.clone(),
        params,
    })?;
    Ok(())
}
pub(super) fn choose(h: &mut Host, value: &str) -> Result<()> {
    let (attempt, task, output): (String, String, String) =
        serde_json::from_str(value).map_err(|e| e.to_string())?;
    let scope = h.images.account.flow_scope();
    let s = h.controller.session.as_mut().ok_or("请先打开工程")?;
    let selected = s.graph().nodes.get(&s.selected).ok_or("节点已切换")?;
    let node = if selected.kind == "select" {
        s.graph()
            .edges
            .iter()
            .find(|e| e.to == selected.id && e.input == "candidates")
            .map(|e| e.from.as_str())
            .unwrap_or(&s.selected)
    } else {
        &s.selected
    };
    if s.store
        .execution()
        .attempts
        .iter()
        .rev()
        .find(|a| a.draft.context.node_id == node && a.draft.context.scope == scope)
        .is_none_or(|a| a.id != attempt)
    {
        return Err("结果批次已切换".into());
    }
    let mut e = s.store.execution().clone();
    e.select(&attempt, &task, &output, &scope)?;
    let mut params = selected.params.clone();
    params["selected_output"] = json!({"attempt":attempt,"task":task,"output":output});
    s.edit(Command::Parameters {
        node: s.selected.clone(),
        params,
    })?;
    Ok(())
}
pub(super) fn pick_new(app: &App, host: &Rc<RefCell<Host>>) -> Result<()> {
    host.borrow_mut().select_nodes(Default::default(), false)?;
    pick(app, host)
}
pub(super) fn pick(app: &App, host: &Rc<RefCell<Host>>) -> Result<()> {
    {
        let mut h = host.borrow_mut();
        h.controller.save_draft()?;
        h.controller.flush_viewport()?;
    }
    let (account, token, path, request) = {
        let h = host.borrow();
        let s = h.controller.session.as_ref().ok_or("请先打开工程")?;
        (
            h.images.account.clone(),
            s.token.clone(),
            s.path.clone(),
            seecut_flow_core::host::picker_request_for_store(
                &s.store,
                &h.images.account.flow_scope(),
                &s.graph_id,
                &s.selected,
            )?,
        )
    };
    let host = host.clone();
    account.pick_flow_image(
        app,
        Rc::new(move |_, id| {
            let token = token.clone();
            let path = path.clone();
            let request = request.clone();
            host.borrow_mut().images.spawn(
                format!("pick:{}", request.operation_id),
                None,
                move || {
                    library_image(&id, None)
                        .map(|image| ValueReply::Picked(token, path, request, image))
                },
            );
        }),
    );
    Ok(())
}
fn tick(app: &App, h: &mut Host) -> bool {
    let scope = h.images.account.flow_scope();
    let mut changed = h.images.observed_scope.as_ref() != Some(&scope);
    h.images.observed_scope = Some(scope);
    if let Some(s) = h.controller.session.as_ref() {
        h.images.track(s.path.clone(), s.store.execution().clone());
        if let Some(source) = h
            .images
            .sources
            .get_mut(&s.path.canonicalize().unwrap_or_else(|_| s.path.clone()))
        {
            if source.open_token.as_deref() != Some(s.token.as_str()) {
                source.blocked = false;
                source.open_token = Some(s.token.clone());
            }
            source.blocked |= s.store.is_poisoned();
        }
    }
    while let Ok(reply) = h.images.rx.try_recv() {
        changed = true;
        h.images.running.remove(&reply.key);
        if let Some(key) = &reply.registration {
            h.images.cooldown.insert(
                key.clone(),
                Instant::now() + Duration::from_secs(if reply.result.is_ok() { 2 } else { 30 }),
            );
        }
        h.images.cooldown.insert(
            reply.key.clone(),
            Instant::now()
                + Duration::from_secs(if reply.result.is_ok() || reply.registration.is_some() {
                    2
                } else {
                    8
                }),
        );
        let result = (|| -> Result<()> {
            match reply.result {
                Err(error) => {
                    if let Some((origin, id)) = reply.preparation {
                        let baseline = original(h, &origin)?;
                        let mut e = baseline.clone();
                        e.attempt_mut(&id)?.error = Some("准备失败，请重试".into());
                        save(h, &origin, &baseline, e)?;
                    }
                    Err(error)
                }
                Ok(ValueReply::Catalog(scope, value)) => {
                    if h.images.account.flow_scope() == scope {
                        h.images.catalog = value;
                        h.images.catalog_scope = Some(scope);
                    }
                    Ok(())
                }
                Ok(ValueReply::Discovered(sources)) => {
                    for (origin, e) in sources {
                        h.images
                            .sources
                            .entry(origin.path.clone())
                            .or_insert(Source {
                                origin,
                                execution: e,
                                views: HashMap::new(),
                                prepared: HashMap::new(),
                                blocked: false,
                                open_token: None,
                            });
                    }
                    Ok(())
                }
                Ok(ValueReply::Prepared(origin, id, body)) => {
                    h.images
                        .sources
                        .get_mut(&origin.path)
                        .ok_or("原工程已关闭")?
                        .prepared
                        .insert(id, body);
                    Ok(())
                }
                Ok(ValueReply::Task(origin, id, scope, value)) => {
                    let baseline = original(h, &origin)?;
                    let mut e = baseline.clone();
                    let tasks = value["tasks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_else(|| vec![value.clone()]);
                    for task in &tasks {
                        e.observe(&id, &scope, task)?;
                    }
                    save(h, &origin, &baseline, e)?;
                    for task in tasks {
                        h.images
                            .sources
                            .get_mut(&origin.path)
                            .ok_or("原工程已关闭")?
                            .views
                            .insert(task["id"].as_str().unwrap_or_default().into(), task);
                    }
                    Ok(())
                }
                Ok(ValueReply::SubmissionFailed(origin, id, scope, failure)) => {
                    let baseline = original(h, &origin)?;
                    let mut e = baseline.clone();
                    let message = failure.message.clone();
                    e.submission_failed(&id, &scope, failure)?;
                    save(h, &origin, &baseline, e)?;
                    Err(message)
                }
                Ok(ValueReply::Registered(origin, id, task, output, image)) => {
                    let baseline = original(h, &origin)?;
                    let mut e = baseline.clone();
                    let asset = Asset {
                        id: image.asset.id.clone(),
                        version: serde_json::to_string(
                            image.asset.version.as_ref().ok_or("结果版本缺失")?,
                        )
                        .map_err(|e| e.to_string())?,
                        kind: seecut_flow_core::graph::MediaKind::Image,
                        simulated: false,
                    };
                    e.registered(&id, &task, &output, asset)?;
                    save(h, &origin, &baseline, e)?;
                    h.images.preview(&image);
                    Ok(())
                }
                Ok(ValueReply::Picked(token, path, request, image)) => {
                    let scope = h.images.account.flow_scope();
                    let s = h
                        .controller
                        .session
                        .as_mut()
                        .filter(|s| {
                            s.token == token
                                && s.path == path
                                && s.selected == request.context.node_id
                        })
                        .ok_or("工程或节点已切换，忽略旧素材选择")?;
                    let asset = Asset {
                        id: image.asset.id.clone(),
                        version: serde_json::to_string(
                            image.asset.version.as_ref().ok_or("素材缺少版本")?,
                        )
                        .map_err(|e| e.to_string())?,
                        kind: seecut_flow_core::graph::MediaKind::Image,
                        simulated: false,
                    };
                    s.accept_asset(
                        &scope,
                        &request,
                        LibraryAsset {
                            asset,
                            name: image.asset.name.clone(),
                            available: true,
                        },
                    )?;
                    if matches!(request.target, seecut_flow_core::host::AssetTarget::Create) {
                        let id = request.new_node_id.clone();
                        h.select_nodes(std::collections::BTreeSet::from([id]), true)?;
                    }
                    h.images.preview(&image);
                    Ok(())
                }
                Ok(ValueReply::Preview(image)) => {
                    h.images.preview(&image);
                    Ok(())
                }
                Ok(ValueReply::Deliver(kind, token, scope, node, attempt, image)) => {
                    let s = h
                        .controller
                        .session
                        .as_ref()
                        .filter(|s| s.token == token && s.selected == node)
                        .ok_or("工程或节点已切换，忽略旧交付")?;
                    if h.images.account.flow_scope() != scope {
                        return Err("账户或服务已切换，忽略旧交付".into());
                    }
                    let selected_node = s.graph().nodes.get(&s.selected).ok_or("节点已切换")?;
                    let image_node = if selected_node.kind == "select" {
                        s.graph()
                            .edges
                            .iter()
                            .find(|e| e.to == s.selected && e.input == "candidates")
                            .map(|e| e.from.as_str())
                            .ok_or("上游已切换")?
                    } else {
                        &s.selected
                    };
                    let a = h
                        .images
                        .latest(&s.store, &s.graph_id, image_node)
                        .filter(|a| a.id == attempt)
                        .ok_or("结果批次已切换")?;
                    let (task, out) = seecut_flow_core::host::node_choice(selected_node, a)
                        .ok_or("结果选择已变化")?;
                    let selected = a
                        .tasks
                        .get(&task)
                        .and_then(|t| t.outputs.get(&out))
                        .and_then(|o| o.asset.as_ref())
                        .ok_or("结果选择已变化")?;
                    if selected.id != image.asset.id
                        || Some(&selected.version)
                            != image
                                .asset
                                .version
                                .as_ref()
                                .and_then(|v| serde_json::to_string(v).ok())
                                .as_ref()
                    {
                        return Err("结果版本已变化".into());
                    }
                    h.images.account.flow_handoff(app, &kind, &image.asset)
                }
            }
        })();
        if let Err(error) = result {
            h.error = true;
            h.message = error;
        }
    }
    let Some(client) = h.images.account.image_client() else {
        h.images.catalog_scope = None;
        return changed;
    };
    if h.images.catalog_scope.as_ref() != Some(&client.scope) {
        let c = client.clone();
        h.images.spawn(
            format!("catalog:{}", seecut_flow_core::digest(&client.scope)),
            None,
            move || c.catalog().map(|v| ValueReply::Catalog(c.scope, v)),
        );
    }
    let mut jobs = Vec::new();
    for source in h.images.sources.values_mut() {
        for attempt in source
            .execution
            .attempts
            .iter()
            .filter(|a| a.draft.context.scope == client.scope)
        {
            let prefix = format!(
                "{}:{}",
                seecut_flow_core::digest(&source.origin.path),
                attempt.id
            );
            if attempt.phase == Phase::Prepared
                && !source.execution.paused
                && !source.blocked
                && attempt.error.is_none()
            {
                if source.prepared.contains_key(&attempt.id) {
                    let key = format!(
                        "{}:side-effect",
                        seecut_flow_core::digest(&source.origin.path)
                    );
                    if !h.images.running.contains(&key)
                        && h.images
                            .cooldown
                            .get(&key)
                            .is_none_or(|until| Instant::now() >= *until)
                    {
                        h.images.running.insert(key.clone());
                        let body = source
                            .prepared
                            .remove(&attempt.id)
                            .expect("prepared body checked");
                        jobs.push((source.origin.clone(), attempt.id.clone(), body, key));
                    }
                } else {
                    let c = client.clone();
                    let a = attempt.clone();
                    let o = source.origin.clone();
                    let key = format!(
                        "{}:side-effect",
                        seecut_flow_core::digest(&source.origin.path)
                    );
                    let tx = h.images.tx.clone();
                    if !h.images.running.contains(&key)
                        && h.images
                            .cooldown
                            .get(&key)
                            .is_none_or(|until| Instant::now() >= *until)
                    {
                        h.images.running.insert(key.clone());
                        std::thread::spawn(move || {
                            let result = c
                                .prepare(&a.draft)
                                .map(|body| ValueReply::Prepared(o.clone(), a.id.clone(), body));
                            let _ = tx.send(Reply {
                                key,
                                preparation: Some((o, a.id)),
                                registration: None,
                                result,
                            });
                        });
                    }
                }
            } else if attempt.phase == Phase::Intent {
                if h.images.running.contains(&format!(
                    "{}:side-effect",
                    seecut_flow_core::digest(&source.origin.path)
                )) {
                    continue;
                }
                let c = client.clone();
                let a = attempt.clone();
                let o = source.origin.clone();
                let key = format!("{prefix}:service");
                let tx = h.images.tx.clone();
                if !h.images.running.contains(&key)
                    && h.images
                        .cooldown
                        .get(&key)
                        .is_none_or(|until| Instant::now() >= *until)
                {
                    h.images.running.insert(key.clone());
                    std::thread::spawn(move || {
                        let result = match c.lookup(&a) {
                            Ok(value) => Ok(ValueReply::Task(o, a.id, c.scope, value)),
                            Err(failure) if failure.confirmed_unaccepted() => {
                                Ok(ValueReply::SubmissionFailed(o, a.id, c.scope, failure))
                            }
                            Err(failure) => Err(format!("{}: {}", failure.code, failure.message)),
                        };
                        let _ = tx.send(Reply {
                            key,
                            preparation: None,
                            registration: None,
                            result,
                        });
                    });
                }
            } else if attempt.phase == Phase::Accepted {
                for task in attempt.tasks.values() {
                    for output in task
                        .outputs
                        .values()
                        .filter(|o| !source.blocked && o.asset.is_none())
                    {
                        if let Some(view) = source.views.get(&task.id) {
                            let retry_key = format!("{prefix}:register:{}:{}", task.id, output.id);
                            if h.images
                                .cooldown
                                .get(&retry_key)
                                .is_some_and(|until| Instant::now() < *until)
                            {
                                continue;
                            }
                            let Some(c) = h.images.account.image_output_client() else {
                                continue;
                            };
                            let a = attempt.clone();
                            let o = source.origin.clone();
                            let view = view.clone();
                            let task_id = task.id.clone();
                            let output = output.id.clone();
                            let key = format!(
                                "{}:side-effect",
                                seecut_flow_core::digest(&source.origin.path)
                            );
                            let tx = h.images.tx.clone();
                            if !h.images.running.contains(&key)
                                && h.images
                                    .cooldown
                                    .get(&key)
                                    .is_none_or(|until| Instant::now() >= *until)
                            {
                                h.images.running.insert(key.clone());
                                std::thread::spawn(move || {
                                    let result = c.register(&a, &view, &output).map(|v| {
                                        ValueReply::Registered(o, a.id, task_id, output, v)
                                    });
                                    let _ = tx.send(Reply {
                                        key,
                                        preparation: None,
                                        registration: Some(retry_key),
                                        result,
                                    });
                                });
                            }
                        }
                    }
                    if !matches!(task.status.as_str(), "failed" | "expired")
                        || !source.views.contains_key(&task.id)
                    {
                        let c = client.clone();
                        let a = attempt.clone();
                        let o = source.origin.clone();
                        let task_id = task.id.clone();
                        let key = format!("{prefix}:poll:{task_id}");
                        let tx = h.images.tx.clone();
                        if !h.images.running.contains(&key)
                            && h.images
                                .cooldown
                                .get(&key)
                                .is_none_or(|until| Instant::now() >= *until)
                        {
                            h.images.running.insert(key.clone());
                            std::thread::spawn(move || {
                                let result = c
                                    .task(&task_id)
                                    .and_then(|v| {
                                        if v["id"] == task_id {
                                            Ok(v)
                                        } else {
                                            Err("任务身份已变化".into())
                                        }
                                    })
                                    .map(|v| ValueReply::Task(o, a.id, c.scope, v));
                                let _ = tx.send(Reply {
                                    key,
                                    preparation: None,
                                    registration: None,
                                    result,
                                });
                            });
                        }
                    }
                }
            }
        }
    }
    for (origin, id, body, key) in jobs {
        changed = true;
        let submitted = key.clone();
        let result = (|| -> Result<()> {
            let baseline = original(h, &origin)?;
            let mut e = baseline.clone();
            if e.paused
                || h.images
                    .sources
                    .get(&origin.path)
                    .is_some_and(|s| s.blocked)
            {
                h.images.running.remove(&submitted);
                return Ok(());
            }
            e.intent(&id, body)?;
            save(h, &origin, &baseline, e.clone())?;
            let a = e
                .attempts
                .iter()
                .find(|a| a.id == id)
                .ok_or("attempt missing")?
                .clone();
            let c = client.clone();
            let tx = h.images.tx.clone();
            std::thread::spawn(move || {
                let result = Ok(match c.submit(&a) {
                    Ok(value) => ValueReply::Task(origin, a.id, c.scope, value),
                    Err(failure) => ValueReply::SubmissionFailed(origin, a.id, c.scope, failure),
                });
                let _ = tx.send(Reply {
                    key,
                    preparation: None,
                    registration: None,
                    result,
                });
            });
            Ok(())
        })();
        if let Err(error) = result {
            h.images.running.remove(&submitted);
            h.fail(error);
        }
    }
    changed
}
pub(super) fn heartbeat(weak: slint::Weak<App>, host: Rc<RefCell<Host>>) {
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        {
            let mut h = host.borrow_mut();
            if tick(&app, &mut h) {
                h.publish(&app);
            }
        }
        heartbeat(weak, host);
    });
}
pub(super) fn bind(app: &App, host: &Rc<RefCell<Host>>) {
    let ui = app.global::<Flow>();
    ui.on_image_settings({
        let weak = app.as_weak();
        let host = host.clone();
        move |field| {
            if let Some(app) = weak.upgrade() {
                if host.borrow().images.account.image_client().is_none() {
                    app.global::<SeeCut>()
                        .invoke_action("auth-open".into(), "".into());
                    return;
                }
                let result = settings(&app, &mut host.borrow_mut(), field.as_str());
                finish(&app, &host, result.map(|()| None));
            }
        }
    });
    ui.on_setting_choice({
        let weak = app.as_weak();
        let host = host.clone();
        move |token, value| {
            if let Some(app) = weak.upgrade() {
                let result = set_setting(&mut host.borrow_mut(), token.as_str(), value.as_str());
                if result.is_ok() {
                    app.global::<Flow>().set_settings_open(false);
                }
                finish(&app, &host, result.map(|()| None));
            }
        }
    });
    ui.on_image_assets({
        let weak = app.as_weak();
        let host = host.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                let result = pick(&app, &host);
                finish(&app, &host, result.map(|()| None));
            }
        }
    });
    ui.on_image_choose({
        let weak = app.as_weak();
        let host = host.clone();
        move |value| {
            if let Some(app) = weak.upgrade() {
                let result = choose(&mut host.borrow_mut(), value.as_str());
                finish(&app, &host, result.map(|()| None));
            }
        }
    });
    heartbeat(app.as_weak(), host.clone());
}
