// SPDX-License-Identifier: AGPL-3.0-or-later
//! 原生人类输入与真实 COTO 事件的关联，不从模型文字建立导航许可。
use crate::agent_identity::AppIdentity;
use concat_editor_mcp::{AppPageId, AppViewBinding, app_operation_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

const LIMIT: usize = 128;

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

/// 发问时的完整公开来源；凭证不进入该对象。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InputBinding {
    pub app_instance_id: String,
    pub client_id: String,
    pub account_scope: String,
    pub app_session_id: String,
    pub page_id: AppPageId,
    pub page_epoch: u64,
    pub page_state_id: String,
}
impl InputBinding {
    pub fn new(identity: &AppIdentity, client: &str, page: &AppViewBinding) -> Self {
        Self {
            app_instance_id: identity.instance_id.clone(),
            client_id: client.into(),
            account_scope: identity.account_scope.clone(),
            app_session_id: identity.app_session_id.clone(),
            page_id: page.page_id,
            page_epoch: page.page_epoch,
            page_state_id: page.page_state_id.clone(),
        }
    }
    pub fn page(&self) -> AppViewBinding {
        AppViewBinding {
            page_id: self.page_id,
            page_epoch: self.page_epoch,
            page_state_id: self.page_state_id.clone(),
        }
    }
    pub fn valid(&self) -> bool {
        identifier(&self.app_instance_id)
            && identifier(&self.client_id)
            && identifier(&self.app_session_id)
            && self.account_scope.len() == 64
            && self
                .account_scope
                .bytes()
                .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
            && self.page().valid()
    }
    pub fn matches_scope(&self, identity: &AppIdentity, client: &str) -> bool {
        self.valid()
            && self.app_instance_id == identity.instance_id
            && self.account_scope == identity.account_scope
            && self.app_session_id == identity.app_session_id
            && self.client_id == client
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ControlIntent {
    pub version: u8,
    pub input_id: String,
    pub operation_id: String,
    pub client_sequence: u64,
    pub before: AppViewBinding,
    pub target: AppPageId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Contribution {
    pub input_id: String,
    pub applied_seq: u64,
    pub binding: InputBinding,
}

/// 只能由本宿主保存的输入和实际 applied/started 事件建立。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proposal {
    pub session_id: String,
    pub turn_id: String,
    pub invocation_id: String,
    pub started_seq: u64,
    pub intent: ControlIntent,
    pub source: InputBinding,
    pub contributions: Vec<Contribution>,
    epoch: u64,
}

#[derive(Default)]
pub(crate) struct Tracker {
    inputs: BTreeMap<String, InputBinding>,
    applied: BTreeMap<(String, String), u64>,
    started: BTreeMap<String, Proposal>,
    pending: VecDeque<Proposal>,
    epoch: u64,
}
impl Tracker {
    pub fn can_remember(&self, input: &str, binding: &InputBinding) -> bool {
        identifier(input)
            && binding.valid()
            && self
                .inputs
                .get(input)
                .map_or(self.inputs.len() < LIMIT, |previous| previous == binding)
    }
    pub fn remember(&mut self, input: &str, binding: InputBinding) -> bool {
        if !self.can_remember(input, &binding) {
            return false;
        }
        self.inputs.insert(input.into(), binding);
        true
    }
    pub fn applied(&mut self, turn: &str, input: &str, seq: u64) -> bool {
        if !identifier(turn) || seq == 0 || !self.inputs.contains_key(input) {
            return false;
        }
        let key = (turn.to_owned(), input.to_owned());
        if let Some(previous) = self.applied.get(&key) {
            return *previous == seq;
        }
        if self.applied.len() >= LIMIT
            || self
                .applied
                .keys()
                .any(|(old_turn, old_input)| old_input == input && old_turn != turn)
        {
            return false;
        }
        self.applied.insert(key, seq);
        true
    }
    pub fn started(
        &mut self,
        session: &str,
        turn: &str,
        seq: u64,
        data: &Value,
    ) -> Option<Proposal> {
        let invocation = data["invocationId"].as_str()?;
        if !identifier(session)
            || !identifier(turn)
            || !identifier(invocation)
            || self.started.len() >= LIMIT
            || self.started.contains_key(invocation)
            || data["call"]["name"] != "app_navigate"
        {
            return None;
        }
        let intent: ControlIntent =
            serde_json::from_value(data["appControlIntent"].clone()).ok()?;
        let source: InputBinding = serde_json::from_value(data["appViewBinding"].clone()).ok()?;
        let arguments = data["call"]["arguments"].as_object()?;
        if intent.version != 1
            || !intent.before.valid()
            || intent.client_sequence > LIMIT as u64
            || arguments.len() != 1
            || arguments.get("target") != Some(&serde_json::to_value(intent.target).ok()?)
            || self.inputs.get(&intent.input_id) != Some(&source)
            || app_operation_id(&source.client_id, session, intent.client_sequence).ok()?
                != intent.operation_id
            || self
                .started
                .values()
                .any(|previous| previous.intent.operation_id == intent.operation_id)
        {
            return None;
        }
        let mut contributions = self
            .applied
            .iter()
            .filter(|((applied_turn, _), applied_seq)| applied_turn == turn && **applied_seq < seq)
            .map(|((_, input), applied_seq)| {
                Some(Contribution {
                    input_id: input.clone(),
                    applied_seq: *applied_seq,
                    binding: self.inputs.get(input)?.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        contributions.sort_by_key(|input| input.applied_seq);
        // 最新实际贡献输入必须已出现；后到 steer 不会改写旧 Proposal。
        if contributions.last()?.input_id != intent.input_id {
            return None;
        }
        let proposal = Proposal {
            session_id: session.into(),
            turn_id: turn.into(),
            invocation_id: invocation.into(),
            started_seq: seq,
            intent,
            source,
            contributions,
            epoch: self.epoch,
        };
        self.started.insert(invocation.into(), proposal.clone());
        self.pending.push_back(proposal.clone());
        Some(proposal)
    }
    pub fn proposal(&self, invocation: &str) -> Option<&Proposal> {
        self.started.get(invocation)
    }
    pub fn take_pending(&mut self) -> Vec<Proposal> {
        self.pending.drain(..).collect()
    }
    pub fn current(&self, proposal: &Proposal) -> bool {
        proposal.epoch == self.epoch && self.started.get(&proposal.invocation_id) == Some(proposal)
    }
    pub fn fence(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending.clear();
    }
}

/// 当前 turn 的来源链，只能由原生已确认的导航采纳推进。
#[derive(Clone)]
pub(crate) struct SourceChain {
    current: AppViewBinding,
    inputs: BTreeMap<String, (u64, InputBinding)>,
    last_started_seq: u64,
    pending_operation: Option<String>,
}
impl SourceChain {
    pub fn new(proposal: &Proposal) -> Option<Self> {
        Some(Self {
            current: proposal.contributions.first()?.binding.page(),
            inputs: BTreeMap::new(),
            last_started_seq: 0,
            pending_operation: None,
        })
    }
    pub fn accept(&mut self, proposal: &Proposal, actual: &AppViewBinding) -> bool {
        let mut next = self.clone();
        if next.pending_operation.is_some()
            || proposal.started_seq <= next.last_started_seq
            || &next.current != actual
            || next.current != proposal.intent.before
        {
            return false;
        }
        for input in &proposal.contributions {
            if let Some((seq, binding)) = next.inputs.get(&input.input_id) {
                if *seq != input.applied_seq || binding != &input.binding {
                    return false;
                }
            } else {
                if input.applied_seq <= next.last_started_seq
                    || input.binding.page() != next.current
                {
                    return false;
                }
                next.inputs.insert(
                    input.input_id.clone(),
                    (input.applied_seq, input.binding.clone()),
                );
            }
        }
        next.last_started_seq = proposal.started_seq;
        next.pending_operation = Some(proposal.intent.operation_id.clone());
        *self = next;
        true
    }
    pub fn adopted(&mut self, operation: &str, page: &AppViewBinding) -> bool {
        if self.pending_operation.as_deref() != Some(operation) || !page.valid() {
            return false;
        }
        self.current = page.clone();
        self.pending_operation = None;
        true
    }
    pub fn cancelled(&mut self, operation: &str) {
        if self.pending_operation.as_deref() == Some(operation) {
            self.pending_operation = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(page: AppPageId, epoch: u64) -> InputBinding {
        InputBinding::new(
            &AppIdentity {
                instance_id: "instance".into(),
                account_scope: "a".repeat(64),
                app_session_id: "app-session".into(),
            },
            "seecut-assistant",
            &AppViewBinding {
                page_id: page,
                page_epoch: epoch,
                page_state_id: format!("app-page-v1-{}", "b".repeat(64)),
            },
        )
    }
    fn started(input: &str, source: &InputBinding, sequence: u64) -> Value {
        json!({
            "invocationId":format!("invocation-{sequence}"),
            "call":{"name":"app_navigate","arguments":{"target":"settings"}},
            "appViewBinding":source,
            "appControlIntent":{"version":1,"inputId":input,
                "operationId":app_operation_id(&source.client_id,"session",sequence).unwrap(),
                "clientSequence":sequence,"before":source.page(),"target":"settings"}
        })
    }
    #[test]
    fn full_tracker_accepts_identical_input_without_replacing_original() {
        let source = binding(AppPageId::Generation, 1);
        let mut tracker = Tracker::default();
        for index in 0..LIMIT {
            assert!(tracker.remember(&format!("input-{index}"), source.clone()));
        }
        assert!(tracker.remember("input-0", source.clone()));
        assert!(!tracker.remember("input-new", source));
        assert!(!tracker.remember("input-0", binding(AppPageId::Settings, 2)));
    }
    #[test]
    fn only_frozen_human_input_and_prior_applied_can_authorize_started() {
        let source = binding(AppPageId::Generation, 1);
        let mut tracker = Tracker::default();
        assert!(
            tracker
                .started("session", "turn", 3, &started("input", &source, 1))
                .is_none()
        );
        assert!(tracker.remember("input", source.clone()));
        assert!(!tracker.applied("turn", "foreign", 2));
        assert!(tracker.applied("turn", "input", 2));
        assert!(
            tracker
                .started("session", "turn", 2, &started("input", &source, 1))
                .is_none()
        );
        let proposal = tracker
            .started("session", "turn", 3, &started("input", &source, 1))
            .unwrap();
        assert!(tracker.current(&proposal));
        assert!(
            tracker
                .started("session", "turn", 4, &started("input", &source, 1))
                .is_none()
        );
        tracker.fence();
        assert!(!tracker.current(&proposal));
        assert!(tracker.take_pending().is_empty());
    }
    #[test]
    fn multiple_applied_inputs_keep_original_sources_and_old_started_immutable() {
        let source = binding(AppPageId::Generation, 1);
        let mut tracker = Tracker::default();
        for id in ["first", "steer"] {
            assert!(tracker.remember(id, source.clone()));
        }
        assert!(tracker.applied("turn", "first", 2));
        let first = tracker
            .started("session", "turn", 3, &started("first", &source, 1))
            .unwrap();
        assert!(tracker.applied("turn", "steer", 4));
        assert!(
            tracker
                .started("session", "turn", 5, &started("first", &source, 2))
                .is_none()
        );
        let second = tracker
            .started("session", "turn", 5, &started("steer", &source, 2))
            .unwrap();
        assert_eq!(first.contributions.len(), 1);
        assert_eq!(second.contributions.len(), 2);
        assert!(tracker.current(&first));
        assert!(!tracker.applied("other-turn", "steer", 6));
    }
    #[test]
    fn started_rejects_changed_source_target_identity_and_unknown_fields() {
        let source = binding(AppPageId::Generation, 1);
        for mode in 0..5 {
            let mut tracker = Tracker::default();
            tracker.remember("input", source.clone());
            tracker.applied("turn", "input", 2);
            let mut event = started("input", &source, 1);
            match mode {
                0 => event["appViewBinding"]["pageEpoch"] = json!(2),
                1 => event["call"]["arguments"]["target"] = json!("account"),
                2 => event["appControlIntent"]["operationId"] = json!("foreign"),
                3 => event["appControlIntent"]["extra"] = json!(true),
                _ => event["call"]["arguments"]["before"] = json!({}),
            }
            assert!(tracker.started("session", "turn", 3, &event).is_none());
        }
    }
    #[test]
    fn native_chain_requires_exact_source_and_own_confirmed_navigation() {
        let source = binding(AppPageId::Generation, 1);
        let mut tracker = Tracker::default();
        tracker.remember("input", source.clone());
        tracker.applied("turn", "input", 2);
        let first = tracker
            .started("session", "turn", 3, &started("input", &source, 1))
            .unwrap();
        let mut chain = SourceChain::new(&first).unwrap();
        assert!(!chain.accept(&first, &binding(AppPageId::Generation, 3).page()));
        assert!(chain.accept(&first, &source.page()));
        let target = binding(AppPageId::Settings, 4);
        assert!(!chain.adopted("other-operation", &target.page()));
        assert!(chain.adopted(&first.intent.operation_id, &target.page()));
        let mut next_event = started("input", &source, 2);
        next_event["appControlIntent"]["before"] = json!(target.page());
        let next = tracker.started("session", "turn", 5, &next_event).unwrap();
        assert!(chain.accept(&next, &target.page()));
        assert!(!chain.accept(&next, &target.page()));
    }
    #[test]
    fn new_steer_must_match_the_native_verified_baseline() {
        let source = binding(AppPageId::Generation, 1);
        let mut tracker = Tracker::default();
        tracker.remember("input", source.clone());
        tracker.applied("turn", "input", 2);
        let first = tracker
            .started("session", "turn", 3, &started("input", &source, 1))
            .unwrap();
        let mut chain = SourceChain::new(&first).unwrap();
        assert!(chain.accept(&first, &source.page()));
        let target = binding(AppPageId::Settings, 4);
        assert!(chain.adopted(&first.intent.operation_id, &target.page()));
        tracker.remember("steer", source.clone());
        tracker.applied("turn", "steer", 4);
        let mut event = started("steer", &source, 2);
        event["appControlIntent"]["before"] = json!(target.page());
        let next = tracker.started("session", "turn", 5, &event).unwrap();
        assert!(!chain.accept(&next, &target.page()));
    }
}
