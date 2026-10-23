// SPDX-License-Identifier: AGPL-3.0-or-later
//! In-memory ordering only. This module performs no IO and never accesses UI.
//! A successful claim means commit STARTED, never rename or durability success.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

const PENDING: u8 = 0;
const COMMITTING: u8 = 1;
const FINISHED: u8 = 2;

/// Native lifecycle invalidation. No serialization or model-created authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SaveInvalidation {
    /// The native grant or token was revoked.
    Revoked = 16,
    /// The original account or workspace changed.
    AccountChanged = 17,
    /// The original conversation changed.
    ConversationChanged = 18,
    /// The original document changed or its Session was dropped.
    DocumentChanged = 19,
    /// The frozen document revision may have changed.
    RevisionChanged = 20,
    /// The original editor binding or gesture is no longer stable.
    BindingUnstable = 21,
    /// The original operation was stopped before its claim.
    Stopped = 22,
    /// A newer save replaced this pending operation.
    Superseded = 23,
    /// The original monotonic deadline expired.
    Expired = 24,
    /// The one-shot execution capability was dropped.
    Discarded = 25,
}

impl SaveInvalidation {
    fn from_state(value: u8) -> Self {
        match value {
            16 => Self::Revoked,
            17 => Self::AccountChanged,
            18 => Self::ConversationChanged,
            19 => Self::DocumentChanged,
            20 => Self::RevisionChanged,
            21 => Self::BindingUnstable,
            22 => Self::Stopped,
            23 => Self::Superseded,
            24 => Self::Expired,
            25 => Self::Discarded,
            _ => unreachable!("private gate state"),
        }
    }
}

/// Observable lifecycle of one original save gate; it is not a save receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveGateState {
    /// The permit can still compete for commit authority.
    Pending,
    /// The commit-start CAS succeeded; rename and durability are unproven.
    Committing,
    /// The worker recorded a terminal result; consult that result for success.
    Finished,
    /// The permit was invalidated before commit started.
    Invalidated(SaveInvalidation),
}

/// Result of attempting to invalidate one original pending save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevokeSaveResult {
    /// This call invalidated the pending permit.
    Invalidated,
    /// An earlier invalidation won; its original reason is retained.
    AlreadyInvalidated(SaveInvalidation),
    /// Commit already started and cannot be cancelled by this call.
    CommitAlreadyStarted,
    /// The worker recorded a terminal result; consult that result for success.
    Finished,
}

/// Reason the original one-shot permit could not claim this lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavePermitRefusal {
    /// The permit belongs to a different lane allocation.
    WrongLane,
    /// The permit was invalidated before commit started.
    Invalidated(SaveInvalidation),
}

struct Gate {
    state: AtomicU8,
}

impl Gate {
    fn invalidate(&self, reason: SaveInvalidation) -> RevokeSaveResult {
        match self.state.compare_exchange(
            PENDING,
            reason as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => RevokeSaveResult::Invalidated,
            Err(COMMITTING) => RevokeSaveResult::CommitAlreadyStarted,
            Err(FINISHED) => RevokeSaveResult::Finished,
            Err(value) => RevokeSaveResult::AlreadyInvalidated(SaveInvalidation::from_state(value)),
        }
    }
}

/// A trusted native caller may invalidate pending work without taking an IO
/// lock. Retain this original handle on every relevant lifecycle change.
/// Revocation after Committing prevents future work through the UI registry;
/// it cannot cancel or relabel this already-started original operation.
#[derive(Clone)]
pub struct SaveRevoker {
    gate: Arc<Gate>,
}

impl SaveRevoker {
    /// Atomically invalidates Pending without IO; a started commit keeps its result.
    pub fn invalidate(&self, reason: SaveInvalidation) -> RevokeSaveResult {
        self.gate.invalidate(reason)
    }

    /// Returns the current gate state without waiting for IO.
    pub fn state(&self) -> SaveGateState {
        match self.gate.state.load(Ordering::Acquire) {
            PENDING => SaveGateState::Pending,
            COMMITTING => SaveGateState::Committing,
            FINISHED => SaveGateState::Finished,
            value => SaveGateState::Invalidated(SaveInvalidation::from_state(value)),
        }
    }
}

/// Execution capability is deliberately not Clone and has no public constructor.
/// The same allocation also binds the exact lane and ticket; copying a numeric
/// ticket is never sufficient authority. Dropping it cannot cause any IO.
pub(crate) struct SavePermit {
    order: Arc<SaveOrder>,
    gate: Arc<Gate>,
    ticket: u64,
    deadline: Instant,
}

impl SavePermit {
    /// Returns the ticket bound to this sealed permit.
    pub(crate) fn ticket(&self) -> u64 {
        self.ticket
    }
}

impl Drop for SavePermit {
    fn drop(&mut self) {
        // A consumed or abandoned execution object can never leave reusable
        // pending authority. This is an atomic only, including on the UI thread.
        self.gate.invalidate(SaveInvalidation::Discarded);
    }
}

