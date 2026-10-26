//! Parent-only metadata preparation. No read lease, PNG, or model authority.
use super::{Command, target::ManagedSource};
use seecut_photo_host::{NativeHost, PhotoBackingBinding, PhotoIdentity, Uuid};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnedSource {
    pub(crate) managed: ManagedSource,
    pub(crate) target_epoch: u64,
    pub(crate) account_epoch: Option<u64>,
    pub(crate) ui_id: Uuid,
    pub(crate) ui_epoch: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnedOrigin {
    pub(crate) request_id: Uuid,
    pub(crate) source: OwnedSource,
    pub(crate) deadline: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnedError {
    Unavailable,
    Busy,
    Expired,
    Stale,
    Failed,
}

/// Shared lifecycle facts only; file and TCP ownership remain on the host worker.
pub(super) struct OwnedControl {
    pub(super) bind_id: Uuid,
    lifecycle: Mutex<(u64, Option<Uuid>, bool, bool)>,
    pending: AtomicBool,
}
impl OwnedControl {
    pub(super) fn new() -> Self {
        Self {
            bind_id: Uuid::new_v4(),
            lifecycle: Mutex::new((0, None, false, false)),
            pending: AtomicBool::new(false),
        }
    }
    /// Called synchronously before enqueueing Open/Close, and on child loss.
    pub(super) fn invalidate(&self) -> Option<u64> {
        let mut state = self.lifecycle.lock().ok()?;
        state.1 = None;
        state.2 = false;
        if state.3 {
            return None;
        }
        let Some(next) = state.0.checked_add(1) else {
            state.3 = true;
            return None;
        };
        state.0 = next;
        Some(state.0)
    }
    pub(super) fn ready(&self, epoch: u64, instance: Uuid) -> bool {
        let Ok(mut state) = self.lifecycle.lock() else {
            return false;
        };
        if state.3 || state.0 != epoch {
            return false;
        }
        state.1 = Some(instance);
        state.2 = true;
        true
    }
    pub(super) fn current(&self) -> Option<ManagedSource> {
        let state = self.lifecycle.lock().ok()?;
        state.2.then_some(())?;
        Some(ManagedSource {
            bind_id: self.bind_id,
            epoch: state.0,
            instance: state.1?,
        })
    }
}

struct Reservation(Arc<OwnedControl>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.pending.store(false, Ordering::SeqCst);
    }
}
struct Ticket {
    live: AtomicBool,
    installed: AtomicBool,
}
impl Ticket {
    fn check(&self, origin: OwnedOrigin, control: &OwnedControl) -> Result<(), OwnedError> {
        if !self.live.load(Ordering::SeqCst) || control.current() != Some(origin.source.managed) {
            return Err(OwnedError::Stale);
        }
        if !self.installed.load(Ordering::SeqCst) && Instant::now() >= origin.deadline {
            return Err(OwnedError::Expired);
        }
        Ok(())
    }
}
/// Historical association only. Its private key never enters display/model data.
pub(crate) struct OwnedMetadata {
    origin: OwnedOrigin,
    identity: PhotoIdentity,
    history_key: String,
    reservation: Option<Arc<Reservation>>,
    ticket: Arc<Ticket>,
}
impl OwnedMetadata {
    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        let identity = self.identity();
        let other_identity = other.identity();
        self.origin.source == other.origin.source
            && self.history_key == other.history_key
            && identity.host_instance_id == other_identity.host_instance_id
            && identity.doc_id == other_identity.doc_id
            && identity.document_generation == other_identity.document_generation
    }
    pub(crate) fn origin(&self) -> OwnedOrigin {
        self.origin
    }
    pub(crate) fn identity(&self) -> &PhotoIdentity {
        &self.identity
    }
    pub(crate) fn history_key(&self) -> &str {
        &self.history_key
    }
    pub(crate) fn current(&self, source: OwnedSource, client: &PhotoWorkerClient) -> bool {
        self.origin.source == source
            && client.available()
            && self.ticket.check(self.origin, &client.owned).is_ok()
    }
    pub(crate) fn install(
        &mut self,
        expected: OwnedOrigin,
        source: OwnedSource,
        client: &PhotoWorkerClient,
    ) -> Result<(), OwnedError> {
        if self.origin != expected || expected.source != source || !client.available() {
            return Err(OwnedError::Stale);
        }
        self.ticket.check(expected, &client.owned)?;
        // Installation is bounded. The installed historical key itself does not
        // expire on a timer; a later read still needs a fresh actual read gate.
        if Instant::now() >= expected.deadline {
            return Err(OwnedError::Expired);
        }
        self.ticket.installed.store(true, Ordering::SeqCst);
        self.reservation.take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_pending_generation_cannot_restore_old_source() {
        let control = OwnedControl::new();
        let first_epoch = control.invalidate().unwrap();
        let instance = Uuid::new_v4();
        assert!(control.ready(first_epoch, instance));
        let old = control.current().unwrap();
        let close_epoch = control.invalidate().unwrap();
        assert!(!control.ready(first_epoch, instance));
        assert!(control.current().is_none());
        // The worker has separately confirmed the same still-live Ready child.
        // This models NeedsUser and confers no model read or edit capability.
        assert!(control.ready(close_epoch, instance));
        assert!(control.current().unwrap() != old);
        control.invalidate(); // worker observed exit
        assert!(!control.ready(close_epoch, instance));
        assert!(control.current().is_none());
    }
    #[test]
    fn lifecycle_overflow_is_permanently_unavailable() {
        let control = OwnedControl::new();
        control.lifecycle.lock().unwrap().0 = u64::MAX;
        assert!(control.invalidate().is_none());
        assert!(!control.ready(u64::MAX, Uuid::new_v4()));
        assert!(control.invalidate().is_none());
        assert!(control.current().is_none());
    }
    #[test]
    fn only_installed_historical_association_outlives_preparation_deadline() {
        let control = OwnedControl::new();
        assert!(control.ready(0, Uuid::new_v4()));
        let origin = OwnedOrigin {
            request_id: Uuid::new_v4(),
            source: OwnedSource {
                managed: control.current().unwrap(),
                target_epoch: 1,
                account_epoch: Some(1),
                ui_id: Uuid::new_v4(),
                ui_epoch: 0,
            },
            deadline: Instant::now(),
        };
        let ticket = Ticket {
            live: AtomicBool::new(true),
            installed: AtomicBool::new(false),
        };
        assert!(matches!(
            ticket.check(origin, &control),
            Err(OwnedError::Expired)
        ));
        ticket.installed.store(true, Ordering::SeqCst);
        assert!(ticket.check(origin, &control).is_ok());
        control.invalidate();
        assert!(matches!(
            ticket.check(origin, &control),
            Err(OwnedError::Stale)
        ));
    }
}
impl Drop for OwnedMetadata {
    fn drop(&mut self) {
        self.ticket.live.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct PendingOwnedMetadata {
    _reservation: Arc<Reservation>,
    origin: OwnedOrigin,
    ticket: Arc<Ticket>,
    receiver: Receiver<Result<OwnedMetadata, OwnedError>>,
    done: bool,
}
impl PendingOwnedMetadata {
    pub(crate) fn origin(&self) -> OwnedOrigin {
        self.origin
    }
    pub(crate) fn try_result(
        &mut self,
        client: &PhotoWorkerClient,
    ) -> Option<Result<OwnedMetadata, OwnedError>> {
        if self.done {
            return None;
        }
        if let Err(e) = self.ticket.check(self.origin, &client.owned) {
            self.done = true;
            return Some(Err(e));
        }
        match self.receiver.try_recv() {
            Ok(value) => {
                self.done = true;
                Some(value.and_then(|metadata| {
                    if metadata.origin != self.origin
                        || !Arc::ptr_eq(&metadata.ticket, &self.ticket)
                    {
                        return Err(OwnedError::Stale);
                    }
                    metadata.ticket.check(self.origin, &client.owned)?;
                    Ok(metadata)
                }))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                Some(Err(OwnedError::Unavailable))
            }
        }
    }
}
impl Drop for PendingOwnedMetadata {
    fn drop(&mut self) {
        if !self.done {
            self.ticket.live.store(false, Ordering::SeqCst);
        }
    }
}
pub(super) struct OwnedJob {
    reservation: Arc<Reservation>,
    origin: OwnedOrigin,
    ticket: Arc<Ticket>,
    reply: SyncSender<Result<OwnedMetadata, OwnedError>>,
}
#[derive(Clone)]
pub(crate) struct PhotoWorkerClient {
    pub(super) sender: SyncSender<Command>,
    connected: Arc<AtomicBool>,
    pub(super) agent_pending: Arc<AtomicBool>,
    pub(super) owned: Arc<OwnedControl>,
}
impl PhotoWorkerClient {
    pub(super) fn new(sender: SyncSender<Command>, connected: Arc<AtomicBool>) -> Self {
        Self {
            sender,
            connected,
            agent_pending: Arc::new(AtomicBool::new(false)),
            owned: Arc::new(OwnedControl::new()),
        }
    }
    pub(crate) fn available(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub(crate) fn preparing_owned_metadata(&self) -> bool {
        self.owned.pending.load(Ordering::SeqCst)
    }
    pub(crate) fn managed_source(&self) -> Option<ManagedSource> {
        self.available().then(|| self.owned.current()).flatten()
    }
    pub(crate) fn prepare_owned_metadata(
        &self,
        origin: OwnedOrigin,
    ) -> Result<PendingOwnedMetadata, OwnedError> {
        if !self.available() {
            return Err(OwnedError::Unavailable);
        }
        let ticket = Arc::new(Ticket {
            live: AtomicBool::new(true),
            installed: AtomicBool::new(false),
        });
        ticket.check(origin, &self.owned)?;
        if self.owned.pending.swap(true, Ordering::SeqCst) {
            return Err(OwnedError::Busy);
        }
        let reservation = Arc::new(Reservation(self.owned.clone()));
        let (reply, receiver) = mpsc::sync_channel(1);
        let job = OwnedJob {
            reservation: reservation.clone(),
            origin,
            ticket: ticket.clone(),
            reply,
        };
        if self
            .sender
            .try_send(Command::PrepareOwnedMetadata(job))
            .is_err()
        {
            return Err(OwnedError::Busy);
        }
        Ok(PendingOwnedMetadata {
            _reservation: reservation,
            origin,
            ticket,
            receiver,
            done: false,
        })
    }
}

#[derive(Default)]
pub(super) struct OwnedStore {
    held: Option<(OwnedOrigin, Arc<Ticket>, PhotoBackingBinding)>,
}
impl OwnedStore {
    pub(super) fn clear(&mut self) {
        if let Some((_, ticket, _)) = self.held.take() {
            ticket.live.store(false, Ordering::SeqCst);
        }
    }
    pub(super) fn prune(&mut self, control: &OwnedControl) {
        if self.held.as_ref().is_some_and(|(origin, ticket, _)| {
            ticket.check(*origin, control).is_err() || Arc::strong_count(ticket) == 1
        }) {
            self.clear();
        }
    }
    pub(super) fn run(&mut self, host: &mut NativeHost, control: &OwnedControl, job: OwnedJob) {
        let result = (|| {
            job.ticket.check(job.origin, control)?;
            self.clear();
            let binding = host
                .owned_photo_workspace_binding(job.origin.source.managed.instance)
                .map_err(|_| OwnedError::Failed)?;
            job.ticket.check(job.origin, control)?;
            if binding.identity().host_instance_id != job.origin.source.managed.instance {
                return Err(OwnedError::Stale);
            }
            let metadata = OwnedMetadata {
                origin: job.origin,
                identity: binding.identity().clone(),
                history_key: binding.history_key().to_owned(),
                reservation: Some(job.reservation.clone()),
                ticket: job.ticket.clone(),
            };
            self.held = Some((job.origin, job.ticket.clone(), binding));
            Ok(metadata)
        })();
        let _ = job.reply.send(result);
        self.prune(control);
    }
}
