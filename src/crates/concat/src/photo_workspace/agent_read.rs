// SPDX-License-Identifier: AGPL-3.0-or-later
//! One authorized metadata read on the existing serial NativeHost worker.
//! Cancellation and deadlines discard results; they cannot cancel filesystem I/O.

use super::owned_metadata::OwnedControl;
use super::{Command, OwnedMetadata, OwnedSource, PhotoWorkerClient};
use seecut_photo_host::{NativeHost, PhotoIdentity, PhotoState, Uuid};
#[cfg(test)]
use std::sync::mpsc::TryRecvError;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadOrigin {
    pub(crate) request_id: Uuid,
    pub(crate) source: OwnedSource,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadError {
    Unavailable,
    Busy,
    Expired,
    Stale,
    Failed,
}

/// This contains a private history key. It never leaves trusted Rust code.
/// A copied original binding is a comparison value, never a read permission.
pub(crate) struct ReadBinding {
    source: OwnedSource,
    identity: PhotoIdentity,
    history_key: String,
}
impl ReadBinding {
    pub(crate) fn from_owned(metadata: &OwnedMetadata) -> Self {
        Self {
            source: metadata.origin().source,
            identity: metadata.identity().clone(),
            history_key: metadata.history_key().to_owned(),
        }
    }

    fn check_backing(&self, identity: &PhotoIdentity, history_key: &str) -> Result<(), ReadError> {
        if self.identity.host_instance_id != identity.host_instance_id
            || self.identity.doc_id != identity.doc_id
            || self.identity.document_generation != identity.document_generation
            || self.history_key != history_key
        {
            Err(ReadError::Stale)
        } else {
            Ok(())
        }
    }
}

/// Monotonic publication cancellation, shared with the original UI source gate.
#[derive(Clone)]
pub(crate) struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn check(&self, origin: ReadOrigin, control: &OwnedControl) -> Result<(), ReadError> {
        if self.0.load(Ordering::SeqCst) || control.current() != Some(origin.source.managed) {
            Err(ReadError::Stale)
        } else if Instant::now() >= origin.deadline {
            Err(ReadError::Expired)
        } else {
            Ok(())
        }
    }
}

struct Reservation(Arc<AtomicBool>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct PendingState {
    _reservation: Arc<Reservation>,
    origin: ReadOrigin,
    cancellation: Cancellation,
    receiver: Receiver<Result<PhotoState, ReadError>>,
    done: bool,
}
impl PendingState {
    pub(crate) fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    fn check(&self, client: &PhotoWorkerClient) -> Result<(), ReadError> {
        if !client.available() {
            Err(ReadError::Unavailable)
        } else {
            self.cancellation.check(self.origin, &client.owned)
        }
    }
    #[cfg(test)]
    pub(crate) fn try_result(
        &mut self,
        client: &PhotoWorkerClient,
    ) -> Option<Result<PhotoState, ReadError>> {
        if self.done {
            return None;
        }
        if let Err(error) = self.check(client) {
            self.done = true;
            self.cancellation.cancel();
            return Some(Err(error));
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                self.done = true;
                Some(self.check(client).and(result))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                Some(Err(ReadError::Unavailable))
            }
        }
    }

    /// Only call on the MCP dispatch thread. The original deadline bounds the
    /// receiving wait, while a slow native operation continues on its worker.
    pub(crate) fn wait(mut self, client: &PhotoWorkerClient) -> Result<PhotoState, ReadError> {
        self.check(client)?;
        let remaining = self
            .origin
            .deadline
            .saturating_duration_since(Instant::now());
        let result = match self.receiver.recv_timeout(remaining) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(ReadError::Expired),
            Err(RecvTimeoutError::Disconnected) => Err(ReadError::Unavailable),
        };
        self.done = true;
        let checked = self.check(client).and(result);
        if checked.is_err() {
            self.cancellation.cancel();
        }
        checked
    }
}
impl Drop for PendingState {
    fn drop(&mut self) {
        if !self.done {
            self.cancellation.cancel();
        }
    }
}