#[derive(Default)]
struct Pending {
    gate: Option<Weak<Gate>>,
}

/// The mutex only covers a few atomics/Arc operations. Never hold it while
/// taking a writer lock, invoking a callback, waiting, or performing any IO.
#[derive(Default)]
pub(crate) struct SaveOrder {
    latest: AtomicU64,
    pending: Mutex<Pending>,
}

impl SaveOrder {
    /// Checks allocation identity rather than a caller-supplied numeric ticket.
    pub(crate) fn belongs(self: &Arc<Self>, permit: &SavePermit) -> bool {
        Arc::ptr_eq(self, &permit.order)
    }

    fn next_locked(&self, pending: &mut Pending) -> u64 {
        if let Some(gate) = pending.gate.take().and_then(|gate| gate.upgrade()) {
            gate.invalidate(SaveInvalidation::Superseded);
        }
        // No ticket changes before the old pending gate has been invalidated.
        // Claim takes this same short mutex, closing check-latest -> CAS races.
        let previous = self.latest.load(Ordering::Acquire);
        let ticket = previous.checked_add(1).expect("save ticket exhausted");
        self.latest.store(ticket, Ordering::Release);
        ticket
    }

    /// Invalidates the previous Pending gate before advancing the latest ticket.
    pub(crate) fn next(&self) -> u64 {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        self.next_locked(&mut pending)
    }

    /// Checks the current ticket; a commit still needs the locked claim.
    pub(crate) fn is_latest(&self, ticket: u64) -> bool {
        self.latest.load(Ordering::Acquire) == ticket
    }

    /// Issues a one-shot permit and its original revoker with the given deadline.
    pub(crate) fn issue(self: &Arc<Self>, deadline: Instant) -> (SavePermit, SaveRevoker) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = self.next_locked(&mut pending);
        let gate = Arc::new(Gate {
            state: AtomicU8::new(PENDING),
        });
        pending.gate = Some(Arc::downgrade(&gate));
        (
            SavePermit {
                order: Arc::clone(self),
                gate: Arc::clone(&gate),
                ticket,
                deadline,
            },
            SaveRevoker { gate },
        )
    }

    /// Consumes the original permit and arbitrates the commit-start CAS.
    pub(crate) fn begin(
        self: &Arc<Self>,
        permit: SavePermit,
    ) -> Result<StartedSave, SavePermitRefusal> {
        self.begin_after_cas(permit, || {})
    }

    // Private hook exists only to pause the real CAS boundary in unit tests.
    /// Runs the claim with an internal test hook after releasing the memory lock.
    pub(crate) fn begin_after_cas(
        self: &Arc<Self>,
        permit: SavePermit,
        after_cas: impl FnOnce(),
    ) -> Result<StartedSave, SavePermitRefusal> {
        if !Arc::ptr_eq(self, &permit.order) {
            return Err(SavePermitRefusal::WrongLane);
        }
        {
            let _pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if !self.is_latest(permit.ticket) {
                permit.gate.invalidate(SaveInvalidation::Superseded);
                return Err(SavePermitRefusal::Invalidated(SaveInvalidation::Superseded));
            }
            if Instant::now() >= permit.deadline {
                permit.gate.invalidate(SaveInvalidation::Expired);
            }
            permit
                .gate
                .state
                .compare_exchange(PENDING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|value| {
                    SavePermitRefusal::Invalidated(SaveInvalidation::from_state(value))
                })?;
        }
        // No memory mutex is held across this hook or anything after it.
        after_cas();
        // If scheduling paused after the pre-check, this proves that a claim
        // allowed to continue actually happened before the original deadline.
        let deadline_valid = Instant::now() < permit.deadline;
        // Even expiry observed here cannot rewrite the already-won CAS as
        // never-claimed. The caller can prove NotCommitted(expired) with
        // commit_claimed=true / rename_attempted=false, and must not rename.
        Ok(StartedSave {
            gate: Arc::clone(&permit.gate),
            deadline_valid,
        })
    }
}

/// Not Clone. Keep Committing on panic/drop so a lost result remains unknown.
pub(crate) struct StartedSave {
    gate: Arc<Gate>,
    deadline_valid: bool,
}

impl StartedSave {
    /// Reports the monotonic deadline check performed immediately after claim.
    pub(crate) fn deadline_valid(&self) -> bool {
        self.deadline_valid
    }

