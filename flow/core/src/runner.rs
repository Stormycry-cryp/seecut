use crate::{
    Result, digest,
    graph::{Graph, MediaKind, Node},
    id,
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub account: String,
    pub service: String,
}
impl Scope {
    pub fn synthetic() -> Self {
        Self {
            account: "synthetic-account".into(),
            service: "deterministic-local-v1".into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub version: String,
    pub kind: MediaKind,
    pub simulated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Output {
    Text(String),
    Media(Asset),
    Candidates { batch: String, assets: Vec<Asset> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Intent,
    Unknown,
    CancelRequested,
    Cancelled,
    Failed,
    Succeeded,
}
impl Status {
    fn unresolved(&self) -> bool {
        matches!(self, Self::Intent | Self::Unknown | Self::CancelRequested)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub port: String,
    pub order: u32,
    pub source: String,
    pub output: Output,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub key: String,
    pub run_id: String,
    pub graph_id: String,
    pub node_id: String,
    pub revision: u64,
    pub scope: Scope,
    pub fingerprint: String,
    pub node: Node,
    pub inputs: Vec<Input>,
    pub virtual_quote: u64,
    pub status: Status,
    pub output: Option<Output>,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub graph_id: String,
    pub target: String,
    pub revision: u64,
    pub order: Vec<String>,
    pub status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedTask {
    pub fingerprint: String,
    pub scope: Scope,
    pub output: Output,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runtime {
    pub format: u32,
    pub document_id: String,
    pub attempts: Vec<Attempt>,
    pub runs: Vec<Run>,
    /// Local test fixture only; production must supply the existing account/points client.
    pub virtual_spent: u64,
    pub tasks: BTreeMap<String, SimulatedTask>,
    pub deliveries: BTreeMap<String, Asset>,
}
impl Runtime {
    pub fn new(document_id: String) -> Self {
        Self {
            format: 1,
            document_id,
            attempts: vec![],
            runs: vec![],
            virtual_spent: 0,
            tasks: BTreeMap::new(),
            deliveries: BTreeMap::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for a in &self.attempts {
            if !ids.insert(&a.id)
                || a.id != a.key
                || a.node_id != a.node.id
                || a.fingerprint != fingerprint(&a.node, &a.inputs)
            {
                return Err("corrupt attempt identity/input snapshot".into());
            }
            if a.status == Status::Succeeded && a.output.is_none() {
                return Err("successful attempt missing output".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    BeforeSubmit,
    Reject,
    AfterAccept,
    BeforeDelivery,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Complete(Output),
    Paused { node: String, reason: String },
}
/// Scheduling is serial. Every call uses the frozen graph clone and a single service/account scope.
/// The deterministic transport is intentionally the only implementation in this candidate.
pub struct Runner {
    pub store: Store,
    scope: Scope,
}
impl Runner {
    pub fn new(store: Store, scope: Scope) -> Result<Self> {
        if scope.account.is_empty() || scope.service.is_empty() {
            return Err("account/service required".into());
        }
        Ok(Self { store, scope })
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn run_to(&mut self, graph_id: &str, target: &str) -> Result<Outcome> {
        let graph = self.graph(graph_id)?;
        let order = graph.ancestors(target)?;
        let run_id = id();
        let mut runtime = self.store.runtime().clone();
        runtime.runs.push(Run {
            id: run_id.clone(),
            graph_id: graph.id.clone(),
            target: target.into(),
            revision: graph.revision,
            order: order.clone(),
            status: "running".into(),
        });
        self.store.save_runtime(runtime)?;
        for node in order {
            match self.execute(&graph, &node, &run_id, false, Fault::None) {
                Ok(_) => {}
                Err(reason) => {
                    self.finish_run(&run_id, "paused")?;
                    return Ok(Outcome::Paused { node, reason });
                }
            }
        }
        self.finish_run(&run_id, "succeeded")?;
        Ok(Outcome::Complete(self.current_output(graph_id, target)?))
    }
    pub fn run_node(
        &mut self,
        graph_id: &str,
        node: &str,
        retry: bool,
        fault: Fault,
    ) -> Result<Output> {
        let graph = self.graph(graph_id)?;
        self.execute(&graph, node, &id(), retry, fault)
    }
    fn graph(&self, graph_id: &str) -> Result<Graph> {
        let graph = self
            .store
            .document()
            .graphs
            .get(graph_id)
            .ok_or("graph missing")?
            .clone();
        graph.validate(false)?;
        Ok(graph)
    }
    pub fn current_output(&self, graph_id: &str, node_id: &str) -> Result<Output> {
        let graph = self.graph(graph_id)?;
        self.output_in(&graph, node_id)
    }
    fn output_in(&self, graph: &Graph, node_id: &str) -> Result<Output> {
        let mut resolved: BTreeMap<String, Output> = BTreeMap::new();
        for key in graph.ancestors(node_id)? {
            let node = &graph.nodes[&key];
            let mut inputs = Vec::new();
            for port in node.ports()?.0 {
                let mut edges: Vec<_> = graph
                    .edges
                    .iter()
                    .filter(|e| e.to == key && e.input == port.name)
                    .collect();
                edges.sort_by_key(|e| e.order);
                if port.required && edges.is_empty() {
                    return Err(format!("{key}: missing {}", port.name));
                }
                for e in edges {
                    inputs.push(Input {
                        port: e.input.clone(),
                        order: e.order,
                        source: e.from.clone(),
                        output: resolved
                            .get(&e.from)
                            .ok_or("upstream output missing")?
                            .clone(),
                    });
                }
            }
            let fp = fingerprint(node, &inputs);
            let attempt = self
                .store
                .runtime()
                .attempts
                .iter()
                .rev()
                .find(|a| {
                    a.graph_id == graph.id
                        && a.node_id == key
                        && a.scope == self.scope
                        && a.fingerprint == fp
                })
                .ok_or_else(|| format!("{key}: upstream output missing or stale"))?;
            if attempt.status != Status::Succeeded {
                return Err(format!(
                    "{key}: upstream output not usable ({:?})",
                    attempt.status
                ));
            }
            resolved.insert(key, attempt.output.clone().ok_or("output missing")?);
        }
        resolved.remove(node_id).ok_or("output missing".into())
    }

    pub(crate) fn inputs(&self, graph: &Graph, node: &Node) -> Result<Vec<Input>> {
        let mut inputs = Vec::new();
        for port in node.ports()?.0 {
            let mut edges: Vec<_> = graph
                .edges
                .iter()
                .filter(|e| e.to == node.id && e.input == port.name)
                .collect();
            edges.sort_by_key(|e| e.order);
            if port.required && edges.is_empty() {
                return Err(format!("{}: missing {}", node.id, port.name));
            }
            for e in edges {
                inputs.push(Input {
                    port: e.input.clone(),
                    order: e.order,
                    source: e.from.clone(),
                    output: self.output_in(graph, &e.from)?,
                });
            }
        }
        Ok(inputs)
    }
    fn execute(
        &mut self,
        graph: &Graph,
        node_id: &str,
        run_id: &str,
        retry: bool,
        fault: Fault,
    ) -> Result<Output> {
        let node = graph.nodes.get(node_id).ok_or("node missing")?.clone();
        if node.kind.ends_with("_generate")
            && (node.params["model"].as_str() != Some("deterministic-v1")
                || !(1..=3).contains(&node.params["count"].as_u64().unwrap_or(0)))
        {
            return Err("this simulator cannot execute SeeCut generation capabilities; connect the shared host first".into());
        }
        let inputs = self.inputs(graph, &node)?;
        let fp = fingerprint(&node, &inputs);
        // Block unresolved attempts even after editing parameters or switching account.
        if self
            .store
            .runtime()
            .attempts
            .iter()
            .any(|a| a.graph_id == graph.id && a.node_id == node_id && a.status.unresolved())
        {
            return Err(
                "attempt awaiting original account/service reconciliation; no resubmit".into(),
            );
        }
        if let Some(a) = self.store.runtime().attempts.iter().rev().find(|a| {
            a.graph_id == graph.id
                && a.node_id == node_id
                && a.fingerprint == fp
                && a.scope == self.scope
        }) {
            if a.status == Status::Succeeded {
                return a.output.clone().ok_or("output missing".into());
            }
            if !retry {
                return Err("explicit retry required after terminal failure/cancellation".into());
            }
        }
        // Selection validates the current batch before an attempt can be recorded.
        if node.kind == "select" {
            select(&node, &inputs)?;
        }
        let attempt_id = id();
        let quote = if node.kind.ends_with("_generate") {
            node.params["count"].as_u64().unwrap() * 10
        } else {
            0
        };
        let attempt = Attempt {
            id: attempt_id.clone(),
            key: attempt_id.clone(),
            run_id: run_id.into(),
            graph_id: graph.id.clone(),
            node_id: node.id.clone(),
            revision: graph.revision,
            scope: self.scope.clone(),
            fingerprint: fp,
            node: node.clone(),
            inputs: inputs.clone(),
            virtual_quote: quote,
            status: Status::Intent,
            output: None,
            message: String::new(),
        };
        let mut runtime = self.store.runtime().clone();
        runtime.attempts.push(attempt);
        self.store.save_runtime(runtime)?; // must succeed before any execution or virtual debit
        if fault == Fault::BeforeSubmit {
            return Err("simulated interruption before submit; intent persisted".into());
        }
        if fault == Fault::Reject {
            self.update_attempt(&attempt_id, Status::Failed, None, "deterministic rejection")?;
            return Err("deterministic rejection".into());
        }
        let output = match simulate(&node, &inputs, &attempt_id) {
            Ok(output) => output,
            Err(error) => {
                self.update_attempt(&attempt_id, Status::Failed, None, &error)?;
                return Err(error);
            }
        };
        let mut runtime = self.store.runtime().clone();
        let attempt = runtime
            .attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .unwrap();
        // Simulator acceptance, virtual debit and task record form one atomic local transaction.
        runtime.tasks.insert(
            attempt_id.clone(),
            SimulatedTask {
                fingerprint: attempt.fingerprint.clone(),
                scope: self.scope.clone(),
                output: output.clone(),
            },
        );
        runtime.virtual_spent += quote;
        attempt.status = Status::Unknown;
        attempt.message = "accepted by deterministic simulator; needs result readback".into();
        self.store.save_runtime(runtime)?;
        if fault == Fault::AfterAccept {
            return Err("simulated lost response after acceptance".into());
        }
        if fault == Fault::BeforeDelivery && node.kind == "deliver" {
            return Err("simulated delivery interruption; reconcile original operation".into());
        }
        self.reconcile(&attempt_id)?;
        self.output_in(graph, node_id)
    }
    /// Read-only task lookup by original key. It never submits, charges, or changes scopes.
    pub fn reconcile(&mut self, attempt_id: &str) -> Result<Status> {
        let mut runtime = self.store.runtime().clone();
        let attempt = runtime
            .attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .ok_or("attempt missing")?;
        if attempt.scope != self.scope {
            return Err("original account/service required".into());
        }
        if !attempt.status.unresolved() {
            return Ok(attempt.status.clone());
        }
        if let Some(task) = runtime.tasks.get(&attempt.key) {
            if task.scope != attempt.scope || task.fingerprint != attempt.fingerprint {
                return Err("task identity conflict".into());
            }
            attempt.output = Some(task.output.clone());
            attempt.status = if attempt.status == Status::CancelRequested {
                Status::Cancelled
            } else {
                Status::Succeeded
            };
            if attempt.node.kind == "deliver"
                && attempt.status == Status::Succeeded
                && let Output::Media(asset) = &task.output
            {
                runtime
                    .deliveries
                    .entry(attempt.key.clone())
                    .or_insert(asset.clone());
            }
            attempt.message = if attempt.status == Status::Cancelled {
                "late output retained; downstream stopped"
            } else {
                "simulated output read back"
            }
            .into();
        } else if matches!(attempt.status, Status::Intent | Status::CancelRequested) {
            // Only local simulator can prove absence, because its acceptance and ledger are atomic.
            attempt.status = if attempt.status == Status::CancelRequested {
                Status::Cancelled
            } else {
                Status::Failed
            };
            attempt.message =
                "simulator proves request was never accepted; explicit retry permitted".into();
        } else {
            return Err("task absence is not proof of failure; keep UNKNOWN".into());
        }
        let status = attempt.status.clone();
        self.store.save_runtime(runtime)?;
        Ok(status)
    }
    pub fn cancel(&mut self, attempt_id: &str) -> Result<()> {
        let a = self
            .store
            .runtime()
            .attempts
            .iter()
            .find(|a| a.id == attempt_id)
            .ok_or("attempt missing")?;
        if a.scope != self.scope {
            return Err("original account/service required".into());
        }
        if !a.status.unresolved() {
            return Err("attempt is already terminal".into());
        }
        self.update_attempt(
            attempt_id,
            Status::CancelRequested,
            None,
            "stop requested; original task still needs verification",
        )
    }
    pub fn choose(&mut self, graph_id: &str, node_id: &str, asset_id: &str) -> Result<()> {
        let graph = self.graph(graph_id)?;
        let node = graph.nodes.get(node_id).ok_or("node missing")?;
        if node.kind != "select" {
            return Err("not a selection node".into());
        }
        let inputs = self.inputs(&graph, node)?;
        let Output::Candidates { batch, assets } = &inputs[0].output else {
            return Err("candidate input required".into());
        };
        if !assets.iter().any(|a| a.id == asset_id) {
            return Err("selection not in current batch".into());
        }
        let mut doc = self.store.document().clone();
        let graph = doc.graphs.get_mut(graph_id).unwrap();
        let node = graph.nodes.get_mut(node_id).unwrap();
        node.params["batch"] = batch.clone().into();
        node.params["asset_id"] = asset_id.into();
        graph.revision += 1;
        self.store.save_document(doc)
    }
    fn update_attempt(
        &mut self,
        id: &str,
        status: Status,
        output: Option<Output>,
        message: &str,
    ) -> Result<()> {
        let mut runtime = self.store.runtime().clone();
        let a = runtime
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or("attempt missing")?;
        a.status = status;
        a.output = output;
        a.message = message.into();
        self.store.save_runtime(runtime)
    }
    fn finish_run(&mut self, id: &str, status: &str) -> Result<()> {
        let mut runtime = self.store.runtime().clone();
        runtime
            .runs
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or("run missing")?
            .status = status.into();
        self.store.save_runtime(runtime)
    }
}
fn fingerprint(node: &Node, inputs: &[Input]) -> String {
    digest(&(node.kind.as_str(), node.version, &node.params, inputs))
}
fn select(node: &Node, inputs: &[Input]) -> Result<Output> {
    let Some(Input {
        output: Output::Candidates { batch, assets },
        ..
    }) = inputs.first()
    else {
        return Err("candidate input missing".into());
    };
    if node.params["batch"].as_str() != Some(batch) {
        return Err("choose one result from the current batch before continuing".into());
    }
    assets
        .iter()
        .find(|a| Some(a.id.as_str()) == node.params["asset_id"].as_str())
        .cloned()
        .map(Output::Media)
        .ok_or("choose one result from the current batch before continuing".into())
}

fn simulate(node: &Node, inputs: &[Input], attempt: &str) -> Result<Output> {
    Ok(match node.kind.as_str() {
        "prompt" => Output::Text(node.params["text"].as_str().unwrap().into()),
        "asset" => {
            let asset_id = node.params["asset_id"].as_str().unwrap();
            if !asset_id.starts_with("synthetic:") {
                return Err("simulator only accepts synthetic asset IDs".into());
            }
            Output::Media(Asset {
                id: asset_id.into(),
                version: node.params["content_version"].as_str().unwrap().into(),
                kind: node.media_kind()?,
                simulated: true,
            })
        }
        "image_generate" | "video_generate" => Output::Candidates {
            batch: attempt.into(),
            assets: (0..node.params["count"].as_u64().unwrap())
                .map(|index| Asset {
                    id: format!("synthetic:{attempt}:{index}"),
                    version: digest(&(fingerprint(node, inputs), index)),
                    kind: if node.kind == "image_generate" {
                        MediaKind::Image
                    } else {
                        MediaKind::Video
                    },
                    simulated: true,
                })
                .collect(),
        },
        "select" => select(node, inputs)?,
        "deliver" => inputs.first().ok_or("media input missing")?.output.clone(),
        _ => return Err("unsupported executor".into()),
    })
}
