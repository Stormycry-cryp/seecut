//! Closed worker reads. Callers own the original account/read/Task gates.
//! No result here is authority to send document content to a model.
use super::Command;
use seecut_photo_host::{
    NativeHost, PhotoBackingBinding, PhotoFileIdentity, PhotoIdentity, PhotoSnapshot,
    PhotoSnapshotValidation, PhotoState, Uuid,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
};
use std::time::{Duration, Instant};

const BACKING_CAPACITY: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadError {
    QueueFull,
    Unavailable,
    Expired,
    Revoked,
    WrongInstance,
    InvalidToken,
    AlreadyTaken,
    ResourceLimit,
    ReadFailed,
}

/// Freeze only after the caller's real read gate passed. This is a revocation
/// fence, not a replacement for that gate. Revocation is monotonic across clones.
#[derive(Clone)]
pub(crate) struct ReadContext {
    id: Uuid,
    instance: Uuid,
    deadline: Instant,
    live: Arc<AtomicBool>,
}
impl ReadContext {
    pub(crate) fn after_gate(id: Uuid, instance: Uuid, deadline: Instant) -> Self {
        Self {
            id,
            instance,
            deadline,
            live: Arc::new(AtomicBool::new(true)),
        }
    }
    pub(crate) fn revoke(&self) {
        self.live.store(false, Ordering::SeqCst);
    }
    pub(crate) fn request_id(&self) -> Uuid {
        self.id
    }
    pub(crate) fn instance(&self) -> Uuid {
        self.instance
    }
    fn check(&self) -> Result<(), ReadError> {
        if !self.live.load(Ordering::SeqCst) {
            Err(ReadError::Revoked)
        } else if Instant::now() >= self.deadline {
            Err(ReadError::Expired)
        } else {
            Ok(())
        }
    }
    fn same(&self, other: &Self) -> bool {
        self.id == other.id
            && self.instance == other.instance
            && self.deadline == other.deadline
            && Arc::ptr_eq(&self.live, &other.live)
    }
}

/// Dropping an unanswered request cancels publication, not the in-flight native
/// read. No timeout/cancellation kills the child or retries the read.
pub(crate) struct PendingRead {
    context: ReadContext,
    receiver: Receiver<Result<ReadResult, ReadError>>,
    abandoned: Arc<AtomicBool>,
    done: bool,
}
impl PendingRead {
    /// Poll from an existing event loop or receiving worker. Recheck the original
    /// account/lease/Task/source after this returns, and again before delivery.
    pub(crate) fn try_result(&mut self) -> Option<Result<ReadResult, ReadError>> {
        if self.done {
            return None;
        }
        if let Err(e) = self.context.check() {
            self.done = true;
            self.abandoned.store(true, Ordering::SeqCst);
            return Some(Err(e));
        }
        match self.receiver.try_recv() {
            Ok(value) => {
                self.done = true;
                Some(self.context.check().and(value))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                Some(Err(ReadError::Unavailable))
            }
        }
    }
}
impl Drop for PendingRead {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::SeqCst);
    }
}

/// Trusted parent history binding only. No Serialize or Debug. Never project
/// the real path into Slint, model input, telemetry, or logs. It is not a grant.
pub(crate) struct BackingSummary {
    identity: PhotoIdentity,
    file_identity: PhotoFileIdentity,
    history_key: String,
}
impl BackingSummary {
    pub(crate) fn identity(&self) -> &PhotoIdentity {
        &self.identity
    }
    pub(crate) fn file_identity(&self) -> &PhotoFileIdentity {
        &self.file_identity
    }
    pub(crate) fn history_key(&self) -> &str {
        &self.history_key
    }
}