    /// Records completion without asserting rename or verification success.
    pub(crate) fn finish(self) {
        self.gate.state.store(FINISHED, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, mpsc};
    use std::time::Duration;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn invalidation_before_claim_is_known_not_started() {
        let order = Arc::new(SaveOrder::default());
        let (permit, revoke) = order.issue(deadline());
        assert_eq!(
            revoke.invalidate(SaveInvalidation::AccountChanged),
            RevokeSaveResult::Invalidated
        );
        assert!(matches!(
            order.begin(permit),
            Err(SavePermitRefusal::Invalidated(
                SaveInvalidation::AccountChanged
            ))
        ));
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::AccountChanged)
        );
    }

    #[test]
    fn started_operation_cannot_be_relabelled_cancelled() {
        let order = Arc::new(SaveOrder::default());
        let (permit, revoke) = order.issue(deadline());
        let started = order.begin(permit).unwrap_or_else(|_| panic!("must claim"));
        assert_eq!(
            revoke.invalidate(SaveInvalidation::Stopped),
            RevokeSaveResult::CommitAlreadyStarted
        );
        order.next();
        assert_eq!(revoke.state(), SaveGateState::Committing);
        started.finish();
        assert_eq!(
            revoke.invalidate(SaveInvalidation::Revoked),
            RevokeSaveResult::Finished
        );
    }

    #[test]
    fn latest_precheck_cannot_escape_new_ticket_invalidation() {
        let order = Arc::new(SaveOrder::default());
        let (permit, revoke) = order.issue(deadline());
        assert!(order.is_latest(permit.ticket()));
        let newest = order.next();
        assert!(order.is_latest(newest));
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Superseded)
        );
        assert!(matches!(
            order.begin(permit),
            Err(SavePermitRefusal::Invalidated(SaveInvalidation::Superseded))
        ));
    }

    #[test]
    fn reserve_and_claim_race_has_only_two_valid_orders() {
        for _ in 0..128 {
            let order = Arc::new(SaveOrder::default());
            let (permit, revoke) = order.issue(deadline());
            let start = Arc::new(Barrier::new(2));
            let worker_order = Arc::clone(&order);
            let worker_start = Arc::clone(&start);
            let worker = std::thread::spawn(move || {
                worker_start.wait();
                worker_order.begin(permit)
            });
            start.wait();
            let ticket = order.next();
            assert!(order.is_latest(ticket));
            match worker.join().unwrap() {
                Ok(started) => {
                    assert_eq!(revoke.state(), SaveGateState::Committing);
                    started.finish();
                }
                Err(error) => {
                    assert_eq!(
                        error,
                        SavePermitRefusal::Invalidated(SaveInvalidation::Superseded)
                    );
                    assert_eq!(
                        revoke.state(),
                        SaveGateState::Invalidated(SaveInvalidation::Superseded)
                    );
                }
            }
        }
    }

    #[test]
    fn wrong_lane_is_rejected_without_claiming() {
        let a = Arc::new(SaveOrder::default());
        let b = Arc::new(SaveOrder::default());
        let (permit, revoke) = a.issue(deadline());
        assert!(a.belongs(&permit));
        assert!(!b.belongs(&permit));
        assert!(matches!(b.begin(permit), Err(SavePermitRefusal::WrongLane)));
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Discarded)
        );
    }

    #[test]
    fn expiration_is_checked_after_the_actual_cas_without_holding_order_lock() {
        let order = Arc::new(SaveOrder::default());
        let expires = Instant::now() + Duration::from_millis(100);
        let (permit, revoke) = order.issue(expires);
        let (entered, wait_entered) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker_order = Arc::clone(&order);
        let worker = std::thread::spawn(move || {
            worker_order.begin_after_cas(permit, || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            })
        });
        wait_entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(revoke.state(), SaveGateState::Committing);
        // Must not wait for the paused worker: begin released the short mutex.
        order.next();
        std::thread::sleep(expires.saturating_duration_since(Instant::now()));
        release.send(()).unwrap();
        let started = worker
            .join()
            .unwrap()
            .unwrap_or_else(|_| panic!("CAS already won"));
        assert!(!started.deadline_valid());
        assert_eq!(revoke.state(), SaveGateState::Committing);
        started.finish();
    }

    #[test]
    fn expired_before_claim_does_not_start() {
        let order = Arc::new(SaveOrder::default());
        let (permit, revoke) = order.issue(Instant::now());
        assert!(matches!(
            order.begin(permit),
            Err(SavePermitRefusal::Invalidated(SaveInvalidation::Expired))
        ));
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Expired)
        );
    }

    #[test]
    fn dropping_execution_object_discards_pending_authority_without_io() {
        let order = Arc::new(SaveOrder::default());
        let (permit, revoke) = order.issue(deadline());
        drop(permit);
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Discarded)
        );
    }

    #[test]
    fn new_issuance_never_revives_old_gate_or_lets_old_revoker_cancel_new_work() {
        let order = Arc::new(SaveOrder::default());
        let (old, old_revoke) = order.issue(deadline());
        old_revoke.invalidate(SaveInvalidation::Revoked);
        let (new, new_revoke) = order.issue(deadline());
        assert_eq!(
            old_revoke.invalidate(SaveInvalidation::Stopped),
            RevokeSaveResult::AlreadyInvalidated(SaveInvalidation::Revoked)
        );
        assert!(order.begin(old).is_err());
        assert_eq!(new_revoke.state(), SaveGateState::Pending);
        let started = order
            .begin(new)
            .unwrap_or_else(|_| panic!("new native issuance"));
        started.finish();
    }
}
