// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::agent_app_control_core::*;
use crate::agent_app_control_journal::*;
use crate::agent_app_control_worker::{
    self as worker, Handle, Reply, SubmitError, SubmitFailure, WorkerError,
};
use concat_editor_mcp::{AppPageId, AppViewBinding};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf, sync::mpsc, time::Duration};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> Journal {
        Journal::open(&self.0, scope()).unwrap()
    }
    fn spawn(&self) -> Handle {
        let (handle, ready) = worker::spawn(self.0.clone(), scope()).unwrap();
        assert_eq!(receive(ready), Ok(()));
        handle
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
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
        session_id: "unadmitted-coto".into(),
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
        operation_id: operation_id("client", "unadmitted-coto", n).unwrap(),
        session_id: "unadmitted-coto".into(),
        client_sequence: n,
        authority: authority(),
        before: page(),
        target: Action::Navigate {
            target: AppPageId::PersonalLibrary,
        },
    }
}
fn receive<T>(reply: Reply<T>) -> Result<T, WorkerError> {
    match reply.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(WorkerError::WorkerStopped),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("worker reply within test deadline"),
    }
}
fn submitted<T, R>(result: Result<Reply<R>, Box<SubmitError<T>>>) -> Reply<R> {
    result.unwrap_or_else(|error| panic!("submit failed: {:?}", error.kind))
}
fn assert_cancelled(view: &RecoveryView, original: &Intent) {
    assert!(view.is_durable());
    assert!(!view.execution_recoverable());
    assert_eq!(view.effective_status(), Status::Cancelled);
    let mut expected = Record::queued(original.clone()).unwrap();
    expected.status = Status::Cancelled;
    expected.revision = 2;
    assert_eq!(view.recorded(), &expected);
}
fn disk_record(journal: &Journal, id: &str) -> Record {
    serde_json::from_slice(&fs::read(journal.test_file(id)).unwrap()).unwrap()
}
fn paused(handle: &Handle) -> mpsc::Sender<()> {
    let (entered, wait) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    handle.pause_for_test(entered, gate);
    wait.recv_timeout(Duration::from_secs(10)).unwrap();
    release
}

#[test]
fn fresh_prepared_cancels_before_or_after_execution_fence_without_registry() {
    for fenced in [false, true] {
        let root = Root::new();
        let mut j = root.open();
        let original = intent(1);
        let prepared = j.prepare(original.clone()).unwrap();
        if fenced {
            j.execution_fence().fence();
        }
        let view = j
            .cancel_prepared(prepared)
            .unwrap()
            .into_recovery()
            .unwrap();
        assert_cancelled(&view, &original);
        assert_eq!(disk_record(&j, &original.operation_id), *view.recorded());
        assert!(matches!(
            Registry::default().query(&original.operation_id),
            Err(Error::InvalidInput)
        ));
        assert!(!j.query(&original.operation_id).unwrap().is_durable());
        assert!(matches!(
            Registry::default().accept(j.prepare(original).unwrap()),
            Err(Error::QueryOnly)
        ));
    }
}

#[test]
fn duplicate_and_recovered_queued_prepared_are_query_only() {
    let root = Root::new();
    let mut j = root.open();
    let original = intent(1);
    let fresh = j.prepare(original.clone()).unwrap();
    let duplicate = j.prepare(original.clone()).unwrap();
    assert!(matches!(
        j.cancel_prepared(duplicate),
        Err(Error::QueryOnly)
    ));
    assert_eq!(
        j.read(&original.operation_id).unwrap(),
        Record::queued(original.clone()).unwrap()
    );
    drop(fresh);
    drop(j);
    let mut j = root.open();
    let recovered = j.prepare(original.clone()).unwrap();
    assert!(matches!(
        j.cancel_prepared(recovered),
        Err(Error::QueryOnly)
    ));
    assert_eq!(
        j.read(&original.operation_id).unwrap(),
        Record::queued(original).unwrap()
    );
}

