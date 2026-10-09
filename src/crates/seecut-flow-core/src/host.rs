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
    let graph = runner
        .store
        .document()
        .graphs
        .get(graph_id)
        .ok_or("graph missing")?;
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
            document_id: runner.store.document().id.clone(),
            graph_id: graph_id.into(),
            node_id: node_id.into(),
            revision: graph.revision,
            scope: runner.scope().clone(),
        },
        target: if node.kind == "asset" {
            AssetTarget::Replace
        } else {
            AssetTarget::Reference
        },
        operation_id: id(),
        allowed: vec![media_kind],
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
    let params = json!({"asset_id":asset.asset.id,"content_version":asset.asset.version,"media_kind":asset.asset.kind});
    let command = match request.target {
        AssetTarget::Replace => Command::Parameters {
            node: request.context.node_id.clone(),
            params,
        },
        AssetTarget::Reference if asset.asset.kind == MediaKind::Image => Command::AddConnected {
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
        _ => return Err("incompatible asset target".into()),
    };
    if commands::receipt(store.document(), &request.operation_id).is_none() {
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
                if node.kind == "image_generate" || node.kind == "video_generate" => {}
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
        prompt: prompt.ok_or("prompt missing")?,
        parameters: node.params.clone(),
        references,
    })
}
