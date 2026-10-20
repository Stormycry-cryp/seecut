// SPDX-License-Identifier: AGPL-3.0-or-later
//! App 页面操作的 UI 适配层；所有 journal I/O 由单独 worker 持有。
use super::*;
use crate::agent_app_control_core::{
    self as core, Action, Authority, Completion, Intent, Record, Registry, Status,
};
use crate::agent_app_control_host::{NativePermit, PermitBook};
use crate::agent_app_control_input::Proposal;
use crate::agent_app_control_journal::{self as journal, QueryScope, Saved};
use crate::agent_app_control_worker::{self as worker, Handle, WorkerError};
use concat_editor_mcp::{
    AppCurrentObservation, AppNavigationCompletion, AppNavigationHumanGate, AppNavigationReceipt,
    AppOperationDurability, AppOperationOrigin, AppOperationStatus, AppViewBinding,
};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;

struct SaveWork {
    record: Record,
    reply: Option<worker::Reply<Saved>>,
    adoption: Option<(Arc<NativePermit>, crate::cloud::AppNavigationAdoption)>,
}
struct Registration {
    proposal: Proposal,
    reply: worker::Reply<journal::Prepared>,
}
pub(crate) struct ReadyRegistration {
    pub proposal: Proposal,
    generation: u64,
    result: Result<journal::Prepared, WorkerError>,
}
struct Generation {
    authority: Authority,
    session_id: String,
    token: String,
    root: PathBuf,
    registry: Registry,
    permits: PermitBook,
    worker: Option<Arc<Handle>>,
    retired: bool,
    in_flight: usize,
    completed_flights: Arc<AtomicUsize>,
    operations: Vec<String>,
    registrations: Vec<Registration>,
    // A rejected rev2 enqueue must precede any rev3 cancellation in the same journal.
    running_queue: BTreeMap<String, core::RunningWrite>,
    running_replies: Vec<worker::Reply<core::RunningProof>>,
    saves: VecDeque<SaveWork>,
    unadmitted: Vec<journal::Prepared>,
    cancellation_replies: Vec<worker::Reply<journal::RecoveryView>>,
}
impl Generation {
    fn worker(&mut self) -> Result<Arc<Handle>, Response> {
        if let Some(worker) = &self.worker {
            return Ok(worker.clone());
        }
        let (worker, _opened) = worker::spawn(
            self.root.clone(),
            QueryScope {
                account_scope: self.authority.account_scope.clone(),
                client_id: self.authority.client_id.clone(),
                session_id: self.session_id.clone(),
            },
        )
        .map_err(|_| Response::error("ioFailure"))?;
        let worker = Arc::new(worker);
        self.worker = Some(worker.clone());
        Ok(worker)
    }
    fn queue_save(
        &mut self,
        record: Record,
        adoption: Option<(Arc<NativePermit>, crate::cloud::AppNavigationAdoption)>,
    ) {
        if self.saves.iter().any(|work| work.record == record) {
            return;
        }
        self.saves.push_back(SaveWork {
            record,
            reply: None,
            adoption,
        });
    }
    fn fence(&mut self, revoke: bool) {
        self.permits.fence();
        if revoke {
            self.retired = true;
            self.token.clear();
            if let Some(worker) = &self.worker {
                worker.fence();
            }
            if let Ok(records) = self.registry.revoke(&self.authority.client_id) {
                for record in records {
                    self.queue_save(record, None);
                }
            }
        } else {
            for id in self.operations.clone() {
                if self.registry.query(&id).is_ok_and(|reply| {
                    matches!(reply.record.status, Status::Queued | Status::Running)
                }) && let Ok(record) = self.registry.cancel(&id)
                {
                    self.queue_save(record, None);
                }
            }
        }
        self.drain();
    }
    fn finish_registration(
        &mut self,
        operation: &str,
        result: Result<journal::Prepared, WorkerError>,
        live: bool,
    ) -> bool {
        match result {
            Ok(prepared) if live => {
                if self.registry.accept(prepared).is_ok() {
                    if !self.operations.iter().any(|id| id == operation) {
                        self.operations.push(operation.into());
                    }
                    true
                } else {
                    self.permits.cancel(operation);
                    false
                }
            }
            Ok(prepared) => {
                self.permits.cancel(operation);
                if let Some(worker) = &self.worker {
                    match worker.cancel_prepared(prepared) {
                        Ok(reply) => self.cancellation_replies.push(reply),
                        Err(error) => self.unadmitted.push(error.input),
                    }
                }
                false
            }
            Err(_) => {
                self.permits.cancel(operation);
                false
            }
        }
    }
    fn cancel_operation(&mut self, operation: &str) {
        self.permits.cancel(operation);
        if self
            .registry
            .query(operation)
            .is_ok_and(|view| matches!(view.record.status, Status::Queued | Status::Running))
            && let Ok(record) = self.registry.cancel(operation)
        {
            self.queue_save(record, None);
        }
        self.drain();
    }
    fn commit_navigation(
        &mut self,
        permit: &NativePermit,
        proof: core::RunningProof,
        actual: &Authority,
        before: &AppViewBinding,
        now: Instant,
    ) -> Result<core::HostDispatch, core::Error> {
        if !self.permits.current(permit, now) {
            return Err(core::Error::NotAuthorized);
        }
        let dispatch = self.registry.commit(proof, actual, &self.token, before)?;
        // Same callback and same instant: expiry cannot occur between both owners.
        if !self.permits.dispatched(permit, now) {
            return Err(core::Error::Unknown);
        }
        Ok(dispatch)
    }
    fn finish_navigation(
        &mut self,
        job: &NavigationJob,
        adoption: Option<crate::cloud::AppNavigationAdoption>,
    ) -> Result<Finalized, Response> {
        let Action::Navigate { target } = job.permit.intent().target;
        match adoption {
            Some(adoption) if adoption.matches(&job.permit.intent().operation_id, target) => {
                let record = self
                    .registry
                    .complete(
                        &job.permit.intent().operation_id,
                        Completion::Navigation {
                            page: adoption.page().clone(),
                        },
                    )
                    .map_err(status)?;
                Ok(enqueue_terminal(
                    self,
                    &job.worker,
                    record,
                    &job.permit,
                    Some(adoption),
                ))
            }
            _ => Ok(cancel_admitted(self, job)),
        }
    }
    fn drain(&mut self) {
        self.in_flight = self
            .in_flight
            .saturating_sub(self.completed_flights.swap(0, Ordering::AcqRel));
        self.permits.expire(Instant::now());
        for id in self.operations.clone() {
            if !self.permits.retained(&id)
                && self.registry.query(&id).is_ok_and(|view| {
                    matches!(view.record.status, Status::Queued | Status::Running)
                })
                && let Ok(record) = self.registry.cancel(&id)
            {
                self.queue_save(record, None);
            }
        }
        let Some(worker) = self.worker.clone() else {
            return;
        };
        let unadmitted = std::mem::take(&mut self.unadmitted);
        for prepared in unadmitted {
            match worker.cancel_prepared(prepared) {
                Ok(reply) => self.cancellation_replies.push(reply),
                Err(error) => self.unadmitted.push(error.input),
            }
        }
        self.cancellation_replies
            .retain(|reply| worker::poll_reply(reply).is_none());
        let keys = self.running_queue.keys().cloned().collect::<Vec<_>>();
        for id in keys {
            let write = self
                .running_queue
                .remove(&id)
                .expect("known queued running write");
            match worker.persist_running(write) {
                Ok(reply) => self.running_replies.push(reply),
                Err(error) => {
                    self.running_queue.insert(id, error.input);
                    break;
                }
            }
        }
        self.running_replies
            .retain(|reply| worker::poll_reply(reply).is_none());
        let mut pending = VecDeque::new();
        while let Some(mut work) = self.saves.pop_front() {
            if work.reply.is_none()
                && !self
                    .running_queue
                    .contains_key(&work.record.intent.operation_id)
            {
                match worker.save(work.record.clone()) {
                    Ok(reply) => work.reply = Some(reply),
                    Err(_) => {
                        pending.push_back(work);
                        continue;
                    }
                }
            }
            match work.reply.as_ref().and_then(worker::poll_reply) {
                None => pending.push_back(work),
                Some(Ok(saved)) => {
                    if self.registry.acknowledge(saved).is_ok()
                        && let Some((permit, adoption)) = work.adoption
                    {
                        self.permits.adopted(&permit, adoption.page());
                    }
                }
                Some(Err(_)) => {} // Failed persistence never marks a record durable.
            }
        }
        self.saves = pending;
    }
}