#[test]
fn prior_generation_and_other_live_root_cannot_cancel_prepared() {
    let root = Root::new();
    let mut old = root.open();
    let original = intent(1);
    let prepared = old.prepare(original.clone()).unwrap();
    drop(old);
    let mut new = root.open();
    assert!(matches!(new.cancel_prepared(prepared), Err(Error::Busy)));
    assert_eq!(
        new.read(&original.operation_id).unwrap().status,
        Status::Queued
    );
    let other = Root::new();
    let mut other = other.open();
    let fresh = other.prepare(original.clone()).unwrap();
    assert!(matches!(new.cancel_prepared(fresh), Err(Error::QueryOnly)));
    assert_eq!(
        new.read(&original.operation_id).unwrap().status,
        Status::Queued
    );
    assert_eq!(
        other.read(&original.operation_id).unwrap().status,
        Status::Queued
    );
}

#[test]
fn replaced_lock_inode_rejects_cancellation_and_invalidates_execution() {
    let root = Root::new();
    let mut old = root.open();
    let original = intent(1);
    let prepared = old.prepare(original.clone()).unwrap();
    let pending_execution = old.prepare(intent(2)).unwrap();
    let lock = old
        .test_file(&original.operation_id)
        .parent()
        .unwrap()
        .join("writer.lock");
    fs::remove_file(lock).unwrap();
    let new = root.open();
    assert!(matches!(old.cancel_prepared(prepared), Err(Error::Busy)));
    assert!(matches!(
        Registry::default().accept(pending_execution),
        Err(Error::Busy)
    ));
    assert_eq!(
        new.read(&original.operation_id).unwrap().status,
        Status::Queued
    );
}

