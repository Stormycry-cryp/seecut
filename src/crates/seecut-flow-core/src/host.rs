//! Host seams: reuse SeeCut's generation catalogue, account/points and asset picker.
//! These types carry business references only. No credentials, provider URLs or filesystem paths.
use crate::{
    Result,
    commands::{self, Command, Receipt, Request},
    graph::{Edge, MediaKind, Node},
    id,
    runner::{Asset, Output, Runner, Scope},
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub document_id: String,
    pub graph_id: String,
    pub node_id: String,
    pub revision: u64,
    pub scope: Scope,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AssetTarget {
    Create,
    Replace,
    Reference,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetPickerRequest {
    pub context: Context,
    pub target: AssetTarget,
    pub operation_id: String,
    pub allowed: Vec<MediaKind>,
    pub new_node_id: String,
    pub next_order: u32,
    pub position: [f64; 2],
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryAsset {
    pub asset: Asset,
    pub name: String,
    pub available: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationDraft {
    pub context: Context,
    pub model_id: String,
    pub capability_version: String,
    pub media_kind: MediaKind,
    pub prompt: String,
    pub parameters: Value,
    pub references: Vec<Asset>,
}
pub fn picker_request(
    runner: &Runner,
    graph_id: &str,
    node_id: &str,
) -> Result<AssetPickerRequest> {
    picker_request_for_store(&runner.store, runner.scope(), graph_id, node_id)
}
pub fn picker_request_for_store(
    store: &Store,
    scope: &Scope,
    graph_id: &str,
    node_id: &str,
) -> Result<AssetPickerRequest> {
    let graph = store
        .document()
        .graphs
        .get(graph_id)
        .ok_or("graph missing")?;
    if node_id.is_empty() {
        let mut position = [
            (80. - graph.viewport[0]) / graph.viewport[2],
            (80. - graph.viewport[1]) / graph.viewport[2],
        ];
        // Keep the new card and title clear of existing nodes at the current viewport.
        for _ in 0..=graph.nodes.len() {
            let Some(occupied) = graph.nodes.values().find(|n| {
                (n.position[0] - position[0]).abs() < 260.
                    && (n.position[1] - position[1]).abs() < 230.
            }) else {
                break;
            };
            position[1] = occupied.position[1] + 230.;
        }
        return Ok(AssetPickerRequest {
            context: Context {
                document_id: store.document().id.clone(),
                graph_id: graph_id.into(),
                node_id: String::new(),
                revision: graph.revision,
                scope: scope.clone(),
            },
            target: AssetTarget::Create,
            operation_id: id(),
            allowed: vec![MediaKind::Image, MediaKind::Video, MediaKind::Audio],
            new_node_id: id(),
            next_order: 0,
            position,
        });
    }
    let node = graph.nodes.get(node_id).ok_or("node missing")?;
    let media_kind = match node.kind.as_str() {
        "asset" => node.media_kind()?,
        "image_generate" | "video_generate" => MediaKind::Image,
        _ => return Err("asset picker requires a material or generation node".into()),
    };
    let next_order = graph
        .edges
        .iter()
        .filter(|e| e.to == node_id && e.input == "references")
        .map(|e| e.order)
        .max()
        .map_or(Ok(0), |v| v.checked_add(1).ok_or("reference order limit"))?;
    let x = node.position[0] - 290.;
    let y = graph
        .nodes
        .values()
        .filter(|n| (n.position[0] - x).abs() < 240.)
        .map(|n| n.position[1] + 230.)
        .fold(node.position[1], f64::max);
    Ok(AssetPickerRequest {
        context: Context {
            document_id: store.document().id.clone(),
            graph_id: graph_id.into(),
            node_id: node_id.into(),
            revision: graph.revision,
            scope: scope.clone(),
        },
        target: if node.kind == "asset" {
            AssetTarget::Replace
        } else {
            AssetTarget::Reference
        },
        operation_id: id(),
        allowed: if node.kind == "video_generate" {
            vec![MediaKind::Image, MediaKind::Video, MediaKind::Audio]
        } else {
            vec![media_kind]
        },
        new_node_id: id(),
        next_order,
        position: [x, y],
    })
}
/// Accept the existing SeeCut picker/importer's completed, managed and versioned reference.
/// Failed/unavailable items and stale returns leave the graph untouched.
pub fn accept_asset(
    store: &mut Store,
    current_scope: &Scope,
    request: &AssetPickerRequest,
    asset: LibraryAsset,
) -> Result<Receipt> {
    if request.context.document_id != store.document().id || &request.context.scope != current_scope
    {
        return Err("host context changed".into());
    }
    if !asset.available
        || asset.asset.id.is_empty()
        || asset.asset.version.is_empty()
        || !request.allowed.contains(&asset.asset.kind)
    {
        return Err("asset missing, unversioned, or incompatible".into());
    }
    let params = json!({"asset_id":asset.asset.id,"content_version":asset.asset.version,"media_kind":asset.asset.kind,"asset_name":asset.name.clone()});
    let command = match request.target {
        AssetTarget::Create => Command::Add {
            node: Node {
                id: request.new_node_id.clone(),
                kind: "asset".into(),
                version: 1,
                title: asset.name,
                params,
                position: request.position,
            },
        },
        AssetTarget::Replace => Command::Parameters {
            node: request.context.node_id.clone(),
            params,
        },
        AssetTarget::Reference => Command::AddConnected {
            node: Node {
                id: request.new_node_id.clone(),
                kind: "asset".into(),
                version: 1,
                title: asset.name,
                params,
                position: request.position,
            },
            edge: Edge {
                from: request.new_node_id.clone(),
                output: "out".into(),
                to: request.context.node_id.clone(),
                input: "references".into(),
                order: request.next_order,
            },
        },
    };
    if !matches!(request.target, AssetTarget::Create)
        && commands::receipt(store.document(), &request.operation_id).is_none()
    {
        let graph = store
            .document()
            .graphs
            .get(&request.context.graph_id)
            .ok_or("graph missing")?;
        let node = graph
            .nodes
            .get(&request.context.node_id)
            .ok_or("node missing")?;
        match request.target {
            AssetTarget::Replace
                if node.kind == "asset" && node.media_kind()? == asset.asset.kind => {}
            AssetTarget::Reference
                if (node.kind == "image_generate" && asset.asset.kind == MediaKind::Image)
                    || node.kind == "video_generate" => {}
            _ => return Err("incompatible asset target".into()),
        }
    }
    // Command receipt lookup precedes revision validation: an exact callback replay is safe.
    // Any different or stale callback is refused before mutating the graph.
    commands::apply(
        store,
        Request {
            document_id: request.context.document_id.clone(),
            graph_id: request.context.graph_id.clone(),
            expected_revision: request.context.revision,
            operation: request.operation_id.clone(),
            command,
        },
    )
}

/// Immutable configuration for SeeCut's shared generation flow. Host validates current catalogue,
/// shows its existing quote/points confirmation and owns durable submit/query/download/registration.
/// This preparation method does not submit a request or charge anything.
pub fn generation_draft(runner: &Runner, graph_id: &str, node_id: &str) -> Result<GenerationDraft> {
    let graph = runner
        .store
        .document()
        .graphs
        .get(graph_id)
        .ok_or("graph missing")?;
    graph.validate(false)?;
    let node = graph.nodes.get(node_id).ok_or("node missing")?;
    let media_kind = match node.kind.as_str() {
        "image_generate" => MediaKind::Image,
        "video_generate" => MediaKind::Video,
        _ => return Err("not a generation node".into()),
    };
    let inputs = runner.inputs(graph, node)?;
    let mut prompt = None;
    let mut references = Vec::new();
    for input in inputs {
        match (input.port.as_str(), input.output) {
            ("prompt", Output::Text(text)) => prompt = Some(text),
            ("references", Output::Media(asset)) => references.push(asset),
            _ => return Err("incompatible generation input".into()),
        }
    }
    Ok(GenerationDraft {
        context: Context {
            document_id: runner.store.document().id.clone(),
            graph_id: graph_id.into(),
            node_id: node_id.into(),
            revision: graph.revision,
            scope: runner.scope().clone(),
        },
        model_id: node.params["model"]
            .as_str()
            .ok_or("model required")?
            .into(),
        capability_version: node.params["capability_version"]
            .as_str()
            .or_else(|| (node.params["model"] == "deterministic-v1").then_some("deterministic-v1"))
            .ok_or("shared capability version required")?
            .into(),
        media_kind,
        prompt: composed_prompt(prompt.as_deref(), node.params["prompt"].as_str())?,
        parameters: generation_parameters(node),
        references,
    })
}

/// Prepare native image inputs from business references without running the simulator.
pub fn image_draft(
    store: &Store,
    scope: &Scope,
    graph_id: &str,
    node_id: &str,
) -> Result<GenerationDraft> {
    if store.is_read_only() {
        return Err("只读工程不能运行".into());
    }
    let graph = store
        .document()
        .graphs
        .get(graph_id)
        .ok_or("graph missing")?;
    graph.validate(false)?;
    let node = graph.nodes.get(node_id).ok_or("node missing")?;
    if node.kind != "image_generate" {
        return Err("本包只支持图片生成".into());
    }
    let mut edges = graph
        .edges
        .iter()
        .filter(|e| e.to == node_id)
        .collect::<Vec<_>>();
    edges.sort_by_key(|e| e.order);
    let mut prompt = None;
    let mut references = Vec::new();
    for edge in edges {
        let source = graph.nodes.get(&edge.from).ok_or("input node missing")?;
        match (edge.input.as_str(), source.kind.as_str()) {
            ("prompt", "prompt") => prompt = source.params["text"].as_str().map(str::to_owned),
            ("references", "asset") if source.media_kind()? == MediaKind::Image => {
                references.push(Asset {
                    id: source.params["asset_id"]
                        .as_str()
                        .ok_or("asset ID missing")?
                        .into(),
                    version: source.params["content_version"]
                        .as_str()
                        .ok_or("asset version missing")?
                        .into(),
                    kind: MediaKind::Image,
                    simulated: false,
                })
            }
            ("references", "select") if source.media_kind()? == MediaKind::Image => {
                let upstream = graph
                    .edges
                    .iter()
                    .find(|e| e.to == source.id && e.input == "candidates")
                    .ok_or("请连接上游图片生成节点")?;
                let attempt = result_attempt(store.execution(), graph, source, scope)
                    .ok_or("请先生成上游图片")?;
                if attempt.draft.context.node_id == upstream.from {
                    let current = image_draft(store, scope, graph_id, &upstream.from)?;
                    if attempt.fingerprint != crate::execution::fingerprint(&current) {
                        return Err("上游输入已变化，请生成并选用当前版本".into());
                    }
                }
                let (task, output) = node_choice(source, attempt).ok_or("请先选用上游图片结果")?;
                references.push(
                    attempt
                        .tasks
                        .get(&task)
                        .and_then(|t| t.outputs.get(&output))
                        .and_then(|o| o.asset.clone())
                        .ok_or("上游图片尚未完成入库")?,
                );
            }
            _ => return Err("请连接提示词与库内图片素材".into()),
        }
    }
    Ok(GenerationDraft {
        context: Context {
            document_id: store.document().id.clone(),
            graph_id: graph_id.into(),
            node_id: node_id.into(),
            revision: graph.revision,
            scope: scope.clone(),
        },
        model_id: node.params["model"].as_str().ok_or("model missing")?.into(),
        capability_version: node.params["capability_version"]
            .as_str()
            .ok_or("请先选择可用模型")?
            .into(),
        media_kind: MediaKind::Image,
        prompt: composed_prompt(prompt.as_deref(), node.params["prompt"].as_str())?,
        parameters: generation_parameters(node),
        references,
    })
}

/// Connected text is a prefix; a node's own draft is always retained independently.
pub fn composed_prompt(upstream: Option<&str>, own: Option<&str>) -> Result<String> {
    let pieces: Vec<_> = [upstream, own]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if pieces.is_empty() {
        return Err("填写提示词或连接非空文本节点".into());
    }
    Ok(pieces.join("\n\n"))
}

/// Read a node's own attempt, or the fixed historical batch retained by a copy.
/// This never creates an attempt or transfers execution/billing ownership to a copy.
pub fn result_attempt<'a>(
    execution: &'a crate::execution::Execution,
    graph: &crate::graph::Graph,
    node: &Node,
    scope: &Scope,
) -> Option<&'a crate::execution::Attempt> {
    let source = if node.kind == "select" {
        graph
            .edges
            .iter()
            .find(|e| e.to == node.id && e.input == "candidates")
            .and_then(|e| graph.nodes.get(&e.from))
            .unwrap_or(node)
    } else {
        node
    };
    let attempts = result_attempts(execution, graph, node, scope);
    let live = execution
        .runs
        .iter()
        .rev()
        .find(|run| {
            run.graph.id == graph.id
                && &run.scope == scope
                && !matches!(
                    run.state,
                    crate::workflow::State::Complete | crate::workflow::State::Abandoned
                )
        })
        .and_then(|run| run.steps.get(&source.id))
        .and_then(|step| step.attempt.as_deref());
    let explicit = (node.kind == "select")
        .then(|| node.params["selected_output"]["attempt"].as_str())
        .flatten();
    explicit
        .or(live)
        .or_else(|| {
            (node.params["selected_output"]["observed_attempts"].as_u64()
                == Some(attempts.len() as u64))
            .then(|| node.params["selected_output"]["attempt"].as_str())
            .flatten()
        })
        .and_then(|id| attempts.iter().find(|a| a.id == id).copied())
        .or_else(|| attempts.first().copied())
}
/// All accessible historical batches, newest first. Copies retain only their frozen references.
pub fn result_attempts<'a>(
    execution: &'a crate::execution::Execution,
    graph: &crate::graph::Graph,
    node: &Node,
    scope: &Scope,
) -> Vec<&'a crate::execution::Attempt> {
    let source = if node.kind == "select" {
        graph
            .edges
            .iter()
            .find(|e| e.to == node.id && e.input == "candidates")
            .and_then(|e| graph.nodes.get(&e.from))
            .unwrap_or(node)
    } else {
        node
    };
    execution
        .attempts
        .iter()
        .rev()
        .filter(|a| {
            !a.superseded
                && a.draft.context.graph_id == graph.id
                && &a.draft.context.scope == scope
                && (a.draft.context.node_id == source.id
                    || (a.phase == crate::execution::Phase::Finished
                        && source.params["copied_results"]
                            .as_array()
                            .is_some_and(|refs| refs.iter().any(|r| r["attempt"] == a.id))))
        })
        .collect()
}

