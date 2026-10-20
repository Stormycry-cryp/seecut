// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::agent_app_control_core as app_control;
use crate::agent_app_control_journal as journal;
use app_control::*;
use concat_editor_mcp::{AppPageId, AppViewBinding};
use journal::*;
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let base = std::env::temp_dir();
        let path = base.join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> Journal {
        Journal::open(&self.0, scope()).unwrap()
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
        session_id: "original-session".into(),
    }
}
fn page() -> AppViewBinding {
    AppViewBinding {
        page_id: AppPageId::Generation,
        page_epoch: 1,
        page_state_id: format!("app-page-v1-{}", "b".repeat(64)),
    }
}
fn intent() -> Intent {
    Intent {
        operation_id: operation_id("client", "original-session", 1).unwrap(),
        session_id: "original-session".into(),
        client_sequence: 1,
        authority: authority(),
        before: page(),
        target: Action::Navigate {
            target: AppPageId::PersonalLibrary,
        },
    }
}
fn approve(r: &mut Registry, id: &str) -> String {
    let token = r.grants.grant(authority()).unwrap();
    r.approve(id, &authority(), &token, &page()).unwrap();
    token
}
fn completion() -> Completion {
    let mut after = page();
    after.page_id = AppPageId::PersonalLibrary;
    after.page_epoch = 2;
    Completion::Navigation { page: after }
}

#[test]
fn fixed_after_publish_identical_retry_requires_directory_sync_before_saved() {
    let root = Root::new();
    let mut j = root.open();
    let mut r = Registry::default();
    let original = intent();
    let id = original.operation_id.clone();
    r.accept(j.prepare(original).unwrap()).unwrap();
    let token = approve(&mut r, &id);
    let write = r.stage_running(&id).unwrap();
    let proof = j.persist_running(write).unwrap();
    r.commit(proof, &authority(), &token, &page()).unwrap();
    let done = r.complete(&id, completion()).unwrap();
    j.inject_fault(Fault::AfterPublish);
    assert!(matches!(j.save(done.clone()), Err(Error::Unknown)));
    assert_eq!(j.read(&id).unwrap(), done);
    for _ in 0..2 {
        j.inject_fault(Fault::DirectorySync);
        assert!(matches!(j.save(done.clone()), Err(Error::Unknown)));
        let reply = r.query(&id).unwrap();
        assert!(!reply.durable);
        assert_eq!(reply.effective_status, Status::OutcomeUnknown);
    }
    let saved = j.save(done).unwrap();
    let reply = r.acknowledge(saved).unwrap();
    assert!(reply.durable);
    assert_eq!(reply.effective_status, Status::Completed);
    println!("verified: identical retry cannot acknowledge completed while DirectorySync fails");
}

#[test]
fn fixed_delayed_prepared_after_drop_and_new_lease_is_rejected() {
    let root = Root::new();
    let mut first = root.open();
    let original = intent();
    let delayed = first.prepare(original.clone()).unwrap();
    drop(first);
    let mut second = root.open();
    let mut r = Registry::default();
    assert!(matches!(
        r.accept(second.prepare(original).unwrap()),
        Err(Error::QueryOnly)
    ));
    assert!(matches!(r.accept(delayed), Err(Error::Busy)));
    println!(
        "verified: delayed prior-lease Prepared cannot restore the original record to execution"
    );
}

