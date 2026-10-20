//! Navigation-only AppControl business core. No UI, worker wiring, Task or Doc grant.
use concat_editor_mcp::{AppPageId, AppViewBinding};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_OPERATIONS: usize = 128;
pub const MAX_RECORD_BYTES: usize = 16_384;
pub const MAX_SESSION_BYTES: usize = 2_097_152;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidInput,
    NotAuthorized,
    Conflict,
    StalePage,
    Cancelled,
    ResourceLimit,
    Unknown,
    Io,
    Corrupt,
    Busy,
    QueryOnly,
}

fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Host-owned authority; no model/IPC value alone establishes this identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Authority {
    pub instance_id: String,
    pub client_id: String,
    pub account_scope: String,
    pub account_epoch: u64,
    pub app_session_id: String,
}
impl Authority {
    pub fn valid(&self) -> bool {
        bounded(&self.instance_id, 64)
            && bounded(&self.client_id, 64)
            && hash64(&self.account_scope)
            && self.account_epoch > 0
            && self.account_epoch <= SAFE_INTEGER
            && bounded(&self.app_session_id, 64)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum Action {
    Navigate { target: AppPageId },
}

/// Canonical identity uses UTF-8 byte lengths, exactly once, without normalization.
pub fn operation_id(client: &str, session: &str, sequence: u64) -> Result<String, Error> {
    if !bounded(client, 64) || !bounded(session, 64) || !(1..=SAFE_INTEGER).contains(&sequence) {
        return Err(Error::InvalidInput);
    }
    Ok(format!(
        "sca1:1:{}:{client}:{}:{session}:{sequence}",
        client.len(),
        session.len()
    ))
}

/// Immutable request: original COTO session is not the host AppSession.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Intent {
    pub operation_id: String,
    pub session_id: String,
    pub client_sequence: u64,
    pub authority: Authority,
    pub before: AppViewBinding,
    pub target: Action,
}
impl Intent {
    pub fn validate(&self) -> Result<(), Error> {
        if !self.authority.valid()
            || !self.before.valid()
            || operation_id(
                &self.authority.client_id,
                &self.session_id,
                self.client_sequence,
            )? != self.operation_id
        {
            return Err(Error::InvalidInput);
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String, Error> {
        self.validate()?;
        // Closed typed structs serialize in a fixed field order; no arbitrary JSON.
        Ok(digest(
            &serde_json::to_vec(self).map_err(|_| Error::InvalidInput)?,
        ))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Queued,
    Running,
    Completed,
    Cancelled,
    OutcomeUnknown,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HumanGate {
    DocumentLeaveConfirmation,
}

/// Trusted navigation adoption fact supplied by the synchronous native adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Completion {
    Navigation { page: AppViewBinding },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    pub intent: Intent,
    pub intent_sha256: String,
    pub revision: u64,
    pub status: Status,
    pub human_gate: Option<HumanGate>,
    pub completion: Option<Completion>,
}
impl Record {
    pub fn queued(intent: Intent) -> Result<Self, Error> {
        let sha = intent.fingerprint()?;
        Ok(Self {
            version: 1,
            intent,
            intent_sha256: sha,
            revision: 1,
            status: Status::Queued,
            human_gate: None,
            completion: None,
        })
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || !(1..=SAFE_INTEGER).contains(&self.revision)
            || self.intent.fingerprint()? != self.intent_sha256
        {
            return Err(Error::Corrupt);
        }
        if self.status == Status::Completed {
            match (&self.intent.target, &self.completion) {
                (Action::Navigate { target }, Some(Completion::Navigation { page }))
                    if page.valid() && page.page_id == *target => {}
                _ => return Err(Error::Corrupt),
            }
        } else if self.completion.is_some() {
            return Err(Error::Corrupt);
        }
        if self.human_gate.is_some() && self.status != Status::Running {
            return Err(Error::Corrupt);
        }
        if let Some(gate) = self.human_gate
            && !matches!(
                (&self.intent.target, gate),
                (
                    Action::Navigate { .. },
                    HumanGate::DocumentLeaveConfirmation
                )
            )
        {
            return Err(Error::Corrupt);
        }
        if self.status == Status::Queued && self.revision != 1 {
            return Err(Error::Corrupt);
        }
        if self.status != Status::Queued && self.revision < 2 {
            return Err(Error::Corrupt);
        }
        if serde_json::to_vec(self).map_err(|_| Error::Corrupt)?.len() > MAX_RECORD_BYTES {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }
    fn advance(&mut self, status: Status) -> Result<(), Error> {
        self.revision = self
            .revision
            .checked_add(1)
            .filter(|n| *n <= SAFE_INTEGER)
            .ok_or(Error::ResourceLimit)?;
        self.status = status;
        self.human_gate = None;
        self.completion = None;
        Ok(())
    }
}

/// Tokens remain in memory and are independent of AppRead and Doc write tokens.
#[derive(Default)]
pub struct Grants {
    grants: HashMap<String, (Authority, String)>,
}
impl Grants {
    pub fn grant(&mut self, authority: Authority) -> Result<String, Error> {
        if !authority.valid() {
            return Err(Error::InvalidInput);
        }
        if self.grants.len() >= 32 && !self.grants.contains_key(&authority.client_id) {
            return Err(Error::ResourceLimit);
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.grants
            .insert(authority.client_id.clone(), (authority, token.clone()));
        Ok(token)
    }
    pub fn revoke(&mut self, client: &str) {
        self.grants.remove(client);
    }
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.grants.clear();
    }
    pub fn authorized(&self, authority: &Authority, token: &str) -> bool {
        self.grants
            .get(&authority.client_id)
            .is_some_and(|(a, t)| a == authority && t == token)
    }
}

/// Exact human-approved action, frozen before-page and target, in-memory only.
struct Permit {
    authority: Authority,
    token: String,
    digest: String,
}
struct Entry {
    owner: crate::agent_app_control_journal::LeaseOwner,
    record: Record,
    permit: Option<Permit>,
    ticket: Option<String>,
    dispatched: bool,
    durable: bool,
}
/// Cannot be created by model JSON. Worker persists its exact record before dispatch.
pub struct RunningWrite {
    pub(crate) owner: crate::agent_app_control_journal::LeaseOwner,
    pub(crate) record: Record,
    pub(crate) ticket: String,
}
/// Constructed only by Journal after a successful durable running write.
pub struct RunningProof {
    pub(crate) owner: crate::agent_app_control_journal::LeaseOwner,
    pub(crate) record: Record,
    pub(crate) ticket: String,
}
/// Actual business adapter receives this once; paths are never model parameters.
pub struct HostDispatch {
    pub intent: Intent,
    pub correlation_id: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    pub record: Record,
    pub effective_status: Status,
    pub durable: bool,
    pub execution_recoverable: bool,
}

/// UI-thread state only. Does not perform I/O or call a business callback.
#[derive(Default)]
pub struct Registry {
    entries: HashMap<String, Entry>,
    bound: Option<(String, String, String)>,
    pub grants: Grants,
}
impl Registry {
    #[cfg(test)]
    pub(crate) fn entries_dispatched_for_test(&self, id: &str) -> bool {
        self.entries.get(id).unwrap().dispatched
    }
    /// Fresh Journal::prepare result only; restored records use query-only APIs.
    pub fn accept(
        &mut self,
        prepared: crate::agent_app_control_journal::Prepared,
    ) -> Result<Reply, Error> {
        let (record, owner) = prepared.into_fresh()?;
        let scope = (
            record.intent.authority.account_scope.clone(),
            record.intent.authority.client_id.clone(),
            record.intent.session_id.clone(),
        );
        if self.bound.as_ref().is_some_and(|old| old != &scope) {
            return Err(Error::NotAuthorized);
        }
        let key = record.intent.operation_id.clone();
        if let Some(e) = self.entries.get(&key) {
            if e.record.intent_sha256 != record.intent_sha256 {
                return Err(Error::Conflict);
            }
            return self.query(&key);
        }
        if self.entries.len() >= MAX_OPERATIONS {
            return Err(Error::ResourceLimit);
        }
        self.bound = Some(scope);
        self.entries.insert(
            key.clone(),
            Entry {
                owner,
                record,
                permit: None,
                ticket: None,
                dispatched: false,
                durable: true,
            },
        );
        self.query(&key)
    }
    pub fn query(&self, id: &str) -> Result<Reply, Error> {
        let e = self.entries.get(id).ok_or(Error::InvalidInput)?;
        let effective_status = if (!e.durable
            && matches!(
                e.record.status,
                Status::Running | Status::Completed | Status::OutcomeUnknown
            ))
            || (e.record.status == Status::Running && e.owner.check_execution().is_err())
        {
            Status::OutcomeUnknown
        } else {
            e.record.status
        };
        Ok(Reply {
            record: e.record.clone(),
            effective_status,
            durable: e.durable,
            execution_recoverable: false,
        })
    }
    /// Host approval boundary; must not be exposed as a model tool.
    pub fn approve(
        &mut self,
        id: &str,
        authority: &Authority,
        token: &str,
        before: &AppViewBinding,
    ) -> Result<(), Error> {
        if !self.grants.authorized(authority, token) {
            return Err(Error::NotAuthorized);
        }
        let e = self.entries.get_mut(id).ok_or(Error::InvalidInput)?;
        e.owner.check_execution()?;
        if e.record.status != Status::Queued {
            return Err(Error::Conflict);
        }
        if &e.record.intent.authority != authority || &e.record.intent.before != before {
            return Err(Error::StalePage);
        }
        e.permit = Some(Permit {
            authority: authority.clone(),
            token: token.into(),
            digest: e.record.intent_sha256.clone(),
        });
        Ok(())
    }
    /// Produces worker work, never a UI callback. The exact token is not persisted.
    pub fn stage_running(&mut self, id: &str) -> Result<RunningWrite, Error> {
        let e = self.entries.get_mut(id).ok_or(Error::InvalidInput)?;
        e.owner.check_execution()?;
        if e.record.status != Status::Queued || e.ticket.is_some() {
            return Err(Error::Conflict);
        }
        let p = e.permit.as_ref().ok_or(Error::NotAuthorized)?;
        if !self.grants.authorized(&p.authority, &p.token) {
            return Err(Error::NotAuthorized);
        }
        let ticket = uuid::Uuid::new_v4().to_string();
        e.ticket = Some(ticket.clone());
        let mut record = e.record.clone();
        record.advance(Status::Running)?;
        e.record = record.clone();
        e.durable = false;
        Ok(RunningWrite {
            record,
            ticket,
            owner: e.owner.clone(),
        })
    }
    /// Final synchronous UI check immediately before the original navigation wrapper.
    pub fn commit(
        &mut self,
        proof: RunningProof,
        current: &Authority,
        token: &str,
        before: &AppViewBinding,
    ) -> Result<HostDispatch, Error> {
        let e = self
            .entries
            .get_mut(&proof.record.intent.operation_id)
            .ok_or(Error::InvalidInput)?;
        if e.record.status == Status::Cancelled {
            return Err(Error::Cancelled);
        }
        e.owner.same_writer(&proof.owner)?;
        e.owner.check_execution()?;
        if e.record.status != Status::Running
            || e.dispatched
            || e.ticket.as_ref() != Some(&proof.ticket)
            || e.record.intent_sha256 != proof.record.intent_sha256
            || e.record != proof.record
        {
            return Err(Error::Conflict);
        }
        let p = e.permit.as_ref().ok_or(Error::NotAuthorized)?;
        if !self.grants.authorized(current, token)
            || &p.authority != current
            || p.token != token
            || p.digest != proof.record.intent_sha256
            || &e.record.intent.authority != current
        {
            return Err(Error::NotAuthorized);
        }
        if &e.record.intent.before != before {
            return Err(Error::StalePage);
        }
        e.record = proof.record;
        e.dispatched = true;
        e.ticket = None;
        e.permit = None;
        e.durable = true;
        Ok(HostDispatch {
            intent: e.record.intent.clone(),
            correlation_id: e.record.intent.operation_id.clone(),
        })
    }
    /// Cancel/revoke fences dispatch first. Running may already have affected UI.
    pub fn cancel(&mut self, id: &str) -> Result<Record, Error> {
        let e = self.entries.get_mut(id).ok_or(Error::InvalidInput)?;
        if matches!(
            e.record.status,
            Status::Completed | Status::Cancelled | Status::OutcomeUnknown
        ) {
            return Ok(e.record.clone());
        }
        let status = if e.dispatched {
            Status::OutcomeUnknown
        } else {
            Status::Cancelled
        };
        e.record.advance(status)?;
        e.permit = None;
        e.ticket = None;
        e.durable = false;
        Ok(e.record.clone())
    }
    pub fn revoke(&mut self, client: &str) -> Result<Vec<Record>, Error> {
        self.grants.revoke(client);
        for e in self
            .entries
            .values()
            .filter(|e| e.record.intent.authority.client_id == client)
        {
            e.owner.fence_execution();
        }
        let ids = self
            .entries
            .iter()
            .filter(|(_, e)| e.record.intent.authority.client_id == client)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.iter().map(|id| self.cancel(id)).collect()
    }
    /// Actual account/app lifetime invalidation fences all clients before worker I/O.
    #[cfg(test)]
    pub fn invalidate_all(&mut self) -> Result<Vec<Record>, Error> {
        self.grants.clear();
        for e in self.entries.values() {
            e.owner.fence_execution();
        }
        let ids = self.entries.keys().cloned().collect::<Vec<_>>();
        ids.iter().map(|id| self.cancel(id)).collect()
    }
    /// Host completion stays factual even after cancel. No implicit redispatch.
    pub fn complete(&mut self, id: &str, completion: Completion) -> Result<Record, Error> {
        let e = self.entries.get_mut(id).ok_or(Error::InvalidInput)?;
        if e.record.status == Status::Completed {
            return if e.record.completion.as_ref() == Some(&completion) {
                Ok(e.record.clone())
            } else {
                Err(Error::Conflict)
            };
        }
        if !e.dispatched || !matches!(e.record.status, Status::Running | Status::OutcomeUnknown) {
            return Err(Error::Conflict);
        }
        let mut next = e.record.clone();
        next.advance(Status::Completed)?;
        next.completion = Some(completion);
        next.validate()?;
        e.record = next;
        e.durable = false;
        Ok(e.record.clone())
    }
    #[cfg(test)]
    pub fn needs_human(&mut self, id: &str, gate: HumanGate) -> Result<Record, Error> {
        let e = self.entries.get_mut(id).ok_or(Error::InvalidInput)?;
        if !e.dispatched || e.record.status != Status::Running {
            return Err(Error::Conflict);
        }
        if !matches!(
            (&e.record.intent.target, gate),
            (
                Action::Navigate { .. },
                HumanGate::DocumentLeaveConfirmation
            )
        ) {
            return Err(Error::InvalidInput);
        }
        e.record.advance(Status::Running)?;
        e.record.human_gate = Some(gate);
        e.durable = false;
        Ok(e.record.clone())
    }
    /// Call only with Journal::save's durable acknowledgment, never on failed I/O.
    pub fn acknowledge(
        &mut self,
        saved: crate::agent_app_control_journal::Saved,
    ) -> Result<Reply, Error> {
        let (record, owner) = saved.into_record()?;
        let e = self
            .entries
            .get_mut(&record.intent.operation_id)
            .ok_or(Error::InvalidInput)?;
        e.owner.same_writer(&owner)?;
        if e.record != record {
            return Err(Error::Conflict);
        }
        e.durable = true;
        self.query(&record.intent.operation_id)
    }
}

#[cfg(test)]
#[path = "agent_app_control_core_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_app_control_recovery_tests.rs"]
mod recovery_tests;
