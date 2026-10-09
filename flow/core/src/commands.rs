//! Public graph commands for the native host. No credentials, paths, or generation authority.
use crate::{
    Result, digest,
    graph::{Document, Edge, Graph, Node},
    id,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Add {
        node: Node,
    },
    AddConnected {
        node: Node,
        edge: Edge,
    },
    Remove {
        nodes: Vec<String>,
    },
    Move {
        positions: BTreeMap<String, [f64; 2]>,
    },
    Rename {
        node: String,
        title: String,
    },
    Parameters {
        node: String,
        params: Value,
    },
    Connect {
        edge: Edge,
    },
    Disconnect {
        edge: Edge,
    },
    Duplicate {
        nodes: Vec<String>,
        incoming: bool,
    },
    Arrange,
    Viewport {
        viewport: [f64; 3],
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub operation: String,
    pub request_hash: String,
    pub graph_id: String,
    pub revision: u64,
    pub created: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub document_id: String,
    pub graph_id: String,
    pub expected_revision: u64,
    pub operation: String,
    pub command: Command,
}
/// Host must authorize the document/graph before entering this boundary. This is not MCP authentication.
pub fn apply(store: &mut Store, request: Request) -> Result<Receipt> {
    if request.document_id != store.document().id || request.operation.is_empty() {
        return Err("document/operation identity mismatch".into());
    }
    let hash = digest(&request);
    if let Some(receipt) = store.document().operations.get(&request.operation) {
        if receipt.request_hash == hash {
            return Ok(receipt.clone());
        }
        return Err("operation ID already used with a different request".into());
    }
    let mut document = store.document().clone();
    let graph = document
        .graphs
        .get_mut(&request.graph_id)
        .ok_or("graph missing")?;
    if graph.revision != request.expected_revision {
        return Err("graph revision conflict".into());
    }
    let previous = graph.clone();
    let created = edit(graph, &request.command)?;
    graph.validate(false)?;
    if *graph != previous {
        graph.revision = crate::next_revision(graph.revision)?;
    }
    let receipt = Receipt {
        operation: request.operation.clone(),
        request_hash: hash,
        graph_id: graph.id.clone(),
        revision: graph.revision,
        created,
    };
    document
        .operations
        .insert(request.operation, receipt.clone());
    store.save_document(document)?;
    Ok(receipt)
}
pub fn receipt(document: &Document, operation: &str) -> Option<Receipt> {
    document.operations.get(operation).cloned()
}
fn edit(graph: &mut Graph, command: &Command) -> Result<Vec<String>> {
    let mut created = Vec::new();
    match command {
        Command::Add { node } | Command::AddConnected { node, .. } => {
            if graph.nodes.contains_key(&node.id) {
                return Err("node already exists".into());
            }
            graph.nodes.insert(node.id.clone(), node.clone());
            created.push(node.id.clone());
            if let Command::AddConnected { edge, .. } = command {
                if edge.from != node.id {
                    return Err("new connection must originate from the added node".into());
                }
                graph.edges.push(edge.clone());
            }
        }
        Command::Remove { nodes } => {
            require_nodes(graph, nodes)?;
            for node in nodes {
                graph.nodes.remove(node);
            }
            graph
                .edges
                .retain(|e| !nodes.contains(&e.from) && !nodes.contains(&e.to));
        }
        Command::Move { positions } => {
            for (id, position) in positions {
                graph.nodes.get_mut(id).ok_or("node missing")?.position = *position;
            }
        }
        Command::Rename { node, title } => {
            graph.nodes.get_mut(node).ok_or("node missing")?.title = title.clone()
        }
        Command::Parameters { node, params } => {
            graph.nodes.get_mut(node).ok_or("node missing")?.params = params.clone()
        }
        Command::Connect { edge } => graph.edges.push(edge.clone()),
        Command::Disconnect { edge } => {
            let index = graph
                .edges
                .iter()
                .position(|e| e == edge)
                .ok_or("edge missing")?;
            graph.edges.remove(index);
        }
        Command::Duplicate { nodes, incoming } => {
            require_nodes(graph, nodes)?;
            let remap: BTreeMap<_, _> = nodes.iter().map(|n| (n.clone(), id())).collect();
            let edges = graph.edges.clone();
            for original in nodes {
                let mut node = graph.nodes[original].clone();
                node.id = remap[original].clone();
                node.position[0] += 36.;
                node.position[1] += 36.;
                // Adoption is tied to the source batch, so copies require an explicit new choice.
                if node.kind == "select" {
                    node.params.as_object_mut().unwrap().remove("batch");
                    node.params.as_object_mut().unwrap().remove("asset_id");
                }
                created.push(node.id.clone());
                graph.nodes.insert(node.id.clone(), node);
            }
            for mut edge in edges {
                if remap.contains_key(&edge.to) && (*incoming || remap.contains_key(&edge.from)) {
                    edge.to = remap[&edge.to].clone();
                    if let Some(from) = remap.get(&edge.from) {
                        edge.from = from.clone();
                    }
                    graph.edges.push(edge);
                }
            }
        }
        Command::Arrange => {
            let order = graph.validate(false)?;
            let mut levels = BTreeMap::new();
            let mut rows: BTreeMap<u32, u32> = BTreeMap::new();
            for id in order {
                let level = graph
                    .edges
                    .iter()
                    .filter(|e| e.to == id)
                    .map(|e| levels[&e.from] + 1)
                    .max()
                    .unwrap_or(0);
                let row = rows.entry(level).or_default();
                graph.nodes.get_mut(&id).unwrap().position =
                    [f64::from(level) * 290., f64::from(*row) * 230.];
                *row += 1;
                levels.insert(id, level);
            }
        }
        Command::Viewport { viewport } => graph.viewport = *viewport,
    }
    Ok(created)
}
fn require_nodes(graph: &Graph, nodes: &[String]) -> Result<()> {
    if nodes.is_empty()
        || nodes.iter().collect::<BTreeSet<_>>().len() != nodes.len()
        || nodes.iter().any(|n| !graph.nodes.contains_key(n))
    {
        return Err("selection contains missing/duplicate nodes".into());
    }
    Ok(())
}
