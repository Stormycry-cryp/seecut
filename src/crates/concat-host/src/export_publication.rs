// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! One original export's in-memory publication arbitration.
//!
//! Trusted native code must route every applicable stop or authority change
//! through the original [`PublishRevoker`]. The original Job's raw cancel flag
//! is only a cooperative render/decode signal: writing it directly does not
//! arbitrate a concurrent publication claim. This module installs no native
//! lifecycle hooks and performs no file publication, authorization, library
//! registration, receipt tracking or IO.
//!
//! Claim only as the last step of the trusted finished-output hook, after
//! input/output verification and applicable synchronization. A claim records
//! publication STARTED; it never proves file publication or durability, and
//! later errors must preserve any actual publication result.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Instant;

const PENDING: u8 = 0;
const PUBLISHING: u8 = 1;

/// Closed native reasons for revoking one pending original export.
/// The reason itself is the atomic revoked state, with no separate reason slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PublishRevocation {
    /// The original task was paused or stopped.
    Stopped = 16,
    /// The original write authority was explicitly revoked.
    Revoked = 17,
    /// The original account or workspace changed.
    AccountChanged = 18,
    /// The original Agent session changed.
    SessionChanged = 19,
    /// The original document closed or changed identity, not ordinary editing.
    DocumentChanged = 20,
    /// The original finite write lease expired.
    LeaseExpired = 21,
    /// The unique pending permit was dropped without a claim.
    Abandoned = 22,
}

impl PublishRevocation {
    fn from_state(value: u8) -> Self {
        match value {
            16 => Self::Stopped,
            17 => Self::Revoked,
            18 => Self::AccountChanged,
            19 => Self::SessionChanged,
            20 => Self::DocumentChanged,
            21 => Self::LeaseExpired,
            22 => Self::Abandoned,
            _ => unreachable!("private publication gate state"),
        }
    }
}

/// Current arbitration state, never an output or operation receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishState {
    /// The unique permit may still compete with revocation.
    Pending,
    /// The publication-start CAS won; actual file success remains unproven.
    Publishing,
    /// Revocation won and permanently retained its first reason.
    Revoked(PublishRevocation),
}

/// Result of revoking the original pending export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishRevokeResult {
    /// This call won revocation and set the original cooperative stop flag.
    Revoked,
    /// An earlier revocation won; its first reason is retained.
    AlreadyRevoked(PublishRevocation),
    /// Publication already started; this call did not change the stop flag.
    AlreadyPublishing,
}

/// Why no new publication claim was issued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishRefusal {
    /// The pending export was revoked with this retained first reason.
    Revoked(PublishRevocation),
    /// The publication-start CAS has already been won; no new claim is issued.
    AlreadyPublishing,
    /// The publication-start CAS won, but the original lease had expired at
    /// the immediate post-CAS check. No claim is returned: reject the output
    /// hook. State remains Publishing, preserving the actual CAS fact; this
    /// result never proves that a file was published.
    LeaseExpiredAfterClaim,
}

#[derive(Debug)]
struct Gate {
    state: AtomicU8,
    cancel: Arc<AtomicBool>,
    lease_deadline: Option<Instant>,
}

impl Gate {
    fn revoke(&self, reason: PublishRevocation) -> PublishRevokeResult {
        match self.state.compare_exchange(
            PENDING,
            reason as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // State/reason are already indivisible. Only the winning
                // revocation changes the Job's cooperative stop signal.
                self.cancel.store(true, Ordering::Release);
                PublishRevokeResult::Revoked
            }
            Err(PUBLISHING) => PublishRevokeResult::AlreadyPublishing,
            Err(value) => PublishRevokeResult::AlreadyRevoked(PublishRevocation::from_state(value)),
        }
    }

    fn refuse(&self, reason: PublishRevocation) -> PublishRefusal {
        match self.revoke(reason) {
            PublishRevokeResult::Revoked => PublishRefusal::Revoked(reason),
            PublishRevokeResult::AlreadyRevoked(first) => PublishRefusal::Revoked(first),
            PublishRevokeResult::AlreadyPublishing => PublishRefusal::AlreadyPublishing,
        }
    }

    fn refusal_from_state(value: u8) -> PublishRefusal {
        match value {
            PUBLISHING => PublishRefusal::AlreadyPublishing,
            value => PublishRefusal::Revoked(PublishRevocation::from_state(value)),
        }
    }

    fn state(&self) -> PublishState {
        match self.state.load(Ordering::Acquire) {
            PENDING => PublishState::Pending,
            PUBLISHING => PublishState::Publishing,
            value => PublishState::Revoked(PublishRevocation::from_state(value)),
        }
    }
}