/// Opaque continuation. No raw path, PNG, JSON, or Debug implementation.
#[derive(Clone)]
pub(crate) struct CaptureToken {
    id: Uuid,
    context: ReadContext,
    live: Arc<AtomicBool>,
}
impl CaptureToken {
    pub(crate) fn release(self) {
        self.live.store(false, Ordering::SeqCst);
    }
}
#[derive(Clone)]
pub(crate) struct BackingToken {
    slot: Arc<BackingSlot>,
}
struct BackingSlot {
    context: ReadContext,
    value: Mutex<Option<PhotoBackingBinding>>,
    live: AtomicBool,
    summary: BackingSummary,
}
impl BackingToken {
    /// Explicit release works without a read gate. The worker also releases on
    /// cancellation, deadline, close, exit or replacement of the child.
    pub(crate) fn release(self) {
        self.slot.live.store(false, Ordering::SeqCst);
    }
    /// Bounded observation only. The verified File stays in the worker; the path getter is trusted-parent only.
    /// This does not grant access to a project or make the backing current forever.
    pub(crate) fn observation_after_gate(
        &self,
        context: &ReadContext,
    ) -> Result<BackingSummary, ReadError> {
        context.check()?;
        if !self.slot.context.same(context) {
            return Err(ReadError::InvalidToken);
        }
        if !self.slot.live.load(Ordering::SeqCst) {
            return Err(ReadError::InvalidToken);
        }
        let value = BackingSummary {
            identity: self.slot.summary.identity.clone(),
            file_identity: self.slot.summary.file_identity.clone(),
            history_key: self.slot.summary.history_key.clone(),
        };
        context.check()?;
        if !self.slot.live.load(Ordering::SeqCst) {
            return Err(ReadError::InvalidToken);
        }
        Ok(value)
    }
}
/// Temporary owned handle after validating the exact stored capture. The PNG
/// allocation is shared once; the worker keeps the same witness for final runtime
/// validation. A second take is refused, while explicit validation remains usable.
/// It is historical native evidence, not current account/lease authorization.
pub(crate) struct CapturedEvidence {
    pub(crate) capture: Arc<PhotoSnapshot>,
    pub(crate) validation: PhotoSnapshotValidation,
}
pub(crate) enum ReadResult {
    State(PhotoState),
    Backing(BackingToken),
    Capture(CaptureToken),
    Validation(PhotoSnapshotValidation),
    CapturedEvidence(CapturedEvidence),
}
enum ReadOperation {
    State,
    Backing,
    Capture {
        expected: PhotoIdentity,
        max_edge: u32,
    },
    Validate {
        token: CaptureToken,
        take: bool,
    },
}
pub(super) struct ReadJob {
    context: ReadContext,
    operation: ReadOperation,
    wire_id: Uuid,
    abandoned: Arc<AtomicBool>,
    reply: SyncSender<Result<ReadResult, ReadError>>,
}
impl ReadJob {
    fn check(&self) -> Result<(), ReadError> {
        if self.abandoned.load(Ordering::SeqCst) {
            Err(ReadError::Revoked)
        } else {
            self.context.check()
        }
    }
}
#[derive(Clone)]
pub(crate) struct PhotoWorkerClient {
    sender: SyncSender<Command>,
    connected: Arc<AtomicBool>,
}
impl PhotoWorkerClient {
    pub(super) fn new(sender: SyncSender<Command>, connected: Arc<AtomicBool>) -> Self {
        Self { sender, connected }
    }
    pub(crate) fn available(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    fn submit(
        &self,
        context: &ReadContext,
        operation: ReadOperation,
    ) -> Result<PendingRead, ReadError> {
        context.check()?;
        if !self.available() {
            return Err(ReadError::Unavailable);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        let job = ReadJob {
            context: context.clone(),
            operation,
            wire_id: Uuid::new_v4(),
            abandoned: abandoned.clone(),
            reply,
        };
        self.sender
            .try_send(Command::Read(job))
            .map_err(|e| match e {
                TrySendError::Full(_) => ReadError::QueueFull,
                TrySendError::Disconnected(_) => ReadError::Unavailable,
            })?;
        Ok(PendingRead {
            context: context.clone(),
            receiver,
            abandoned,
            done: false,
        })
    }
    pub(crate) fn state_after_gate(&self, context: &ReadContext) -> Result<PendingRead, ReadError> {
        self.submit(context, ReadOperation::State)
    }
    pub(crate) fn backing_after_gate(
        &self,
        context: &ReadContext,
    ) -> Result<PendingRead, ReadError> {
        self.submit(context, ReadOperation::Backing)
    }
    pub(crate) fn capture_after_gate(
        &self,
        context: &ReadContext,
        expected: PhotoIdentity,
        max_edge: u32,
    ) -> Result<PendingRead, ReadError> {
        if expected.host_instance_id != context.instance {
            return Err(ReadError::WrongInstance);
        }
        if !(1..=256).contains(&max_edge) {
            return Err(ReadError::ReadFailed);
        }
        self.submit(context, ReadOperation::Capture { expected, max_edge })
    }
    pub(crate) fn validate_after_gate(
        &self,
        context: &ReadContext,
        token: CaptureToken,
    ) -> Result<PendingRead, ReadError> {
        self.validate(context, token, false)
    }
    /// Explicit single take of an Arc handle; never substitutes a new capture.
    /// Retain the original token for runtime validation before model delivery.
    pub(crate) fn validate_and_take_after_gate(
        &self,
        context: &ReadContext,
        token: CaptureToken,
    ) -> Result<PendingRead, ReadError> {
        self.validate(context, token, true)
    }
    fn validate(
        &self,
        context: &ReadContext,
        token: CaptureToken,
        take: bool,
    ) -> Result<PendingRead, ReadError> {
        if !token.context.same(context) || !token.live.load(Ordering::SeqCst) {
            return Err(ReadError::InvalidToken);
        }
        self.submit(context, ReadOperation::Validate { token, take })
    }
}

pub(super) trait ReadHost {
    fn owned_instance(&mut self) -> Option<Uuid>;
    fn state(&mut self, id: Uuid) -> Result<PhotoState, ReadError>;
    fn backing(&mut self, id: Uuid) -> Result<PhotoBackingBinding, ReadError>;
    fn capture(
        &mut self,
        id: Uuid,
        request: &str,
        expected: PhotoIdentity,
        edge: u32,
    ) -> Result<PhotoSnapshot, ReadError>;
    fn validate(
        &mut self,
        id: Uuid,
        request: &str,
        snapshot: &PhotoSnapshot,
    ) -> Result<PhotoSnapshotValidation, ReadError>;
}
impl ReadHost for NativeHost {
    fn owned_instance(&mut self) -> Option<Uuid> {
        self.current()
            .ok()
            .flatten()
            .filter(|i| i.status != seecut_photo_host::InstanceStatus::Exited && i.ready)
            .map(|i| i.id)
    }
    fn state(&mut self, id: Uuid) -> Result<PhotoState, ReadError> {
        self.photo_state_after_gate(id)
            .map_err(|_| ReadError::ReadFailed)
    }
    fn backing(&mut self, id: Uuid) -> Result<PhotoBackingBinding, ReadError> {
        self.photo_backing_after_gate(id)
            .map_err(|_| ReadError::ReadFailed)
    }
    fn capture(
        &mut self,
        id: Uuid,
        request: &str,
        expected: PhotoIdentity,
        edge: u32,
    ) -> Result<PhotoSnapshot, ReadError> {
        self.photo_snapshot_after_gate(id, request, expected, edge)
            .map_err(|_| ReadError::ReadFailed)
    }
    fn validate(
        &mut self,
        id: Uuid,
        request: &str,
        snapshot: &PhotoSnapshot,
    ) -> Result<PhotoSnapshotValidation, ReadError> {
        self.validate_photo_snapshot_after_gate(id, request, snapshot)
            .map_err(|_| ReadError::ReadFailed)
    }
}
struct StoredCapture {
    token: CaptureToken,
    snapshot: Arc<PhotoSnapshot>,
    delivered: bool,
}
#[derive(Default)]
pub(super) struct ReadStore {
    capture: Option<StoredCapture>,
    backings: Vec<Arc<BackingSlot>>,
}
impl ReadStore {
    fn clear_capture(&mut self) {
        if let Some(stored) = self.capture.take() {
            stored.token.live.store(false, Ordering::SeqCst);
        }
    }
    pub(super) fn clear(&mut self) {
        self.clear_capture();
        for slot in self.backings.drain(..) {
            slot.live.store(false, Ordering::SeqCst);
            if let Ok(mut v) = slot.value.lock() {
                v.take();
            }
        }
    }
    pub(super) fn prune(&mut self, instance: Option<Uuid>) {
        if self.capture.as_ref().is_some_and(|s| {
            !s.token.live.load(Ordering::SeqCst)
                || s.token.context.check().is_err()
                || Some(s.token.context.instance) != instance
                || s.snapshot.request_started_at().elapsed() >= Duration::from_secs(5)
        }) {
            self.clear_capture();
        }
        self.backings.retain(|s| {
            let Ok(mut v) = s.value.lock() else {
                return false;
            };
            if !s.live.load(Ordering::SeqCst)
                || s.context.check().is_err()
                || Some(s.context.instance) != instance
                || Arc::strong_count(s) == 1
            {
                v.take();
            }
            if v.is_none() {
                s.live.store(false, Ordering::SeqCst);
            }
            v.is_some()
        });
    }
    pub(super) fn run(&mut self, host: &mut impl ReadHost, instance: Option<Uuid>, job: ReadJob) {
        self.prune(instance);
        let result = job.check().and_then(|()| {
            if Some(job.context.instance) != instance {
                return Err(ReadError::WrongInstance);
            }
            self.execute(host, &job)
        });
        // Host calls and filesystem checks can outlast the deadline. Discard all
        // resulting material, never claim the native read did not occur.
        let result = job.check().and_then(|()| {
            if host.owned_instance() != Some(job.context.instance) {
                Err(ReadError::WrongInstance)
            } else {
                result
            }
        });
        let result = job.check().and(result);
        if result
            .as_ref()
            .is_err_and(|e| *e != ReadError::AlreadyTaken)
        {
            self.remove_request(&job.context);
        }
        if job.reply.try_send(result).is_err() {
            self.remove_request(&job.context);
        }
    }
    /// Observe only facts obtained by an explicitly gated read. No background
    /// metadata polling. Same-path atomic save/revision does not clear history.
    fn observe_identity(&mut self, identity: Option<&PhotoIdentity>) {
        let same_generation = |old: &PhotoIdentity| {
            identity.is_some_and(|new| {
                new.host_instance_id == old.host_instance_id
                    && new.doc_id == old.doc_id
                    && new.document_generation == old.document_generation
            })
        };
        if self
            .capture
            .as_ref()
            .is_some_and(|s| !same_generation(s.snapshot.identity()))
        {
            self.clear_capture();
        }
        self.backings.retain(|s| {
            let Ok(mut value) = s.value.lock() else {
                return false;
            };
            if value
                .as_ref()
                .is_some_and(|b| !same_generation(b.identity()))
            {
                value.take();
            }
            if value.is_none() {
                s.live.store(false, Ordering::SeqCst);
            }
            value.is_some()
        });
    }
    fn remove_request(&mut self, context: &ReadContext) {
        if self
            .capture
            .as_ref()
            .is_some_and(|s| s.token.context.same(context))
        {
            self.clear_capture();
        }
        self.backings.retain(|s| {
            if !s.context.same(context) {
                return true;
            }
            s.live.store(false, Ordering::SeqCst);
            if let Ok(mut value) = s.value.lock() {
                value.take();
            }
            false
        });
    }
    fn execute(
        &mut self,
        host: &mut impl ReadHost,
        job: &ReadJob,
    ) -> Result<ReadResult, ReadError> {
        let id = job.context.instance;
        match &job.operation {
            ReadOperation::State => {
                let state = host.state(id)?;
                self.observe_identity(state.document.as_ref().map(|d| &d.identity));
                Ok(ReadResult::State(state))
            }
            ReadOperation::Backing => {
                if self.backings.len() >= BACKING_CAPACITY {
                    return Err(ReadError::ResourceLimit);
                }
                let binding = host.backing(id)?;
                self.observe_identity(Some(binding.identity()));
                job.check()?;
                let slot = Arc::new(BackingSlot {
                    context: job.context.clone(),
                    live: AtomicBool::new(true),
                    summary: BackingSummary {
                        identity: binding.identity().clone(),
                        file_identity: binding.file_identity().clone(),
                        history_key: binding.history_key().to_owned(),
                    },
                    value: Mutex::new(Some(binding)),
                });
                self.backings.push(slot.clone());
                Ok(ReadResult::Backing(BackingToken { slot }))
            }
            ReadOperation::Capture { expected, max_edge } => {
                // Native owns one live witness. Replacement is monotonic even if
                // the new request fails: old tokens never become usable again.
                self.clear_capture();
                let snapshot =
                    host.capture(id, &job.context.id.to_string(), expected.clone(), *max_edge)?;
                job.check()?;
                self.observe_identity(Some(snapshot.identity()));
                let token = CaptureToken {
                    id: Uuid::new_v4(),
                    context: job.context.clone(),
                    live: Arc::new(AtomicBool::new(true)),
                };
                self.capture = Some(StoredCapture {
                    token: token.clone(),
                    snapshot: Arc::new(snapshot),
                    delivered: false,
                });
                Ok(ReadResult::Capture(token))
            }
            ReadOperation::Validate { token, take } => {
                let stored = self
                    .capture
                    .as_ref()
                    .filter(|s| {
                        s.token.id == token.id
                            && s.token.context.same(&job.context)
                            && s.token.live.load(Ordering::SeqCst)
                    })
                    .ok_or(ReadError::InvalidToken)?;
                if *take && stored.delivered {
                    return Err(ReadError::AlreadyTaken);
                }
                let validation = host.validate(id, &job.wire_id.to_string(), &stored.snapshot)?;
                job.check()?;
                if *take {
                    let stored = self.capture.as_mut().ok_or(ReadError::InvalidToken)?;
                    stored.delivered = true;
                    let capture = stored.snapshot.clone();
                    Ok(ReadResult::CapturedEvidence(CapturedEvidence {
                        capture,
                        validation,
                    }))
                } else {
                    Ok(ReadResult::Validation(validation))
                }
            }
        }
    }
}
impl Drop for ReadStore {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock {
        calls: usize,
        revoke: Option<ReadContext>,
        fail: bool,
        instance: Option<Uuid>,
    }
    impl ReadHost for Mock {
        fn owned_instance(&mut self) -> Option<Uuid> {
            self.instance
        }
        fn state(&mut self, id: Uuid) -> Result<PhotoState, ReadError> {
            self.calls += 1;
            if let Some(c) = &self.revoke {
                c.revoke();
            }
            if self.fail {
                Err(ReadError::ReadFailed)
            } else {
                Ok(PhotoState {
                    host_instance_id: id,
                    frame: 1,
                    document: None,
                    busy: None,
                })
            }
        }
        fn backing(&mut self, _: Uuid) -> Result<PhotoBackingBinding, ReadError> {
            panic!("unexpected backing")
        }
        fn capture(
            &mut self,
            _: Uuid,
            _: &str,
            _: PhotoIdentity,
            _: u32,
        ) -> Result<PhotoSnapshot, ReadError> {
            panic!("unexpected capture")
        }
        fn validate(
            &mut self,
            _: Uuid,
            _: &str,
            _: &PhotoSnapshot,
        ) -> Result<PhotoSnapshotValidation, ReadError> {
            panic!("unexpected validate")
        }
    }
    fn setup() -> (PhotoWorkerClient, Receiver<Command>, ReadContext) {
        let (tx, rx) = mpsc::sync_channel(8);
        (
            PhotoWorkerClient::new(tx, Arc::new(AtomicBool::new(true))),
            rx,
            ReadContext::after_gate(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Instant::now() + Duration::from_secs(30),
            ),
        )
    }
    fn process(rx: &Receiver<Command>, store: &mut ReadStore, host: &mut Mock, instance: Uuid) {
        let Command::Read(job) = rx.try_recv().unwrap() else {
            panic!("expected read")
        };
        store.run(host, Some(instance), job);
    }
    #[test]
    fn bounded_queue_and_expired_admission_do_not_dispatch() {
        let (c, rx, context) = setup();
        let held: Vec<_> = (0..8)
            .map(|_| c.state_after_gate(&context).unwrap())
            .collect();
        assert!(matches!(
            c.state_after_gate(&context),
            Err(ReadError::QueueFull)
        ));
        let expired = ReadContext::after_gate(Uuid::new_v4(), context.instance, Instant::now());
        assert!(matches!(
            c.state_after_gate(&expired),
            Err(ReadError::Expired)
        ));
        assert_eq!(rx.try_iter().count(), 8);
        drop(held);
    }
    #[test]
    fn expired_queued_and_wrong_instance_and_cancelled_never_call_host() {
        for mode in 0..3 {
            let (c, rx, context) = setup();
            let mut pending = c.state_after_gate(&context).unwrap();
            let mut store = ReadStore::default();
            let mut host = Mock {
                calls: 0,
                revoke: None,
                fail: false,
                instance: Some(context.instance),
            };
            if mode == 0 {
                context.revoke();
            }
            if mode == 1 {
                pending.abandoned.store(true, Ordering::SeqCst);
            }
            process(
                &rx,
                &mut store,
                &mut host,
                if mode == 2 {
                    Uuid::new_v4()
                } else {
                    context.instance
                },
            );
            assert_eq!(host.calls, 0);
            assert!(pending.try_result().unwrap().is_err());
        }
    }
    #[test]
    fn expired_after_queue_and_child_exit_drop_result() {
        let (c, rx, context) = setup();
        let mut pending = c.state_after_gate(&context).unwrap();
        let Command::Read(mut job) = rx.try_recv().unwrap() else {
            panic!("read")
        };
        job.context.deadline = Instant::now();
        let mut host = Mock {
            calls: 0,
            revoke: None,
            fail: false,
            instance: Some(context.instance),
        };
        ReadStore::default().run(&mut host, Some(context.instance), job);
        assert_eq!(host.calls, 0);
        assert!(matches!(
            pending.try_result(),
            Some(Err(ReadError::Expired))
        ));
        let mut pending = c.state_after_gate(&context).unwrap();
        host.instance = None;
        process(&rx, &mut ReadStore::default(), &mut host, context.instance);
        assert!(matches!(
            pending.try_result(),
            Some(Err(ReadError::WrongInstance))
        ));
        assert_eq!(host.calls, 1);
    }
    #[test]
    fn revoked_during_read_discards_result_and_failure_never_retries() {
        for fail in [false, true] {
            let (c, rx, context) = setup();
            let mut pending = c.state_after_gate(&context).unwrap();
            let mut host = Mock {
                calls: 0,
                revoke: (!fail).then(|| context.clone()),
                fail,
                instance: Some(context.instance),
            };
            process(&rx, &mut ReadStore::default(), &mut host, context.instance);
            assert!(pending.try_result().unwrap().is_err());
            assert_eq!(host.calls, 1);
        }
    }
    #[test]
    fn continuation_rejects_other_context_and_unknown_token_without_host() {
        let (c, rx, context) = setup();
        let other = ReadContext::after_gate(context.id, context.instance, context.deadline);
        let token = CaptureToken {
            id: Uuid::new_v4(),
            context: context.clone(),
            live: Arc::new(AtomicBool::new(true)),
        };
        assert!(matches!(
            c.validate_after_gate(&other, token.clone()),
            Err(ReadError::InvalidToken)
        ));
        let released = token.clone();
        released.release();
        assert!(matches!(
            c.validate_after_gate(&context, token.clone()),
            Err(ReadError::InvalidToken)
        ));
        let token = CaptureToken {
            id: Uuid::new_v4(),
            context: context.clone(),
            live: Arc::new(AtomicBool::new(true)),
        };
        let mut pending = c.validate_after_gate(&context, token).unwrap();
        let mut host = Mock {
            calls: 0,
            revoke: None,
            fail: false,
            instance: Some(context.instance),
        };
        process(&rx, &mut ReadStore::default(), &mut host, context.instance);
        assert!(matches!(
            pending.try_result(),
            Some(Err(ReadError::InvalidToken))
        ));
        assert_eq!(host.calls, 0);
    }
}
