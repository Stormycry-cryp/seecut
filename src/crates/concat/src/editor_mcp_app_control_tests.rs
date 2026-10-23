// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exercise the actual adapter state and worker; no window or live navigation.
use super::*;
use crate::agent_app_control_input::{InputBinding, Tracker};
use crate::agent_identity::AppIdentity;
use concat_editor_mcp::{AppPageId, app_operation_id};
use serde_json::json;
use std::{fs, os::unix::fs::DirBuilderExt};

struct Fixture {
    root: PathBuf,
    generation: Generation,
    proposal: Proposal,
    now: Instant,
}
fn receive<T>(reply: worker::Reply<T>) -> T {
    reply
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap()
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
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
        assert!(tracker.remember("input", source.clone()));
        assert!(tracker.applied("turn", "input", 1));
        let proposal = tracker
            .started(
                "session",
                "turn",
                2,
                &json!({
                    "invocationId":"invoke",
                    "call":{"name":"app_navigate","arguments":{"target":"settings"}},
                    "appViewBinding":source,
                    "appControlIntent":{"version":1,"inputId":"input",
                        "operationId":app_operation_id("client","session",1).unwrap(),
                        "clientSequence":1,"before":source.page(),"target":"settings"}
                }),
            )
            .unwrap();
        let mut registry = Registry::default();
        let token = registry.grants.grant(authority.clone()).unwrap();
        let mut permits = PermitBook::new(1, authority.clone(), "session".into()).unwrap();
        let now = Instant::now();
        permits
            .offer(proposal.clone(), &source.page(), now)
            .unwrap();
        let generation = Generation {
            authority,
            session_id: "session".into(),
            token,
            root: root.clone(),
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
        };
        Self {
            root,
            generation,
            proposal,
            now,
        }
    }
    fn prepare(&mut self) -> journal::Prepared {
        let intent = self
            .generation
            .permits
            .offered_intent(&self.proposal.intent.operation_id, self.now)
            .unwrap()
            .clone();
        let worker = self.generation.worker().unwrap();
        receive(
            worker
                .prepare(intent)
                .unwrap_or_else(|e| panic!("{:?}", e.kind)),
        )
    }
    fn admit(&mut self) {
        let prepared = self.prepare();
        assert!(self.generation.finish_registration(
            &self.proposal.intent.operation_id,
            Ok(prepared),
            true,
        ));
    }
    fn job(&mut self) -> NavigationJob {
        let original = self
            .generation
            .registry
            .query(&self.proposal.intent.operation_id)
            .unwrap()
            .record
            .intent;
        NavigationJob {
            completed_flights: self.generation.completed_flights.clone(),
            request: serde_json::from_value(json!({
                "instanceId":"instance","clientId":"client",
                "clientToken":"read","method":"appNavigate"
            }))
            .unwrap(),
            permit: self.generation.permits.take(&original, self.now).unwrap(),
            worker: self.generation.worker().unwrap(),
        }
    }
    fn running(&mut self, job: &NavigationJob) -> core::RunningProof {
        let g = &mut self.generation;
        let id = &job.permit.intent().operation_id;
        g.registry
            .approve(id, &g.authority, &g.token, &job.permit.intent().before)
            .unwrap();
        let write = g.registry.stage_running(id).unwrap();
        receive(
            job.worker
                .persist_running(write)
                .unwrap_or_else(|e| panic!("{:?}", e.kind)),
        )
    }
    fn query(&mut self) -> journal::RecoveryView {
        self.generation.drain();
        let worker = self.generation.worker().unwrap();
        receive(
            worker
                .confirm_query(self.proposal.intent.operation_id.clone())
                .unwrap_or_else(|e| panic!("{:?}", e.kind)),
        )
    }
    fn pause(&self) -> mpsc::Sender<()> {
        let (entered, wait) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        self.generation
            .worker
            .as_ref()
            .unwrap()
            .pause_for_test(entered, gate);
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        release
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.generation.fence(true);
        let worker = self.generation.worker.take().unwrap();
        let handle = Arc::try_unwrap(worker).unwrap_or_else(|_| panic!("worker retained"));
        receive(handle.close());
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn stop_after_prepare_before_accept_consumes_cancel_prepared() {
    let mut f = Fixture::new();
    let prepared = f.prepare();
    f.generation.fence(false);
    assert!(!f.generation.finish_registration(
        &f.proposal.intent.operation_id,
        Ok(prepared),
        false,
    ));
    let view = f.query();
    assert!(view.is_durable());
    assert_eq!(view.effective_status(), Status::Cancelled);
    assert_eq!(view.recorded().revision, 2);
    assert!(!view.execution_recoverable());
    assert!(
        f.generation
            .registry
            .query(&f.proposal.intent.operation_id)
            .is_err()
    );
}

#[test]
fn allowed_ack_then_stop_before_raw_cancels_original_operation() {
    let mut f = Fixture::new();
    f.admit();
    let id = f.proposal.intent.operation_id.clone();
    assert_eq!(
        f.generation.registry.query(&id).unwrap().record.status,
        Status::Queued
    );
    f.generation.fence(false);
    let view = f.query();
    assert!(view.is_durable());
    assert_eq!(view.effective_status(), Status::Cancelled);
    assert_eq!(view.recorded().intent.operation_id, id);
    assert!(!f.generation.permits.retained(&id));
}

#[test]
fn failed_host_ack_immediately_cancels_accepted_operation() {
    let mut f = Fixture::new();
    f.admit();
    f.generation
        .cancel_operation(&f.proposal.intent.operation_id);
    assert_eq!(f.query().effective_status(), Status::Cancelled);
}

#[test]
fn revoke_after_running_proof_prevents_commit_and_navigation() {
    let mut f = Fixture::new();
    f.admit();
    let job = f.job();
    let proof = f.running(&job);
    f.generation.fence(true);
    let actual = f.generation.authority.clone();
    assert!(
        f.generation
            .commit_navigation(
                &job.permit,
                proof,
                &actual,
                &job.permit.intent().before,
                f.now,
            )
            .is_err()
    );
    assert_eq!(f.query().effective_status(), Status::Cancelled);
}

#[test]
fn adopted_navigation_with_delayed_save_preserves_real_completion() {
    let mut f = Fixture::new();
    f.admit();
    let job = f.job();
    let proof = f.running(&job);
    let actual = f.generation.authority.clone();
    let dispatch = f
        .generation
        .commit_navigation(
            &job.permit,
            proof,
            &actual,
            &job.permit.intent().before,
            f.now,
        )
        .unwrap();
    let mut page = job.permit.intent().before.clone();
    page.page_id = AppPageId::Settings;
    page.page_epoch += 1;
    let adoption =
        crate::cloud::AppNavigationAdoption::for_state_machine_test(dispatch, page.clone());
    let release = f.pause();
    let finalized = f
        .generation
        .finish_navigation(&job, Some(adoption))
        .unwrap();
    let id = job.permit.intent().operation_id.clone();
    let view = f.generation.registry.query(&id).unwrap();
    assert_eq!(view.record.status, Status::Completed);
    assert!(!view.durable);
    f.generation.permits.expire(f.now + Duration::from_secs(35));
    assert!(f.generation.permits.retained(&id));
    release.send(()).unwrap();
    let Finalized::Save { reply, adoption } = finalized else {
        panic!("queued save")
    };
    f.generation.registry.acknowledge(receive(reply)).unwrap();
    assert!(
        f.generation
            .permits
            .adopted(&job.permit, adoption.unwrap().page())
    );
    let saved = f.query();
    assert!(saved.is_durable());
    assert_eq!(saved.effective_status(), Status::Completed);
    assert_eq!(
        saved.recorded().completion,
        Some(Completion::Navigation { page })
    );
}

#[test]
fn manual_target_page_without_original_adoption_cannot_complete() {
    let mut f = Fixture::new();
    f.admit();
    let job = f.job();
    let proof = f.running(&job);
    let actual = f.generation.authority.clone();
    let _dispatch = f
        .generation
        .commit_navigation(
            &job.permit,
            proof,
            &actual,
            &job.permit.intent().before,
            f.now,
        )
        .unwrap();
    // A page observation cannot construct AppNavigationAdoption.
    let finalized = f.generation.finish_navigation(&job, None).unwrap();
    let Finalized::Save { reply, .. } = finalized else {
        panic!("queued cancellation")
    };
    f.generation.registry.acknowledge(receive(reply)).unwrap();
    let view = f.query();
    assert_eq!(view.effective_status(), Status::OutcomeUnknown);
    assert!(view.recorded().completion.is_none());
}

#[test]
fn background_completion_survives_busy_ui_until_next_drain() {
    let mut f = Fixture::new();
    f.admit();
    f.generation.in_flight = 1;
    let completion = FlightCompletion(f.generation.completed_flights.clone());
    f.generation.fence(true);
    assert_eq!(f.generation.in_flight, 1);
    // No event-loop callback or RefCell borrow is involved in this notification.
    std::thread::spawn(move || drop(completion)).join().unwrap();
    assert_eq!(f.generation.in_flight, 1);
    f.generation.drain();
    assert_eq!(f.generation.in_flight, 0);
    assert_eq!(f.generation.completed_flights.load(Ordering::Acquire), 0);
    f.generation.drain();
    assert_eq!(f.generation.in_flight, 0);
    assert_eq!(f.query().effective_status(), Status::Cancelled);
    // Fixture Drop consumes the actual worker close receipt before deleting its root.
}