#[test]
fn any_advanced_record_or_changed_original_cannot_be_overwritten() {
    for state in [Status::Running, Status::Cancelled, Status::Completed] {
        let root = Root::new();
        let mut j = root.open();
        let original = intent(1);
        let prepared = j.prepare(original.clone()).unwrap();
        let mut advanced = j.read(&original.operation_id).unwrap();
        advanced.status = if state == Status::Completed {
            Status::Running
        } else {
            state
        };
        advanced.revision = 2;
        j.save(advanced.clone()).unwrap();
        if state == Status::Completed {
            advanced.status = Status::Completed;
            advanced.revision = 3;
            let mut actual = page();
            actual.page_id = AppPageId::PersonalLibrary;
            advanced.completion = Some(Completion::Navigation { page: actual });
            j.save(advanced.clone()).unwrap();
        }
        let bytes = fs::read(j.test_file(&original.operation_id)).unwrap();
        assert!(matches!(j.cancel_prepared(prepared), Err(Error::Conflict)));
        assert_eq!(j.read(&original.operation_id).unwrap(), advanced);
        assert_eq!(
            fs::read(j.test_file(&original.operation_id)).unwrap(),
            bytes
        );
    }
    let root = Root::new();
    let mut j = root.open();
    let original = intent(1);
    let prepared = j.prepare(original.clone()).unwrap();
    let mut changed = original.clone();
    changed.before.page_epoch += 1;
    let changed = Record::queued(changed).unwrap();
    fs::write(
        j.test_file(&original.operation_id),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(matches!(j.cancel_prepared(prepared), Err(Error::Conflict)));
    assert_eq!(j.read(&original.operation_id).unwrap(), changed);
}

#[test]
fn publish_and_directory_faults_never_return_a_confirmed_cancellation() {
    for fault in [Fault::AfterPublish, Fault::DirectorySync] {
        let root = Root::new();
        let mut j = root.open();
        let original = intent(1);
        let prepared = j.prepare(original.clone()).unwrap();
        j.execution_fence().fence();
        j.inject_fault(fault);
        assert!(matches!(j.cancel_prepared(prepared), Err(Error::Unknown)));
        let observed = j.query(&original.operation_id).unwrap();
        assert!(!observed.is_durable());
        assert_eq!(observed.recorded().status, Status::Cancelled);
        j.inject_fault(Fault::DirectorySync);
        assert!(matches!(
            j.confirm_query(&original.operation_id),
            Err(Error::Unknown)
        ));
        assert!(!j.query(&original.operation_id).unwrap().is_durable());
        let view = j
            .confirm_query(&original.operation_id)
            .unwrap()
            .into_recovery()
            .unwrap();
        assert_cancelled(&view, &original);
    }
}

#[test]
fn before_publish_or_unreadable_original_never_manufactures_cancellation() {
    let root = Root::new();
    let mut j = root.open();
    let original = intent(1);
    let prepared = j.prepare(original.clone()).unwrap();
    j.inject_fault(Fault::BeforePublish);
    assert!(matches!(j.cancel_prepared(prepared), Err(Error::Io)));
    assert_eq!(
        j.read(&original.operation_id).unwrap().status,
        Status::Queued
    );
    let confirmed = j
        .confirm_query(&original.operation_id)
        .unwrap()
        .into_recovery()
        .unwrap();
    assert!(confirmed.is_durable());
    assert_eq!(confirmed.effective_status(), Status::Queued);
    let original = intent(2);
    let prepared = j.prepare(original.clone()).unwrap();
    fs::remove_file(j.test_file(&original.operation_id)).unwrap();
    assert!(matches!(j.cancel_prepared(prepared), Err(Error::Io)));
    assert!(matches!(j.query(&original.operation_id), Err(Error::Io)));
}

#[test]
fn dispatched_record_has_no_fresh_capability_for_this_cancel_entry() {
    let root = Root::new();
    let mut j = root.open();
    let original = intent(1);
    let id = original.operation_id.clone();
    let mut registry = Registry::default();
    registry
        .accept(j.prepare(original.clone()).unwrap())
        .unwrap();
    let token = registry.grants.grant(authority()).unwrap();
    registry
        .approve(&id, &authority(), &token, &page())
        .unwrap();
    let proof = j
        .persist_running(registry.stage_running(&id).unwrap())
        .unwrap();
    registry
        .commit(proof, &authority(), &token, &page())
        .unwrap();
    let duplicate = j.prepare(original).unwrap();
    assert!(matches!(
        j.cancel_prepared(duplicate),
        Err(Error::QueryOnly)
    ));
    assert_eq!(j.read(&id).unwrap().status, Status::Running);
    let unknown = registry.cancel(&id).unwrap();
    assert_eq!(unknown.status, Status::OutcomeUnknown);
}

#[test]
fn worker_returns_confirmed_cancellation_after_fence_and_fifo_close() {
    let root = Root::new();
    let handle = root.spawn();
    let original = intent(1);
    let prepared = receive(submitted(handle.prepare(original.clone()))).unwrap();
    let release = paused(&handle);
    handle.fence();
    let cancelled = submitted(handle.cancel_prepared(prepared));
    let confirmation = submitted(handle.confirm_query(original.operation_id.clone()));
    let closed = handle.close();
    assert!(matches!(Journal::open(&root.0, scope()), Err(Error::Busy)));
    assert!(worker::poll_reply(&cancelled).is_none());
    assert_eq!(worker::poll_reply(&closed), None);
    release.send(()).unwrap();
    assert_cancelled(&receive(cancelled).unwrap(), &original);
    assert_cancelled(&receive(confirmation).unwrap(), &original);
    assert_eq!(receive(closed), Ok(()));
    let j = root.open();
    assert_eq!(
        disk_record(&j, &original.operation_id).status,
        Status::Cancelled
    );
    assert_eq!(disk_record(&j, &original.operation_id).revision, 2);
}

#[test]
fn full_queue_returns_same_prepared_which_can_still_be_accepted() {
    let root = Root::new();
    let handle = root.spawn();
    let original = intent(1);
    let prepared = receive(submitted(handle.prepare(original.clone()))).unwrap();
    let release = paused(&handle);
    let replies: Vec<_> = (0..worker::QUEUE_CAPACITY)
        .map(|_| submitted(handle.confirm_query(original.operation_id.clone())))
        .collect();
    let returned = match handle.cancel_prepared(prepared) {
        Err(error) => error,
        Ok(_) => panic!("queue must be full"),
    };
    assert_eq!(returned.kind, SubmitFailure::QueueFull);
    let mut registry = Registry::default();
    let accepted = registry.accept(returned.input).unwrap();
    assert_eq!(accepted.record, Record::queued(original).unwrap());
    release.send(()).unwrap();
    for reply in replies {
        assert_eq!(receive(reply).unwrap().effective_status(), Status::Queued);
    }
    assert_eq!(receive(handle.close()), Ok(()));
}

#[test]
fn full_queue_returned_prepared_can_retry_cancellation_and_full_close_drains_it() {
    let root = Root::new();
    let handle = root.spawn();
    let original = intent(1);
    let prepared = receive(submitted(handle.prepare(original.clone()))).unwrap();
    let release = paused(&handle);
    let replies: Vec<_> = (0..worker::QUEUE_CAPACITY)
        .map(|_| submitted(handle.confirm_query(original.operation_id.clone())))
        .collect();
    let error = match handle.cancel_prepared(prepared) {
        Err(error) => error,
        Ok(_) => panic!("queue must be full"),
    };
    assert_eq!(error.kind, SubmitFailure::QueueFull);
    release.send(()).unwrap();
    for reply in replies {
        assert!(receive(reply).unwrap().is_durable());
    }
    let release = paused(&handle);
    let cancelled = submitted(handle.cancel_prepared(error.input));
    let confirmations: Vec<_> = (1..worker::QUEUE_CAPACITY)
        .map(|_| submitted(handle.confirm_query(original.operation_id.clone())))
        .collect();
    let closed = handle.close();
    assert_eq!(worker::poll_reply(&closed), None);
    assert!(matches!(Journal::open(&root.0, scope()), Err(Error::Busy)));
    release.send(()).unwrap();
    assert_cancelled(&receive(cancelled).unwrap(), &original);
    for reply in confirmations {
        assert_cancelled(&receive(reply).unwrap(), &original);
    }
    assert_eq!(receive(closed), Ok(()));
    let j = root.open();
    assert_eq!(j.read(&original.operation_id).unwrap().revision, 2);
    assert_eq!(
        j.read(&original.operation_id).unwrap().status,
        Status::Cancelled
    );
}

#[test]
fn stopped_worker_returns_prepared_and_foreign_worker_rejects_owner() {
    let root = Root::new();
    let handle = root.spawn();
    let original = intent(1);
    let prepared = receive(submitted(handle.prepare(original.clone()))).unwrap();
    let release = paused(&handle);
    handle.crash_for_test();
    let pending = submitted(handle.confirm_query(original.operation_id.clone()));
    release.send(()).unwrap();
    assert!(matches!(receive(pending), Err(WorkerError::WorkerStopped)));
    let error = match handle.cancel_prepared(prepared) {
        Err(error) => error,
        Ok(_) => panic!("dead worker must reject"),
    };
    assert_eq!(error.kind, SubmitFailure::WorkerStopped);
    assert_eq!(receive(handle.close()), Err(WorkerError::WorkerStopped));
    let mut j = root.open();
    assert!(matches!(j.cancel_prepared(error.input), Err(Error::Busy)));
    assert_eq!(
        j.read(&original.operation_id).unwrap().status,
        Status::Queued
    );
    drop(j);
    let other_root = Root::new();
    let other = other_root.spawn();
    let owner = root.spawn();
    let prepared = receive(submitted(owner.prepare(intent(2)))).unwrap();
    assert!(matches!(
        receive(submitted(other.cancel_prepared(prepared))),
        Err(WorkerError::Core(Error::QueryOnly))
    ));
    assert_eq!(receive(owner.close()), Ok(()));
    assert_eq!(receive(other.close()), Ok(()));
}