/// Issues native internal handles for the original Job and optional original
/// write-lease expiry. `None` adds no deadline. A request/reply deadline must
/// never be passed here, and reissuing a request must not extend an old lease.
/// This function grants no authority; the caller must already hold it.
pub fn new_publish_gate(
    cancel: Arc<AtomicBool>,
    lease_deadline: Option<Instant>,
) -> (PublishPermit, PublishRevoker) {
    let gate = Arc::new(Gate {
        state: AtomicU8::new(PENDING),
        cancel,
        lease_deadline,
    });
    (
        PublishPermit {
            gate: Arc::clone(&gate),
        },
        PublishRevoker { gate },
    )
}

/// Unique native execution capability, deliberately not Clone or deserializable.
/// Dropping a pending permit revokes it; dropping after claim cannot undo it.
#[derive(Debug)]
pub struct PublishPermit {
    gate: Arc<Gate>,
}

impl PublishPermit {
    /// Competes once for publication start using the original lease and Job
    /// signal. Call as the final finished-output-hook step. A successful claim
    /// is permanent even if the following exclusive file publish fails.
    /// A finite original lease is checked immediately before and after the
    /// successful CAS. `LeaseExpiredAfterClaim` returns no claim and must
    /// reject the hook, while preserving Publishing rather than undoing the
    /// actual CAS. These checks do not make the clock and filesystem atomic.
    pub fn try_claim(&self) -> Result<PublishClaim, PublishRefusal> {
        self.try_claim_with_clock(Instant::now)
    }

    // Private clock seam for exact boundary tests. Native/model callers cannot
    // supply a fabricated time. No clock is needed for a lease without expiry.
    fn try_claim_with_clock(
        &self,
        mut now: impl FnMut() -> Instant,
    ) -> Result<PublishClaim, PublishRefusal> {
        let state = self.gate.state.load(Ordering::Acquire);
        if state != PENDING {
            return Err(Gate::refusal_from_state(state));
        }
        if self.gate.cancel.load(Ordering::Acquire) {
            return Err(self.gate.refuse(PublishRevocation::Stopped));
        }
        if self
            .gate
            .lease_deadline
            .is_some_and(|deadline| now() >= deadline)
        {
            return Err(self.gate.refuse(PublishRevocation::LeaseExpired));
        }
        self.gate
            .state
            .compare_exchange(PENDING, PUBLISHING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(Gate::refusal_from_state)?;
        if self
            .gate
            .lease_deadline
            .is_some_and(|deadline| now() >= deadline)
        {
            // The CAS actually won. Reject the hook without issuing a token,
            // rolling state back, or allowing a later revoker to relabel it.
            return Err(PublishRefusal::LeaseExpiredAfterClaim);
        }
        Ok(PublishClaim {
            _gate: Arc::clone(&self.gate),
        })
    }
}

impl Drop for PublishPermit {
    fn drop(&mut self) {
        self.gate.revoke(PublishRevocation::Abandoned);
    }
}

/// Original native lifecycle handle. Every trusted stop/revocation path must
/// retain and use this handle rather than only writing the raw Job flag.
#[derive(Clone, Debug)]
pub struct PublishRevoker {
    gate: Arc<Gate>,
}

impl PublishRevoker {
    /// Competes with publication start. A losing revoke never changes the
    /// original cancel signal or relabels a publication that already started.
    pub fn revoke(&self, reason: PublishRevocation) -> PublishRevokeResult {
        self.gate.revoke(reason)
    }

    /// Observes arbitration only. Revocation's cooperative flag store follows
    /// its state CAS and is complete when the winning `revoke` call returns.
    pub fn state(&self) -> PublishState {
        self.gate.state()
    }
}

/// Opaque, non-Clone evidence that this original gate's one claim won.
/// Private fields and no deserialization prevent caller-created claims.
/// It records no file path, output success, durability or library result.
#[derive(Debug)]
pub struct PublishClaim {
    _gate: Arc<Gate>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::SingleFlight;
    use std::sync::Barrier;
    use std::time::Duration;

