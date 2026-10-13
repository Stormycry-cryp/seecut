//! Narrow UI-thread access for the authenticated Agent bridge.
use super::Host;
use crate::ui::App;
use concat_editor_mcp::FlowParameters;
use seecut_flow_core::{
    commands::{Command, Request},
    digest,
    runner::Scope,
    workflow::Choice,
};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone)]
pub(crate) struct FlowAccess {
    host: Rc<RefCell<Host>>,
    selection: Rc<RefCell<(String, u64)>>,
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub generation: String,
    pub name: String,
    pub project_key: String,
    pub graph_id: String,
    pub account_scope: String,
    pub revision: u64,
    pub selection_revision: u64,
    pub selected: Vec<String>,
    pub busy: bool,
    pub writable: bool,
    pub document: String,
    pub project: String,
}
impl FlowAccess {
    pub(super) fn new(host: Rc<RefCell<Host>>) -> Self {
        Self {
            host,
            selection: Rc::new(RefCell::new((String::new(), 0))),
        }
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        let h = self.host.try_borrow().ok()?;
        if h.browsing {
            return None;
        }
        let s = h.controller.session.as_ref()?;
        let scope = h.images.account.flow_scope();
        let generation = digest(&(
            &s.store.document().id,
            &s.store.document().project_id,
            &s.token,
            &s.graph_id,
            &scope,
        ));
        let selected: Vec<_> = s
            .graph()
            .nodes
            .keys()
            .filter(|id| h.editor.selected.contains(*id))
            .cloned()
            .collect();
        let selection_key = digest(&(&generation, &selected));
        let mut selection = self.selection.borrow_mut();
        if selection.0 != selection_key {
            selection.0 = selection_key;
            selection.1 = selection.1.saturating_add(1);
        }
        Some(Snapshot {
            generation,
            name: s
                .store
                .document()
                .title
                .clone()
                .unwrap_or_else(|| "Flow".into()),
            project_key: format!("flow:{}", s.path.display()),
            graph_id: s.graph_id.clone(),
            account_scope: digest(&scope),
            revision: s.graph().revision,
            selection_revision: selection.1,
            selected,
            busy: h.controller.uncommitted()
                || !h.preview.is_empty()
                || !h.connecting.is_empty()
                || h.dialog.is_some()
                || h.recovery,
            writable: !s.store.is_read_only() && !s.store.is_poisoned(),
            document: s.store.document().id.clone(),
            project: s.store.document().project_id.clone(),
        })
    }
    pub fn check(&self, p: &FlowParameters, revision: Option<u64>) -> Result<Snapshot> {
        let snapshot = self.snapshot().ok_or("wrongProject")?;
        if snapshot.graph_id != p.graph_id || snapshot.account_scope != p.account_scope {
            return Err("wrongProject".into());
        }
        if p.action != "operation" {
            if snapshot.busy {
                return Err("busy".into());
            }
            if revision != Some(snapshot.revision) {
                return Err("staleRevision".into());
            }
            if p.selection_revision != snapshot.selection_revision {
                return Err("selectionChanged".into());
            }
        }
        Ok(snapshot)
    }
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    pub fn references(&self) -> Result<Vec<crate::agent_sessions::Reference>> {
        let snapshot = self.snapshot().ok_or("请先打开 Flow 并选择节点")?;
        if snapshot.busy || snapshot.selected.is_empty() {
            return Err("请先结束编辑并选择节点".into());
        }
        let h = self.host.try_borrow().map_err(|_| "正在编辑")?;
        let s = h.controller.session.as_ref().ok_or("工程已切换")?;
        snapshot.selected.iter().take(4).map(|id|{
            let node=&s.graph().nodes[id];
            let reference=crate::agent_sessions::Reference {
                kind:"flow_node".into(),label:node.title.clone(),
                target:json!({"documentId":snapshot.document,"graphId":snapshot.graph_id,"accountScope":snapshot.account_scope,
                    "revision":snapshot.revision,"node":node})
            };
            if !reference.valid(){return Err("节点内容过长，请先精简再引用".into());}
            Ok(reference)
        }).collect()
    }
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    pub fn validate_reference(&self, target: &Value) -> Result<()> {
        let snapshot = self.snapshot().ok_or("引用的 Flow 已关闭")?;
        if target["documentId"] != snapshot.document
            || target["graphId"] != snapshot.graph_id
            || target["accountScope"] != snapshot.account_scope
            || target["revision"] != snapshot.revision
            || snapshot.busy
        {
            return Err("引用节点的工程或版本已变化，请重新选择".into());
        }
        let h = self.host.try_borrow().map_err(|_| "正在编辑")?;
        let s = h.controller.session.as_ref().ok_or("工程已关闭")?;
        let id = target["node"]["id"].as_str().ok_or("节点引用无效")?;
        let current = s.graph().nodes.get(id).ok_or("引用节点已删除")?;
        if serde_json::to_value(current).map_err(|_| "节点引用无效")? != target["node"] {
            return Err("引用节点已变化，请重新选择".into());
        }
        Ok(())
    }
    pub fn read(&self, p: &FlowParameters, revision: Option<u64>) -> Result<Value> {
        self.check(p, revision)?;
        let h = self.host.try_borrow().map_err(|_| "busy")?;
        let s = h.controller.session.as_ref().ok_or("wrongProject")?;
        let scope = h.images.account.flow_scope();
        let page = |rows: Vec<Value>| json!({"offset":p.offset,"total":rows.len(),"items":rows.into_iter().skip(p.offset).take(p.limit).collect::<Vec<_>>()});
        let result = match p.action.as_str() {
            "schema" => schema(),
            "graph" => {
                let rows = s
                    .graph()
                    .nodes
                    .values()
                    .map(|n| serde_json::to_value(n).expect("node"))
                    .collect();
                let edges = s
                    .graph()
                    .edges
                    .iter()
                    .skip(p.offset)
                    .take(p.limit)
                    .collect::<Vec<_>>();
                json!({"nodes":page(rows),"edges":edges,"edgeTotal":s.graph().edges.len()})
            }
            "models" => {
                if h.images.catalog_scope.as_ref() != Some(&scope) {
                    return Err("catalogUnavailable".into());
                }
                let models = page(
                    h.images.catalog["models"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|model| {
                            let mut defaults = json!({});
                            super::image_execution::select_parameters(&mut defaults, &model);
                            json!({"model":model,"parameters":defaults})
                        })
                        .collect(),
                );
                let efforts=h.images.catalog["efforts"].as_array().into_iter().flatten().map(|effort|
                    json!({"id":effort["id"],"label":effort["label"],"policy_revision":effort["policy_revision"],
                    "generation_available":super::image_execution::generation_effort_ready(effort),"generation":effort["generation"]})).collect::<Vec<_>>();
                json!({"models":models,"efforts":efforts})
            }
            "runs" => {
                let e = s.store.execution();
                let runs=e.runs.iter().rev().filter(|r|r.graph.id==p.graph_id&&r.scope==scope)
                    .map(|r|json!({"id":r.id,"target":r.target,"state":r.state,"current":r.current,"error":r.error,"choices":r.choices,"steps":r.steps,"actualCredits":r.actual_credits(e)})).collect();
                let attempts=e.attempts.iter().rev().filter(|a|a.draft.context.graph_id==p.graph_id&&a.draft.context.scope==scope)
                    .skip(p.offset).take(p.limit).map(|a|json!({"id":a.id,"nodeId":a.draft.context.node_id,"phase":a.phase,"tasks":a.tasks,"error":a.error})).collect::<Vec<_>>();
                json!({"runs":page(runs),"attempts":attempts,"paused":e.paused})
            }
            "results" => {
                let mut results = Vec::new();
                for run in s
                    .store
                    .execution()
                    .runs
                    .iter()
                    .rev()
                    .filter(|r| {
                        r.graph.id == p.graph_id
                            && r.scope == scope
                            && r.state == seecut_flow_core::workflow::State::Complete
                            && p.payload.get("runId").is_none_or(|id| id == &r.id)
                    })
                    .skip(p.offset)
                    .take(p.limit)
                {
                    if let Some(asset) = run.selected_asset(&run.target) {
                        let registered = crate::cloud::library_receipt(&asset.id, &asset.version)
                            .map_err(|_| "resultUnavailable")?;
                        results
                            .push(json!({"runId":run.id,"nodeId":run.target,"asset":registered}));
                    }
                }
                json!({"items":results})
            }
            "operation" => s
                .store
                .execution()
                .agent_operations
                .get(p.operation_id.as_deref().ok_or("invalidInput")?)
                .filter(|r| r["graphId"] == p.graph_id && r["accountScope"] == p.account_scope)
                .cloned()
                .ok_or("outcomeUnknown")?,
            _ => return Err("invalidInput".into()),
        };
        if serde_json::to_vec(&result)
            .map_err(|_| "invalidInput")?
            .len()
            > 10_000
        {
            return Err("resourceLimit".into());
        }
        Ok(result)
    }
    pub fn write(&self, p: &FlowParameters, revision: u64, envelope: Value) -> Result<Value> {
        let stamp = self.check(p, Some(revision))?;
        if !stamp.writable {
            return Err("readOnly".into());
        }
        let mut h = self.host.try_borrow_mut().map_err(|_| "busy")?;
        let scope = h.images.account.flow_scope();
        let has_client = h.images.account.image_client().is_some();
        let s = h.controller.session.as_mut().ok_or("wrongProject")?;
        let operation = p.operation_id.as_ref().ok_or("invalidInput")?.clone();
        let hash = digest(&(p, revision, &envelope));
        if let Some(previous) = s.store.execution().agent_operations.get(&operation) {
            return if previous["requestHash"] == hash {
                Ok(previous.clone())
            } else {
                Err("parameterMismatch".into())
            };
        }
        if s.store.execution().agent_operations.len() >= 1024 {
            return Err("resourceLimit".into());
        }
        let mut e = s.store.execution().clone();
        let result;
        match p.action.as_str() {
            "edit" => {
                let command: Command =
                    serde_json::from_value(p.payload.clone()).map_err(|_| "invalidInput")?;
                match &command {
                    Command::Add { node } | Command::AddConnected { node, .. } => {
                        validate_node(node, &scope).map_err(|_| "invalidInput")?
                    }
                    Command::Parameters { node, params } => {
                        let mut n = s.graph().nodes.get(node).ok_or("invalidInput")?.clone();
                        n.params = params.clone();
                        validate_node(&n, &scope).map_err(|_| "invalidInput")?;
                    }
                    Command::Connect { .. }
                    | Command::Disconnect { .. }
                    | Command::Rewire { .. }
                    | Command::Reorder { .. }
                    | Command::Move { .. }
                    | Command::Rename { .. }
                    | Command::Remove { .. } => {}
                    _ => return Err("invalidInput".into()),
                }
                seecut_flow_core::commands::validate_command(s.graph(), &command, &e)
                    .map_err(|_| "invalidInput")?;
                let receipt = s
                    .edit_request(Request {
                        document_id: s.store.document().id.clone(),
                        graph_id: s.graph_id.clone(),
                        expected_revision: revision,
                        operation: operation.clone(),
                        command,
                    })
                    .map_err(|_| "outcomeUnknown")?;
                result = json!({"revision":receipt.revision,"created":receipt.created});
            }
            "run" => {
                let target = p.payload["nodeId"].as_str().ok_or("invalidInput")?;
                let mode = p.payload["mode"]
                    .as_str()
                    .filter(|m| matches!(*m, "run_to" | "run_node"))
                    .ok_or("invalidInput")?;
                let graph = s.graph().clone();
                if graph
                    .ancestors(target)
                    .map_err(|_| "invalidInput")?
                    .iter()
                    .any(|id| {
                        matches!(
                            graph.nodes[id].kind.as_str(),
                            "image_generate" | "video_generate"
                        )
                    })
                    && !has_client
                {
                    return Err("loginRequired".into());
                }
                let run = e
                    .begin_run(
                        graph,
                        scope.clone(),
                        target.into(),
                        digest(s.store.document()),
                        mode == "run_node",
                    )
                    .map_err(|_| "invalidInput")?;
                result = json!({"runId":run,"state":"scheduled"});
            }
            "adopt" => {
                let node = p.payload["nodeId"].as_str().ok_or("invalidInput")?;
                let choice:Choice=serde_json::from_value(json!({"attempt":p.payload["attempt"],"task":p.payload["task"],"output":p.payload["output"]})).map_err(|_|"invalidInput")?;
                let selected = s
                    .graph()
                    .nodes
                    .get(node)
                    .filter(|n| n.kind == "select")
                    .ok_or("invalidInput")?;
                let attempts =
                    seecut_flow_core::host::result_attempts(&e, s.graph(), selected, &scope);
                let asset = attempts
                    .iter()
                    .find(|a| {
                        a.id == choice.attempt
                            && a.phase == seecut_flow_core::execution::Phase::Finished
                    })
                    .and_then(|a| a.tasks.get(&choice.task))
                    .and_then(|t| t.outputs.get(&choice.output))
                    .and_then(|o| o.asset.as_ref())
                    .ok_or("invalidInput")?;
                if asset.simulated
                    || asset.kind != selected.media_kind().map_err(|_| "invalidInput")?
                {
                    return Err("invalidInput".into());
                }
                crate::cloud::library_asset(&asset.id, Some(&asset.version))
                    .map_err(|_| "invalidInput")?;
                e.choose_run_output(&s.graph_id, node, choice.clone(), &scope)
                    .map_err(|_| "invalidInput")?;
                let mut params = selected.params.clone();
                params["selected_output"] = serde_json::to_value(&choice).expect("choice");
                s.edit_request(Request {
                    document_id: s.store.document().id.clone(),
                    graph_id: s.graph_id.clone(),
                    expected_revision: revision,
                    operation: operation.clone(),
                    command: Command::Parameters {
                        node: node.into(),
                        params,
                    },
                })
                .map_err(|_| "outcomeUnknown")?;
                result = json!({"choice":choice,"revision":s.graph().revision});
            }
            "pause" => {
                e.paused = p.payload["paused"].as_bool().ok_or("invalidInput")?;
                result = json!({"paused":e.paused});
            }
            _ => return Err("invalidInput".into()),
        }
        let mut receipt = envelope;
        let fields = receipt.as_object_mut().ok_or("invalidInput")?;
        fields.insert("requestHash".into(), json!(hash));
        fields.insert("graphId".into(), json!(p.graph_id));
        fields.insert("accountScope".into(), json!(p.account_scope));
        fields.insert("action".into(), json!(p.action));
        fields.insert("result".into(), result);
        let selected: Vec<_> = stamp
            .selected
            .iter()
            .filter(|id| s.graph().nodes.contains_key(*id))
            .cloned()
            .collect();
        let selection_key = digest(&(&stamp.generation, &selected));
        let mut selection = self.selection.borrow_mut();
        if selection.0 != selection_key {
            selection.0 = selection_key;
            selection.1 = selection.1.saturating_add(1);
        }
        fields.insert(
            "afterSelection".into(),
            json!({"selectionRevision":selection.1,"selectedObjectIds":selected}),
        );
        drop(selection);
        e.agent_operations.insert(operation, receipt.clone());
        s.store
            .save_execution(e.clone())
            .map_err(|_| "outcomeUnknown")?;
        let path = s.path.clone();
        let nodes = s
            .graph()
            .nodes
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        h.editor.selected.retain(|id| nodes.contains(id));
        h.controller.reload_draft();
        h.images.track(path, e);
        Ok(receipt)
    }
    pub fn publish(&self, app: &App) {
        if let Ok(mut h) = self.host.try_borrow_mut() {
            h.publish(app);
        }
    }
}
fn validate_node(node: &seecut_flow_core::graph::Node, _scope: &Scope) -> Result<()> {
    node.ports()?;
    if node.kind == "asset" {
        let id = node.params["asset_id"].as_str().ok_or("invalidAsset")?;
        let version = node.params["content_version"]
            .as_str()
            .ok_or("invalidAsset")?;
        let asset = crate::cloud::library_asset(id, Some(version))?;
        if crate::cloud::media_kind(asset.asset.kind) != node.media_kind()? {
            return Err("invalidAsset".into());
        }
    }
    // Result adoption must use the explicit tool, not fabricated parameter references.
    if node.kind == "select" && !node.params["selected_output"].is_null() {
        return Err("useAdopt".into());
    }
    Ok(())
}
fn schema() -> Value {
    json!({
        "node":{"id":"new-unique-id","kind":"prompt","version":1,"title":"提示词","params":{"text":"内容"},"position":[0,0]},
        "kinds":{
            "prompt":{"params":{"text":"内容"},"output":"Text"},
            "asset":{"params":{"asset_id":"existing-library-id","content_version":"exact serialized immutable version","media_kind":"Image"},"output":"Media"},
            "image_generate":{"params":"models.parameters plus effort_id/policy_revision and optional prompt","inputs":{"prompt":"Text","references":"Image (ordered, optional)"},"output":"Set(Image)"},
            "video_generate":{"params":"models.parameters plus effort_id/policy_revision and optional prompt","inputs":{"prompt":"Text","references":"Media (ordered, optional)"},"output":"Set(Video)"},
            "select":{"params":{"media_kind":"Image"},"inputs":{"candidates":"Set"},"output":"Media"},
            "deliver":{"params":{"media_kind":"Image","target":"library"},"inputs":{"media":"Media"},"output":"Media"}
        },
        "edge":{"from":"source-id","output":"out","to":"target-id","input":"prompt","order":0},
        "edit":[
            {"type":"add","node":"node object"},{"type":"add_connected","node":"node object","edge":"edge object"},
            {"type":"parameters","node":"node-id","params":"complete params object"},
            {"type":"connect","edge":"edge object"},{"type":"disconnect","edge":"edge object"},
            {"type":"rewire","old":"edge object","new":"edge object"},
            {"type":"reorder","node":"node-id","input":"references","sources":["source-id"]},
            {"type":"move","positions":{"node-id":[0,0]}},{"type":"rename","node":"node-id","title":"新名称"},
            {"type":"remove","nodes":["node-id"]}
        ],
        "run":{"nodeId":"target-node-id","mode":"run_to or run_node"},
        "adopt":{"nodeId":"select-node-id","attempt":"attempt-id","task":"task-id","output":"output-id"},
        "pause":{"paused":true},
        "mediaKinds":["Image","Video","Audio"],
        "execution":"run only schedules. Read runs until complete or awaiting_selection; adopt only registered results. pause stops subsequent execution and is not a refund."
    })
}
