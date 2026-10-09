//! Line-delimited JSON adapter for the local native QA harness. No network listener.
use crate::{
    Result,
    commands::{self, Command, Request},
    graph::Graph,
    host::{self, AssetPickerRequest, LibraryAsset},
    id,
    runner::{Fault, Runner},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Session {
    undo: BTreeMap<String, Vec<Graph>>,
    redo: BTreeMap<String, Vec<Graph>>,
}
impl Session {
    pub fn request(&mut self, runner: &mut Runner, request: Value) -> Result<Value> {
        let graph_id = request["graph"]
            .as_str()
            .or_else(|| request["selection"]["context"]["graph_id"].as_str())
            .map(str::to_owned)
            .or_else(|| runner.store.document().graphs.keys().next().cloned())
            .ok_or("graph missing")?;
        let node = request["node"].as_str().unwrap_or("");
        let previous = runner
            .store
            .document()
            .graphs
            .get(&graph_id)
            .ok_or("graph missing")?
            .clone();
        let history_action = request["op"] == "undo" || request["op"] == "redo";
        match request["op"].as_str().ok_or("operation required")? {
            "snapshot" => {}
            "asset_picker" => return Ok(json!(host::picker_request(runner, &graph_id, node)?)),
            "accept_asset" => {
                let selection: AssetPickerRequest =
                    serde_json::from_value(request["selection"].clone())
                        .map_err(|e| e.to_string())?;
                if selection.context.graph_id != graph_id {
                    return Err("asset callback graph mismatch".into());
                }
                let asset: LibraryAsset =
                    serde_json::from_value(request["asset"].clone()).map_err(|e| e.to_string())?;
                let scope = runner.scope().clone();
                host::accept_asset(&mut runner.store, &scope, &selection, asset)?;
            }
            "generation_draft" => {
                return Ok(json!(host::generation_draft(runner, &graph_id, node)?));
            }
            "run_to" => {
                let result = runner.run_to(&graph_id, node)?;
                return Ok(
                    json!({"outcome":format!("{result:?}"), "snapshot":snapshot(runner, &graph_id)?}),
                );
            }
            "run_node" => {
                let fault = match request["fault"].as_str() {
                    Some("lost_response") => Fault::AfterAccept,
                    Some("reject") => Fault::Reject,
                    _ => Fault::None,
                };
                runner.run_node(
                    &graph_id,
                    node,
                    request["retry"].as_bool().unwrap_or(false),
                    fault,
                )?;
            }
            "choose" => runner.choose(
                &graph_id,
                node,
                request["asset"].as_str().ok_or("asset required")?,
            )?,
            "reconcile" => {
                runner.reconcile(request["attempt"].as_str().ok_or("attempt required")?)?;
            }
            "cancel" => runner.cancel(request["attempt"].as_str().ok_or("attempt required")?)?,
            "command" => {
                let command: Command = serde_json::from_value(request["command"].clone())
                    .map_err(|e| e.to_string())?;
                let expected = request["revision"]
                    .as_u64()
                    .ok_or("expected revision required")?;
                commands::apply(
                    &mut runner.store,
                    Request {
                        document_id: request["document"]
                            .as_str()
                            .ok_or("document ID required")?
                            .into(),
                        graph_id: graph_id.clone(),
                        expected_revision: expected,
                        operation: request["operation"]
                            .as_str()
                            .ok_or("operation ID required")?
                            .into(),
                        command,
                    },
                )?;
            }
            "undo" | "redo" => {
                let (from, to) = if request["op"] == "undo" {
                    (&mut self.undo, &mut self.redo)
                } else {
                    (&mut self.redo, &mut self.undo)
                };
                let history = from.entry(graph_id.clone()).or_default();
                let mut graph = history.last().ok_or("no edit history")?.clone();
                let mut document = runner.store.document().clone();
                let previous = document.graphs[&graph_id].clone();
                graph.revision = previous.revision + 1;
                document.graphs.insert(graph_id.clone(), graph);
                runner.store.save_document(document)?;
                history.pop();
                to.entry(graph_id.clone()).or_default().push(previous);
            }
            "new_graph" => {
                let mut document = runner.store.document().clone();
                let graph = Graph::default();
                let new_id = graph.id.clone();
                document.graphs.insert(graph.id.clone(), graph);
                runner.store.save_document(document)?;
                return snapshot(runner, &new_id);
            }
            "operation" => {
                return Ok(json!(commands::receipt(
                    runner.store.document(),
                    request["operation"]
                        .as_str()
                        .ok_or("operation ID required")?
                )));
            }
            _ => return Err("unsupported operation".into()),
        }
        if !history_action && runner.store.document().graphs[&graph_id] != previous {
            let history = self.undo.entry(graph_id.clone()).or_default();
            history.push(previous);
            if history.len() > 100 {
                history.remove(0);
            }
            self.redo.remove(&graph_id);
        }
        snapshot(runner, &graph_id)
    }
}
pub fn snapshot(runner: &Runner, graph_id: &str) -> Result<Value> {
    let graph = runner
        .store
        .document()
        .graphs
        .get(graph_id)
        .ok_or("graph missing")?;
    let mut outputs = BTreeMap::new();
    let mut states = BTreeMap::new();
    for node in graph.nodes.keys() {
        match runner.current_output(graph_id, node) {
            Ok(output) => {
                outputs.insert(node.clone(), output);
                states.insert(node.clone(), "succeeded".to_string());
            }
            Err(_) => {
                let state = runner
                    .store
                    .runtime()
                    .attempts
                    .iter()
                    .rev()
                    .find(|a| a.graph_id == graph_id && a.node_id == *node)
                    .map(|a| format!("{:?}", a.status))
                    .unwrap_or("ready".into());
                states.insert(
                    node.clone(),
                    if state == "Succeeded" {
                        "stale".into()
                    } else {
                        state
                    },
                );
            }
        }
    }
    Ok(
        json!({"document":runner.store.document().id,"graph":graph,"graphs":runner.store.document().graphs.keys().collect::<Vec<_>>(),"outputs":outputs,"states":states,"attempts":runner.store.runtime().attempts,"virtual_spent":runner.store.runtime().virtual_spent,"deliveries":runner.store.runtime().deliveries,"simulated":true}),
    )
}
pub fn command_request(runner: &Runner, graph: &str, command: Command) -> Value {
    json!({"op":"command","document":runner.store.document().id,"graph":graph,"revision":runner.store.document().graphs[graph].revision,"operation":id(),"command":command})
}