    fn fresh_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn revocation_wins_before_claim_and_stops_the_original_job() {
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        assert_eq!(
            revoker.revoke(PublishRevocation::SessionChanged),
            PublishRevokeResult::Revoked
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            permit.try_claim().unwrap_err(),
            PublishRefusal::Revoked(PublishRevocation::SessionChanged)
        );
        assert_eq!(
            revoker.state(),
            PublishState::Revoked(PublishRevocation::SessionChanged)
        );
    }

    #[test]
    fn claim_wins_and_later_revocation_or_drop_cannot_relabel_it() {
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        let claim = permit.try_claim().unwrap();
        assert_eq!(
            revoker.revoke(PublishRevocation::Stopped),
            PublishRevokeResult::AlreadyPublishing
        );
        drop(claim);
        drop(permit);
        assert_eq!(revoker.state(), PublishState::Publishing);
        assert!(!cancel.load(Ordering::Acquire));
    }

    #[test]
    fn only_one_claim_can_be_obtained_from_the_unique_permit() {
        let (permit, revoker) = new_publish_gate(fresh_cancel(), None);
        let _claim = permit.try_claim().unwrap();
        assert_eq!(
            permit.try_claim().unwrap_err(),
            PublishRefusal::AlreadyPublishing
        );
        assert_eq!(revoker.state(), PublishState::Publishing);
    }

