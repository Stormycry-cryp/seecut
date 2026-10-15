//! The write callback must claim this request before accessing the write registry.
//! Claim, the action, dedup recording, and completion belong to the same UI callback.
//! No lock from this module is held while the action, result clone, or rendering runs.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteRequestOutcome<R> {
    Completed(R),
    Busy,
    OutcomeUnknown,
    AppUnavailable,
}

#[derive(Clone, Copy)]
enum Cancellation {
    Busy,
    AppUnavailable,
}

enum State<R> {
    Queued,
    Running,
    Completed(Arc<R>),
    Cancelled(Cancellation),
}

// Copy only an Arc under the state lock; R::clone is caller code and runs outside it.
enum StoredOutcome<R> {
    Completed(Arc<R>),
    Busy,
    OutcomeUnknown,
    AppUnavailable,
}

impl<R: Clone> StoredOutcome<R> {
    fn into_outcome(self) -> WriteRequestOutcome<R> {
        match self {
            Self::Completed(result) => WriteRequestOutcome::Completed((*result).clone()),
            Self::Busy => WriteRequestOutcome::Busy,
            Self::OutcomeUnknown => WriteRequestOutcome::OutcomeUnknown,
            Self::AppUnavailable => WriteRequestOutcome::AppUnavailable,
        }
    }
}

impl<R> State<R> {
    fn terminal_outcome(&self) -> Option<StoredOutcome<R>> {
        match self {
            Self::Completed(result) => Some(StoredOutcome::Completed(Arc::clone(result))),
            Self::Cancelled(Cancellation::Busy) => Some(StoredOutcome::Busy),
            Self::Cancelled(Cancellation::AppUnavailable) => Some(StoredOutcome::AppUnavailable),
            Self::Queued | Self::Running => None,
        }
    }
}

/// Share with `Arc`; dropping the caller's Arc does not discard the callback's result.
/// A successful claim authorizes only the original callback to finish this request.
/// This type supplies no transferable permit, retry, or rollback of a running action.
pub(crate) struct WriteRequest<R> {
    state: Mutex<State<R>>,
    changed: Condvar,
}

