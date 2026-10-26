//! One original Photo operation's immutable input and dispatch arbitration.
//! Integrate below editor_mcp beside write_registry. The original UI gate and
//! Receipts own admission/results; this module grants no permit, performs no IO,
//! calls no child and keeps no operation map. Freeze only on Begin::New, and
//! retain/clone that same value for retries. Clones share exactly one control.

use super::write_registry::{OperationKey, Scope};
use seecut_photo_host::{PhotoEditAction, PhotoIdentity};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

const MAX_SCOPE_BYTES: usize = 1024;
const MAX_WAIT: Duration = Duration::from_secs(300);
const PENDING: u8 = 0;
const READY: u8 = 1;
const STARTED: u8 = 2;

/// Closed local validation failures; never includes input data or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreezeError {
    InvalidScope,
    InvalidOperation,
    InvalidNativeBinding,
    InvalidAction,
    InvalidDeadline,
    OriginalMismatch,
    ParameterMismatch,
}

/// First retained reason why this original job was not dispatched.
/// A reason records a trusted caller's stop event; it does not authorize a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum StopReason {
    Cancelled = 16,
    Revoked = 17,
    ScopeChanged = 18,
    TaskChanged = 19,
    AccountChanged = 20,
    Expired = 21,
    QueueUnavailable = 22,
    UiUnavailable = 23,
}
impl StopReason {
    fn from_tag(tag: u8) -> Self {
        match tag {
            16 => Self::Cancelled,
            17 => Self::Revoked,
            18 => Self::ScopeChanged,
            19 => Self::TaskChanged,
            20 => Self::AccountChanged,
            21 => Self::Expired,
            22 => Self::QueueUnavailable,
            23 => Self::UiUnavailable,
            _ => unreachable!("private Photo dispatch state"),
        }
    }
}

/// This is only the local execution boundary, never a child/result receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchPhase {
    Pending,
    Ready,
    DispatchStarted,
    NotDispatched(StopReason),
}
fn phase(tag: u8) -> DispatchPhase {
    match tag {
        PENDING => DispatchPhase::Pending,
        READY => DispatchPhase::Ready,
        STARTED => DispatchPhase::DispatchStarted,
        value => DispatchPhase::NotDispatched(StopReason::from_tag(value)),
    }
}