pub(crate) fn copy_result_sources(
    execution: &crate::execution::Execution,
    graph: &crate::graph::Graph,
    node: &Node,
) -> Value {
    let mut scopes = std::collections::BTreeSet::new();
    let mut references = Vec::new();
    for entry in execution.attempts.iter().rev() {
        let scope = &entry.draft.context.scope;
        if !scopes.insert(crate::digest(scope)) {
            continue;
        }
        if let Some(a) = result_attempt(execution, graph, node, scope).filter(|a| {
            a.phase == crate::execution::Phase::Finished
                && a.tasks
                    .values()
                    .any(|t| t.outputs.values().any(|o| o.asset.is_some()))
        }) {
            references.push(json!({"attempt": a.id, "choice": node_choice(node, a)}));
        }
    }
    Value::Array(references)
}

/// Adoption belongs to the selected node, so two consumers can choose different outputs.
/// Old documents retain their previously selected result until explicitly changed.
pub fn node_choice(node: &Node, attempt: &crate::execution::Attempt) -> Option<(String, String)> {
    if let Some(choice) = node.params.get("selected_output") {
        if choice["attempt"].as_str()? != attempt.id {
            return None;
        }
        Some((
            choice["task"].as_str()?.into(),
            choice["output"].as_str()?.into(),
        ))
    } else if let Some(reference) = node.params["copied_results"].as_array().and_then(|refs| {
        refs.iter()
            .find(|r| r["attempt"].as_str() == Some(&attempt.id))
    }) {
        serde_json::from_value(reference["choice"].clone())
            .ok()
            .flatten()
    } else {
        attempt.selected.clone()
    }
}
fn generation_parameters(node: &Node) -> Value {
    let mut params = node.params.clone();
    if let Some(object) = params.as_object_mut() {
        object.remove("selected_output");
        object.remove("copied_results");
    }
    params
}
