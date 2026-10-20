// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::agent_app_control_core as app_control;
use crate::agent_app_control_journal as journal;
use app_control::*;
use concat_editor_mcp::{AppPageId, AppViewBinding};
use journal::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let base = std::env::temp_dir();
        let path = base.join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn open(&self, sid: &str) -> Journal {
        Journal::open(&self.0, scope(sid)).unwrap()
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
fn page() -> AppViewBinding {
    AppViewBinding {
        page_id: AppPageId::Generation,
        page_epoch: 1,
        page_state_id: format!("app-page-v1-{}", "b".repeat(64)),
    }
}
fn scope(sid: &str) -> QueryScope {
    QueryScope {
        account_scope: authority().account_scope,
        client_id: authority().client_id,
        session_id: sid.into(),
    }
}
fn intent(sid: &str, n: u64) -> Intent {
    Intent {
        operation_id: operation_id("client", sid, n).unwrap(),
        session_id: sid.into(),
        client_sequence: n,
        authority: authority(),
        before: page(),
        target: Action::Navigate {
            target: AppPageId::PersonalLibrary,
        },
    }
}
fn accepted(j: &mut Journal, r: &mut Registry, intent: Intent) -> String {
    let id = intent.operation_id.clone();
    r.accept(j.prepare(intent).unwrap()).unwrap();
    id
}
fn approved(r: &mut Registry, id: &str) -> String {
    let token = r.grants.grant(authority()).unwrap();
    r.approve(id, &authority(), &token, &page()).unwrap();
    token
}
fn running(j: &mut Journal, r: &mut Registry, id: &str, token: &str) -> HostDispatch {
    let write = r.stage_running(id).unwrap();
    let proof = j.persist_running(write).unwrap();
    r.commit(proof, &authority(), token, &page()).unwrap()
}
fn done() -> Completion {
    let mut after = page();
    after.page_id = AppPageId::PersonalLibrary;
    after.page_epoch = 2;
    Completion::Navigation { page: after }
}

#[test]
fn canonical_identity_is_length_delimited_and_safe() {
    assert_ne!(
        operation_id("a:b", "c", 1).unwrap(),
        operation_id("a", "b:c", 1).unwrap()
    );
    assert_eq!(operation_id("é", "s", 1).unwrap(), "sca1:1:2:é:1:s:1");
    assert_eq!(operation_id("c", "s", 0), Err(Error::InvalidInput));
    assert_eq!(
        operation_id("c", "s", SAFE_INTEGER + 1),
        Err(Error::InvalidInput)
    );
}
#[test]
fn rejects_generic_actions_paths_and_unknown_fields() {
    let data = serde_json::to_value(intent("s", 1)).unwrap();
    let mut invalid = data.clone();
    invalid["target"] = serde_json::json!({"action":"generate","prompt":"x"});
    assert!(serde_json::from_value::<Intent>(invalid).is_err());
    let mut invalid = data.clone();
    invalid["target"] = serde_json::json!({"action":"openProject","projectEntryId":"/tmp/project"});
    assert!(serde_json::from_value::<Intent>(invalid).is_err());
    let mut invalid = data;
    invalid["writeToken"] = serde_json::json!("x");
    assert!(serde_json::from_value::<Intent>(invalid).is_err());
}
#[test]
fn old_token_regrant_and_each_authority_axis_fail() {
    let mut g = Grants::default();
    let a = authority();
    let old = g.grant(a.clone()).unwrap();
    let new = g.grant(a.clone()).unwrap();
    assert!(!g.authorized(&a, &old));
    assert!(g.authorized(&a, &new));
    for index in 0..5 {
        let mut other = a.clone();
        match index {
            0 => other.instance_id = "other".into(),
            1 => other.client_id = "other".into(),
            2 => other.account_scope = "c".repeat(64),
            3 => other.account_epoch += 1,
            _ => other.app_session_id = "other".into(),
        };
        assert!(!g.authorized(&other, &new));
    }
    g.revoke("client");
    assert!(!g.authorized(&a, &new));
}
#[test]
fn durable_before_ui_and_exactly_once_replay() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let original = intent("s", 1);
    let id = accepted(&mut j, &mut r, original.clone());
    let t = approved(&mut r, &id);
    assert_eq!(j.query(&id).unwrap().recorded().status, Status::Queued);
    let dispatch = running(&mut j, &mut r, &id, &t);
    assert_eq!(dispatch.correlation_id, id);
    assert_eq!(dispatch.intent, original);
    assert_eq!(
        j.query(&id).unwrap().effective_status(),
        Status::OutcomeUnknown
    );
    let terminal = r.complete(&id, done()).unwrap();
    assert_eq!(
        r.query(&id).unwrap().effective_status,
        Status::OutcomeUnknown
    );
    r.acknowledge(j.save(terminal).unwrap()).unwrap();
    assert_eq!(r.query(&id).unwrap().effective_status, Status::Completed);
    assert!(matches!(r.stage_running(&id), Err(Error::Conflict)));
    assert!(matches!(
        r.accept(j.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
    assert_eq!(j.query(&id).unwrap().recorded().status, Status::Completed);
}
#[test]
fn same_id_changed_target_or_before_is_conflict() {
    let root = Root::new();
    let mut j = root.open("s");
    let original = intent("s", 1);
    j.prepare(original.clone()).unwrap();
    let mut changed = original.clone();
    changed.target = Action::Navigate {
        target: AppPageId::Credits,
    };
    assert!(matches!(j.prepare(changed), Err(Error::Conflict)));
    let mut changed = original;
    changed.before.page_epoch = 2;
    assert!(matches!(j.prepare(changed), Err(Error::Conflict)));
}
#[test]
fn source_page_change_during_worker_write_never_dispatches() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    let proof = j.persist_running(r.stage_running(&id).unwrap()).unwrap();
    let mut stale = page();
    stale.page_epoch += 2;
    assert!(matches!(
        r.commit(proof, &authority(), &t, &stale),
        Err(Error::StalePage)
    ));
    assert!(!r.entries_dispatched_for_test(&id));
    let saved = r.cancel(&id).unwrap();
    r.acknowledge(j.save(saved).unwrap()).unwrap();
    assert_eq!(j.read(&id).unwrap().status, Status::Cancelled);
}
#[test]
fn revoke_between_queued_and_running_ack_fences_callback() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    let write = r.stage_running(&id).unwrap();
    let cancellations = r.revoke("client").unwrap();
    let proof = j.persist_running(write).unwrap();
    assert!(matches!(
        r.commit(proof, &authority(), &t, &page()),
        Err(Error::Cancelled)
    ));
    for record in cancellations {
        r.acknowledge(j.save(record).unwrap()).unwrap();
    }
    assert_eq!(j.read(&id).unwrap().status, Status::Cancelled);
}
#[test]
fn account_change_between_worker_and_ui_fences_all_clients() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    let proof = j.persist_running(r.stage_running(&id).unwrap()).unwrap();
    r.invalidate_all().unwrap();
    let mut changed = authority();
    changed.account_epoch += 1;
    assert!(matches!(
        r.commit(proof, &changed, &t, &page()),
        Err(Error::Cancelled)
    ));
}
#[test]
fn cancel_after_dispatch_remains_unknown_until_original_host_proof() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    running(&mut j, &mut r, &id, &t);
    let cancelled = r.cancel(&id).unwrap();
    r.acknowledge(j.save(cancelled).unwrap()).unwrap();
    assert_eq!(r.query(&id).unwrap().record.status, Status::OutcomeUnknown);
    assert!(matches!(r.stage_running(&id), Err(Error::Conflict)));
    let completed = r.complete(&id, done()).unwrap();
    r.acknowledge(j.save(completed).unwrap()).unwrap();
    assert_eq!(j.read(&id).unwrap().status, Status::Completed);
}
#[test]
fn restart_queued_and_running_are_query_only_without_new_scope_binding() {
    let root = Root::new();
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    {
        let mut j = root.open("s");
        let mut r = Registry::default();
        accepted(&mut j, &mut r, original.clone());
        let t = approved(&mut r, &id);
        running(&mut j, &mut r, &id, &t);
    }
    let mut restored = root.open("s");
    let view = restored.query(&id).unwrap();
    assert_eq!(view.recorded().intent.authority.instance_id, "instance");
    assert_eq!(view.effective_status(), Status::OutcomeUnknown);
    assert!(!view.execution_recoverable());
    let mut r = Registry::default();
    assert!(matches!(
        r.accept(restored.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
    assert!(matches!(root.open("other-sid").query(&id), Err(Error::Io)));
    let mut other = scope("s");
    other.client_id = "other".into();
    assert!(matches!(
        Journal::open(&root.0, other).unwrap().query(&id),
        Err(Error::Io)
    ));
    let mut other = scope("s");
    other.account_scope = "d".repeat(64);
    assert!(matches!(
        Journal::open(&root.0, other).unwrap().query(&id),
        Err(Error::Io)
    ));
}
#[test]
fn real_os_lock_cannot_be_reopened_until_fd_drops() {
    let root = Root::new();
    let first = root.open("s");
    assert!(matches!(
        Journal::open(&root.0, scope("s")),
        Err(Error::Busy)
    ));
    drop(first);
    root.open("s");
}
#[test]
fn incomplete_creation_never_reaches_registry_and_ambiguous_publish_only_queries() {
    let root = Root::new();
    let mut j = root.open("s");
    j.inject_fault(Fault::BeforePublish);
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    assert!(matches!(j.prepare(original.clone()), Err(Error::Io)));
    assert!(matches!(j.query(&id), Err(Error::Io)));
    j.inject_fault(Fault::AfterPublish);
    assert!(matches!(j.prepare(original.clone()), Err(Error::Unknown)));
    assert_eq!(j.query(&id).unwrap().recorded().status, Status::Queued);
    let mut r = Registry::default();
    assert!(matches!(
        r.accept(j.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
}
#[test]
fn failed_terminal_write_never_acknowledges_success_or_replays_effect() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    running(&mut j, &mut r, &id, &t);
    let record = r.complete(&id, done()).unwrap();
    j.inject_fault(Fault::BeforePublish);
    assert!(matches!(j.save(record), Err(Error::Io)));
    assert_eq!(
        r.query(&id).unwrap().effective_status,
        Status::OutcomeUnknown
    );
    assert!(!r.query(&id).unwrap().durable);
    assert!(matches!(r.stage_running(&id), Err(Error::Conflict)));
    assert_eq!(
        j.query(&id).unwrap().effective_status(),
        Status::OutcomeUnknown
    );
}
#[test]
fn exact_human_pending_uses_original_hook_and_no_resume_action() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    running(&mut j, &mut r, &id, &t);
    let waiting = r
        .needs_human(&id, HumanGate::DocumentLeaveConfirmation)
        .unwrap();
    r.acknowledge(j.save(waiting).unwrap()).unwrap();
    assert_eq!(
        r.query(&id).unwrap().record.human_gate,
        Some(HumanGate::DocumentLeaveConfirmation)
    );
    assert!(matches!(r.stage_running(&id), Err(Error::Conflict)));
    let terminal = r.complete(&id, done()).unwrap();
    r.acknowledge(j.save(terminal).unwrap()).unwrap();
}
#[test]
fn capacity_is_per_original_coto_session_and_never_evicts() {
    let root = Root::new();
    let mut j = root.open("s");
    for n in 1..=MAX_OPERATIONS as u64 {
        j.prepare(intent("s", n)).unwrap();
    }
    assert!(matches!(
        j.prepare(intent("s", MAX_OPERATIONS as u64 + 1)),
        Err(Error::ResourceLimit)
    ));
    assert_eq!(
        j.query(&intent("s", 1).operation_id)
            .unwrap()
            .recorded()
            .status,
        Status::Queued
    );
    root.open("new-sid").prepare(intent("new-sid", 1)).unwrap();
}
#[test]
fn closed_schema_size_and_symlink_boundaries_fail_closed() {
    let root = Root::new();
    let mut j = root.open("s");
    let i = intent("s", 1);
    let id = i.operation_id.clone();
    j.prepare(i).unwrap();
    let file = j.test_file(&id);
    fs::write(&file, vec![b'x'; MAX_RECORD_BYTES + 1]).unwrap();
    assert!(matches!(j.read(&id), Err(Error::ResourceLimit)));
    fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink("missing", &file).unwrap();
    assert!(matches!(j.read(&id), Err(Error::Io)));
    assert_eq!(
        fs::metadata(&root.0).unwrap().permissions().mode() & 0o077,
        0
    );
}
#[test]
fn old_lease_path_replacement_cannot_keep_writing() {
    let root = Root::new();
    let mut old = root.open("s");
    let record = intent("s", 1);
    let id = record.operation_id.clone();
    old.prepare(record).unwrap();
    let lock = old.test_file(&id).parent().unwrap().join("writer.lock");
    fs::remove_file(&lock).unwrap();
    let _new = root.open("s");
    assert!(matches!(old.prepare(intent("s", 2)), Err(Error::Busy)));
    assert!(matches!(old.query(&id), Err(Error::Busy)));
}
#[test]
fn persisted_schema_corruption_and_total_bytes_block_before_new_intent() {
    let root = Root::new();
    let mut j = root.open("s");
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    j.prepare(original.clone()).unwrap();
    let path = j.test_file(&id);
    let mut raw = serde_json::to_value(j.read(&id).unwrap()).unwrap();
    raw["revision"] = serde_json::json!(-1);
    fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(matches!(j.read(&id), Err(Error::Corrupt)));
    fs::write(
        &path,
        serde_json::to_vec(&Record::queued(original).unwrap()).unwrap(),
    )
    .unwrap();
    let blocker = path.parent().unwrap().join("unrecognized-record");
    fs::write(&blocker, vec![0; MAX_SESSION_BYTES]).unwrap();
    let next = intent("s", 2);
    let next_path = j.test_file(&next.operation_id);
    assert!(matches!(j.prepare(next), Err(Error::ResourceLimit)));
    assert!(!next_path.exists());
}
#[test]
fn registry_cannot_combine_capacity_or_authority_from_other_session() {
    let root = Root::new();
    let mut first = root.open("s");
    let mut other = root.open("other");
    let mut r = Registry::default();
    accepted(&mut first, &mut r, intent("s", 1));
    assert!(matches!(
        r.accept(other.prepare(intent("other", 1)).unwrap()),
        Err(Error::NotAuthorized)
    ));
}
#[test]
fn running_persistence_failure_cannot_create_a_dispatch_or_reuse_revision() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    approved(&mut r, &id);
    let write = r.stage_running(&id).unwrap();
    assert_eq!(r.query(&id).unwrap().record.revision, 2);
    j.inject_fault(Fault::BeforePublish);
    assert!(matches!(j.persist_running(write), Err(Error::Io)));
    assert_eq!(
        r.query(&id).unwrap().effective_status,
        Status::OutcomeUnknown
    );
    assert!(!r.entries_dispatched_for_test(&id));
    assert!(matches!(r.stage_running(&id), Err(Error::Conflict)));
    let cancelled = r.cancel(&id).unwrap();
    assert_eq!(cancelled.revision, 3);
    // The missing running revision is not silently manufactured by a later write.
    assert!(matches!(j.save(cancelled), Err(Error::Conflict)));
    assert_eq!(j.read(&id).unwrap().status, Status::Queued);
}
#[test]
fn regression_after_publish_identical_retry_rechecks_file_and_directory_barriers() {
    let root = Root::new();
    let mut j = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut j, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    running(&mut j, &mut r, &id, &t);
    let done = r.complete(&id, done()).unwrap();
    j.inject_fault(Fault::AfterPublish);
    assert!(matches!(j.save(done.clone()), Err(Error::Unknown)));
    // Readable JSON equality does not establish the missing directory barrier.
    assert_eq!(j.read(&id).unwrap(), done);
    assert!(!r.query(&id).unwrap().durable);
    j.inject_fault(Fault::DirectorySync);
    assert!(matches!(j.save(done.clone()), Err(Error::Unknown)));
    assert!(!r.query(&id).unwrap().durable);
    j.inject_fault(Fault::DirectorySync);
    assert!(matches!(j.save(done.clone()), Err(Error::Unknown)));
    assert_eq!(
        r.query(&id).unwrap().effective_status,
        Status::OutcomeUnknown
    );
    let saved = j.save(done).unwrap();
    let reply = r.acknowledge(saved).unwrap();
    assert!(reply.durable);
    assert_eq!(reply.effective_status, Status::Completed);
}
#[test]
fn regression_delayed_prepared_from_dropped_writer_never_enters_new_lease() {
    let root = Root::new();
    let mut first = root.open("s");
    let original = intent("s", 1);
    let delayed = first.prepare(original.clone()).unwrap();
    drop(first);
    let mut second = root.open("s");
    let mut r = Registry::default();
    assert!(matches!(
        r.accept(second.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
    assert!(matches!(r.accept(delayed), Err(Error::Busy)));
}
#[test]
fn regression_worker_inode_failure_fences_its_already_issued_running_proof() {
    let root = Root::new();
    let mut old = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut old, &mut r, intent("s", 1));
    let t = approved(&mut r, &id);
    let proof = old.persist_running(r.stage_running(&id).unwrap()).unwrap();
    let lock = old.test_file(&id).parent().unwrap().join("writer.lock");
    fs::remove_file(lock).unwrap();
    let _new = root.open("s");
    assert!(matches!(old.query(&id), Err(Error::Busy)));
    assert!(matches!(
        r.commit(proof, &authority(), &t, &page()),
        Err(Error::Busy)
    ));
    assert!(!r.entries_dispatched_for_test(&id));
}
#[test]
fn held_running_write_cannot_be_advanced_by_another_writer_generation() {
    let root = Root::new();
    let mut first = root.open("s");
    let mut r = Registry::default();
    let id = accepted(&mut first, &mut r, intent("s", 1));
    approved(&mut r, &id);
    let held = r.stage_running(&id).unwrap();
    drop(first);
    let mut second = root.open("s");
    assert!(matches!(second.persist_running(held), Err(Error::Busy)));
    assert_eq!(second.read(&id).unwrap().status, Status::Queued);
}
#[test]
fn normal_host_execution_fence_covers_prepared_not_yet_accepted_and_never_revives() {
    let root = Root::new();
    let mut j = root.open("s");
    let fence = j.execution_fence();
    let delayed = j.prepare(intent("s", 1)).unwrap();
    fence.fence();
    let mut r = Registry::default();
    assert!(matches!(r.accept(delayed), Err(Error::Busy)));
    r.grants.grant(authority()).unwrap();
    assert!(matches!(
        r.accept(j.prepare(intent("s", 2)).unwrap()),
        Err(Error::Busy)
    ));
    // The lease still owns the flock and can persist cleanup/query facts on worker.
    assert_eq!(
        j.query(&intent("s", 1).operation_id)
            .unwrap()
            .recorded()
            .status,
        Status::Queued
    );
}

#[test]
fn recovered_completed_requires_real_barrier_and_never_reenters_registry() {
    let root = Root::new();
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    let (recorded, bytes) = {
        let mut j = root.open("s");
        let mut r = Registry::default();
        accepted(&mut j, &mut r, original.clone());
        let t = approved(&mut r, &id);
        running(&mut j, &mut r, &id, &t);
        let terminal = r.complete(&id, done()).unwrap();
        j.inject_fault(Fault::AfterPublish);
        assert!(matches!(j.save(terminal.clone()), Err(Error::Unknown)));
        (terminal, fs::read(j.test_file(&id)).unwrap())
    };
    let mut j = root.open("s");
    let read = j.query(&id).unwrap();
    assert_eq!(read.recorded(), &recorded);
    assert!(!read.is_durable());
    assert_eq!(read.effective_status(), Status::OutcomeUnknown);
    for _ in 0..2 {
        j.inject_fault(Fault::DirectorySync);
        assert!(matches!(j.confirm_query(&id), Err(Error::Unknown)));
        assert!(!j.query(&id).unwrap().is_durable());
    }
    let confirmed = j.confirm_query(&id).unwrap().into_recovery().unwrap();
    assert!(confirmed.is_durable());
    assert_eq!(confirmed.effective_status(), Status::Completed);
    assert_eq!(confirmed.recorded(), &recorded);
    assert!(!confirmed.execution_recoverable());
    assert_eq!(fs::read(j.test_file(&id)).unwrap(), bytes);
    // A later read cannot reuse the earlier barrier as a new proof.
    assert!(!j.query(&id).unwrap().is_durable());
    let mut fresh = Registry::default();
    fresh.grants.grant(authority()).unwrap();
    assert!(matches!(fresh.query(&id), Err(Error::InvalidInput)));
    assert!(matches!(
        fresh.accept(j.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
    assert!(matches!(fresh.stage_running(&id), Err(Error::InvalidInput)));
}

#[test]
fn recovered_running_barrier_stays_unknown_and_does_not_revive_execution() {
    let root = Root::new();
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    {
        let mut j = root.open("s");
        let mut r = Registry::default();
        accepted(&mut j, &mut r, original.clone());
        approved(&mut r, &id);
        j.persist_running(r.stage_running(&id).unwrap()).unwrap();
        // Crash window: running is durable, but no HostDispatch was created.
        assert!(!r.entries_dispatched_for_test(&id));
    }
    let mut j = root.open("s");
    let before = j.read(&id).unwrap();
    j.execution_fence().fence();
    let recovered = j.confirm_query(&id).unwrap().into_recovery().unwrap();
    assert!(recovered.is_durable());
    assert_eq!(recovered.recorded(), &before);
    assert_eq!(recovered.effective_status(), Status::OutcomeUnknown);
    assert!(!recovered.execution_recoverable());
    let mut fresh = Registry::default();
    assert!(matches!(
        fresh.accept(j.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
    assert!(matches!(fresh.stage_running(&id), Err(Error::InvalidInput)));
}

#[test]
fn recovery_saved_from_dropped_writer_cannot_be_consumed_by_new_lease() {
    let root = Root::new();
    let mut first = root.open("s");
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    first.prepare(original).unwrap();
    let saved = first.confirm_query(&id).unwrap();
    drop(first);
    let second = root.open("s");
    assert!(matches!(saved.into_recovery(), Err(Error::Busy)));
    assert!(!second.query(&id).unwrap().is_durable());
}

#[test]
fn recovery_saved_rejects_owner_invalidated_by_lock_inode_detection() {
    let root = Root::new();
    let mut first = root.open("s");
    let original = intent("s", 1);
    let id = original.operation_id.clone();
    first.prepare(original).unwrap();
    let saved = first.confirm_query(&id).unwrap();
    let lock = first.test_file(&id).parent().unwrap().join("writer.lock");
    fs::remove_file(lock).unwrap();
    let _replacement = root.open("s");
    assert!(matches!(first.query(&id), Err(Error::Busy)));
    assert!(matches!(saved.into_recovery(), Err(Error::Busy)));
}

#[test]
fn all_eight_navigation_targets_keep_real_typed_contract_and_completed_binding() {
    for target in [
        AppPageId::Generation,
        AppPageId::PersonalLibrary,
        AppPageId::TeamLibrary,
        AppPageId::Templates,
        AppPageId::Account,
        AppPageId::Credits,
        AppPageId::Settings,
        AppPageId::Projects,
    ] {
        let root = Root::new();
        let mut j = root.open("s");
        let mut r = Registry::default();
        let mut original = intent("s", 1);
        original.target = Action::Navigate { target };
        let id = accepted(&mut j, &mut r, original);
        let token = approved(&mut r, &id);
        let dispatch = running(&mut j, &mut r, &id, &token);
        assert_eq!(dispatch.intent.target, Action::Navigate { target });
        let mut actual = page();
        actual.page_id = target;
        actual.page_epoch = 7;
        let record = r
            .complete(
                &id,
                Completion::Navigation {
                    page: actual.clone(),
                },
            )
            .unwrap();
        let reply = r.acknowledge(j.save(record).unwrap()).unwrap();
        assert!(reply.durable);
        assert_eq!(
            reply.record.completion,
            Some(Completion::Navigation { page: actual })
        );
    }
}

#[test]
fn removed_project_action_and_receipt_fields_are_closed_schema_errors() {
    let mut value = serde_json::to_value(intent("s", 1)).unwrap();
    value["target"] = serde_json::json!({"action":"openProject","projectEntryId":format!("app-item-{}",uuid::Uuid::new_v4())});
    assert!(serde_json::from_value::<Intent>(value).is_err());
    let mut record = serde_json::to_value(Record::queued(intent("s", 1)).unwrap()).unwrap();
    record["hostRequestId"] = serde_json::json!("old-open-id");
    assert!(serde_json::from_value::<Record>(record).is_err());
    assert!(
        serde_json::from_value::<Completion>(
            serde_json::json!({"kind":"projectAdopted","hostRequestId":"old","document":{}})
        )
        .is_err()
    );
}
