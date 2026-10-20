// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::agent_app_control_core::*;
use crate::agent_app_control_journal::*;
use crate::agent_app_control_worker::Reply;
use crate::agent_app_control_worker::*;
use concat_editor_mcp::{AppPageId, AppViewBinding};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf, sync::mpsc, time::Duration};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let base = std::env::temp_dir();
        let path = base.join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn spawn(&self) -> Handle {
        let (handle, ready) = spawn(self.0.clone(), scope()).unwrap();
        assert_eq!(receive(ready), Ok(()));
        handle
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn receive<T>(reply: Reply<T>) -> Result<T, WorkerError> {
    match reply.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(WorkerError::WorkerStopped),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("后台请求未在测试期限内完成"),
    }
}
fn submitted<T, R>(result: Result<Reply<R>, Box<SubmitError<T>>>) -> Reply<R> {
    result.unwrap_or_else(|error| panic!("投递失败: {:?}", error.kind))
}
fn authority() -> Authority {
    Authority {
        instance_id: "instance".into(),
        client_id: "client".into(),
        account_scope: "a".repeat(64),
        account_epoch: 1,
        app_session_id: "app-session".into(),
    }
}
fn scope() -> QueryScope {
    QueryScope {
        account_scope: authority().account_scope,
        client_id: "client".into(),
        session_id: "original-coto".into(),
    }
}
fn page() -> AppViewBinding {
    AppViewBinding {
        page_id: AppPageId::Generation,
        page_epoch: 1,
        page_state_id: format!("app-page-v1-{}", "b".repeat(64)),
    }
}
fn intent(n: u64) -> Intent {
    Intent {
        operation_id: operation_id("client", "original-coto", n).unwrap(),
        session_id: "original-coto".into(),
        client_sequence: n,
        authority: authority(),
        before: page(),
        target: Action::Navigate {
            target: AppPageId::PersonalLibrary,
        },
    }
}
fn accepted(handle: &Handle, registry: &mut Registry, n: u64) -> String {
    let input = intent(n);
    let id = input.operation_id.clone();
    registry
        .accept(receive(submitted(handle.prepare(input))).unwrap())
        .unwrap();
    id
}
fn approve(registry: &mut Registry, id: &str) -> String {
    let token = registry.grants.grant(authority()).unwrap();
    registry.approve(id, &authority(), &token, &page()).unwrap();
    token
}
fn done() -> Completion {
    let mut after = page();
    after.page_id = AppPageId::PersonalLibrary;
    after.page_epoch = 2;
    Completion::Navigation { page: after }
}
fn assert_busy<T>(result: Result<T, WorkerError>) {
    assert!(matches!(result, Err(WorkerError::Core(Error::Busy))));
}

#[test]
fn fence_before_open_covers_late_journal_and_queued_prepare() {
    let root = Root::new();
    let (release, gate) = mpsc::channel();
    let (handle, ready) = spawn_gated(root.0.clone(), scope(), Some(gate), None).unwrap();
    let reply = submitted(handle.prepare(intent(1)));
    handle.fence();
    assert!(!root.0.join(authority().account_scope).exists());
    assert_eq!(poll_reply(&ready), None);
    release.send(()).unwrap();
    assert_eq!(receive(ready), Ok(()));
    assert_busy(receive(reply));
    assert_eq!(receive(handle.close()), Ok(()));
}

#[test]
fn prepared_reply_delayed_by_ui_cannot_execute_after_fence() {
    let root = Root::new();
    let handle = root.spawn();
    let prepared = receive(submitted(handle.prepare(intent(1)))).unwrap();
    handle.fence();
    assert!(matches!(
        Registry::default().accept(prepared),
        Err(Error::Busy)
    ));
    assert_eq!(receive(handle.close()), Ok(()));
}

#[test]
fn fence_invalidates_late_running_proof_and_preserves_queued_settlement() {
    let root = Root::new();
    let handle = root.spawn();
    let mut registry = Registry::default();
    let id = accepted(&handle, &mut registry, 1);
    let token = approve(&mut registry, &id);
    let proof = receive(submitted(
        handle.persist_running(registry.stage_running(&id).unwrap()),
    ))
    .unwrap();
    handle.fence();
    assert!(matches!(
        registry.commit(proof, &authority(), &token, &page()),
        Err(Error::Busy)
    ));
    assert_busy(receive(submitted(handle.prepare(intent(2)))));
    assert_eq!(receive(handle.close()), Ok(()));

    let handle = root.spawn();
    let mut registry = Registry::default();
    let id = accepted(&handle, &mut registry, 3);
    let token = approve(&mut registry, &id);
    let write = registry.stage_running(&id).unwrap();
    let (entered, paused) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    handle.pause_for_test(entered, gate);
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    let proof = submitted(handle.persist_running(write));
    handle.fence();
    let cancelled = registry.cancel(&id).unwrap();
    let saved = submitted(handle.save(cancelled.clone()));
    release.send(()).unwrap();
    assert!(matches!(
        registry.commit(receive(proof).unwrap(), &authority(), &token, &page()),
        Err(Error::Cancelled)
    ));
    registry.acknowledge(receive(saved).unwrap()).unwrap();
    let confirmed = receive(submitted(handle.confirm_query(id))).unwrap();
    assert_eq!(confirmed.recorded(), &cancelled);
    assert_eq!(receive(handle.close()), Ok(()));
}

