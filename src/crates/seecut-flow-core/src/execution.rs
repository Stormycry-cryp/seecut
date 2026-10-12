//! Durable service attempts, separate from the deterministic simulator and its ledger.
use crate::{
    Result, digest,
    graph::MediaKind,
    host::GenerationDraft,
    id,
    runner::{Asset, Scope},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Prepared,
    Intent,
    Accepted,
    Finished,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub sha256: String,
    pub asset: Option<Asset>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub status: String,
    pub captured_credits: u64,
    pub outputs: BTreeMap<String, Candidate>,
    #[serde(default)]
    pub output_error: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub key: String,
    pub fingerprint: String,
    pub source_digest: String,
    pub draft: GenerationDraft,
    pub phase: Phase,
    pub body: Option<Value>,
    pub request_digest: Option<String>,
    pub tasks: BTreeMap<String, Task>,
    pub selected: Option<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Execution {
    pub format: u32,
    pub document_id: String,
    pub project_id: String,
    pub paused: bool,
    pub attempts: Vec<Attempt>,
}
fn valid_id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_digest(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn fingerprint(d: &GenerationDraft) -> String {
    digest(&(
        &d.context.document_id,
        &d.context.graph_id,
        &d.context.node_id,
        &d.context.scope,
        &d.model_id,
        &d.capability_version,
        &d.prompt,
        &d.parameters,
        &d.references,
    ))
}
impl Execution {
    pub fn new(document_id: String, project_id: String) -> Self {
        Self {
            format: 1,
            document_id,
            project_id,
            paused: false,
            attempts: vec![],
        }
    }
    pub fn validate(&self, document_id: &str, project_id: &str) -> Result<()> {
        if self.format != 1 || self.document_id != document_id || self.project_id != project_id {
            return Err("service execution identity/version mismatch".into());
        }
        let mut ids = BTreeSet::new();
        for a in &self.attempts {
            if !valid_id(&a.id)
                || a.id != a.key
                || !ids.insert(&a.id)
                || a.draft.context.document_id != self.document_id
                || a.draft.context.scope.account.is_empty()
                || a.draft.context.scope.service.is_empty()
                || a.draft.media_kind != MediaKind::Image
                || a.draft.model_id.is_empty()
                || a.draft.capability_version.is_empty()
                || a.fingerprint != fingerprint(&a.draft)
                || !valid_digest(&a.source_digest)
            {
                return Err("invalid frozen service attempt".into());
            }
            if a.body.as_ref().map(digest) != a.request_digest
                || (a.phase == Phase::Prepared) != a.body.is_none()
                || a.body
                    .as_ref()
                    .is_some_and(|b| b["quote_id"].as_str().is_none_or(str::is_empty))
            {
                return Err("invalid frozen service request".into());
            }
            for (id, t) in &a.tasks {
                if id != &t.id
                    || !valid_id(id)
                    || t.captured_credits > 1_000_000_000
                    || !matches!(
                        t.status.as_str(),
                        "queued"
                            | "submitting"
                            | "running"
                            | "pending_reconcile"
                            | "succeeded"
                            | "failed"
                            | "expired"
                    )
                {
                    return Err("invalid service task".into());
                }
                for (id, o) in &t.outputs {
                    if id != &o.id || !valid_id(id) || !valid_digest(&o.sha256) {
                        return Err("invalid service output".into());
                    }
                    if let Some(asset) = &o.asset {
                        check_asset(asset, &o.sha256)?;
                    }
                }
            }
            if let Some((task, output)) = &a.selected {
                a.tasks
                    .get(task)
                    .and_then(|t| t.outputs.get(output))
                    .and_then(|o| o.asset.as_ref())
                    .ok_or("selected output has no registered version")?;
            }
        }
        Ok(())
    }
    /// Duplicate clicks retain one frozen in-flight attempt. Completed explicit runs may start anew.
    pub fn begin(&mut self, draft: GenerationDraft, source_digest: String) -> Result<String> {
        if self.paused {
            return Err("后续步骤已暂停".into());
        }
        let fingerprint = fingerprint(&draft);
        if let Some(a) = self.attempts.iter().rev().find(|a| {
            a.draft.context.graph_id == draft.context.graph_id
                && a.draft.context.node_id == draft.context.node_id
                && a.draft.context.scope == draft.context.scope
                && (a.phase == Phase::Intent
                    || a.tasks.values().any(|t| t.status == "pending_reconcile"))
        }) {
            if a.fingerprint == fingerprint {
                return Ok(a.id.clone());
            }
            return Err("原图片任务结果未确定，请先核账".into());
        }
        if let Some(a) = self
            .attempts
            .iter()
            .rev()
            .find(|a| a.fingerprint == fingerprint && a.phase != Phase::Finished)
        {
            return Ok(a.id.clone());
        }
        let id = id();
        self.attempts.push(Attempt {
            id: id.clone(),
            key: id.clone(),
            fingerprint,
            source_digest,
            draft,
            phase: Phase::Prepared,
            body: None,
            request_digest: None,
            tasks: BTreeMap::new(),
            selected: None,
            error: None,
        });
        self.validate(&self.document_id, &self.project_id)?;
        Ok(id)
    }
    pub fn attempt_mut(&mut self, id: &str) -> Result<&mut Attempt> {
        self.attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or("attempt missing".into())
    }
    /// Commit this transition before the shared client is allowed to POST.
    pub fn intent(&mut self, id: &str, body: Value) -> Result<()> {
        let a = self.attempt_mut(id)?;
        if a.phase != Phase::Prepared {
            return Err("request already frozen; query original key".into());
        }
        if body["quote_id"].as_str().is_none_or(str::is_empty) {
            return Err("quote missing".into());
        }
        a.request_digest = Some(digest(&body));
        a.body = Some(body);
        a.phase = Phase::Intent;
        Ok(())
    }
    pub fn observe(&mut self, id: &str, scope: &Scope, task: &Value) -> Result<()> {
        let a = self.attempt_mut(id)?;
        if &a.draft.context.scope != scope || a.body.is_none() {
            return Err("service scope changed".into());
        }
        let task_id = task["id"].as_str().ok_or("task ID missing")?;
        if !valid_id(task_id)
            || !task_id.starts_with("gen_")
            || task["kind"] != "image"
            || task["model"] != a.draft.model_id
        {
            return Err("task identity/model mismatch".into());
        }
        // The original frozen canonical user fields must agree, not a different task found by accident.
        for key in [
            "model",
            "prompt",
            "operation",
            "size",
            "quality",
            "quantity",
            "reference_asset_ids",
            "effort_id",
            "policy_revision",
        ] {
            if let Some(value) = a.body.as_ref().and_then(|b| b.get(key))
                && task["request"].get(key) != Some(value)
            {
                return Err("task request fingerprint mismatch".into());
            }
        }
        let status = task["status"]
            .as_str()
            .ok_or("task status missing")?
            .to_owned();
        let captured = task["captured_credits"]
            .as_u64()
            .ok_or("actual charge missing")?;
        let mut next = Task {
            id: task_id.into(),
            status,
            captured_credits: captured,
            outputs: BTreeMap::new(),
            output_error: false,
        };
        if let Some(old) = a.tasks.get(task_id) {
            next.captured_credits = next.captured_credits.max(old.captured_credits);
            next.outputs = old.outputs.clone();
            if matches!(old.status.as_str(), "succeeded" | "failed" | "expired")
                && !matches!(next.status.as_str(), "succeeded" | "failed" | "expired")
            {
                next.status.clone_from(&old.status);
            }
        }
        let mut seen_outputs = BTreeSet::new();
        for output in task["outputs"].as_array().into_iter().flatten() {
            let output_id = output["id"].as_str().unwrap_or_default();
            let sha = output["sha256"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !valid_id(output_id) || !valid_digest(&sha) || !seen_outputs.insert(output_id) {
                next.output_error = true;
                continue;
            }
            if let Some(previous) = next.outputs.get(output_id) {
                if previous.sha256 != sha {
                    next.output_error = true;
                    continue;
                }
            } else {
                next.outputs.insert(
                    output_id.into(),
                    Candidate {
                        id: output_id.into(),
                        sha256: sha,
                        asset: None,
                    },
                );
            }
        }
        a.error = next
            .output_error
            .then(|| "输出摘要无效，等待原任务核对".into());
        a.tasks.insert(task_id.into(), next);
        a.phase = Phase::Accepted;
        a.finish_if_complete();
        Ok(())
    }
    pub fn registered(
        &mut self,
        attempt: &str,
        task: &str,
        output: &str,
        asset: Asset,
    ) -> Result<()> {
        let a = self.attempt_mut(attempt)?;
        let o = a
            .tasks
            .get_mut(task)
            .and_then(|t| t.outputs.get_mut(output))
            .ok_or("output missing")?;
        check_asset(&asset, &o.sha256)?;
        if o.asset.as_ref().is_some_and(|previous| previous != &asset) {
            return Err("registration identity changed".into());
        }
        o.asset = Some(asset);
        a.finish_if_complete();
        Ok(())
    }
    pub fn select(
        &mut self,
        attempt: &str,
        task: &str,
        output: &str,
        scope: &Scope,
    ) -> Result<Asset> {
        let a = self.attempt_mut(attempt)?;
        if &a.draft.context.scope != scope {
            return Err("service scope changed".into());
        }
        let asset = a
            .tasks
            .get(task)
            .and_then(|t| t.outputs.get(output))
            .and_then(|o| o.asset.clone())
            .ok_or("结果尚未完成入库".to_string())?;
        a.selected = Some((task.into(), output.into()));
        Ok(asset)
    }
    pub fn actual_credits(&self, scope: &Scope) -> Result<u64> {
        let mut totals = BTreeMap::new();
        for a in self
            .attempts
            .iter()
            .filter(|a| &a.draft.context.scope == scope)
        {
            for t in a.tasks.values() {
                totals
                    .entry(&t.id)
                    .and_modify(|v: &mut u64| *v = (*v).max(t.captured_credits))
                    .or_insert(t.captured_credits);
            }
        }
        totals.values().try_fold(0_u64, |sum, value| {
            sum.checked_add(*value)
                .ok_or("actual credit total overflow".into())
        })
    }
}
impl Attempt {
    fn finish_if_complete(&mut self) {
        let expected = self
            .body
            .as_ref()
            .and_then(|b| b["quantity"].as_u64())
            .unwrap_or(1) as usize;
        if self.tasks.len() == expected
            && self.tasks.values().all(|t| {
                t.status == "failed"
                    || (t.status == "expired" && t.outputs.values().all(|o| o.asset.is_some()))
                    || (t.status == "succeeded"
                        && !t.output_error
                        && !t.outputs.is_empty()
                        && t.outputs.values().all(|o| o.asset.is_some()))
            })
        {
            self.phase = Phase::Finished;
        }
    }
}
fn check_asset(asset: &Asset, sha: &str) -> Result<()> {
    let version: Value =
        serde_json::from_str(&asset.version).map_err(|_| "registered version missing")?;
    if asset.id.is_empty()
        || asset.simulated
        || asset.kind != MediaKind::Image
        || version["sha256"] != sha
        || version["registration_id"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        return Err("registered output version mismatch".into());
    }
    Ok(())
}
