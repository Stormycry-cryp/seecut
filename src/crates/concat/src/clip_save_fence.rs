//! UI source invalidation independent of Studio, Bridge and Cloud borrows.
//! Only the original host gate arbitrates against commit; this is not a ledger.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::Deref;

use concat_host::projects::{SaveGateState, SaveInvalidation, SaveRevoker};

struct Entry {
    registration: u64,
    permit: u64,
    revoker: SaveRevoker,
}

thread_local! {
    static ENTRIES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static EPOCH: Cell<u64> = const { Cell::new(0) };

}

pub(crate) fn epoch() -> u64 {
    EPOCH.get()
}

/// Called on the UI thread before queuing the original frozen snapshot.
pub(crate) fn register(permit: u64, revoker: SaveRevoker) -> u64 {
    let registration = NEXT
        .get()
        .checked_add(1)
        .expect("native save registrations exhausted");
    NEXT.set(registration);
    ENTRIES.with(|entries| {
        let mut entries = entries.borrow_mut();
        entries.retain(|entry| matches!(entry.revoker.state(), SaveGateState::Pending));
        entries.push(Entry {
            registration,
            permit,
            revoker,
        });
    });
    registration
}

pub(crate) fn finish(registration: u64) {
    ENTRIES.with(|entries| {
        entries
            .borrow_mut()
            .retain(|entry| entry.registration != registration)
    });
}

/// This never borrows Studio/Bridge/Cloud and never waits for file IO.
pub(crate) fn invalidate(reason: SaveInvalidation) {
    EPOCH.set(EPOCH.get().saturating_add(1));
    let entries = ENTRIES.with(|entries| std::mem::take(&mut *entries.borrow_mut()));
    for entry in entries {
        // CommitAlreadyStarted deliberately preserves the real worker outcome.
        let _ = entry.revoker.invalidate(reason);
    }
}

pub(crate) fn revoke_permit(permit: u64) {
    ENTRIES.with(|entries| {
        entries.borrow_mut().retain(|entry| {
            if entry.permit == permit {
                let _ = entry.revoker.invalidate(SaveInvalidation::Revoked);
                false
            } else {
                true
            }
        });
    });
}

/// Trusted UI action prelude. Reads, publish and worker delivery do not call it.
pub(crate) fn before_change() {
    invalidate(SaveInvalidation::BindingUnstable);
}

/// Read access can dereference the collection, but mutable access must pass the
/// pre-change fence. This covers selection writes from native callbacks too.
#[derive(Default)]
pub(crate) struct Selection<T>(T);

impl<T> Deref for Selection<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: PartialEq> Selection<T> {
    pub(crate) fn replace(&mut self, next: T) {
        if self.0 != next {
            before_change();
            self.0 = next;
        }
    }
}

impl Selection<Vec<String>> {
    pub(crate) fn clear(&mut self) {
        if !self.0.is_empty() {
            before_change();
            self.0.clear();
        }
    }
    pub(crate) fn push(&mut self, value: String) {
        before_change();
        self.0.push(value);
    }
    pub(crate) fn retain(&mut self, predicate: impl FnMut(&String) -> bool) {
        before_change();
        self.0.retain(predicate);
    }
}

impl Selection<HashSet<String>> {
    pub(crate) fn clear(&mut self) {
        if !self.0.is_empty() {
            before_change();
            self.0.clear();
        }
    }
    pub(crate) fn remove(&mut self, value: &str) -> bool {
        if self.0.contains(value) {
            before_change();
        }
        self.0.remove(value)
    }
    pub(crate) fn insert(&mut self, value: String) -> bool {
        if !self.0.contains(&value) {
            before_change();
        }
        self.0.insert(value)
    }
    pub(crate) fn drain(&mut self) -> std::collections::hash_set::Drain<'_, String> {
        if !self.0.is_empty() {
            before_change();
        }
        self.0.drain()
    }
}

// The local-dev picker runs on a worker. Acquire its guard on the UI thread
// before dispatch so revocation is synchronous; the worker may own/drop it.
static NATIVE_DIALOGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Process-visible native dialog lifetime, acquired before original UI mutation.
pub(crate) struct NativeDialog;

impl NativeDialog {
    pub(crate) fn enter() -> Self {
        before_change();
        NATIVE_DIALOGS
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |count| count.checked_add(1),
            )
            .expect("native dialog nesting exhausted");
        Self
    }
}
impl Drop for NativeDialog {
    fn drop(&mut self) {
        NATIVE_DIALOGS
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |count| count.checked_sub(1),
            )
            .expect("native dialog lifetime mismatch");
    }
}
pub(crate) fn native_dialog_open() -> bool {
    NATIVE_DIALOGS.load(std::sync::atomic::Ordering::Acquire) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    static DIALOG_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn nested_native_dialog_scope_remains_busy_until_last_return() {
        let _serial = DIALOG_TEST.lock().unwrap();
        assert!(!native_dialog_open());
        let before = epoch();
        let outer = NativeDialog::enter();
        assert!(native_dialog_open());
        {
            let _inner = NativeDialog::enter();
            assert!(native_dialog_open());
        }
        assert!(native_dialog_open());
        drop(outer);
        assert!(!native_dialog_open());
        assert_eq!(epoch(), before + 2);
    }

    #[test]
    fn worker_owned_native_dialog_is_visible_to_ui_and_revokes_before_dispatch() {
        let _serial = DIALOG_TEST.lock().unwrap();
        let pending = test_support::PendingSave::new();
        let dialog = NativeDialog::enter();
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, returned) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _dialog = dialog;
            entered.send(()).unwrap();
            returned
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        });
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(native_dialog_open());
        pending.assert_not_committed();
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(!native_dialog_open());
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use concat_host::{projects, session::Session};
    use std::time::{Duration, Instant};

    /// Real owned Session/snapshot used to exercise native source entry points.
    pub(crate) struct PendingSave {
        session: Session,
        lane: projects::SaveLane,
        snapshot: projects::AgentSaveSnapshot,
        registration: u64,
        original: Vec<u8>,
        _dir: tempfile::TempDir,
    }
    impl PendingSave {
        pub(crate) fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut session = projects::create_owned(
                &dir.path().to_string_lossy(),
                "Native source",
                1280,
                720,
                30,
                1,
            )
            .unwrap()
            .into_session()
            .unwrap();
            let original =
                std::fs::read(std::path::Path::new(session.path()).join("concat.json")).unwrap();
            session.apply(concat_project::Command::AddTrack).unwrap();
            let lane = projects::SaveLane::default();
            let (snapshot, revoker) = session
                .prepare_agent_save(&lane, Instant::now() + Duration::from_secs(60))
                .unwrap();
            let registration = register(91, revoker);
            Self {
                session,
                lane,
                snapshot,
                registration,
                original,
                _dir: dir,
            }
        }
        pub(crate) fn assert_not_committed(self) {
            assert!(matches!(
                self.lane.write_agent_save(self.snapshot),
                projects::AgentSaveResult::NotCommitted {
                    commit_claimed: false,
                    rename_attempted: false,
                    ..
                }
            ));
            assert_eq!(
                self.original,
                std::fs::read(std::path::Path::new(self.session.path()).join("concat.json"))
                    .unwrap()
            );
            finish(self.registration);
        }
    }
}