#[test]
fn fixed_running_write_requires_same_live_owner_and_rejects_dropped_owner() {
    let root = Root::new();
    let mut first = root.open();
    let mut r = Registry::default();
    let original = intent();
    let id = original.operation_id.clone();
    r.accept(first.prepare(original).unwrap()).unwrap();
    approve(&mut r, &id);
    let held = r.stage_running(&id).unwrap();
    drop(first);
    let mut second = root.open();
    assert!(matches!(second.persist_running(held), Err(Error::Busy)));
    assert_eq!(second.read(&id).unwrap().status, Status::Queued);

    // Both owners remain alive: matching record/scope still cannot substitute
    // a real owner from another journal root/generation.
    let aroot = Root::new();
    let broot = Root::new();
    let mut a = aroot.open();
    let mut b = broot.open();
    let original = intent();
    let id = original.operation_id.clone();
    let mut ar = Registry::default();
    ar.accept(a.prepare(original.clone()).unwrap()).unwrap();
    b.prepare(original).unwrap();
    approve(&mut ar, &id);
    let held = ar.stage_running(&id).unwrap();
    assert!(matches!(b.persist_running(held), Err(Error::QueryOnly)));
    assert_eq!(b.read(&id).unwrap().status, Status::Queued);
    println!("verified: RunningWrite rejects both a dropped owner and a different live owner");
}

#[test]
fn fixed_running_proof_after_journal_drop_cannot_commit() {
    let root = Root::new();
    let mut j = root.open();
    let mut r = Registry::default();
    let original = intent();
    let id = original.operation_id.clone();
    r.accept(j.prepare(original).unwrap()).unwrap();
    let token = approve(&mut r, &id);
    let proof = j.persist_running(r.stage_running(&id).unwrap()).unwrap();
    drop(j);
    let _replacement = root.open();
    assert!(matches!(
        r.commit(proof, &authority(), &token, &page()),
        Err(Error::Busy)
    ));
    assert!(!r.entries_dispatched_for_test(&id));
    println!("verified: Journal Drop fences an already issued RunningProof");
}

#[test]
fn fixed_running_proof_rejected_after_worker_detects_inode_replacement() {
    let root = Root::new();
    let mut old = root.open();
    let mut r = Registry::default();
    let original = intent();
    let id = original.operation_id.clone();
    r.accept(old.prepare(original).unwrap()).unwrap();
    let token = approve(&mut r, &id);
    let proof = old.persist_running(r.stage_running(&id).unwrap()).unwrap();
    let lock = old.test_file(&id).parent().unwrap().join("writer.lock");
    fs::remove_file(lock).unwrap();
    let _new = root.open();
    assert!(matches!(old.query(&id), Err(Error::Busy)));
    assert!(matches!(
        r.commit(proof, &authority(), &token, &page()),
        Err(Error::Busy)
    ));
    assert!(!r.entries_dispatched_for_test(&id));
    println!("verified: worker inode failure revokes its already issued RunningProof");
}

#[test]
fn fixed_host_memory_only_fence_blocks_prepared_and_proof_without_dropping_writer() {
    let root = Root::new();
    let mut j = root.open();
    let fence = j.execution_fence();
    let original = intent();
    let delayed_prepared = j.prepare(original).unwrap();
    let mut second = intent();
    second.client_sequence = 2;
    second.operation_id = operation_id("client", "original-session", 2).unwrap();
    let id = second.operation_id.clone();
    let mut r = Registry::default();
    r.accept(j.prepare(second).unwrap()).unwrap();
    let token = approve(&mut r, &id);
    let proof = j.persist_running(r.stage_running(&id).unwrap()).unwrap();
    fence.fence();
    let mut receiver = Registry::default();
    assert!(matches!(
        receiver.accept(delayed_prepared),
        Err(Error::Busy)
    ));
    assert!(matches!(
        r.commit(proof, &authority(), &token, &page()),
        Err(Error::Busy)
    ));
    assert!(!r.entries_dispatched_for_test(&id));
    assert_eq!(
        j.query(&id).unwrap().effective_status(),
        Status::OutcomeUnknown
    );
    assert!(matches!(Journal::open(&root.0, scope()), Err(Error::Busy)));
    // Fence is execution-only; worker retains a real live writer for cleanup.
    let cancelled = r.cancel(&id).unwrap();
    let saved = j.save(cancelled).unwrap();
    assert_eq!(
        r.acknowledge(saved).unwrap().effective_status,
        Status::Cancelled
    );
    println!(
        "verified: host ExecutionFence blocks pending execution with writer/cleanup still live; UI path is atomic memory only"
    );
}