#[test]
fn dispatched_completion_survives_fence_and_is_confirmed_from_saved() {
    let root = Root::new();
    let handle = root.spawn();
    let mut registry = Registry::default();
    let id = accepted(&handle, &mut registry, 1);
    let token = approve(&mut registry, &id);
    let proof = receive(submitted(
        handle.persist_running(registry.stage_running(&id).unwrap()),
    ))
    .unwrap();
    let dispatch = registry
        .commit(proof, &authority(), &token, &page())
        .unwrap();
    assert_eq!(dispatch.correlation_id, id);
    let completed = registry.complete(&id, done()).unwrap();
    handle.fence();
    let saved = receive(submitted(handle.save(completed.clone()))).unwrap();
    assert!(registry.acknowledge(saved).unwrap().durable);
    let recovered = receive(submitted(handle.confirm_query(id))).unwrap();
    assert_eq!(recovered.recorded(), &completed);
    assert_eq!(recovered.effective_status(), Status::Completed);
    assert!(recovered.is_durable());
    assert!(!recovered.execution_recoverable());
    assert_eq!(receive(handle.close()), Ok(()));
}

#[test]
fn actual_fifo_persists_running_before_later_cancel_and_confirm() {
    let root = Root::new();
    let handle = root.spawn();
    let mut registry = Registry::default();
    let id = accepted(&handle, &mut registry, 1);
    approve(&mut registry, &id);
    let proof = submitted(handle.persist_running(registry.stage_running(&id).unwrap()));
    let cancelled = registry.cancel(&id).unwrap();
    let saved = submitted(handle.save(cancelled.clone()));
    let confirmed = submitted(handle.confirm_query(id.clone()));
    assert!(receive(proof).is_ok());
    registry.acknowledge(receive(saved).unwrap()).unwrap();
    let view = receive(confirmed).unwrap();
    assert_eq!(view.recorded(), &cancelled);
    assert_eq!(view.recorded().revision, 3);
    assert_eq!(view.effective_status(), Status::Cancelled);
    assert!(view.is_durable());
    assert_eq!(receive(handle.close()), Ok(()));
}

#[test]
fn close_releases_flock_and_old_proof_is_invalid_for_new_owner() {
    let root = Root::new();
    let handle = root.spawn();
    let mut registry = Registry::default();
    let id = accepted(&handle, &mut registry, 1);
    let token = approve(&mut registry, &id);
    let proof = receive(submitted(
        handle.persist_running(registry.stage_running(&id).unwrap()),
    ))
    .unwrap();
    assert!(matches!(Journal::open(&root.0, scope()), Err(Error::Busy)));
    assert_eq!(receive(handle.close()), Ok(()));
    let new_owner = Journal::open(&root.0, scope()).unwrap();
    assert!(matches!(
        registry.commit(proof, &authority(), &token, &page()),
        Err(Error::Busy)
    ));
    let view = new_owner.query(&id).unwrap();
    assert_eq!(view.recorded().status, Status::Running);
    assert_eq!(view.effective_status(), Status::OutcomeUnknown);
    assert!(!view.execution_recoverable());
    drop(new_owner);
}

#[test]
fn bounded_queue_returns_original_input_and_full_close_waits_for_real_drop() {
    let root = Root::new();
    let (release, gate) = mpsc::channel();
    let (handle, ready) = spawn_gated(root.0.clone(), scope(), None, Some(gate)).unwrap();
    assert_eq!(receive(ready), Ok(()));
    let mut replies = Vec::new();
    for n in 1..=QUEUE_CAPACITY as u64 {
        replies.push(submitted(handle.prepare(intent(n))));
    }
    let original = intent(100);
    let error = match handle.prepare(original.clone()) {
        Err(error) => error,
        Ok(_) => panic!("队列应已满"),
    };
    assert_eq!(error.kind, SubmitFailure::QueueFull);
    assert_eq!(error.input, original);
    let closed = handle.close();
    assert_eq!(poll_reply(&closed), None);
    assert!(matches!(Journal::open(&root.0, scope()), Err(Error::Busy)));
    release.send(()).unwrap();
    for reply in replies {
        assert_busy(receive(reply));
    }
    assert_eq!(receive(closed), Ok(()));
    drop(Journal::open(&root.0, scope()).unwrap());
}