pub(super) struct StateJob {
    _reservation: Arc<Reservation>,
    origin: ReadOrigin,
    binding: ReadBinding,
    cancellation: Cancellation,
    reply: SyncSender<Result<PhotoState, ReadError>>,
}
impl StateJob {
    pub(super) fn run(self, host: &mut NativeHost, control: &OwnedControl) {
        let result = (|| {
            self.cancellation.check(self.origin, control)?;
            let backing = host
                .photo_backing_after_gate(self.origin.source.managed.instance)
                .map_err(|_| ReadError::Failed);
            self.cancellation.check(self.origin, control)?;
            let backing = backing?;
            self.binding
                .check_backing(backing.identity(), backing.history_key())?;
            let state = host
                .photo_state_after_gate(self.origin.source.managed.instance)
                .map_err(|_| ReadError::Failed);
            self.cancellation.check(self.origin, control)?;
            let state = state?;
            check_state(&state, backing.identity())?;
            // The temporary File is dropped here on this same host worker.
            drop(backing);
            self.cancellation.check(self.origin, control)?;
            Ok(state)
        })();
        let _ = self.reply.try_send(result);
    }
}

fn check_state(state: &PhotoState, backing: &PhotoIdentity) -> Result<(), ReadError> {
    if state.host_instance_id != backing.host_instance_id
        || state.document.as_ref().map(|d| &d.identity) != Some(backing)
    {
        Err(ReadError::Stale)
    } else {
        Ok(())
    }
}

