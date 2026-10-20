// SPDX-License-Identifier: AGPL-3.0-or-later
//! 只在原生宿主内建立的单次导航许可；没有 JSON 授权入口或磁盘访问。
use crate::agent_app_control_core::{Action, Authority, Error, Intent};
use crate::agent_app_control_input::{Proposal, SourceChain};
use concat_editor_mcp::AppViewBinding;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const MAX_PERMITS: usize = 128;
const PERMIT_LIFETIME: Duration = Duration::from_secs(5);

/// 私有的宿主能力。背景任务只能携带它，不能从模型参数构造它。
pub(crate) struct NativePermit {
    generation: u64,
    proposal: Proposal,
    intent: Intent,
    deadline: Instant,
    live: Arc<AtomicBool>,
}
impl NativePermit {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn intent(&self) -> &Intent {
        &self.intent
    }
    pub fn live(&self, now: Instant) -> bool {
        now < self.deadline && self.live.load(Ordering::Acquire)
    }
}
struct Entry {
    permit: Arc<NativePermit>,
    consumed: bool,
    dispatched: bool,
}

/// 与一次真实 control grant 同寿命。token 只由独立 Registry::Grants 管理。
pub(crate) struct PermitBook {
    generation: u64,
    authority: Authority,
    session_id: String,
    chains: BTreeMap<String, SourceChain>,
    entries: BTreeMap<String, Entry>,
}
impl PermitBook {
    pub fn new(generation: u64, authority: Authority, session_id: String) -> Result<Self, Error> {
        // 用同一闭合 Intent 校验可信范围与原始 COTO session。
        if generation == 0
            || !authority.valid()
            || crate::agent_app_control_core::operation_id(&authority.client_id, &session_id, 1)
                .is_err()
        {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            generation,
            authority,
            session_id,
            chains: BTreeMap::new(),
            entries: BTreeMap::new(),
        })
    }
    fn scope_matches(&self, binding: &crate::agent_app_control_input::InputBinding) -> bool {
        binding.valid()
            && binding.app_instance_id == self.authority.instance_id
            && binding.client_id == self.authority.client_id
            && binding.account_scope == self.authority.account_scope
            && binding.app_session_id == self.authority.app_session_id
    }
    /// 只能在 Controller 验证真实事件后调用；再核对全来源链与当前原生页面。
    pub fn offer(
        &mut self,
        proposal: Proposal,
        actual: &AppViewBinding,
        now: Instant,
    ) -> Result<(), Error> {
        self.expire(now);
        if proposal.session_id != self.session_id
            || proposal.intent.version != 1
            || !self.scope_matches(&proposal.source)
            || proposal.started_seq == 0
            || proposal.contributions.is_empty()
            || proposal.contributions.len() > MAX_PERMITS
            || proposal.intent.client_sequence > MAX_PERMITS as u64
            || proposal.contributions.last().is_none_or(|input| {
                input.input_id != proposal.intent.input_id || input.binding != proposal.source
            })
        {
            return Err(Error::NotAuthorized);
        }
        let mut seen = BTreeSet::new();
        let mut previous = 0;
        for input in &proposal.contributions {
            if !self.scope_matches(&input.binding)
                || input.applied_seq <= previous
                || input.applied_seq >= proposal.started_seq
                || !seen.insert(input.input_id.as_str())
            {
                return Err(Error::NotAuthorized);
            }
            previous = input.applied_seq;
        }
        let intent = Intent {
            operation_id: proposal.intent.operation_id.clone(),
            session_id: self.session_id.clone(),
            client_sequence: proposal.intent.client_sequence,
            authority: self.authority.clone(),
            before: proposal.intent.before.clone(),
            target: Action::Navigate {
                target: proposal.intent.target,
            },
        };
        intent.validate()?;
        if self.entries.contains_key(&intent.operation_id) {
            return Err(Error::Conflict);
        }
        if self.entries.len() >= MAX_PERMITS || self.chains.len() >= MAX_PERMITS {
            return Err(Error::ResourceLimit);
        }
        let chain = self
            .chains
            .entry(proposal.turn_id.clone())
            .or_insert_with(|| SourceChain::new(&proposal).expect("nonempty validated inputs"));
        if !chain.accept(&proposal, actual) {
            return Err(Error::StalePage);
        }
        self.entries.insert(
            intent.operation_id.clone(),
            Entry {
                permit: Arc::new(NativePermit {
                    generation: self.generation,
                    proposal,
                    intent,
                    deadline: now + PERMIT_LIFETIME,
                    live: Arc::new(AtomicBool::new(true)),
                }),
                consumed: false,
                dispatched: false,
            },
        );
        Ok(())
    }
    pub fn offered_intent(&self, operation: &str, now: Instant) -> Option<&Intent> {
        self.entries
            .get(operation)
            .filter(|entry| !entry.consumed && entry.permit.live(now))
            .map(|entry| &entry.permit.intent)
    }
    pub fn retained(&self, operation: &str) -> bool {
        self.entries.contains_key(operation)
    }
    /// 原始 raw request 必须逐字段匹配，只有第一次能取走执行能力。
    pub fn take(&mut self, intent: &Intent, now: Instant) -> Result<Arc<NativePermit>, Error> {
        self.expire(now);
        let entry = self
            .entries
            .get_mut(&intent.operation_id)
            .ok_or(Error::NotAuthorized)?;
        if entry.consumed || &entry.permit.intent != intent || !entry.permit.live(now) {
            return Err(Error::NotAuthorized);
        }
        entry.consumed = true;
        Ok(entry.permit.clone())
    }
    pub fn current(&self, permit: &NativePermit, now: Instant) -> bool {
        permit.generation == self.generation
            && permit.live(now)
            && self
                .entries
                .get(&permit.intent.operation_id)
                .is_some_and(|entry| {
                    entry.consumed && Arc::ptr_eq(&entry.permit.live, &permit.live)
                })
    }
    /// 必须与 Registry.commit 位于同一 UI 回调，过期后不能首次派发。
    pub fn dispatched(&mut self, permit: &NativePermit, now: Instant) -> bool {
        if !self.current(permit, now) {
            return false;
        }
        let entry = self
            .entries
            .get_mut(&permit.intent.operation_id)
            .expect("current permit");
        if entry.dispatched {
            return false;
        }
        entry.dispatched = true;
        true
    }
    /// 只供原 NavID 采纳且 durable Saved ACK 后调用；保存延迟不会删除归属事实。
    pub fn adopted(&mut self, permit: &NativePermit, page: &AppViewBinding) -> bool {
        if permit.generation != self.generation || !permit.live.load(Ordering::Acquire) {
            return false;
        }
        let Action::Navigate { target } = permit.intent.target;
        if page.page_id != target {
            return false;
        }
        let operation = &permit.intent.operation_id;
        let accepted = self.entries.get(operation).is_some_and(|entry| {
            entry.consumed && entry.dispatched && Arc::ptr_eq(&entry.permit.live, &permit.live)
        }) && self
            .chains
            .get_mut(&permit.proposal.turn_id)
            .is_some_and(|chain| chain.adopted(operation, page));
        if accepted {
            self.entries.remove(operation);
            permit.live.store(false, Ordering::Release);
        }
        accepted
    }
    pub fn cancel(&mut self, operation: &str) {
        if let Some(entry) = self.entries.remove(operation) {
            entry.permit.live.store(false, Ordering::Release);
            if let Some(chain) = self.chains.get_mut(&entry.permit.proposal.turn_id) {
                chain.cancelled(operation);
            }
        }
    }
    pub fn expire(&mut self, now: Instant) {
        let expired = self
            .entries
            .iter()
            .filter(|(_, entry)| !entry.dispatched && !entry.permit.live(now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            self.cancel(&id);
        }
    }
    /// Stop、新人类输入、会话变化同步撤掉旧许可；不会产生后续执行。
    pub fn fence(&mut self) {
        for entry in self.entries.values() {
            entry.permit.live.store(false, Ordering::Release);
        }
        self.entries.clear();
        self.chains.clear();
    }
}
impl Drop for PermitBook {
    fn drop(&mut self) {
        self.fence();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_app_control_input::{InputBinding, Tracker};
    use crate::agent_identity::AppIdentity;
    use concat_editor_mcp::{AppPageId, app_operation_id};
    use serde_json::json;
    fn fixture() -> (PermitBook, Proposal, Instant) {
        let identity = AppIdentity {
            instance_id: "instance".into(),
            account_scope: "a".repeat(64),
            app_session_id: "app-session".into(),
        };
        let source = InputBinding::new(
            &identity,
            "client",
            &AppViewBinding {
                page_id: AppPageId::Generation,
                page_epoch: 1,
                page_state_id: format!("app-page-v1-{}", "b".repeat(64)),
            },
        );
        let authority = Authority {
            instance_id: identity.instance_id,
            client_id: "client".into(),
            account_scope: identity.account_scope,
            account_epoch: 1,
            app_session_id: identity.app_session_id,
        };
        let mut tracker = Tracker::default();
        tracker.remember("input", source.clone());
        tracker.applied("turn", "input", 1);
        let proposal=tracker.started("session","turn",2,&json!({"invocationId":"invoke","call":{"name":"app_navigate","arguments":{"target":"settings"}},"appViewBinding":source,"appControlIntent":{"version":1,"inputId":"input","operationId":app_operation_id("client","session",1).unwrap(),"clientSequence":1,"before":source.page(),"target":"settings"}})).unwrap();
        (
            PermitBook::new(1, authority, "session".into()).unwrap(),
            proposal,
            Instant::now(),
        )
    }
    #[test]
    fn exact_native_source_and_one_consumption_are_required() {
        let (mut book, p, now) = fixture();
        let before = p.intent.before.clone();
        book.offer(p.clone(), &before, now).unwrap();
        let intent = book.entries[&p.intent.operation_id].permit.intent.clone();
        let mut wrong = intent.clone();
        wrong.target = Action::Navigate {
            target: AppPageId::Account,
        };
        assert!(book.take(&wrong, now).is_err());
        let permit = book.take(&intent, now).unwrap();
        assert!(book.current(&permit, now));
        assert!(book.take(&intent, now).is_err());
        book.fence();
        assert!(!book.current(&permit, now));
    }
    #[test]
    fn expiry_and_drop_revoke_already_carried_capabilities() {
        let (mut book, p, now) = fixture();
        book.offer(p.clone(), &p.intent.before, now).unwrap();
        let intent = book.entries[&p.intent.operation_id].permit.intent.clone();
        let permit = book.take(&intent, now).unwrap();
        assert!(!book.current(&permit, now + PERMIT_LIFETIME));
        drop(book);
        assert!(!permit.live(now));
    }
    #[test]
    fn changed_native_page_or_foreign_contributing_scope_is_refused() {
        for mode in 0..3 {
            let (mut book, mut p, now) = fixture();
            let mut actual = p.intent.before.clone();
            match mode {
                0 => actual.page_epoch += 2,
                1 => p.contributions[0].binding.account_scope = "c".repeat(64),
                _ => p.session_id = "foreign".into(),
            };
            assert!(book.offer(p, &actual, now).is_err());
        }
    }
    #[test]
    fn late_durable_ack_retains_dispatched_fact_but_expired_first_dispatch_is_refused() {
        let (mut book, p, now) = fixture();
        book.offer(p.clone(), &p.intent.before, now).unwrap();
        let intent = book.entries[&p.intent.operation_id].permit.intent.clone();
        let permit = book.take(&intent, now).unwrap();
        assert!(book.dispatched(&permit, now));
        book.expire(now + PERMIT_LIFETIME + Duration::from_secs(30));
        let mut after = p.intent.before.clone();
        after.page_id = AppPageId::Settings;
        after.page_epoch += 1;
        assert!(book.adopted(&permit, &after));
        let (mut book, p, now) = fixture();
        book.offer(p.clone(), &p.intent.before, now).unwrap();
        let intent = book.entries[&p.intent.operation_id].permit.intent.clone();
        let permit = book.take(&intent, now).unwrap();
        assert!(!book.dispatched(&permit, now + PERMIT_LIFETIME));
        book.expire(now + PERMIT_LIFETIME);
        assert!(!book.adopted(&permit, &after));
    }
    #[test]
    fn original_unconsumed_permit_cannot_advance_native_source_chain() {
        let (mut book, p, now) = fixture();
        book.offer(p.clone(), &p.intent.before, now).unwrap();
        let permit = book.entries[&p.intent.operation_id].permit.clone();
        let mut after = p.intent.before.clone();
        after.page_id = AppPageId::Settings;
        after.page_epoch += 1;
        assert!(!book.adopted(&permit, &after));
        let intent = permit.intent.clone();
        let permit = book.take(&intent, now).unwrap();
        assert!(!book.adopted(&permit, &after));
        assert!(book.dispatched(&permit, now));
        let mut foreign = after.clone();
        foreign.page_id = AppPageId::Account;
        assert!(!book.adopted(&permit, &foreign));
        assert!(book.adopted(&permit, &after));
        assert!(!book.adopted(&permit, &after));
    }
}