#[test]
fn drop_returns_while_worker_is_gated_and_still_releases_journal() {
    let root = Root::new();
    let (release, gate) = mpsc::channel();
    let (handle, ready) = spawn_gated(root.0.clone(), scope(), None, Some(gate)).unwrap();
    assert_eq!(receive(ready), Ok(()));
    let closed = handle_closed_for_drop_test(handle, &root, &release);
    assert_eq!(receive(closed), Ok(()));
    drop(Journal::open(&root.0, scope()).unwrap());
}
fn handle_closed_for_drop_test(
    mut handle: Handle,
    root: &Root,
    release: &mpsc::Sender<()>,
) -> Reply<()> {
    let closed = handle.take_closed_for_drop_test();
    drop(handle);
    assert!(matches!(Journal::open(&root.0, scope()), Err(Error::Busy)));
    release.send(()).unwrap();
    closed
}

#[test]
fn open_failure_and_worker_death_have_explicit_errors() {
    let root = Root::new();
    let owner = Journal::open(&root.0, scope()).unwrap();
    let (handle, ready) = spawn(root.0.clone(), scope()).unwrap();
    assert_eq!(receive(ready), Err(WorkerError::Core(Error::Busy)));
    assert_eq!(receive(handle.close()), Err(WorkerError::Core(Error::Busy)));
    drop(owner);

    let (release, gate) = mpsc::channel();
    let (handle, ready) = spawn_gated(root.0.clone(), scope(), None, Some(gate)).unwrap();
    assert_eq!(receive(ready), Ok(()));
    handle.crash_for_test();
    let pending = submitted(handle.confirm_query(intent(1).operation_id));
    release.send(()).unwrap();
    assert!(matches!(receive(pending), Err(WorkerError::WorkerStopped)));
    let original = intent(2);
    let error = match handle.prepare(original.clone()) {
        Err(error) => error,
        Ok(_) => panic!("死亡 worker 不应接受新请求"),
    };
    assert_eq!(error.kind, SubmitFailure::WorkerStopped);
    assert_eq!(error.input, original);
    assert_eq!(receive(handle.close()), Err(WorkerError::WorkerStopped));
    drop(Journal::open(&root.0, scope()).unwrap());
}

#[test]
fn poll_maps_disconnected_reply_to_worker_stopped() {
    let (sender, reply) = mpsc::channel::<Result<(), WorkerError>>();
    assert_eq!(poll_reply(&reply), None);
    drop(sender);
    assert_eq!(poll_reply(&reply), Some(Err(WorkerError::WorkerStopped)));
}

#[test]
fn full_close_drains_confirmations_and_settlement_without_fence_rejection() {
    let root = Root::new();
    let handle = root.spawn();
    let mut registry = Registry::default();
    let id = accepted(&handle, &mut registry, 1);
    let cancelled = registry.cancel(&id).unwrap();
    registry
        .acknowledge(receive(submitted(handle.save(cancelled.clone()))).unwrap())
        .unwrap();
    let (entered, paused) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    handle.pause_for_test(entered, gate);
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut confirmations = Vec::new();
    for _ in 0..QUEUE_CAPACITY {
        confirmations.push(submitted(handle.confirm_query(id.clone())));
    }
    let closed = handle.close();
    assert_eq!(poll_reply(&closed), None);
    release.send(()).unwrap();
    for reply in confirmations {
        let view = receive(reply).unwrap();
        assert!(view.is_durable());
        assert_eq!(view.recorded(), &cancelled);
    }
    assert_eq!(receive(closed), Ok(()));
    drop(Journal::open(&root.0, scope()).unwrap());
}

#[test]
fn handle_is_send_sync_and_arc_keeps_only_one_lifetime_owner() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Handle>();
    let root = Root::new();
    let handle = std::sync::Arc::new(root.spawn());
    let shared = std::sync::Arc::clone(&handle);
    let view = std::thread::spawn(move || {
        let prepared = receive(submitted(shared.prepare(intent(1)))).unwrap();
        Registry::default().accept(prepared).unwrap();
    });
    view.join().unwrap();
    let handle = std::sync::Arc::try_unwrap(handle).ok().unwrap();
    assert_eq!(receive(handle.close()), Ok(()));
    drop(Journal::open(&root.0, scope()).unwrap());
}

#[test]
fn old_queued_records_only_query_and_capacity_errors_remain_core_errors() {
    let root = Root::new();
    let handle = root.spawn();
    for n in 1..=MAX_OPERATIONS as u64 {
        assert!(receive(submitted(handle.prepare(intent(n)))).is_ok());
    }
    assert!(matches!(
        receive(submitted(handle.prepare(intent(129)))),
        Err(WorkerError::Core(Error::ResourceLimit))
    ));
    assert_eq!(receive(handle.close()), Ok(()));
    let handle = root.spawn();
    let old = receive(submitted(handle.prepare(intent(1)))).unwrap();
    assert!(matches!(
        Registry::default().accept(old),
        Err(Error::QueryOnly)
    ));
    let view = receive(submitted(handle.confirm_query(intent(1).operation_id))).unwrap();
    assert_eq!(view.effective_status(), Status::Queued);
    assert!(view.is_durable());
    assert!(!view.execution_recoverable());
    assert_eq!(receive(handle.close()), Ok(()));
}