impl PhotoWorkerClient {
    /// The caller must first pass its original Agent read/source/account gate.
    pub(crate) fn prepare_agent_state(
        &self,
        origin: ReadOrigin,
        binding: ReadBinding,
    ) -> Result<PendingState, ReadError> {
        if !self.available() {
            return Err(ReadError::Unavailable);
        }
        let cancellation = Cancellation::new();
        cancellation.check(origin, &self.owned)?;
        if binding.source != origin.source
            || binding.identity.host_instance_id != origin.source.managed.instance
        {
            return Err(ReadError::Stale);
        }
        if self.agent_pending.swap(true, Ordering::SeqCst) {
            return Err(ReadError::Busy);
        }
        let reservation = Arc::new(Reservation(self.agent_pending.clone()));
        let (reply, receiver) = mpsc::sync_channel(1);
        let job = StateJob {
            _reservation: reservation.clone(),
            origin,
            binding,
            cancellation: cancellation.clone(),
            reply,
        };
        self.sender
            .try_send(Command::AgentState(job))
            .map_err(|error| match error {
                TrySendError::Full(_) => ReadError::Busy,
                TrySendError::Disconnected(_) => ReadError::Unavailable,
            })?;
        Ok(PendingState {
            _reservation: reservation,
            origin,
            cancellation,
            receiver,
            done: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn setup() -> (
        PhotoWorkerClient,
        Receiver<Command>,
        ReadOrigin,
        ReadBinding,
    ) {
        let (sender, receiver) = mpsc::sync_channel(8);
        let client = PhotoWorkerClient::new(sender, Arc::new(AtomicBool::new(true)));
        let instance = Uuid::new_v4();
        assert!(client.owned.ready(0, instance));
        let source = OwnedSource {
            managed: client.managed_source().unwrap(),
            target_epoch: 1,
            account_epoch: Some(1),
            ui_id: Uuid::new_v4(),
            ui_epoch: 1,
        };
        let origin = ReadOrigin {
            request_id: Uuid::new_v4(),
            source,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let binding = ReadBinding {
            source,
            identity: PhotoIdentity {
                host_instance_id: instance,
                doc_id: u64::MAX,
                document_generation: u64::MAX - 1,
                revision: u64::MAX - 2,
            },
            history_key: "photo:/private/original.pcraft".into(),
        };
        (client, receiver, origin, binding)
    }

    #[test]
    fn agent_state_original_backing_rejects_save_as_child_and_document_generation() {
        let (_, _, _, binding) = setup();
        let mut newer_revision = binding.identity.clone();
        newer_revision.revision -= 1;
        assert!(
            binding
                .check_backing(&newer_revision, &binding.history_key)
                .is_ok()
        );
        for change in 0..4 {
            let mut identity = binding.identity.clone();
            let mut key = binding.history_key.clone();
            match change {
                0 => identity.host_instance_id = Uuid::new_v4(),
                1 => identity.doc_id -= 1,
                2 => identity.document_generation -= 1,
                _ => key.push_str("-save-as.pcraft"),
            }
            assert_eq!(
                binding.check_backing(&identity, &key),
                Err(ReadError::Stale)
            );
        }
    }

    #[test]
    fn agent_state_expired_and_different_managed_source_do_not_enqueue() {
        for change in 0..4 {
            let (client, receiver, mut origin, binding) = setup();
            match change {
                0 => origin.deadline = Instant::now(),
                1 => origin.source.managed.bind_id = Uuid::new_v4(),
                2 => origin.source.managed.epoch += 1,
                _ => origin.source.managed.instance = Uuid::new_v4(),
            }
            assert!(matches!(
                client.prepare_agent_state(origin, binding),
                Err(ReadError::Expired | ReadError::Stale)
            ));
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn agent_state_cancelled_dropped_and_expired_queued_jobs_never_read_native_host() {
        for change in 0..3 {
            let (client, receiver, origin, binding) = setup();
            let mut pending = client.prepare_agent_state(origin, binding).unwrap();
            let Command::AgentState(mut job) = receiver.try_recv().unwrap() else {
                panic!("expected state command")
            };
            match change {
                0 => pending.cancellation().cancel(),
                1 => job.origin.deadline = Instant::now(),
                _ => {
                    drop(pending);
                    assert!(client.agent_pending.load(Ordering::SeqCst));
                    job.run(&mut NativeHost::new(), &client.owned);
                    assert!(!client.agent_pending.load(Ordering::SeqCst));
                    continue;
                }
            }
            // With an empty host, entering the native call would return Failed.
            job.run(&mut NativeHost::new(), &client.owned);
            assert!(matches!(
                pending.try_result(&client),
                Some(Err(ReadError::Stale | ReadError::Expired))
            ));
            drop(pending);
            assert!(!client.agent_pending.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn agent_state_single_pending_stays_reserved_until_worker_and_receiver_release() {
        let (client, receiver, origin, binding) = setup();
        let second = ReadBinding {
            source: binding.source,
            identity: binding.identity.clone(),
            history_key: binding.history_key.clone(),
        };
        let pending = client.prepare_agent_state(origin, binding).unwrap();
        assert!(matches!(
            client.prepare_agent_state(origin, second),
            Err(ReadError::Busy)
        ));
        let Command::AgentState(job) = receiver.try_recv().unwrap() else {
            panic!("expected state command")
        };
        drop(pending);
        assert!(client.agent_pending.load(Ordering::SeqCst));
        job.run(&mut NativeHost::new(), &client.owned);
        assert!(!client.agent_pending.load(Ordering::SeqCst));
    }

    #[test]
    fn agent_state_wait_has_original_result_deadline_and_cancels_late_worker() {
        let (client, receiver, mut origin, binding) = setup();
        origin.deadline = Instant::now() + Duration::from_millis(5);
        let pending = client.prepare_agent_state(origin, binding).unwrap();
        let cancellation = pending.cancellation();
        let Command::AgentState(job) = receiver.try_recv().unwrap() else {
            panic!("expected state command")
        };
        assert!(matches!(pending.wait(&client), Err(ReadError::Expired)));
        assert!(cancellation.0.load(Ordering::SeqCst));
        job.run(&mut NativeHost::new(), &client.owned);
        assert!(!client.agent_pending.load(Ordering::SeqCst));
    }
}