#[derive(Default)]
pub(super) struct AppControlState {
    bound_session: Option<String>,
    current: Option<u64>,
    next_generation: u64,
    generations: BTreeMap<u64, Generation>,
    closing: Vec<worker::Reply<()>>,
}
impl AppControlState {
    fn drain(&mut self) {
        for generation in self.generations.values_mut() {
            generation.drain();
        }
        let retired = self
            .generations
            .iter()
            .filter(|(_, g)| {
                g.retired
                    && g.in_flight == 0
                    && g.registrations.is_empty()
                    && g.saves.is_empty()
                    && g.running_queue.is_empty()
                    && g.running_replies.is_empty()
                    && g.unadmitted.is_empty()
                    && g.cancellation_replies.is_empty()
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in retired {
            if let Some(mut generation) = self.generations.remove(&id)
                && let Some(worker) = generation.worker.take()
            {
                match Arc::try_unwrap(worker) {
                    Ok(worker) => self.closing.push(worker.close()),
                    Err(worker) => {
                        generation.worker = Some(worker);
                        self.generations.insert(id, generation);
                    }
                }
            }
        }
        self.closing
            .retain(|reply| worker::poll_reply(reply).is_none());
    }
    pub fn revoke(&mut self) {
        if let Some(id) = self.current.take()
            && let Some(generation) = self.generations.get_mut(&id)
        {
            generation.fence(true);
        }
        self.drain();
    }
    pub fn fence(&mut self) {
        if let Some(id) = self.current
            && let Some(generation) = self.generations.get_mut(&id)
        {
            generation.fence(false);
        }
    }
}
impl Drop for AppControlState {
    fn drop(&mut self) {
        for generation in self.generations.values_mut() {
            generation.fence(true);
        }
    }
}

fn authority(instance: &str, client: &str) -> Result<Authority, Response> {
    let (account_epoch, identity) =
        crate::cloud::app_read_identity(instance).map_err(|_| Response::error("notAuthorized"))?;
    let result = Authority {
        instance_id: identity.instance_id,
        client_id: client.into(),
        account_scope: identity.account_scope,
        account_epoch,
        app_session_id: identity.app_session_id,
    };
    if !result.valid() {
        return Err(Response::error("notAuthorized"));
    }
    Ok(result)
}
fn supported(studio: &Studio, app: &App) -> bool {
    let ui = app.global::<SeeCut>();
    app_read::current_page(studio, app).is_some()
        && crate::cloud::app_read_gate(studio, app).is_ok()
        && (ui.get_page() != 0 || studio.on_start)
        && (ui.get_page() != 6 || ui.get_canvas_gallery_open())
        && ui.get_page() != 8
}
fn status(error: core::Error) -> Response {
    Response::error(match error {
        core::Error::InvalidInput => "invalidInput",
        core::Error::NotAuthorized => "notAuthorized",
        core::Error::Conflict => "operationConflict",
        core::Error::StalePage => "stalePage",
        core::Error::Cancelled => "cancelled",
        core::Error::ResourceLimit => "resourceLimit",
        core::Error::Busy => "busy",
        core::Error::Io => "ioFailure",
        core::Error::Corrupt => "corruptJournal",
        core::Error::Unknown | core::Error::QueryOnly => "outcomeUnknown",
    })
}
fn worker_error(error: WorkerError) -> Response {
    match error {
        WorkerError::Core(error) => status(error),
        WorkerError::WorkerStopped => Response::error("outcomeUnknown"),
    }
}
fn submit_failure(error: worker::SubmitFailure) -> Response {
    Response::error(match error {
        worker::SubmitFailure::QueueFull => "busy",
        worker::SubmitFailure::WorkerStopped => "outcomeUnknown",
    })
}
fn public_status(value: Status) -> AppOperationStatus {
    match value {
        Status::Queued => AppOperationStatus::Queued,
        Status::Running => AppOperationStatus::Running,
        Status::Completed => AppOperationStatus::Completed,
        Status::Cancelled => AppOperationStatus::Cancelled,
        Status::OutcomeUnknown => AppOperationStatus::OutcomeUnknown,
    }
}
fn receipt(
    record: &Record,
    effective: Status,
    durable: bool,
    observation: Option<AppCurrentObservation>,
) -> Response {
    let Action::Navigate { target } = record.intent.target;
    let value = AppNavigationReceipt {
        app_control_protocol_version: 1,
        operation_id: record.intent.operation_id.clone(),
        session_id: record.intent.session_id.clone(),
        client_sequence: record.intent.client_sequence,
        origin: AppOperationOrigin {
            app_instance_id: record.intent.authority.instance_id.clone(),
            client_id: record.intent.authority.client_id.clone(),
            app_session_id: record.intent.authority.app_session_id.clone(),
        },
        before: record.intent.before.clone(),
        target,
        recorded_status: public_status(record.status),
        status: public_status(effective),
        durability: if durable {
            AppOperationDurability::Confirmed
        } else {
            AppOperationDurability::Unconfirmed
        },
        completion: record
            .completion
            .as_ref()
            .map(|Completion::Navigation { page }| AppNavigationCompletion { page: page.clone() }),
        human_gate: record
            .human_gate
            .map(|_| AppNavigationHumanGate::DocumentLeaveConfirmation),
        current_observation: observation,
    };
    if !value.valid() {
        return Response::error("outcomeUnknown");
    }
    serde_json::to_value(value)
        .map(Response::ok)
        .unwrap_or_else(|_| Response::error("outcomeUnknown"))
}

impl BridgeUi {
    pub(super) fn app_control_capability(&self, request: &Request) -> bool {
        let Ok(actual) = authority(&self.instance_id, &request.client_id) else {
            return false;
        };
        self.app_controls
            .current
            .and_then(|id| self.app_controls.generations.get(&id))
            .is_some_and(|g| {
                !g.retired
                    && g.authority == actual
                    && g.registry.grants.authorized(&actual, &g.token)
            })
    }
    pub(crate) fn bind_app_control_session(&mut self, session: Option<String>) {
        if self.app_controls.bound_session != session {
            self.app_controls.revoke();
            self.app_controls.bound_session = session;
        }
    }
    pub(crate) fn fence_app_control(&mut self) {
        self.app_controls.fence();
    }
    pub(crate) fn revoke_app_control(&mut self) {
        self.app_controls.revoke();
    }
    pub(super) fn app_control_projection(
        &mut self,
        studio: &Studio,
        app: &App,
        readable: bool,
    ) -> (Option<String>, bool, bool, String) {
        self.app_controls.drain();
        let actual = authority(&self.instance_id, ASSISTANT_CLIENT_ID).ok();
        if self
            .app_controls
            .current
            .and_then(|id| self.app_controls.generations.get(&id))
            .is_some_and(|g| actual.as_ref() != Some(&g.authority))
        {
            self.app_controls.revoke();
        }
        let current = self
            .app_controls
            .current
            .and_then(|id| self.app_controls.generations.get(&id));
        let token = current
            .filter(|g| {
                readable
                    && supported(studio, app)
                    && !g.retired
                    && g.registry.grants.authorized(&g.authority, &g.token)
            })
            .map(|g| g.token.clone());
        let can_grant = readable
            && actual.is_some()
            && supported(studio, app)
            && self.app_controls.bound_session.is_some()
            && self.app_controls.generations.len() < 32
            && self.app_controls.closing.is_empty();
        let text = if token.is_some() {
            "已允许切换 App 页面 · 当前会话有效"
        } else if current.is_some() {
            "当前页面或对话框期间暂停页面操作"
        } else {
            "允许操作后，可切换生成、素材、模板、账户、积分、设置和项目列表"
        };
        (token, can_grant, current.is_some(), text.into())
    }
    pub(crate) fn grant_app_control(
        &mut self,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
        expected: &crate::agent_identity::AppIdentity,
    ) -> Result<(), AssistantPermissionError> {
        let actual = authority(&self.instance_id, ASSISTANT_CLIENT_ID)
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        if actual.instance_id != expected.instance_id
            || actual.account_scope != expected.account_scope
            || actual.app_session_id != expected.app_session_id
        {
            return Err(AssistantPermissionError::StaleContext);
        }
        if !supported(studio, app) || self.assistant_app_view(studio, app, models).is_err() {
            return Err(AssistantPermissionError::NotAllowed);
        }
        let session = self
            .app_controls
            .bound_session
            .clone()
            .ok_or(AssistantPermissionError::NotAllowed)?;
        self.app_controls.revoke();
        if self.app_controls.generations.len() >= 32 || !self.app_controls.closing.is_empty() {
            return Err(AssistantPermissionError::NotAllowed);
        }
        let id = self
            .app_controls
            .next_generation
            .checked_add(1)
            .ok_or(AssistantPermissionError::NotAllowed)?;
        let mut registry = Registry::default();
        let token = registry
            .grants
            .grant(actual.clone())
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        let permits = PermitBook::new(id, actual.clone(), session.clone())
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        self.app_controls.next_generation = id;
        self.app_controls.generations.insert(
            id,
            Generation {
                authority: actual,
                session_id: session,
                token,
                root: studio.host.dirs.data.join("assistant-app-operations"),
                registry,
                permits,
                worker: None,
                retired: false,
                in_flight: 0,
                completed_flights: Arc::new(AtomicUsize::new(0)),
                operations: Vec::new(),
                registrations: Vec::new(),
                running_queue: BTreeMap::new(),
                running_replies: Vec::new(),
                saves: VecDeque::new(),
                unadmitted: Vec::new(),
                cancellation_replies: Vec::new(),
            },
        );
        self.app_controls.current = Some(id);
        Ok(())
    }
    pub(crate) fn register_app_control(
        &mut self,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
        proposal: Proposal,
    ) -> Result<(), AssistantPermissionError> {
        let actual = authority(&self.instance_id, ASSISTANT_CLIENT_ID)
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        if !supported(studio, app) {
            return Err(AssistantPermissionError::NotAllowed);
        }
        let page = self.assistant_app_view(studio, app, models)?;
        let id = self
            .app_controls
            .current
            .ok_or(AssistantPermissionError::NotAllowed)?;
        let generation = self
            .app_controls
            .generations
            .get_mut(&id)
            .ok_or(AssistantPermissionError::NotAllowed)?;
        if generation.retired
            || generation.authority != actual
            || !generation
                .registry
                .grants
                .authorized(&actual, &generation.token)
        {
            return Err(AssistantPermissionError::NotAllowed);
        }
        generation
            .permits
            .offer(proposal.clone(), &page, Instant::now())
            .map_err(|_| AssistantPermissionError::StaleContext)?;
        let operation = &proposal.intent.operation_id;
        let intent = generation
            .permits
            .offered_intent(operation, Instant::now())
            .cloned()
            .ok_or(AssistantPermissionError::StaleContext)?;
        let worker = generation
            .worker()
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        match worker.prepare(intent) {
            Ok(reply) => {
                generation
                    .registrations
                    .push(Registration { proposal, reply });
                Ok(())
            }
            Err(_) => {
                generation.permits.cancel(operation);
                Err(AssistantPermissionError::NotAllowed)
            }
        }
    }
    pub(crate) fn take_ready_app_control(&mut self) -> Vec<ReadyRegistration> {
        let mut ready = Vec::new();
        for (id, generation) in &mut self.app_controls.generations {
            let pending = std::mem::take(&mut generation.registrations);
            for registration in pending {
                match worker::poll_reply(&registration.reply) {
                    None => generation.registrations.push(registration),
                    Some(result) => {
                        generation.in_flight += 1;
                        ready.push(ReadyRegistration {
                            proposal: registration.proposal,
                            generation: *id,
                            result,
                        });
                    }
                }
            }
        }
        ready
    }
    pub(crate) fn finish_app_control_registration(
        &mut self,
        ready: ReadyRegistration,
        controller_current: bool,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
    ) -> bool {
        let actual = authority(&self.instance_id, ASSISTANT_CLIENT_ID).ok();
        let page = if controller_current && supported(studio, app) {
            self.assistant_app_view(studio, app, models).ok()
        } else {
            None
        };
        let current = self.app_controls.current == Some(ready.generation);
        let Some(generation) = self.app_controls.generations.get_mut(&ready.generation) else {
            return false;
        };
        generation.in_flight = generation.in_flight.saturating_sub(1);
        let operation = &ready.proposal.intent.operation_id;
        let live = current
            && !generation.retired
            && actual.as_ref() == Some(&generation.authority)
            && generation
                .registry
                .grants
                .authorized(&generation.authority, &generation.token)
            && page.as_ref() == Some(&ready.proposal.intent.before)
            && generation
                .permits
                .offered_intent(operation, Instant::now())
                .is_some();
        generation.finish_registration(operation, ready.result, live)
    }
    pub(crate) fn cancel_app_control_proposal(&mut self, operation: &str) {
        if let Some(id) = self.app_controls.current
            && let Some(g) = self.app_controls.generations.get_mut(&id)
        {
            g.cancel_operation(operation);
        }
    }
}

// Cleanup never grants execution and must survive a busy UI or expired callback.
struct FlightCompletion(Arc<AtomicUsize>);
impl Drop for FlightCompletion {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}
struct NavigationJob {
    completed_flights: Arc<AtomicUsize>,
    request: Request,
    permit: Arc<NativePermit>,
    worker: Arc<Handle>,
}
impl BridgeUi {
    fn control_read(&mut self, request: &Request) -> Result<Authority, Response> {
        if request.client_id != ASSISTANT_CLIENT_ID || !self.app_read_request_authorized(request) {
            return Err(Response::error("notAuthorized"));
        }
        authority(&self.instance_id, &request.client_id)
    }
    fn control_current(
        &mut self,
        request: &Request,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
        permit: Option<&NativePermit>,
    ) -> Result<(Authority, AppViewBinding), Response> {
        let actual = self.control_read(request)?;
        if !supported(studio, app) {
            return Err(Response::error("notAuthorized"));
        }
        let page = self
            .assistant_app_view(studio, app, models)
            .map_err(|_| Response::error("stalePage"))?;
        let id = self
            .app_controls
            .current
            .ok_or_else(|| Response::error("notAuthorized"))?;
        let generation = self
            .app_controls
            .generations
            .get(&id)
            .ok_or_else(|| Response::error("notAuthorized"))?;
        let token = request
            .app_control_token
            .as_deref()
            .ok_or_else(|| Response::error("notAuthorized"))?;
        if generation.retired
            || generation.authority != actual
            || generation.session_id
                != request
                    .app_navigate
                    .as_ref()
                    .ok_or_else(|| Response::error("invalidInput"))?
                    .session_id
            || !generation.registry.grants.authorized(&actual, token)
            || permit.is_some_and(|permit| {
                permit.generation() != id
                    || !generation.permits.current(permit, Instant::now())
                    || permit.intent().before != page
            })
        {
            return Err(Response::error("notAuthorized"));
        }
        Ok((actual, page))
    }
    fn reserve_navigation(
        &mut self,
        request: Request,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
    ) -> Result<NavigationJob, Response> {
        let (actual, page) = self.control_current(&request, studio, app, models, None)?;
        let arguments = request
            .app_navigate
            .as_ref()
            .ok_or_else(|| Response::error("invalidInput"))?;
        if arguments.before != page {
            return Err(Response::error("stalePage"));
        }
        let intent = Intent {
            operation_id: arguments.operation_id.clone(),
            session_id: arguments.session_id.clone(),
            client_sequence: arguments.client_sequence,
            authority: actual,
            before: arguments.before.clone(),
            target: Action::Navigate {
                target: arguments.target,
            },
        };
        intent.validate().map_err(status)?;
        let id = self
            .app_controls
            .current
            .ok_or_else(|| Response::error("notAuthorized"))?;
        let generation = self
            .app_controls
            .generations
            .get_mut(&id)
            .ok_or_else(|| Response::error("notAuthorized"))?;
        let queued = generation
            .registry
            .query(&intent.operation_id)
            .map_err(status)?;
        if queued.record.intent != intent
            || queued.record.status != Status::Queued
            || !queued.durable
        {
            return Err(Response::error("outcomeUnknown"));
        }
        let permit = generation
            .permits
            .take(&intent, Instant::now())
            .map_err(status)?;
        let worker = match generation.worker() {
            Ok(worker) => worker,
            Err(error) => {
                generation.permits.cancel(&intent.operation_id);
                return Err(error);
            }
        };
        generation.in_flight += 1;
        Ok(NavigationJob {
            completed_flights: generation.completed_flights.clone(),
            request,
            permit,
            worker,
        })
    }
}

/// A timed-out queued callback cannot later acquire a navigation permit or commit.
fn control_ui<T: Send + 'static>(
    body: impl FnOnce(&Shell, App) -> T + Send + 'static,
) -> Result<T, Response> {
    use std::sync::atomic::AtomicU8;
    // 0 pending, 1 running, 2 cancelled. A claimed synchronous callback retains ownership.
    let phase = Arc::new(AtomicU8::new(0));
    let queued = phase.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    slint::invoke_from_event_loop(move || {
        if queued
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        Shell::with(|shell, app| {
            let _ = sender.send(body(shell, app));
        });
    })
    .map_err(|_| Response::error("appUnavailable"))?;
    match receiver.recv_timeout(Duration::from_millis(750)) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if phase
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                Err(Response::error("outcomeUnknown"))
            } else {
                receiver
                    .recv()
                    .map_err(|_| Response::error("outcomeUnknown"))
            }
        }
        Err(_) => Err(Response::error("appUnavailable")),
    }
}
fn wait_worker<T>(reply: worker::Reply<T>) -> Result<T, Response> {
    // Only the MCP connection thread waits; UI work and filesystem work never share a thread.
    reply
        .recv()
        .map_err(|_| Response::error("outcomeUnknown"))?
        .map_err(worker_error)
}
fn with_bridge<T>(
    shell: &Shell,
    app: &App,
    body: impl FnOnce(&mut BridgeUi, &Studio, &App) -> Result<T, Response>,
) -> Result<T, Response> {
    let studio = shell
        .studio
        .try_borrow()
        .map_err(|_| Response::error("busy"))?;
    let mut bridge = studio
        .editor_mcp
        .try_borrow_mut()
        .map_err(|_| Response::error("busy"))?;
    body(&mut bridge, &studio, app)
}
enum Staged {
    Running(worker::Reply<core::RunningProof>),
    Cancelled(Finalized),
    Failed(Response),
}
enum Finalized {
    Save {
        reply: worker::Reply<Saved>,
        adoption: Option<crate::cloud::AppNavigationAdoption>,
    },
    Response(Response),
}
fn enqueue_terminal(
    generation: &mut Generation,
    worker: &Handle,
    record: Record,
    permit: &Arc<NativePermit>,
    adoption: Option<crate::cloud::AppNavigationAdoption>,
) -> Finalized {
    match worker.save(record.clone()) {
        Ok(reply) => Finalized::Save { reply, adoption },
        Err(_) => {
            generation.queue_save(
                record.clone(),
                adoption.map(|value| (permit.clone(), value)),
            );
            let view = generation.registry.query(&record.intent.operation_id);
            Finalized::Response(
                view.map(|view| receipt(&view.record, view.effective_status, view.durable, None))
                    .unwrap_or_else(status),
            )
        }
    }
}
fn cancel_admitted(generation: &mut Generation, job: &NavigationJob) -> Finalized {
    generation.permits.cancel(&job.permit.intent().operation_id);
    match generation
        .registry
        .cancel(&job.permit.intent().operation_id)
    {
        Ok(record) => enqueue_terminal(generation, &job.worker, record, &job.permit, None),
        Err(error) => Finalized::Response(status(error)),
    }
}

