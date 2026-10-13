//! Durable scheduling over real service attempts. No supplier calls or simulated outputs.
use crate::{
    Result, digest,
    execution::{Execution, Phase},
    graph::{Graph, MediaKind, Node},
    host::{Context, GenerationDraft},
    id,
    runner::{Asset, Scope},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Running,
    AwaitingSelection,
    Blocked,
    Complete,
    Abandoned,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Output {
    Text(String),
    Media(Asset),
    Candidates(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub attempt: String,
    pub task: String,
    pub output: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Step {
    pub attempt: Option<String>,
    pub reused: bool,
    pub output: Option<Output>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub scope: Scope,
    pub target: String,
    pub graph: Graph,
    pub graph_fingerprint: String,
    pub source_digest: String,
    pub order: Vec<String>,
    pub steps: BTreeMap<String, Step>,
    pub choices: BTreeMap<String, Choice>,
    pub state: State,
    pub current: Option<String>,
    pub error: Option<String>,
    pub force_target: bool,
}
#[derive(Clone, Debug)]
pub struct Delivery {
    pub run: String,
    pub node: String,
    pub asset: Asset,
}

pub fn graph_fingerprint(graph: &Graph) -> String {
    let nodes: Vec<_> = graph
        .nodes
        .values()
        .map(|n| (&n.id, &n.kind, n.version, &n.params))
        .collect();
    digest(&(nodes, &graph.edges))
}
fn parameters(node: &Node) -> Value {
    let mut params = node.params.clone();
    if let Some(fields) = params.as_object_mut() {
        fields.remove("copied_results");
        fields.remove("selected_output");
    }
    params
}
impl Run {
    fn inputs<'a>(&'a self, node: &str, port: &str) -> Result<Vec<&'a Output>> {
        let mut edges: Vec<_> = self
            .graph
            .edges
            .iter()
            .filter(|e| e.to == node && e.input == port)
            .collect();
        edges.sort_by_key(|e| e.order);
        edges
            .into_iter()
            .map(|e| {
                self.steps
                    .get(&e.from)
                    .and_then(|s| s.output.as_ref())
                    .ok_or("依赖尚未完成".into())
            })
            .collect()
    }
    pub fn selected_asset(&self, node: &str) -> Option<&Asset> {
        match self.steps.get(node)?.output.as_ref()? {
            Output::Media(asset) => Some(asset),
            _ => None,
        }
    }
    fn draft(&self, document: &str, node: &Node) -> Result<GenerationDraft> {
        let prompts = self.inputs(&node.id, "prompt")?;
        let upstream = match prompts.first() {
            Some(Output::Text(text)) => Some(text.as_str()),
            None => None,
            _ => return Err("提示词输入类型不匹配".into()),
        };
        let references = self
            .inputs(&node.id, "references")?
            .into_iter()
            .map(|value| match value {
                Output::Media(asset)
                    if node.kind == "video_generate" || asset.kind == MediaKind::Image =>
                {
                    Ok(asset.clone())
                }
                _ => Err("参考素材类型不匹配".into()),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(GenerationDraft {
            context: Context {
                document_id: document.into(),
                graph_id: self.graph.id.clone(),
                node_id: node.id.clone(),
                revision: self.graph.revision,
                scope: self.scope.clone(),
            },
            model_id: node.params["model"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("请选择模型")?
                .into(),
            capability_version: node.params["capability_version"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("请选择可用模型")?
                .into(),
            media_kind: if node.kind == "video_generate" {
                MediaKind::Video
            } else {
                MediaKind::Image
            },
            prompt: crate::host::composed_prompt(upstream, node.params["prompt"].as_str())?,
            parameters: parameters(node),
            references,
        })
    }
    pub fn actual_credits(&self, execution: &Execution) -> u64 {
        let mut tasks = BTreeMap::new();
        for step in self.steps.values().filter(|s| !s.reused) {
            if let Some(attempt) = step
                .attempt
                .as_ref()
                .and_then(|id| execution.attempts.iter().find(|a| &a.id == id))
            {
                for task in attempt.tasks.values() {
                    tasks.insert(&task.id, task.captured_credits);
                }
            }
        }
        tasks
            .values()
            .fold(0_u64, |sum, value| sum.saturating_add(*value))
    }
}
impl Execution {
    pub fn begin_run(
        &mut self,
        graph: Graph,
        scope: Scope,
        target: String,
        source_digest: String,
        force_target: bool,
    ) -> Result<String> {
        let order = graph.ancestors(&target)?;
        if self.paused {
            return Err("流程已暂停，请先继续".into());
        }
        for node_id in &order {
            let node = &graph.nodes[node_id];
            if matches!(node.kind.as_str(), "image_generate" | "video_generate")
                && node.params["capability_version"]
                    .as_str()
                    .is_none_or(str::is_empty)
            {
                return Err(format!("{}：请选择可用模型", node.title));
            }
        }
        let fingerprint = graph_fingerprint(&graph);
        if let Some(index) = self.runs.iter().rposition(|r| {
            r.graph.id == graph.id && !matches!(r.state, State::Complete | State::Abandoned)
        }) {
            let run = &self.runs[index];
            if run.scope == scope
                && run.target == target
                && run.graph_fingerprint == fingerprint
                && run.force_target == force_target
            {
                return Ok(run.id.clone());
            }
            let pending = run
                .steps
                .values()
                .filter_map(|step| step.attempt.as_ref())
                .any(|id| {
                    self.attempts
                        .iter()
                        .any(|a| &a.id == id && matches!(a.phase, Phase::Intent | Phase::Accepted))
                });
            if run.scope != scope || pending || run.state == State::Running {
                return Err("当前流程尚未结束，请先继续或核实原任务".into());
            }
            // An explicit changed run may replace a stopped, known-safe plan. Its old receipts remain.
            let queued: Vec<_> = run
                .steps
                .values()
                .filter_map(|s| s.attempt.clone())
                .collect();
            for a in self
                .attempts
                .iter_mut()
                .filter(|a| queued.contains(&a.id) && a.phase == Phase::Prepared)
            {
                a.superseded = true;
                a.error = Some("已替换未提交的运行".into());
            }
            self.runs[index].state = State::Abandoned;
        }
        let id = id();
        self.runs.push(Run {
            id: id.clone(),
            scope,
            target,
            graph,
            graph_fingerprint: fingerprint,
            source_digest,
            order,
            steps: BTreeMap::new(),
            choices: BTreeMap::new(),
            state: State::Running,
            current: None,
            error: None,
            force_target,
        });
        self.validate(&self.document_id, &self.project_id)?;
        Ok(id)
    }
    /// Mutates only local execution state. Host commits it before dispatching a prepared attempt.
    pub fn advance_run(&mut self, run_id: &str, scope: &Scope) -> Result<Option<Delivery>> {
        let index = self
            .runs
            .iter()
            .position(|r| r.id == run_id && &r.scope == scope)
            .ok_or("运行身份已变化")?;
        if self.paused
            || matches!(
                self.runs[index].state,
                State::Blocked | State::Complete | State::Abandoned
            )
        {
            return Ok(None);
        }
        let mut run = self.runs[index].clone();
        let result = self.advance_frozen(&mut run);
        if let Err(error) = &result {
            run.state = State::Blocked;
            run.error = Some(error.clone());
        }
        self.runs[index] = run;
        result
    }
    fn advance_frozen(&mut self, run: &mut Run) -> Result<Option<Delivery>> {
        for node_id in run.order.clone() {
            if run.steps.get(&node_id).is_some_and(|s| s.output.is_some()) {
                continue;
            }
            run.current = Some(node_id.clone());
            run.state = State::Running;
            run.error = None;
            let node = run.graph.nodes.get(&node_id).ok_or("冻结节点缺失")?.clone();
            let mut step = run.steps.get(&node_id).cloned().unwrap_or_default();
            let output = match node.kind.as_str() {
                "prompt" => Output::Text(node.params["text"].as_str().ok_or("提示词缺失")?.into()),
                "asset" => Output::Media(Asset {
                    id: node.params["asset_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("请选择素材")?
                        .into(),
                    version: node.params["content_version"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("素材版本缺失")?
                        .into(),
                    kind: node.media_kind()?,
                    simulated: false,
                }),
                "image_generate" | "video_generate" => {
                    if step.attempt.is_none() {
                        let draft = run.draft(&self.document_id, &node)?;
                        let fingerprint = crate::execution::fingerprint(&draft);
                        let pending = self.pending_attempt(&draft)?.map(|a| a.id.clone());
                        let preferred = run.order.iter().find_map(|id| {
                            let selected = run.graph.nodes.get(id)?;
                            (selected.kind == "select"
                                && run.graph.edges.iter().any(|e| {
                                    e.from == node_id && e.to == *id && e.input == "candidates"
                                }))
                            .then(|| selected.params["selected_output"]["attempt"].as_str())
                            .flatten()
                        });
                        let cached = (pending.is_none()
                            && !(run.force_target && run.target == node_id))
                            .then(|| {
                                self.attempts
                                    .iter()
                                    .rev()
                                    .filter(|a| {
                                        a.fingerprint == fingerprint
                                            && !a.superseded
                                            && a.phase == Phase::Finished
                                            && a.tasks.values().any(|t| {
                                                t.outputs.values().any(|o| o.asset.is_some())
                                            })
                                    })
                                    .min_by_key(|a| Some(a.id.as_str()) != preferred)
                                    .map(|a| a.id.clone())
                            })
                            .flatten();
                        if run.force_target && run.target != node_id && cached.is_none() {
                            return Err("上游结果尚未就绪；可使用运行到这里完成依赖".into());
                        }
                        step.reused = cached.is_some();
                        step.attempt = Some(match pending.or(cached) {
                            Some(id) => id,
                            None => self.begin(draft, run.source_digest.clone())?,
                        });
                        run.steps.insert(node_id.clone(), step.clone());
                    }
                    let attempt = self
                        .attempts
                        .iter()
                        .find(|a| Some(&a.id) == step.attempt.as_ref())
                        .ok_or("运行任务缺失")?;
                    if attempt.phase == Phase::Rejected
                        || (attempt.phase == Phase::Prepared && attempt.error.is_some())
                    {
                        return Err(attempt
                            .error
                            .clone()
                            .unwrap_or_else(|| "任务未提交，请重试".into()));
                    }
                    if attempt.phase != Phase::Finished {
                        return Ok(None);
                    }
                    if !attempt
                        .tasks
                        .values()
                        .any(|t| t.outputs.values().any(|o| o.asset.is_some()))
                    {
                        return Err("生成未成功，可重试当前步骤".into());
                    }
                    Output::Candidates(attempt.id.clone())
                }
                "select" => {
                    let inputs = run.inputs(&node_id, "candidates")?;
                    let attempt = match inputs.first() {
                        Some(Output::Candidates(attempt_id)) => {
                            self.attempts.iter().find(|a| &a.id == attempt_id)
                        }
                        None => crate::host::result_attempt(self, &run.graph, &node, &run.scope),
                        _ => None,
                    }
                    .ok_or("请连接生成结果")?;
                    let choice = run
                        .choices
                        .get(&node_id)
                        .cloned()
                        .or_else(|| {
                            serde_json::from_value(node.params["selected_output"].clone()).ok()
                        })
                        .or_else(|| {
                            crate::host::node_choice(&node, attempt).map(|(task, output)| Choice {
                                attempt: attempt.id.clone(),
                                task,
                                output,
                            })
                        });
                    let Some((choice, asset)) = choice.and_then(|choice| {
                        self.run_choice_asset(run, &node_id, &choice)
                            .ok()
                            .map(|asset| (choice, asset))
                    }) else {
                        run.state = State::AwaitingSelection;
                        return Ok(None);
                    };
                    run.choices.insert(node_id.clone(), choice);
                    Output::Media(asset)
                }
                "deliver" => {
                    let inputs = run.inputs(&node_id, "media")?;
                    let Some(Output::Media(asset)) = inputs.first() else {
                        return Err("请连接要交付的素材".into());
                    };
                    if asset.kind != node.media_kind()? || node.params["target"] != "library" {
                        return Err("请选择全局资产库交付".into());
                    }
                    return Ok(Some(Delivery {
                        run: run.id.clone(),
                        node: node_id,
                        asset: asset.clone(),
                    }));
                }
                _ => return Err("此节点尚不支持运行".into()),
            };
            step.output = Some(output);
            run.steps.insert(node_id, step);
        }
        run.current = None;
        run.state = State::Complete;
        Ok(None)
    }
    pub fn choose_run_output(
        &mut self,
        graph: &str,
        node: &str,
        choice: Choice,
        scope: &Scope,
    ) -> Result<bool> {
        let Some(index) = self.runs.iter().rposition(|r| {
            r.graph.id == graph
                && &r.scope == scope
                && !matches!(r.state, State::Complete | State::Abandoned)
        }) else {
            return Ok(false);
        };
        let run = &self.runs[index];
        if run.graph.nodes.get(node).is_none_or(|n| n.kind != "select") {
            return Ok(false);
        }
        if run.steps.get(node).is_some_and(|s| s.output.is_some()) {
            return Ok(false);
        }
        if run.inputs(node, "candidates").is_err() {
            return Ok(false);
        }
        self.run_choice_asset(run, node, &choice)?;
        let run = &mut self.runs[index];
        run.choices.insert(node.into(), choice);
        run.state = State::Running;
        run.error = None;
        Ok(true)
    }
    /// A consumer may adopt a historical batch with the same frozen inputs; other
    /// consumers keep their own choices. Unresolved submissions still take precedence.
    pub fn run_choice_asset(&self, run: &Run, node: &str, choice: &Choice) -> Result<Asset> {
        let selected = run
            .graph
            .nodes
            .get(node)
            .filter(|n| n.kind == "select")
            .ok_or("采用节点已变化")?;
        let attempt = self
            .attempts
            .iter()
            .find(|a| {
                a.id == choice.attempt
                    && !a.superseded
                    && a.phase == Phase::Finished
                    && a.draft.context.document_id == self.document_id
                    && a.draft.context.graph_id == run.graph.id
                    && a.draft.context.scope == run.scope
            })
            .ok_or("结果身份或状态不匹配")?;
        let inputs = run.inputs(node, "candidates")?;
        if let Some(edge) = run
            .graph
            .edges
            .iter()
            .find(|e| e.to == node && e.input == "candidates")
        {
            if !matches!(inputs.first(), Some(Output::Candidates(_))) {
                return Err("生成依赖尚未完成".into());
            }
            let source = run.graph.nodes.get(&edge.from).ok_or("生成节点缺失")?;
            let draft = run.draft(&self.document_id, source)?;
            if attempt.fingerprint != crate::execution::fingerprint(&draft) {
                return Err("上游输入已变化，请选用当前版本的结果".into());
            }
            if self.pending_attempt(&draft)?.is_some() {
                return Err("原任务尚未结束，请先核实原任务".into());
            }
        } else if !selected.params["copied_results"]
            .as_array()
            .is_some_and(|refs| refs.iter().any(|r| r["attempt"] == choice.attempt))
        {
            return Err("请选择当前节点的结果".into());
        }
        let asset = chosen_asset(self, choice, &run.scope)?;
        if asset.kind != selected.media_kind()? || asset.simulated {
            return Err("采用素材类型不匹配".into());
        }
        Ok(asset)
    }
    pub fn complete_delivery(&mut self, receipt: &Delivery, scope: &Scope) -> Result<()> {
        let run = self
            .runs
            .iter_mut()
            .find(|r| r.id == receipt.run && &r.scope == scope)
            .ok_or("交付运行身份已变化")?;
        let node = run
            .graph
            .nodes
            .get(&receipt.node)
            .filter(|n| n.kind == "deliver" && n.params["target"] == "library")
            .ok_or("交付节点已变化")?;
        let inputs = run.inputs(&receipt.node, "media")?;
        if node.media_kind()? != receipt.asset.kind
            || !matches!(inputs.first(),Some(Output::Media(asset)) if *asset == receipt.asset)
        {
            return Err("交付素材版本不匹配".into());
        }
        run.steps.entry(receipt.node.clone()).or_default().output =
            Some(Output::Media(receipt.asset.clone()));
        Ok(())
    }
    pub fn retry_run(&mut self, id: &str, scope: &Scope) -> Result<()> {
        let index = self
            .runs
            .iter()
            .position(|r| r.id == id && &r.scope == scope)
            .ok_or("运行身份已变化")?;
        let current = self.runs[index]
            .current
            .clone()
            .ok_or("当前运行没有待重试步骤")?;
        if let Some(attempt_id) = self.runs[index]
            .steps
            .get(&current)
            .and_then(|s| s.attempt.clone())
        {
            let attempt = self.attempt_mut(&attempt_id)?;
            match attempt.phase {
                Phase::Prepared => attempt.error = None,
                Phase::Finished | Phase::Rejected => {
                    self.runs[index].steps.remove(&current);
                }
                _ => return Err("任务结果尚未确定，请核实原任务".into()),
            }
        }
        self.runs[index].state = State::Running;
        self.runs[index].error = None;
        Ok(())
    }
    pub fn allows_scheduling(&self, attempt: &str) -> bool {
        if self
            .attempts
            .iter()
            .any(|a| a.id == attempt && a.superseded)
        {
            return false;
        }
        let owners: Vec<_> = self
            .runs
            .iter()
            .filter(|r| {
                r.steps
                    .values()
                    .any(|s| s.attempt.as_deref() == Some(attempt))
            })
            .collect();
        owners.is_empty()
            || owners
                .iter()
                .any(|r| !matches!(r.state, State::Complete | State::Abandoned))
    }
    pub(crate) fn validate_runs(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        for run in &self.runs {
            if run.id.is_empty()
                || !ids.insert(&run.id)
                || run.scope.account.is_empty()
                || run.scope.service.is_empty()
                || run.order != run.graph.ancestors(&run.target)?
                || run.graph_fingerprint != graph_fingerprint(&run.graph)
                || run.source_digest.len() != 64
                || !run.source_digest.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("invalid frozen workflow run".into());
            }
            for (node, step) in &run.steps {
                if !run.order.contains(node) {
                    return Err("step outside frozen workflow".into());
                }
                if let Some(id) = &step.attempt
                    && !self.attempts.iter().any(|a| {
                        &a.id == id
                            && a.draft.context.node_id == *node
                            && a.draft.context.graph_id == run.graph.id
                            && a.draft.context.scope == run.scope
                    })
                {
                    return Err("workflow attempt identity mismatch".into());
                }
                if let Some(Output::Candidates(id)) = &step.output
                    && step.attempt.as_ref() != Some(id)
                {
                    return Err("workflow candidate identity mismatch".into());
                }
                if matches!(&step.output,Some(Output::Media(asset)) if asset.simulated || asset.id.is_empty() || asset.version.is_empty())
                {
                    return Err("workflow requires a registered real asset".into());
                }
            }
            for (node, choice) in &run.choices {
                if run.graph.nodes.get(node).is_none_or(|n| n.kind != "select") {
                    return Err("workflow choice node mismatch".into());
                }
                chosen_asset(self, choice, &run.scope)?;
            }
            if run.state == State::Complete
                && run
                    .order
                    .iter()
                    .any(|id| run.steps.get(id).is_none_or(|s| s.output.is_none()))
            {
                return Err("incomplete workflow marked complete".into());
            }
        }
        Ok(())
    }
}
fn chosen_asset(execution: &Execution, choice: &Choice, scope: &Scope) -> Result<Asset> {
    execution
        .attempts
        .iter()
        .find(|a| a.id == choice.attempt && &a.draft.context.scope == scope)
        .and_then(|a| a.tasks.get(&choice.task))
        .and_then(|t| t.outputs.get(&choice.output))
        .and_then(|o| o.asset.clone())
        .ok_or("结果尚未完成入库".into())
}