/// Refusing a second start never implies that an earlier start did not execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchRefusal {
    NotReady,
    AlreadyReady,
    InFlight,
    NotDispatched(StopReason),
    /// The CAS won, but the immediate post-CAS deadline check failed. No start
    /// marker is issued and the caller must not execute. State stays Started;
    /// a later cancellation cannot relabel that boundary as never claimed.
    ExpiredAfterStart,
}
fn refusal(tag: u8) -> DispatchRefusal {
    match phase(tag) {
        DispatchPhase::Pending => DispatchRefusal::NotReady,
        DispatchPhase::Ready => DispatchRefusal::AlreadyReady,
        DispatchPhase::DispatchStarted => DispatchRefusal::InFlight,
        DispatchPhase::NotDispatched(reason) => DispatchRefusal::NotDispatched(reason),
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopOutcome {
    NotDispatched(StopReason),
    InFlight,
}

struct Original<P> {
    scope: Scope,
    key: OperationKey,
    parent_operation_id: String,
    child_operation_id: String,
    expected: PhotoIdentity,
    action: PhotoEditAction,
    payload: P,
}
struct Control {
    phase: AtomicU8,
    deadline: Instant,
}
/// The original absolute deadline and one captured clock observation.
struct FreezeTime {
    deadline: Instant,
    now: Instant,
}
impl Control {
    fn stop(&self, reason: StopReason) -> StopOutcome {
        loop {
            let current = self.phase.load(Ordering::Acquire);
            match phase(current) {
                DispatchPhase::Pending | DispatchPhase::Ready => {
                    if self
                        .phase
                        .compare_exchange(
                            current,
                            reason as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return StopOutcome::NotDispatched(reason);
                    }
                }
                DispatchPhase::DispatchStarted => return StopOutcome::InFlight,
                DispatchPhase::NotDispatched(first) => return StopOutcome::NotDispatched(first),
            }
        }
    }
    fn stop_refusal(&self, reason: StopReason) -> DispatchRefusal {
        match self.stop(reason) {
            StopOutcome::NotDispatched(first) => DispatchRefusal::NotDispatched(first),
            StopOutcome::InFlight => DispatchRefusal::InFlight,
        }
    }
}

/// Retain in the existing original Receipts attachment; never create a second
/// copy with new control on retry. P is the complete validated parent payload,
/// not a hash. The adapter still bounds P and checks account/Task/permit state.
/// No Debug/Serialize bound is imposed on P and none of its data is formatted.
pub(crate) struct FrozenPhotoOperation<P: PartialEq + Clone> {
    original: Arc<Original<P>>,
    control: Arc<Control>,
}
impl<P: PartialEq + Clone> Clone for FrozenPhotoOperation<P> {
    fn clone(&self) -> Self {
        Self {
            original: Arc::clone(&self.original),
            control: Arc::clone(&self.control),
        }
    }
}
impl<P: PartialEq + Clone> std::fmt::Debug for FrozenPhotoOperation<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenPhotoOperation")
            .field("phase", &phase(self.control.phase.load(Ordering::Acquire)))
            .finish_non_exhaustive()
    }
}
impl<P: PartialEq + Clone> PartialEq for FrozenPhotoOperation<P> {
    fn eq(&self, other: &Self) -> bool {
        let b = &other.original;
        self.check_original(
            &b.scope,
            &b.key,
            &b.parent_operation_id,
            &b.expected,
            b.action,
            &b.payload,
        )
        .is_ok()
    }
}

/// Non-Clone evidence that this shared control's one start CAS won before its
/// post-check deadline. It is NOT an authorization permit or a TCP/child receipt.
/// Use only to guard one real call on the already owned serial worker. Dropping
/// it does not reset Started or permit a second call.
pub(crate) struct DispatchStart {
    control: Arc<Control>,
}
impl std::fmt::Debug for DispatchStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchStart").finish_non_exhaustive()
    }
}
impl DispatchStart {
    pub(crate) fn is_for<P: PartialEq + Clone>(&self, original: &FrozenPhotoOperation<P>) -> bool {
        Arc::ptr_eq(&self.control, &original.control)
    }
}