pub(super) fn dispatch_navigation(request: Request) -> Response {
    let reservation = control_ui(move |shell, app| {
        with_bridge(shell, &app, |bridge, studio, app| {
            bridge.reserve_navigation(request, studio, app, &shell.models)
        })
    });
    let job = match reservation {
        Ok(Ok(job)) => Arc::new(job),
        Ok(Err(error)) | Err(error) => return error,
    };
    let _completion = FlightCompletion(job.completed_flights.clone());
    let result = run_navigation(job.clone());
    if result.is_err() {
        let cleanup = job.clone();
        let _ = slint::invoke_from_event_loop(move || {
            Shell::with(|shell, app| {
                let _ = with_bridge(shell, &app, |bridge, _, _| {
                    if let Some(g) = bridge
                        .app_controls
                        .generations
                        .get_mut(&cleanup.permit.generation())
                    {
                        g.permits.cancel(&cleanup.permit.intent().operation_id);
                        if let Ok(record) = g.registry.cancel(&cleanup.permit.intent().operation_id)
                        {
                            g.queue_save(record, None);
                            g.drain();
                        }
                    }
                    Ok(())
                });
            })
        });
    }
    result.unwrap_or_else(|response| response)
}
fn run_navigation(job: Arc<NavigationJob>) -> Result<Response, Response> {
    let staged = control_ui({
        let job = job.clone();
        move |shell, app| {
            with_bridge(shell, &app, |bridge, studio, app| {
                let checked = bridge.control_current(
                    &job.request,
                    studio,
                    app,
                    &shell.models,
                    Some(&job.permit),
                );
                let generation = bridge
                    .app_controls
                    .generations
                    .get_mut(&job.permit.generation())
                    .ok_or_else(|| Response::error("notAuthorized"))?;
                if checked.is_err() {
                    return Ok(Staged::Cancelled(cancel_admitted(generation, &job)));
                }
                let id = &job.permit.intent().operation_id;
                let queued = generation.registry.query(id).map_err(status)?;
                if queued.record.intent != *job.permit.intent()
                    || queued.record.status != Status::Queued
                    || !queued.durable
                {
                    return Ok(Staged::Failed(Response::error("outcomeUnknown")));
                }
                if generation
                    .registry
                    .approve(
                        id,
                        &generation.authority,
                        &generation.token,
                        &job.permit.intent().before,
                    )
                    .is_err()
                {
                    return Ok(Staged::Cancelled(cancel_admitted(generation, &job)));
                }
                let running = generation.registry.stage_running(id).map_err(status)?;
                // Stage and enqueue are one UI callback, so a cancel can never overtake rev2.
                match job.worker.persist_running(running) {
                    Ok(reply) => Ok(Staged::Running(reply)),
                    Err(error) => {
                        generation.running_queue.insert(id.clone(), error.input);
                        generation.permits.cancel(id);
                        if let Ok(record) = generation.registry.cancel(id) {
                            generation.queue_save(record, None);
                        }
                        Ok(Staged::Failed(Response::error("outcomeUnknown")))
                    }
                }
            })
        }
    })??;
    let proof = match staged {
        Staged::Running(reply) => wait_worker(reply)?,
        Staged::Failed(error) => return Err(error),
        Staged::Cancelled(finalized) => return complete_finalized(job, finalized),
    };
    let finalized = control_ui({
        let job = job.clone();
        move |shell, app| {
            let dispatch = with_bridge(shell, &app, |bridge, studio, app| {
                let checked = bridge.control_current(
                    &job.request,
                    studio,
                    app,
                    &shell.models,
                    Some(&job.permit),
                );
                let generation = bridge
                    .app_controls
                    .generations
                    .get_mut(&job.permit.generation())
                    .ok_or_else(|| Response::error("notAuthorized"))?;
                let (actual, before) = match checked {
                    Ok(value) => value,
                    Err(_) => return Ok(Err(cancel_admitted(generation, &job))),
                };
                let dispatch = match generation.commit_navigation(
                    &job.permit,
                    proof,
                    &actual,
                    &before,
                    Instant::now(),
                ) {
                    Ok(dispatch) => dispatch,
                    Err(_) => return Ok(Err(cancel_admitted(generation, &job))),
                };
                Ok(Ok(dispatch))
            })?;
            let dispatch = match dispatch {
                Ok(dispatch) => dispatch,
                Err(result) => return Ok(result),
            };
            // No Studio/Bridge borrow crosses the original navigation wrapper.
            let adoption = crate::cloud::app_control_navigate(shell, &app, dispatch);
            with_bridge(shell, &app, |bridge, _, _| {
                let generation = bridge
                    .app_controls
                    .generations
                    .get_mut(&job.permit.generation())
                    .ok_or_else(|| Response::error("outcomeUnknown"))?;
                generation.finish_navigation(&job, adoption.ok())
            })
        }
    })??;
    complete_finalized(job, finalized)
}
fn complete_finalized(job: Arc<NavigationJob>, finalized: Finalized) -> Result<Response, Response> {
    match finalized {
        Finalized::Response(response) => Ok(response),
        Finalized::Save { reply, adoption } => {
            let saved = wait_worker(reply)?;
            control_ui(move |shell, app| {
                with_bridge(shell, &app, |bridge, _, _| {
                    let generation = bridge
                        .app_controls
                        .generations
                        .get_mut(&job.permit.generation())
                        .ok_or_else(|| Response::error("outcomeUnknown"))?;
                    let result = generation.registry.acknowledge(saved).map_err(status)?;
                    if let Some(adoption) = adoption {
                        let Action::Navigate { target } = job.permit.intent().target;
                        if adoption.matches(&job.permit.intent().operation_id, target) {
                            generation.permits.adopted(&job.permit, adoption.page());
                        }
                    }
                    Ok(receipt(
                        &result.record,
                        result.effective_status,
                        result.durable,
                        None,
                    ))
                })
            })?
        }
    }
}