    #[test]
    fn first_reason_survives_later_revoke_cancel_expiry_and_permit_drop() {
        let deadline = Instant::now();
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), Some(deadline));
        assert_eq!(
            revoker.revoke(PublishRevocation::AccountChanged),
            PublishRevokeResult::Revoked
        );
        assert_eq!(
            revoker.clone().revoke(PublishRevocation::DocumentChanged),
            PublishRevokeResult::AlreadyRevoked(PublishRevocation::AccountChanged)
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            permit.try_claim_with_clock(|| deadline).unwrap_err(),
            PublishRefusal::Revoked(PublishRevocation::AccountChanged)
        );
        drop(permit);
        assert_eq!(
            revoker.state(),
            PublishState::Revoked(PublishRevocation::AccountChanged)
        );
    }

    #[test]
    fn an_original_pre_cancelled_job_becomes_stopped() {
        let cancel = Arc::new(AtomicBool::new(true));
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        assert_eq!(
            permit.try_claim().unwrap_err(),
            PublishRefusal::Revoked(PublishRevocation::Stopped)
        );
        assert_eq!(
            revoker.state(),
            PublishState::Revoked(PublishRevocation::Stopped)
        );
        assert_eq!(
            revoker.revoke(PublishRevocation::Revoked),
            PublishRevokeResult::AlreadyRevoked(PublishRevocation::Stopped)
        );
    }

    #[test]
    fn original_lease_is_valid_only_strictly_before_the_exact_deadline() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let before = deadline - Duration::from_nanos(1);
        let (permit, revoker) = new_publish_gate(fresh_cancel(), Some(deadline));
        let _claim = permit.try_claim_with_clock(|| before).unwrap();
        assert_eq!(revoker.state(), PublishState::Publishing);

        for now in [deadline, deadline + Duration::from_nanos(1)] {
            let cancel = fresh_cancel();
            let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), Some(deadline));
            assert_eq!(
                permit.try_claim_with_clock(|| now).unwrap_err(),
                PublishRefusal::Revoked(PublishRevocation::LeaseExpired)
            );
            assert_eq!(
                revoker.state(),
                PublishState::Revoked(PublishRevocation::LeaseExpired)
            );
            assert!(cancel.load(Ordering::Acquire));
        }
    }

    #[test]
    fn no_lease_deadline_never_consults_a_clock_or_invents_an_expiry() {
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        let _claim = permit
            .try_claim_with_clock(|| panic!("no finite lease needs a clock"))
            .unwrap();
        assert_eq!(revoker.state(), PublishState::Publishing);
        assert!(!cancel.load(Ordering::Acquire));
    }

    #[test]
    fn lease_expiring_between_checks_returns_no_claim_and_preserves_the_won_cas() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let before = deadline - Duration::from_nanos(1);
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), Some(deadline));
        let mut clock = [before, deadline].into_iter();
        let result = permit.try_claim_with_clock(|| clock.next().expect("two lease checks"));
        assert_eq!(result.unwrap_err(), PublishRefusal::LeaseExpiredAfterClaim);
        assert!(clock.next().is_none(), "both clock samples were consumed");
        assert_eq!(revoker.state(), PublishState::Publishing);
        assert_eq!(
            revoker.revoke(PublishRevocation::LeaseExpired),
            PublishRevokeResult::AlreadyPublishing
        );
        assert_eq!(
            permit.try_claim().unwrap_err(),
            PublishRefusal::AlreadyPublishing
        );
        drop(permit);
        assert_eq!(revoker.state(), PublishState::Publishing);
        assert!(!cancel.load(Ordering::Acquire));
    }

    #[test]
    fn dropping_a_pending_permit_abandons_and_stops_it() {
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        drop(permit);
        assert_eq!(
            revoker.state(),
            PublishState::Revoked(PublishRevocation::Abandoned)
        );
        assert!(cancel.load(Ordering::Acquire));
    }

    #[test]
    fn an_old_revoker_cannot_stop_the_next_real_job() {
        let flight = Arc::new(SingleFlight::new());
        let first = flight.begin("export").unwrap();
        let (first_permit, first_revoker) = new_publish_gate(first.cancel_handle(), None);
        drop(first_permit);
        assert!(first.cancelled());
        drop(first);
        let second = flight.begin("export").unwrap();
        let (second_permit, second_revoker) = new_publish_gate(second.cancel_handle(), None);
        assert_eq!(
            first_revoker.revoke(PublishRevocation::Stopped),
            PublishRevokeResult::AlreadyRevoked(PublishRevocation::Abandoned)
        );
        assert!(!second.cancelled());
        assert_eq!(second_revoker.state(), PublishState::Pending);
        let _claim = second_permit.try_claim().unwrap();
        assert!(!second.cancelled());
    }

    #[test]
    fn barrier_race_has_one_winner_and_consistent_reason_and_stop_signal() {
        let cancel = fresh_cancel();
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        let barrier = Barrier::new(3);
        let (claim, first, second) = std::thread::scope(|scope| {
            let claim = scope.spawn(|| {
                barrier.wait();
                permit.try_claim()
            });
            let first_revoker = revoker.clone();
            let first_barrier = &barrier;
            let first = scope.spawn(move || {
                first_barrier.wait();
                first_revoker.revoke(PublishRevocation::Stopped)
            });
            let second_revoker = revoker.clone();
            let second_barrier = &barrier;
            let second = scope.spawn(move || {
                second_barrier.wait();
                second_revoker.revoke(PublishRevocation::Revoked)
            });
            (
                claim.join().unwrap(),
                first.join().unwrap(),
                second.join().unwrap(),
            )
        });
        match claim {
            Ok(_claim) => {
                assert_eq!(first, PublishRevokeResult::AlreadyPublishing);
                assert_eq!(second, PublishRevokeResult::AlreadyPublishing);
                assert_eq!(revoker.state(), PublishState::Publishing);
                assert!(!cancel.load(Ordering::Acquire));
            }
            Err(PublishRefusal::Revoked(reason)) => {
                match (first, second) {
                    (PublishRevokeResult::Revoked, PublishRevokeResult::AlreadyRevoked(first)) => {
                        assert_eq!(reason, PublishRevocation::Stopped);
                        assert_eq!(first, reason);
                    }
                    (PublishRevokeResult::AlreadyRevoked(first), PublishRevokeResult::Revoked) => {
                        assert_eq!(reason, PublishRevocation::Revoked);
                        assert_eq!(first, reason);
                    }
                    other => panic!("exactly one revoker must win: {other:?}"),
                }
                assert_eq!(revoker.state(), PublishState::Revoked(reason));
                assert!(cancel.load(Ordering::Acquire));
            }
            Err(other) => panic!("the sole claimant cannot already have published: {other:?}"),
        }
    }

    #[test]
    fn original_handles_and_opaque_claim_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PublishPermit>();
        assert_send_sync::<PublishRevoker>();
        assert_send_sync::<PublishClaim>();
        let (permit, revoker) = new_publish_gate(fresh_cancel(), None);
        let claim = std::thread::spawn(move || permit.try_claim().unwrap())
            .join()
            .unwrap();
        assert_eq!(revoker.state(), PublishState::Publishing);
        drop(claim);
    }
}