impl<R: Clone> WriteRequest<R> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::Queued),
            changed: Condvar::new(),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State<R>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Call at the start of the UI callback, before registry access or any action.
    /// Exactly one callback can win; the lock is released before returning.
    pub(crate) fn claim(&self) -> bool {
        let mut state = self.lock_state();
        if !matches!(*state, State::Queued) {
            return false;
        }
        *state = State::Running;
        self.changed.notify_all();
        true
    }

    /// Record after the session's dedup cache and before rendering/publishing.
    /// Retain the first result even if the caller timed out or disconnected.
    pub(crate) fn complete(&self, result: R) -> bool {
        let result = Arc::new(result);
        let mut state = self.lock_state();
        if !matches!(*state, State::Running) {
            // Drop caller-owned R only after releasing the state lock.
            drop(state);
            return false;
        }
        *state = State::Completed(result);
        self.changed.notify_all();
        true
    }

    /// Cancel only a request which has never started. A winning cancellation
    /// guarantees that a queued callback's later claim fails.
    #[cfg(test)]
    pub(crate) fn cancel_if_queued(&self) -> bool {
        let mut state = self.lock_state();
        if !matches!(*state, State::Queued) {
            return false;
        }
        *state = State::Cancelled(Cancellation::Busy);
        self.changed.notify_all();
        true
    }

    /// Wait up to the supplied duration (750 ms for the UI bridge). At the deadline,
    /// cancel Queued atomically, but never cancel or reschedule a Running action.
    /// A completion visible at that same decision point always returns its result.
    pub(crate) fn wait(&self, timeout: Duration) -> WriteRequestOutcome<R> {
        let started = Instant::now();
        let mut state = self.lock_state();
        let outcome = loop {
            if let Some(outcome) = state.terminal_outcome() {
                break outcome;
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break self.stop_waiting(&mut state, Cancellation::Busy);
            }
            let (next_state, _) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next_state;
        };
        drop(state);
        outcome.into_outcome()
    }

    /// Only an unclaimed request can report scheduling failure as zero execution.
    pub(crate) fn scheduling_failed(&self) -> WriteRequestOutcome<R> {
        self.finish_waiting(Cancellation::AppUnavailable)
    }

    /// Use if the transport detects a disconnected caller. Once claim succeeded,
    /// the result is unknown until completion and must not imply zero execution.
    #[cfg(test)]
    pub(crate) fn caller_disconnected(&self) -> WriteRequestOutcome<R> {
        self.finish_waiting(Cancellation::Busy)
    }

    fn finish_waiting(&self, cancellation: Cancellation) -> WriteRequestOutcome<R> {
        let mut state = self.lock_state();
        let outcome = self.stop_waiting(&mut state, cancellation);
        drop(state);
        outcome.into_outcome()
    }

    fn stop_waiting(&self, state: &mut State<R>, cancellation: Cancellation) -> StoredOutcome<R> {
        if let Some(outcome) = state.terminal_outcome() {
            return outcome;
        }
        match state {
            State::Queued => {
                *state = State::Cancelled(cancellation);
                self.changed.notify_all();
                state.terminal_outcome().expect("just cancelled")
            }
            State::Running => StoredOutcome::OutcomeUnknown,
            State::Completed(_) | State::Cancelled(_) => unreachable!("checked terminal state"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WriteRequest, WriteRequestOutcome};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;

    const WATCHDOG: Duration = Duration::from_secs(2);

    #[test]
    fn queued_timeout_prevents_later_callback_action() {
        let request = Arc::new(WriteRequest::new());
        let actions = Arc::new(AtomicUsize::new(0));
        let (release, queued) = mpsc::channel();
        let callback = {
            let request = Arc::clone(&request);
            let actions = Arc::clone(&actions);
            thread::spawn(move || {
                queued.recv_timeout(WATCHDOG).unwrap();
                if request.claim() {
                    actions.fetch_add(1, Ordering::SeqCst);
                    request.complete(7);
                }
            })
        };
        assert_eq!(request.wait(Duration::ZERO), WriteRequestOutcome::Busy);
        release.send(()).unwrap();
        callback.join().unwrap();
        assert_eq!(actions.load(Ordering::SeqCst), 0);
        assert_eq!(request.wait(Duration::ZERO), WriteRequestOutcome::Busy);
        assert!(!request.complete(8));
    }

    #[test]
    fn running_timeout_is_unknown_then_completes_once() {
        let request = Arc::new(WriteRequest::new());
        let actions = Arc::new(AtomicUsize::new(0));
        let (claimed, observed_claim) = mpsc::channel();
        let (release, continue_action) = mpsc::channel();
        let callback = {
            let request = Arc::clone(&request);
            let actions = Arc::clone(&actions);
            thread::spawn(move || {
                assert!(request.claim());
                actions.fetch_add(1, Ordering::SeqCst);
                claimed.send(()).unwrap();
                continue_action.recv_timeout(WATCHDOG).unwrap();
                assert!(request.complete(11));
                assert!(!request.claim());
            })
        };
        observed_claim.recv_timeout(WATCHDOG).unwrap();
        assert_eq!(
            request.wait(Duration::ZERO),
            WriteRequestOutcome::OutcomeUnknown
        );
        assert_eq!(
            request.caller_disconnected(),
            WriteRequestOutcome::OutcomeUnknown
        );
        assert!(!request.cancel_if_queued());
        release.send(()).unwrap();
        callback.join().unwrap();
        assert_eq!(actions.load(Ordering::SeqCst), 1);
        assert_eq!(
            request.wait(Duration::ZERO),
            WriteRequestOutcome::Completed(11)
        );
    }

    #[test]
    fn completion_before_wait_returns_original_result() {
        let request = WriteRequest::new();
        assert!(!request.complete(String::from("not claimed")));
        assert!(request.claim());
        assert!(request.complete(String::from("original")));
        assert!(!request.complete(String::from("replacement")));
        let expected = WriteRequestOutcome::Completed(String::from("original"));
        assert_eq!(request.wait(Duration::ZERO), expected);
        assert_eq!(request.caller_disconnected(), expected);
        assert_eq!(request.scheduling_failed(), expected);
    }

    #[test]
    fn completion_and_timeout_race_preserves_exact_result() {
        for _ in 0..64 {
            let request = Arc::new(WriteRequest::new());
            assert!(request.claim());
            let start = Arc::new(Barrier::new(2));
            let callback = {
                let request = Arc::clone(&request);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    assert!(request.complete(29));
                })
            };
            start.wait();
            assert!(matches!(
                request.wait(Duration::ZERO),
                WriteRequestOutcome::Completed(29) | WriteRequestOutcome::OutcomeUnknown
            ));
            callback.join().unwrap();
            assert_eq!(
                request.wait(Duration::ZERO),
                WriteRequestOutcome::Completed(29)
            );
        }
    }

    #[test]
    fn claim_and_timeout_race_has_at_most_one_action() {
        for _ in 0..64 {
            let request = Arc::new(WriteRequest::new());
            let start = Arc::new(Barrier::new(2));
            let callback = {
                let request = Arc::clone(&request);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    if request.claim() {
                        assert!(!request.claim());
                        assert!(request.complete(1));
                        1
                    } else {
                        0
                    }
                })
            };
            start.wait();
            let outcome = request.wait(Duration::ZERO);
            let actions = callback.join().unwrap();
            match outcome {
                WriteRequestOutcome::Busy => {
                    assert_eq!(actions, 0);
                    assert_eq!(request.wait(Duration::ZERO), WriteRequestOutcome::Busy);
                }
                WriteRequestOutcome::OutcomeUnknown | WriteRequestOutcome::Completed(1) => {
                    assert_eq!(actions, 1);
                    assert_eq!(
                        request.wait(Duration::ZERO),
                        WriteRequestOutcome::Completed(1)
                    );
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
        }
    }

    #[test]
    fn dropping_caller_preserves_callback_result() {
        let caller = Arc::new(WriteRequest::new());
        let (claimed, observed_claim) = mpsc::channel();
        let (release, continue_action) = mpsc::channel();
        let callback = {
            let callback_request = Arc::clone(&caller);
            thread::spawn(move || {
                assert!(callback_request.claim());
                claimed.send(()).unwrap();
                continue_action.recv_timeout(WATCHDOG).unwrap();
                assert!(callback_request.complete(41));
                callback_request.wait(Duration::ZERO)
            })
        };
        observed_claim.recv_timeout(WATCHDOG).unwrap();
        drop(caller);
        release.send(()).unwrap();
        assert_eq!(callback.join().unwrap(), WriteRequestOutcome::Completed(41));
    }

    #[test]
    fn scheduling_failure_before_claim_prevents_execution() {
        let request = WriteRequest::new();
        assert_eq!(
            request.scheduling_failed(),
            WriteRequestOutcome::<u8>::AppUnavailable
        );
        assert!(!request.claim());
        assert!(!request.complete(1));
        assert_eq!(
            request.wait(Duration::ZERO),
            WriteRequestOutcome::AppUnavailable
        );
        assert_eq!(
            request.caller_disconnected(),
            WriteRequestOutcome::AppUnavailable
        );
    }

    #[test]
    fn scheduling_failure_after_claim_is_unknown() {
        let request = WriteRequest::new();
        assert!(request.claim());
        assert_eq!(
            request.scheduling_failed(),
            WriteRequestOutcome::OutcomeUnknown
        );
        assert!(request.complete(3));
        assert_eq!(
            request.scheduling_failed(),
            WriteRequestOutcome::Completed(3)
        );
    }

    #[test]
    fn duplicate_claim_cannot_start_second_action() {
        let request = WriteRequest::new();
        assert!(request.claim());
        assert!(!request.claim());
        assert!(request.complete(5));
        assert!(!request.claim());
        assert!(!request.complete(6));
    }

    #[test]
    fn explicit_cancellation_and_queued_disconnect_prevent_claim() {
        let request = WriteRequest::<u8>::new();
        assert!(request.cancel_if_queued());
        assert!(!request.cancel_if_queued());
        assert!(!request.claim());
        assert_eq!(request.scheduling_failed(), WriteRequestOutcome::Busy);
        let disconnected = WriteRequest::<u8>::new();
        assert_eq!(
            disconnected.caller_disconnected(),
            WriteRequestOutcome::Busy
        );
        assert!(!disconnected.claim());
    }

    #[test]
    fn waiting_deadline_does_not_wait_for_action_lock() {
        let request = Arc::new(WriteRequest::new());
        let (claimed, observed_claim) = mpsc::channel();
        let (release, continue_action) = mpsc::channel();
        let callback = {
            let request = Arc::clone(&request);
            thread::spawn(move || {
                assert!(request.claim());
                claimed.send(()).unwrap();
                // Model an action stalled outside the state lock.
                continue_action.recv_timeout(WATCHDOG).unwrap();
                assert!(request.complete(53));
            })
        };
        observed_claim.recv_timeout(WATCHDOG).unwrap();
        let (finished, result) = mpsc::channel();
        let waiter = {
            let request = Arc::clone(&request);
            thread::spawn(move || {
                finished
                    .send(request.wait(Duration::from_millis(20)))
                    .unwrap();
            })
        };
        let outcome = result.recv_timeout(WATCHDOG);
        // Always release the callback before asserting, even if the waiter failed.
        release.send(()).unwrap();
        callback.join().unwrap();
        waiter.join().unwrap();
        assert_eq!(outcome.unwrap(), WriteRequestOutcome::OutcomeUnknown);
        assert_eq!(
            request.wait(Duration::ZERO),
            WriteRequestOutcome::Completed(53)
        );
    }

    #[test]
    fn waiting_caller_observes_completion_notification() {
        let request = Arc::new(WriteRequest::new());
        assert!(request.claim());
        let start = Arc::new(Barrier::new(2));
        let waiter = {
            let request = Arc::clone(&request);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                request.wait(WATCHDOG)
            })
        };
        start.wait();
        assert!(request.complete(67));
        assert_eq!(waiter.join().unwrap(), WriteRequestOutcome::Completed(67));
    }

    #[test]
    fn result_clone_and_rejected_result_drop_run_outside_state_lock() {
        struct CheckedResult {
            request: std::sync::Weak<WriteRequest<CheckedResult>>,
            clones: Arc<AtomicUsize>,
        }

        impl Clone for CheckedResult {
            fn clone(&self) -> Self {
                let request = self.request.upgrade().unwrap();
                assert!(request.state.try_lock().is_ok());
                self.clones.fetch_add(1, Ordering::SeqCst);
                Self {
                    request: self.request.clone(),
                    clones: Arc::clone(&self.clones),
                }
            }
        }

        impl Drop for CheckedResult {
            fn drop(&mut self) {
                if let Some(request) = self.request.upgrade() {
                    assert!(request.state.try_lock().is_ok());
                }
            }
        }

        let request = Arc::new(WriteRequest::new());
        let clones = Arc::new(AtomicUsize::new(0));
        let result = || CheckedResult {
            request: Arc::downgrade(&request),
            clones: Arc::clone(&clones),
        };
        assert!(!request.complete(result()));
        assert!(request.claim());
        assert!(request.complete(result()));
        assert!(!request.complete(result()));
        assert!(matches!(
            request.wait(Duration::ZERO),
            WriteRequestOutcome::Completed(_)
        ));
        assert!(matches!(
            request.caller_disconnected(),
            WriteRequestOutcome::Completed(_)
        ));
        assert!(matches!(
            request.scheduling_failed(),
            WriteRequestOutcome::Completed(_)
        ));
        assert_eq!(clones.load(Ordering::SeqCst), 3);
    }
}