impl<P: PartialEq + Clone> FrozenPhotoOperation<P> {
    pub(crate) fn freeze(
        scope: Scope,
        key: OperationKey,
        parent_operation_id: String,
        expected: PhotoIdentity,
        action: PhotoEditAction,
        payload: P,
        deadline: Instant,
    ) -> Result<Self, FreezeError> {
        Self::freeze_at(
            scope,
            key,
            parent_operation_id,
            expected,
            action,
            payload,
            FreezeTime {
                deadline,
                now: Instant::now(),
            },
        )
    }
    fn freeze_at(
        scope: Scope,
        key: OperationKey,
        parent_operation_id: String,
        expected: PhotoIdentity,
        action: PhotoEditAction,
        payload: P,
        time: FreezeTime,
    ) -> Result<Self, FreezeError> {
        let FreezeTime { deadline, now } = time;
        if [
            &scope.instance,
            &scope.project,
            &scope.session,
            &key.client,
            &key.document_session,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > MAX_SCOPE_BYTES || s.contains('\0'))
        {
            return Err(FreezeError::InvalidScope);
        }
        if key.document_session != scope.session
            || concat_editor_mcp::operation_id(&key.client, &scope.session, key.client_sequence)
                .map_err(|_| FreezeError::InvalidOperation)?
                != parent_operation_id
        {
            return Err(FreezeError::InvalidOperation);
        }
        if expected.host_instance_id.is_nil()
            || expected.doc_id == 0
            || expected.document_generation == 0
            || expected.revision == 0
        {
            return Err(FreezeError::InvalidNativeBinding);
        }
        match action {
            PhotoEditAction::LayerSetOpacity { layer_id, opacity }
                if layer_id > 0 && opacity.is_finite() && (0.0..=1.0).contains(&opacity) => {}
            PhotoEditAction::Undo => {}
            _ => return Err(FreezeError::InvalidAction),
        }
        if deadline <= now
            || now
                .checked_add(MAX_WAIT)
                .is_none_or(|limit| deadline > limit)
        {
            return Err(FreezeError::InvalidDeadline);
        }
        let mut hash = Sha256::new();
        hash.update(b"seecut-photo-child-operation-v1\0");
        for text in [&scope.instance, &scope.project, &scope.session, &key.client] {
            length_prefixed(&mut hash, text)?;
        }
        hash.update(key.client_sequence.to_le_bytes());
        length_prefixed(&mut hash, &parent_operation_id)?;
        hash.update(expected.host_instance_id.as_bytes());
        hash.update(expected.doc_id.to_le_bytes());
        hash.update(expected.document_generation.to_le_bytes());
        let child_operation_id = format!("photo-op-v1-{:x}", hash.finalize());
        Ok(Self {
            original: Arc::new(Original {
                scope,
                key,
                parent_operation_id,
                child_operation_id,
                expected,
                action,
                payload,
            }),
            control: Arc::new(Control {
                phase: AtomicU8::new(PENDING),
                deadline,
            }),
        })
    }
    /// Run after the original UI account/scope authorization and before using
    /// its original registry entry. No identity is derived from current state.
    pub(crate) fn check_original(
        &self,
        scope: &Scope,
        key: &OperationKey,
        parent_operation_id: &str,
        expected: &PhotoIdentity,
        action: PhotoEditAction,
        payload: &P,
    ) -> Result<(), FreezeError> {
        let original = &self.original;
        if original.scope != *scope
            || original.key != *key
            || original.parent_operation_id != parent_operation_id
        {
            return Err(FreezeError::OriginalMismatch);
        }
        if original.expected != *expected
            || original.action != action
            || original.payload != *payload
        {
            return Err(FreezeError::ParameterMismatch);
        }
        Ok(())
    }
    pub(crate) fn child_operation_id(&self) -> &str {
        &self.original.child_operation_id
    }
    pub(crate) fn expected(&self) -> &PhotoIdentity {
        &self.original.expected
    }
    pub(crate) fn action(&self) -> PhotoEditAction {
        self.original.action
    }
    pub(crate) fn payload(&self) -> &P {
        &self.original.payload
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.control.deadline
    }
    /// No timer/thread is created. Expiry is checked whenever this state or a
    /// transition is accessed, and again immediately around the start CAS.
    pub(crate) fn phase(&self) -> DispatchPhase {
        if Instant::now() >= self.control.deadline {
            self.control.stop(StopReason::Expired);
        }
        phase(self.control.phase.load(Ordering::Acquire))
    }
    /// Only the trusted UI's completed original checks may call this. Recording
    /// Ready itself issues no permit and cannot replace those checks.
    pub(crate) fn mark_ready(&self) -> Result<(), DispatchRefusal> {
        self.mark_ready_at(Instant::now())
    }
    fn mark_ready_at(&self, now: Instant) -> Result<(), DispatchRefusal> {
        if now >= self.control.deadline {
            return Err(self.control.stop_refusal(StopReason::Expired));
        }
        self.control
            .phase
            .compare_exchange(PENDING, READY, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(refusal)
    }
    pub(crate) fn stop_before_dispatch(&self, reason: StopReason) -> StopOutcome {
        self.control.stop(reason)
    }
    /// A sole winner may call the real host once. Started is a local claim,
    /// never evidence that bytes were sent or that the child edited anything.
    pub(crate) fn try_start(&self) -> Result<DispatchStart, DispatchRefusal> {
        self.try_start_with_clock(Instant::now)
    }
    fn try_start_with_clock(
        &self,
        mut now: impl FnMut() -> Instant,
    ) -> Result<DispatchStart, DispatchRefusal> {
        if now() >= self.control.deadline {
            return Err(self.control.stop_refusal(StopReason::Expired));
        }
        self.control
            .phase
            .compare_exchange(READY, STARTED, Ordering::AcqRel, Ordering::Acquire)
            .map_err(refusal)?;
        if now() >= self.control.deadline {
            return Err(DispatchRefusal::ExpiredAfterStart);
        }
        Ok(DispatchStart {
            control: Arc::clone(&self.control),
        })
    }
}
fn length_prefixed(hash: &mut Sha256, value: &str) -> Result<(), FreezeError> {
    let bytes = u32::try_from(value.len()).map_err(|_| FreezeError::InvalidScope)?;
    hash.update(bytes.to_le_bytes());
    hash.update(value.as_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seecut_photo_host::Uuid;
    use std::sync::Barrier;

    // Deliberately no Debug/Serialize: generic private payloads must not acquire
    // a formatting requirement or leak via the operation's Debug implementation.
    #[derive(Clone, PartialEq)]
    struct Payload {
        target: String,
        value: String,
    }
    #[derive(Clone)]
    struct Input {
        scope: Scope,
        key: OperationKey,
        parent: String,
        expected: PhotoIdentity,
        action: PhotoEditAction,
        payload: Payload,
    }
    impl Input {
        fn fixture() -> Self {
            let scope = Scope {
                instance: "parent:instance".into(),
                project: "工程:p".into(),
                session: "文档:s".into(),
            };
            let key = OperationKey {
                client: "客:户".into(),
                document_session: scope.session.clone(),
                client_sequence: u64::MAX,
            };
            let parent =
                concat_editor_mcp::operation_id(&key.client, &scope.session, key.client_sequence)
                    .unwrap();
            Self {
                scope,
                key,
                parent,
                expected: PhotoIdentity {
                    host_instance_id: Uuid::parse_str("b7b10cf7-a908-4fbb-80f4-56a2dfe169aa")
                        .unwrap(),
                    doc_id: u64::MAX,
                    document_generation: u64::MAX - 1,
                    revision: (1u64 << 53) + 1,
                },
                action: PhotoEditAction::LayerSetOpacity {
                    layer_id: u64::MAX - 2,
                    opacity: 0.4,
                },
                payload: Payload {
                    target: "opaque-original-target".into(),
                    value: "private-token-like-payload".into(),
                },
            }
        }
        fn freeze(&self) -> FrozenPhotoOperation<Payload> {
            self.freeze_at(Instant::now(), Duration::from_secs(60))
                .unwrap()
        }
        fn freeze_at(
            &self,
            now: Instant,
            wait: Duration,
        ) -> Result<FrozenPhotoOperation<Payload>, FreezeError> {
            FrozenPhotoOperation::freeze_at(
                self.scope.clone(),
                self.key.clone(),
                self.parent.clone(),
                self.expected.clone(),
                self.action,
                self.payload.clone(),
                FreezeTime {
                    deadline: now + wait,
                    now,
                },
            )
        }
        fn check(&self, original: &FrozenPhotoOperation<Payload>) -> Result<(), FreezeError> {
            original.check_original(
                &self.scope,
                &self.key,
                &self.parent,
                &self.expected,
                self.action,
                &self.payload,
            )
        }
        fn update_parent(&mut self) {
            self.key.document_session = self.scope.session.clone();
            self.parent = concat_editor_mcp::operation_id(
                &self.key.client,
                &self.scope.session,
                self.key.client_sequence,
            )
            .unwrap();
        }
    }
    #[test]
    fn seecut_photo_control_exact_byte_golden_unicode_u64_and_private_debug() {
        let input = Input::fixture();
        assert_eq!(input.parent, "scm2:1:7:客:户:8:文档:s:18446744073709551615");
        let original = input.freeze();
        // Independently calculated with Python stdlib LP/u64LE/UUID.bytes/SHA256;
        // this literal catches endian, byte-length, field-order and normalization drift.
        assert_eq!(
            original.child_operation_id(),
            "photo-op-v1-e1e586bfb128f1ea7cba54b382c9e87ffd91b9407efdbf210fb4e6641f25740a"
        );
        assert_eq!(original.child_operation_id().len(), 76);
        assert!(
            original
                .child_operation_id()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        );
        assert_eq!(original.expected().doc_id, u64::MAX);
        assert_eq!(original.expected().document_generation, u64::MAX - 1);
        assert_eq!(original.expected().revision, (1u64 << 53) + 1);
        assert!(
            matches!(original.action(), PhotoEditAction::LayerSetOpacity { layer_id, opacity } if layer_id == u64::MAX-2 && opacity == 0.4)
        );
        assert_eq!(original.payload().target, input.payload.target);
        input.check(&original).unwrap();
        let debug = format!("{original:?}");
        for hidden in [
            &input.scope.instance,
            &input.scope.project,
            &input.scope.session,
            &input.key.client,
            &input.parent,
            &input.payload.target,
            &input.payload.value,
        ] {
            assert!(!debug.contains(hidden));
        }
    }
    #[test]
    fn seecut_photo_control_independent_parent_vectors_match_public_constructor() {
        // Root generated these inputs/outputs independently of this Rust module.
        // Keep literal expected IDs, not this module's encoder as the oracle.
        for (instance, project, session, client, sequence, host, doc, generation, parent, child) in [
            (
                "app-instance",
                "project-A",
                "session",
                "client",
                17,
                "00112233-4455-4677-8899-aabbccddeeff",
                9_007_199_254_740_993,
                u64::MAX,
                "scm2:1:6:client:7:session:17",
                "photo-op-v1-882aad19a7ec060c7ed78600c175037e6cf4c75110f84dcdb1623076b99253d0",
            ),
            (
                "应用:一",
                "项目:甲",
                "会话:乙",
                "客:户",
                9_007_199_254_740_991,
                "ffffffff-ffff-4fff-bfff-ffffffffffff",
                u64::MAX,
                9_007_199_254_740_993,
                "scm2:1:7:客:户:10:会话:乙:9007199254740991",
                "photo-op-v1-ad5ebcd7d3723a52e4d7545e8220e4e30a352821e6cd05c99a45ea22b496e605",
            ),
        ] {
            let expected = PhotoIdentity {
                host_instance_id: Uuid::parse_str(host).unwrap(),
                doc_id: doc,
                document_generation: generation,
                revision: u64::MAX,
            };
            assert_eq!(
                concat_editor_mcp::operation_id(client, session, sequence).unwrap(),
                parent
            );
            let original = FrozenPhotoOperation::freeze(
                Scope {
                    instance: instance.into(),
                    project: project.into(),
                    session: session.into(),
                },
                OperationKey {
                    client: client.into(),
                    document_session: session.into(),
                    client_sequence: sequence,
                },
                parent.into(),
                expected.clone(),
                PhotoEditAction::Undo,
                Payload {
                    target: "owned".into(),
                    value: "original".into(),
                },
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            assert_eq!(original.child_operation_id(), child);
            assert_eq!(original.expected(), &expected);
        }
    }
    #[test]
    fn seecut_photo_control_original_payload_conflicts_never_change_child_identity() {
        let input = Input::fixture();
        let original = input.freeze();
        for changed in 0..4 {
            let mut other = input.clone();
            match changed {
                0 => other.payload.value = "different".into(),
                1 => other.expected.revision += 1,
                2 => other.action = PhotoEditAction::Undo,
                _ => {
                    other.action = PhotoEditAction::LayerSetOpacity {
                        layer_id: 1,
                        opacity: 0.8,
                    }
                }
            }
            assert_eq!(other.check(&original), Err(FreezeError::ParameterMismatch));
            let separate = other.freeze();
            // Hashing does not create a second ID for a modified request. The
            // original registry must reject the full changed payload before queueing.
            assert_eq!(separate.child_operation_id(), original.child_operation_id());
            assert_ne!(separate, original);
        }
        let clone = original.clone();
        original.mark_ready().unwrap();
        assert_eq!(
            original, clone,
            "fingerprint equality does not change with dispatch phase"
        );
        assert_eq!(clone.phase(), DispatchPhase::Ready);
        clone.stop_before_dispatch(StopReason::Revoked);
        assert_eq!(original, clone);
        assert_eq!(
            original.phase(),
            DispatchPhase::NotDispatched(StopReason::Revoked)
        );
    }
    #[test]
    fn seecut_photo_control_parent_id_scope_and_byte_boundaries_are_validated() {
        let input = Input::fixture();
        for change in 0..12 {
            let mut bad = input.clone();
            match change {
                0 => bad.scope.instance.clear(),
                1 => bad.scope.project.clear(),
                2 => bad.scope.session.clear(),
                3 => bad.key.client.clear(),
                4 => bad.scope.project = "x".repeat(MAX_SCOPE_BYTES + 1),
                5 => bad.scope.instance = "x".repeat(MAX_SCOPE_BYTES + 1),
                6 => bad.scope.project = "nul\0scope".into(),
                7 => bad.key.document_session = "different-session".into(),
                8 => bad.key.client_sequence = 0,
                9 => bad.parent.push('0'),
                10 => bad.key.client = "x".repeat(65),
                _ => bad.scope.session = "文".repeat(22),
            }
            assert!(
                bad.freeze_at(Instant::now(), Duration::from_secs(60))
                    .is_err(),
                "case {change}"
            );
        }
        let mut maximum = input.clone();
        maximum.key.client = "c".repeat(64);
        maximum.scope.session = "s".repeat(64);
        maximum.scope.project = "p".repeat(MAX_SCOPE_BYTES);
        maximum.scope.instance = "i".repeat(MAX_SCOPE_BYTES);
        maximum.update_parent();
        assert_eq!(maximum.freeze().child_operation_id().len(), 76);
        let original = input.freeze();
        let mut other = input;
        other.scope.project.push('x');
        assert_eq!(other.check(&original), Err(FreezeError::OriginalMismatch));
    }
    #[test]
    fn seecut_photo_control_unicode_is_not_normalized_and_colons_are_unambiguous() {
        let mut first = Input::fixture();
        first.key.client = "a:b".into();
        first.scope.session = "c".into();
        first.update_parent();
        let mut second = first.clone();
        second.key.client = "a".into();
        second.scope.session = "b:c".into();
        second.update_parent();
        assert_ne!(
            first.freeze().child_operation_id(),
            second.freeze().child_operation_id()
        );
        first.key.client = "é".into();
        first.update_parent();
        second = first.clone();
        second.key.client = "e\u{301}".into();
        second.update_parent();
        assert_ne!(
            first.freeze().child_operation_id(),
            second.freeze().child_operation_id()
        );
        second = first.clone();
        second.scope.instance.push('x');
        assert_ne!(
            first.freeze().child_operation_id(),
            second.freeze().child_operation_id()
        );
        second = first.clone();
        second.expected.host_instance_id = Uuid::new_v4();
        assert_ne!(
            first.freeze().child_operation_id(),
            second.freeze().child_operation_id()
        );
        second = first.clone();
        second.expected.doc_id -= 1;
        assert_ne!(
            first.freeze().child_operation_id(),
            second.freeze().child_operation_id()
        );
        second = first.clone();
        second.expected.document_generation -= 1;
        assert_ne!(
            first.freeze().child_operation_id(),
            second.freeze().child_operation_id()
        );
    }
    #[test]
    fn seecut_photo_control_native_zero_nan_and_unbounded_deadline_are_rejected() {
        let input = Input::fixture();
        for change in 0..8 {
            let mut bad = input.clone();
            match change {
                0 => bad.expected.host_instance_id = Uuid::nil(),
                1 => bad.expected.doc_id = 0,
                2 => bad.expected.document_generation = 0,
                3 => bad.expected.revision = 0,
                4 => {
                    bad.action = PhotoEditAction::LayerSetOpacity {
                        layer_id: 0,
                        opacity: 0.5,
                    }
                }
                5 => {
                    bad.action = PhotoEditAction::LayerSetOpacity {
                        layer_id: 1,
                        opacity: f64::NAN,
                    }
                }
                6 => {
                    bad.action = PhotoEditAction::LayerSetOpacity {
                        layer_id: 1,
                        opacity: f64::INFINITY,
                    }
                }
                _ => {
                    bad.action = PhotoEditAction::LayerSetOpacity {
                        layer_id: 1,
                        opacity: 1.1,
                    }
                }
            }
            assert!(
                bad.freeze_at(Instant::now(), Duration::from_secs(60))
                    .is_err()
            );
        }
        let now = Instant::now();
        assert_eq!(
            input.freeze_at(now, Duration::ZERO).unwrap_err(),
            FreezeError::InvalidDeadline
        );
        assert_eq!(
            input
                .freeze_at(now, MAX_WAIT + Duration::from_nanos(1))
                .unwrap_err(),
            FreezeError::InvalidDeadline
        );
        assert!(input.freeze_at(now, MAX_WAIT).is_ok());
        for opacity in [0.0, 1.0] {
            let mut valid = input.clone();
            valid.action = PhotoEditAction::LayerSetOpacity {
                layer_id: u64::MAX,
                opacity,
            };
            assert!(valid.freeze_at(now, Duration::from_secs(60)).is_ok());
        }
    }
    #[test]
    fn seecut_photo_control_cancel_before_ready_or_start_is_known_not_dispatched() {
        for ready in [false, true] {
            let original = Input::fixture().freeze();
            assert_eq!(original.try_start().unwrap_err(), DispatchRefusal::NotReady);
            if ready {
                original.mark_ready().unwrap();
            }
            assert_eq!(
                original.stop_before_dispatch(StopReason::Cancelled),
                StopOutcome::NotDispatched(StopReason::Cancelled)
            );
            assert_eq!(
                original.mark_ready(),
                Err(DispatchRefusal::NotDispatched(StopReason::Cancelled))
            );
            assert_eq!(
                original.try_start().unwrap_err(),
                DispatchRefusal::NotDispatched(StopReason::Cancelled)
            );
            assert_eq!(
                original.stop_before_dispatch(StopReason::AccountChanged),
                StopOutcome::NotDispatched(StopReason::Cancelled)
            );
        }
    }
    #[test]
    fn seecut_photo_control_started_then_cancel_is_in_flight_and_never_starts_twice() {
        let input = Input::fixture();
        let original = input.freeze();
        let clone = original.clone();
        original.mark_ready().unwrap();
        assert_eq!(clone.mark_ready(), Err(DispatchRefusal::AlreadyReady));
        let started = original.try_start().unwrap();
        assert!(started.is_for(&clone));
        assert!(!started.is_for(&input.freeze()));
        assert!(!format!("{started:?}").contains(&input.scope.project));
        assert_eq!(
            clone.stop_before_dispatch(StopReason::TaskChanged),
            StopOutcome::InFlight
        );
        assert_eq!(clone.try_start().unwrap_err(), DispatchRefusal::InFlight);
        assert_eq!(clone.phase(), DispatchPhase::DispatchStarted);
        drop(started);
        assert_eq!(original.try_start().unwrap_err(), DispatchRefusal::InFlight);
    }
    #[test]
    fn seecut_photo_control_deadline_before_and_after_start_preserves_boundary() {
        let original = Input::fixture().freeze();
        let deadline = original.deadline();
        assert_eq!(
            original.mark_ready_at(deadline),
            Err(DispatchRefusal::NotDispatched(StopReason::Expired))
        );
        assert_eq!(
            original.try_start().unwrap_err(),
            DispatchRefusal::NotDispatched(StopReason::Expired)
        );
        let original = Input::fixture().freeze();
        let deadline = original.deadline();
        original.mark_ready().unwrap();
        assert_eq!(
            original.try_start_with_clock(|| deadline).unwrap_err(),
            DispatchRefusal::NotDispatched(StopReason::Expired)
        );
        let original = Input::fixture().freeze();
        let deadline = original.deadline();
        original.mark_ready().unwrap();
        let mut reads = [deadline - Duration::from_nanos(1), deadline].into_iter();
        assert_eq!(
            original
                .try_start_with_clock(|| reads.next().unwrap())
                .unwrap_err(),
            DispatchRefusal::ExpiredAfterStart
        );
        assert_eq!(original.phase(), DispatchPhase::DispatchStarted);
        assert_eq!(
            original.stop_before_dispatch(StopReason::Cancelled),
            StopOutcome::InFlight
        );
        assert_eq!(original.try_start().unwrap_err(), DispatchRefusal::InFlight);
    }
    #[test]
    fn seecut_photo_control_concurrent_starts_issue_one_original_start_marker() {
        let original = Input::fixture().freeze();
        original.mark_ready().unwrap();
        let barrier = Arc::new(Barrier::new(17));
        let mut workers = Vec::new();
        for _ in 0..16 {
            let op = original.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                op.try_start()
            }));
        }
        barrier.wait();
        let mut winners = Vec::new();
        for worker in workers {
            match worker.join().unwrap() {
                Ok(marker) => winners.push(marker),
                Err(error) => assert_eq!(error, DispatchRefusal::InFlight),
            }
        }
        assert_eq!(winners.len(), 1);
        assert!(winners[0].is_for(&original));
        assert_eq!(original.phase(), DispatchPhase::DispatchStarted);
    }
    #[test]
    fn seecut_photo_control_concurrent_cancel_and_start_have_only_two_legal_orders() {
        for _ in 0..16 {
            let original = Input::fixture().freeze();
            original.mark_ready().unwrap();
            let barrier = Arc::new(Barrier::new(3));
            let begin = original.clone();
            let cancel = original.clone();
            let a = Arc::clone(&barrier);
            let start = std::thread::spawn(move || {
                a.wait();
                begin.try_start()
            });
            let b = Arc::clone(&barrier);
            let stop = std::thread::spawn(move || {
                b.wait();
                cancel.stop_before_dispatch(StopReason::Revoked)
            });
            barrier.wait();
            let started = start.join().unwrap();
            let stopped = stop.join().unwrap();
            match (started, stopped) {
                (Ok(marker), StopOutcome::InFlight) => {
                    assert!(marker.is_for(&original));
                    assert_eq!(original.phase(), DispatchPhase::DispatchStarted);
                }
                (
                    Err(DispatchRefusal::NotDispatched(StopReason::Revoked)),
                    StopOutcome::NotDispatched(StopReason::Revoked),
                ) => assert_eq!(
                    original.phase(),
                    DispatchPhase::NotDispatched(StopReason::Revoked)
                ),
                _ => panic!("start/cancel arbitration lost its monotonic boundary"),
            }
        }
    }
}