struct QueryJob {
    request: Request,
    authority: Authority,
    root: PathBuf,
    worker: Option<Arc<Handle>>,
}
pub(super) fn dispatch_query(request: Request) -> Response {
    let query = match request.app_operation_query.clone() {
        Some(query) => query,
        None => return Response::error("invalidInput"),
    };
    let job = match control_ui(move |shell, app| {
        with_bridge(shell, &app, |bridge, studio, _| {
            let actual = bridge.control_read(&request)?;
            let original = &request
                .app_operation_query
                .as_ref()
                .ok_or_else(|| Response::error("invalidInput"))?
                .session_id;
            let worker = bridge
                .app_controls
                .generations
                .values()
                .find(|g| {
                    g.authority.account_scope == actual.account_scope
                        && g.authority.client_id == actual.client_id
                        && &g.session_id == original
                })
                .and_then(|g| g.worker.clone());
            Ok(QueryJob {
                request,
                authority: actual,
                root: studio.host.dirs.data.join("assistant-app-operations"),
                worker,
            })
        })
    }) {
        Ok(Ok(job)) => job,
        Ok(Err(error)) | Err(error) => return error,
    };
    let worker = match job.worker.clone() {
        Some(worker) => worker,
        None => {
            let (worker, _opened) = match worker::spawn(
                job.root.clone(),
                QueryScope {
                    account_scope: job.authority.account_scope.clone(),
                    client_id: job.authority.client_id.clone(),
                    session_id: query.session_id.clone(),
                },
            ) {
                Ok(result) => result,
                Err(_) => return Response::error("ioFailure"),
            };
            worker.fence();
            Arc::new(worker)
        }
    };
    let result = worker
        .confirm_query(query.operation_id.clone())
        .map_err(|error| submit_failure(error.kind))
        .and_then(wait_worker);
    let view = match result {
        Ok(view) => view,
        Err(error) => return error,
    };
    // The live worker stays held through receipt construction, including query-only recovery.
    let checked = control_ui(move |shell, app| {
        with_bridge(shell, &app, |bridge, studio, app| {
            let actual = bridge.control_read(&job.request)?;
            if view.execution_recoverable() {
                return Err(Response::error("outcomeUnknown"));
            }
            if actual != job.authority
                || view.recorded().intent.authority.account_scope != actual.account_scope
                || view.recorded().intent.authority.client_id != actual.client_id
                || view.recorded().intent.session_id != query.session_id
                || view.recorded().intent.client_sequence != query.client_sequence
            {
                return Err(Response::error("notAuthorized"));
            }
            let observed = bridge
                .assistant_app_view(studio, app, &shell.models)
                .ok()
                .map(|page| AppCurrentObservation {
                    app_instance_id: actual.instance_id,
                    app_session_id: actual.app_session_id,
                    page,
                });
            Ok(receipt(
                view.recorded(),
                view.effective_status(),
                view.is_durable(),
                observed,
            ))
        })
    });
    drop(worker);
    match checked {
        Ok(Ok(response)) => response,
        Ok(Err(error)) | Err(error) => error,
    }
}

#[cfg(test)]
#[path = "editor_mcp_app_control_tests.rs"]
mod lifecycle_tests;
