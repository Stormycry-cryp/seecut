use crate::{Result, id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataType {
    Text,
    Media(MediaKind),
    Set(MediaKind),
}
#[derive(Debug, Clone)]
pub struct Port {
    pub name: &'static str,
    pub data: DataType,
    pub required: bool,
    pub multiple: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    /// Open string preserves future node kinds and their opaque parameters on read.
    pub kind: String,
    pub version: u32,
    pub title: String,
    pub params: Value,
    pub position: [f64; 2],
}
impl Node {
    pub fn new(kind: &str, params: Value) -> Self {
        Self {
            id: id(),
            kind: kind.into(),
            version: 1,
            title: kind.into(),
            params,
            position: [0., 0.],
        }
    }
    pub fn media_kind(&self) -> Result<MediaKind> {
        serde_json::from_value(
            self.params
                .get("media_kind")
                .cloned()
                .unwrap_or(json!("Image")),
        )
        .map_err(|_| format!("{}: invalid media kind", self.id))
    }
    pub fn ports(&self) -> Result<(Vec<Port>, DataType)> {
        if self.version != 1 {
            return Err(format!("{}: unsupported node version", self.id));
        }
        let port = |name, data, required, multiple| Port {
            name,
            data,
            required,
            multiple,
        };
        use DataType::*;
        use MediaKind::*;
        Ok(match self.kind.as_str() {
            "prompt" => (vec![], Text),
            "asset" => (vec![], Media(self.media_kind()?)),
            "image_generate" => (
                vec![
                    port("prompt", Text, true, false),
                    port("references", Media(Image), false, true),
                ],
                Set(Image),
            ),
            "video_generate" => (
                vec![
                    port("prompt", Text, true, false),
                    port("references", Media(Image), true, true),
                ],
                Set(Video),
            ),
            "select" => (
                vec![port("candidates", Set(self.media_kind()?), true, false)],
                Media(self.media_kind()?),
            ),
            "deliver" => (
                vec![port("media", Media(self.media_kind()?), true, false)],
                Media(self.media_kind()?),
            ),
            _ => return Err(format!("{}: unsupported node kind {}", self.id, self.kind)),
        })
    }
    pub fn validate_params(&self) -> Result<()> {
        if !self.params.is_object() || self.position.iter().any(|x| !x.is_finite()) {
            return Err(format!("{}: invalid parameters/position", self.id));
        }
        match self.kind.as_str() {
            "prompt" if self.params["text"].as_str().is_none() => {
                return Err(format!("{}: text required", self.id));
            }
            "asset"
                if self.params["asset_id"].as_str().is_none()
                    || self.params["content_version"].as_str().is_none() =>
            {
                return Err(format!(
                    "{}: stable asset and content version required",
                    self.id
                ));
            }
            "image_generate" | "video_generate" => {
                let count = self.params["count"].as_u64().unwrap_or(0);
                if count == 0 || self.params["model"].as_str().is_none_or(str::is_empty) {
                    return Err(format!(
                        "{}: model capability and positive candidate count required",
                        self.id
                    ));
                }
            }
            "deliver" if self.params["target"].as_str() != Some("synthetic-assets") => {
                return Err(format!(
                    "{}: only synthetic-assets delivery is wired",
                    self.id
                ));
            }
            _ => {}
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub output: String,
    pub to: String,
    pub input: String,
    pub order: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub id: String,
    pub revision: u64,
    pub nodes: BTreeMap<String, Node>,
    pub edges: Vec<Edge>,
    pub viewport: [f64; 3],
}
impl Default for Graph {
    fn default() -> Self {
        Self {
            id: id(),
            revision: 0,
            nodes: BTreeMap::new(),
            edges: vec![],
            viewport: [0., 0., 1.],
        }
    }
}
impl Graph {
    /// Structural checks are also applied to newly connected edges; incomplete drafts remain editable.
    pub fn validate(&self, complete: bool) -> Result<Vec<String>> {
        if self.id.is_empty()
            || self.viewport.iter().any(|x| !x.is_finite())
            || self.viewport[2] <= 0.
        {
            return Err("invalid graph identity/viewport".into());
        }
        let mut inputs: BTreeMap<(String, String), BTreeSet<u32>> = BTreeMap::new();
        let mut indegree: BTreeMap<String, usize> =
            self.nodes.keys().map(|n| (n.clone(), 0)).collect();
        for (key, node) in &self.nodes {
            if node.id != *key || key.is_empty() {
                return Err("node identity mismatch".into());
            }
            node.ports()?;
            node.validate_params()?;
        }
        for edge in &self.edges {
            let from = self.nodes.get(&edge.from).ok_or("missing source node")?;
            let to = self.nodes.get(&edge.to).ok_or("missing destination node")?;
            if edge.output != "out" {
                return Err("missing output port".into());
            }
            let (ports, _) = to.ports()?;
            let port = ports
                .iter()
                .find(|p| p.name == edge.input)
                .ok_or("missing input port")?;
            if from.ports()?.1 != port.data {
                return Err(format!("{}: incompatible port types", to.id));
            }
            let orders = inputs
                .entry((to.id.clone(), edge.input.clone()))
                .or_default();
            if (!port.multiple && !orders.is_empty()) || !orders.insert(edge.order) {
                return Err("input cardinality/order conflict".into());
            }
            *indegree.get_mut(&edge.to).unwrap() += 1;
        }
        if complete {
            for node in self.nodes.values() {
                for port in node.ports()?.0 {
                    if port.required && !inputs.contains_key(&(node.id.clone(), port.name.into())) {
                        return Err(format!("{}: missing {}", node.id, port.name));
                    }
                }
            }
        }
        let mut ready: BTreeSet<String> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| n.clone())
            .collect();
        let mut order = Vec::new();
        while let Some(node) = ready.pop_first() {
            for edge in self.edges.iter().filter(|e| e.from == node) {
                let degree = indegree.get_mut(&edge.to).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(edge.to.clone());
                }
            }
            order.push(node);
        }
        if order.len() != self.nodes.len() {
            return Err("cycle detected".into());
        }
        Ok(order)
    }
    pub fn ancestors(&self, target: &str) -> Result<Vec<String>> {
        if !self.nodes.contains_key(target) {
            return Err("target missing".into());
        }
        let order = self.validate(false)?;
        let mut needed = BTreeSet::from([target.to_string()]);
        for node in order.iter().rev() {
            if needed.contains(node) {
                for e in self.edges.iter().filter(|e| e.to == *node) {
                    needed.insert(e.from.clone());
                }
            }
        }
        Ok(order.into_iter().filter(|n| needed.contains(n)).collect())
    }
    pub fn connect(&mut self, edge: Edge) -> Result<()> {
        let mut next = self.clone();
        next.edges.push(edge);
        next.validate(false)?;
        next.revision = crate::next_revision(next.revision)?;
        *self = next;
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub format: u32,
    #[serde(default)]
    pub revision: u64,
    pub id: String,
    pub project_id: String,
    pub graphs: BTreeMap<String, Graph>,
    #[serde(default)]
    pub operations: BTreeMap<String, crate::commands::Receipt>,
    #[serde(default)]
    pub source_document: Option<String>,
}
impl Document {
    pub fn new(graph: Graph) -> Self {
        Self {
            format: 1,
            revision: 0,
            id: id(),
            project_id: id(),
            graphs: BTreeMap::from([(graph.id.clone(), graph)]),
            operations: BTreeMap::new(),
            source_document: None,
        }
    }
    pub fn check_envelope(&self) -> Result<()> {
        if self.format != 1 {
            return Err("unsupported document format: original preserved".into());
        }
        if self.id.is_empty()
            || self.project_id.is_empty()
            || self.graphs.iter().any(|(k, g)| k != &g.id)
        {
            return Err("document identity mismatch".into());
        }
        for graph in self.graphs.values() {
            if graph.id.is_empty()
                || graph.viewport.iter().any(|x| !x.is_finite())
                || graph.viewport[2] <= 0.
                || graph.nodes.iter().any(|(id, n)| {
                    *id != n.id || id.is_empty() || n.position.iter().any(|x| !x.is_finite())
                })
            {
                return Err("invalid stored graph identity/geometry".into());
            }
        }
        Ok(())
    }
}
/// History owns graph edits only. Undo never erases run records or calls an executor.
#[derive(Default)]
pub struct History {
    undo: Vec<Graph>,
    redo: Vec<Graph>,
}
impl History {
    pub fn edit(
        &mut self,
        graph: &mut Graph,
        f: impl FnOnce(&mut Graph) -> Result<()>,
    ) -> Result<()> {
        let mut next = graph.clone();
        f(&mut next)?;
        next.validate(false)?;
        if next == *graph {
            return Ok(());
        }
        next.revision = crate::next_revision(graph.revision)?;
        self.undo.push(graph.clone());
        self.redo.clear();
        *graph = next;
        Ok(())
    }
    pub fn undo(&mut self, graph: &mut Graph) -> Result<bool> {
        Self::swap(graph, &mut self.undo, &mut self.redo)
    }
    pub fn redo(&mut self, graph: &mut Graph) -> Result<bool> {
        Self::swap(graph, &mut self.redo, &mut self.undo)
    }
    fn swap(graph: &mut Graph, from: &mut Vec<Graph>, to: &mut Vec<Graph>) -> Result<bool> {
        if let Some(mut next) = from.last().cloned() {
            next.revision = crate::next_revision(graph.revision)?;
            from.pop();
            to.push(graph.clone());
            *graph = next;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
