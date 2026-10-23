//! Closed private edits and receipt reads. The existing parent gate remains required.
//! No operation-ID allocation, authorization, durable ledger or automatic retries.

use crate::{HostError, NativeHost, PhotoIdentity, PhotoLayer, Result, Uuid};
use serde_json::{Map, Value, json};
use std::io::Write;

const MAX_RESULT: usize = 512 * 1024;

/// The first closed native edit actions; no command or arbitrary parameter escape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PhotoEditAction {
    /// Change only an existing non-background layer's opacity. Native guards apply.
    LayerSetOpacity {
        /// Positive original native layer ID.
        layer_id: u64,
        /// Finite inclusive zero-to-one opacity.
        opacity: f64,
    },
    /// One independent native history undo; requires its own outer exact-call gate.
    Undo,
}
impl PhotoEditAction {
    fn value(self) -> Result<Value> {
        match self {
            Self::LayerSetOpacity { layer_id, opacity }
                if layer_id > 0 && opacity.is_finite() && (0.0..=1.0).contains(&opacity) =>
            {
                Ok(json!({"kind":"layerSetOpacity","layerId":layer_id,"opacity":opacity}))
            }
            Self::Undo => Ok(json!({"kind":"undo"})),
            _ => Err(HostError::NotSent("invalid closed Photo action")),
        }
    }
}

/// Native completion classification, not a successful-work or permission claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoEditStatus {
    /// Native dispatch reported success, including a possible no-op.
    Applied,
    /// Native preflight or command policy rejected the attempt.
    Rejected,
    /// Native dispatch reported failure; side effects are not ruled out.
    Failed,
}

/// Real before/after facts, preserving native integer values and optional targets.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoReceiptState {
    /// Exact owned native identity at this callback boundary.
    pub identity: PhotoIdentity,
    /// Native active layer when a document exists.
    pub active_layer: Option<u64>,
    /// Actual target metadata; PhotoLayer Debug omits its untrusted name.
    pub target: Option<PhotoLayer>,
    /// Actual native undo availability.
    pub can_undo: bool,
    /// Actual native redo availability.
    pub can_redo: bool,
    /// Actual count, without inferring that an edit added an entry.
    pub history_entries: u64,
}

/// A typed child receipt. No Serialize/raw Value output and no resend permission.
#[derive(Clone, PartialEq)]
pub struct PhotoReceipt {
    operation_id: String,
    /// Native applied/rejected/failed classification.
    pub status: PhotoEditStatus,
    /// Bounded native code; no arbitrary remote error text is returned.
    pub error_code: Option<String>,
    /// Actual before facts, possibly absent or different from the attempted expected.
    pub before: Option<PhotoReceiptState>,
    /// Actual after facts; rejected/failed never imply no side effects.
    pub after: Option<PhotoReceiptState>,
}
impl PhotoReceipt {
    /// Original child operation ID, fixed by the parent operation mapping.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    /// Always false; this receipt never authorizes retry or a second write.
    pub fn safe_to_resend(&self) -> bool {
        false
    }
}
impl std::fmt::Debug for PhotoReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoReceipt")
            .field("status", &self.status)
            .field("before", &self.before)
            .field("after", &self.after)
            .field("safe_to_resend", &false)
            .finish_non_exhaustive()
    }
}

/// The child's independent exact cache-miss shape. A miss is never safe to resend.
#[derive(Clone, PartialEq, Eq)]
pub struct UnknownReceipt {
    operation_id: String,
}
impl UnknownReceipt {
    /// The original queried child operation ID, never a newly allocated ID.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    /// Always false, including after a child restart or in-memory cache miss.
    pub fn safe_to_resend(&self) -> bool {
        false
    }
    /// Always false: the child's receipt cache does not survive restart.
    pub fn restart_recovery(&self) -> bool {
        false
    }
}
impl std::fmt::Debug for UnknownReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnknownReceipt")
            .field("safe_to_resend", &false)
            .field("restart_recovery", &false)
            .finish_non_exhaustive()
    }
}

/// Closed result of a read-only lookup; Unknown remains distinct from completion.
#[derive(Clone, Debug, PartialEq)]
pub enum PhotoReceiptLookup {
    /// A strictly parsed native completion receipt, without resend permission.
    Known(Box<PhotoReceipt>),
    /// The original ID is not recoverable from this child's memory cache.
    Unknown(UnknownReceipt),
}

impl NativeHost {
    /// Execute exactly once after the parent's existing exact-call/account/Task gate.
    /// The child ID must be the fixed mapping of the original parent operation;
    /// this method never creates an operation ID or a ledger. Run off the UI thread.
    /// After dispatch, any unvalidated response is Unknown and must not be resent.
    pub fn photo_execute_after_gate(
        &mut self,
        id: Uuid,
        original_child_operation_id: &str,
        expected: &PhotoIdentity,
        action: PhotoEditAction,
    ) -> Result<PhotoReceipt> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(HostError::Unsupported);
        }
        if !valid_id(original_child_operation_id)
            || id.is_nil()
            || expected.host_instance_id != id
            || expected.doc_id == 0
            || expected.document_generation == 0
            || expected.revision == 0
        {
            return Err(HostError::NotSent("invalid closed Photo edit binding"));
        }
        let action_value = action.value()?;
        let params = json!({"operationId":original_child_operation_id,"expected":{
            "hostInstanceId":id.to_string(),"docId":expected.doc_id,"documentGeneration":expected.document_generation,"revision":expected.revision},
            "action":action_value});
        let running = self.instance(id)?;
        if !running.info.ready {
            return Err(HostError::NotReady);
        }
        let value = super::call(running, "seecut.execute", params, true)?;
        // Even an apparently normal post-dispatch schema error cannot prove no edit.
        let parsed = parse_lookup(&value, id, original_child_operation_id)
            .map_err(|_| HostError::Unknown("unvalidated Photo execute receipt"))?;
        let PhotoReceiptLookup::Known(receipt) = parsed else {
            return Err(HostError::Unknown(
                "Photo execute completion is not recoverable",
            ));
        };
        if receipt.status == PhotoEditStatus::Applied {
            let valid = receipt
                .before
                .as_ref()
                .is_some_and(|before| &before.identity == expected)
                && receipt
                    .after
                    .as_ref()
                    .is_some_and(|after| same_document(&after.identity, expected));
            if !valid {
                return Err(HostError::Unknown("Photo applied receipt binding mismatch"));
            }
            let target_matches = |state: &PhotoReceiptState| match action {
                PhotoEditAction::LayerSetOpacity { layer_id, .. } => state
                    .target
                    .as_ref()
                    .is_some_and(|target| target.id == layer_id),
                PhotoEditAction::Undo => state.target.is_none(),
            };
            if !receipt.before.as_ref().is_some_and(target_matches)
                || !receipt.after.as_ref().is_some_and(target_matches)
            {
                return Err(HostError::Unknown("Photo applied receipt target mismatch"));
            }
        }
        Ok(*receipt)
    }

    /// Read only the original child operation ID, after the parent's existing gate.
    /// Unknown/missing/restarted receipts never trigger execute or generate a new ID.
    pub fn photo_receipt_after_gate(
        &mut self,
        id: Uuid,
        original_child_operation_id: &str,
    ) -> Result<PhotoReceiptLookup> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(HostError::Unsupported);
        }
        if !valid_id(original_child_operation_id) {
            return Err(HostError::NotSent("invalid Photo receipt operation ID"));
        }
        let running = self.instance(id)?;
        if !running.info.ready {
            return Err(HostError::NotReady);
        }
        let value = super::call(
            running,
            "seecut.receipt",
            json!({"operationId":original_child_operation_id}),
            false,
        )?;
        parse_lookup(&value, id, original_child_operation_id)
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn same_document(a: &PhotoIdentity, b: &PhotoIdentity) -> bool {
    a.host_instance_id == b.host_instance_id
        && a.doc_id == b.doc_id
        && a.document_generation == b.document_generation
}
fn invalid() -> HostError {
    HostError::ReadFailed("invalid bounded Photo receipt")
}
struct ResultBudget(usize);
impl Write for ResultBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let size = self.0.saturating_add(bytes.len());
        if size > MAX_RESULT {
            return Err(std::io::Error::other("receipt exceeds budget"));
        }
        self.0 = size;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn object<'a>(v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let m = v.as_object().ok_or_else(invalid)?;
    if m.len() != keys.len() || keys.iter().any(|key| !m.contains_key(*key)) {
        return Err(invalid());
    }
    Ok(m)
}
fn positive(v: &Value) -> Result<u64> {
    v.as_u64().filter(|v| *v > 0).ok_or_else(invalid)
}
fn optional_id(v: &Value) -> Result<Option<u64>> {
    if v.is_null() {
        Ok(None)
    } else {
        positive(v).map(Some)
    }
}
fn boolean(v: &Value) -> Result<bool> {
    v.as_bool().ok_or_else(invalid)
}
fn identity(v: &Value, owned: Uuid) -> Result<PhotoIdentity> {
    let m = object(
        v,
        &["hostInstanceId", "docId", "documentGeneration", "revision"],
    )?;
    if m["hostInstanceId"].as_str() != Some(owned.to_string().as_str()) {
        return Err(invalid());
    }
    Ok(PhotoIdentity {
        host_instance_id: owned,
        doc_id: positive(&m["docId"])?,
        document_generation: positive(&m["documentGeneration"])?,
        revision: positive(&m["revision"])?,
    })
}
fn layer(v: &Value) -> Result<PhotoLayer> {
    let m = object(v, &["id", "name", "opacity", "visible"])?;
    let name = m["name"]
        .as_str()
        .filter(|s| s.len() <= 1024 && !s.contains('\0'))
        .ok_or_else(invalid)?;
    let opacity = m["opacity"]
        .as_f64()
        .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
        .ok_or_else(invalid)?;
    Ok(PhotoLayer {
        id: positive(&m["id"])?,
        name: name.into(),
        opacity,
        visible: boolean(&m["visible"])?,
    })
}
fn state(v: &Value, owned: Uuid) -> Result<Option<PhotoReceiptState>> {
    if v.is_null() {
        return Ok(None);
    }
    let m = object(
        v,
        &[
            "identity",
            "activeLayer",
            "target",
            "canUndo",
            "canRedo",
            "historyEntries",
        ],
    )?;
    Ok(Some(PhotoReceiptState {
        identity: identity(&m["identity"], owned)?,
        active_layer: optional_id(&m["activeLayer"])?,
        target: if m["target"].is_null() {
            None
        } else {
            Some(layer(&m["target"])?)
        },
        can_undo: boolean(&m["canUndo"])?,
        can_redo: boolean(&m["canRedo"])?,
        history_entries: m["historyEntries"].as_u64().ok_or_else(invalid)?,
    }))
}
fn parse_lookup(v: &Value, owned: Uuid, operation_id: &str) -> Result<PhotoReceiptLookup> {
    serde_json::to_writer(&mut ResultBudget(0), v).map_err(|_| invalid())?;
    if v["status"] == "unknown" {
        let m = object(v, &["operationId", "status", "safeToResend", "cache"])?;
        if m["operationId"].as_str() != Some(operation_id)
            || m["safeToResend"] != false
            || m["cache"] != "memoryOnlyRestartDoesNotRecover"
        {
            return Err(invalid());
        }
        return Ok(PhotoReceiptLookup::Unknown(UnknownReceipt {
            operation_id: operation_id.into(),
        }));
    }
    let m = object(
        v,
        &[
            "operationId",
            "status",
            "errorCode",
            "before",
            "after",
            "safeToResend",
        ],
    )?;
    if m["operationId"].as_str() != Some(operation_id) || m["safeToResend"] != false {
        return Err(invalid());
    }
    let status = match m["status"].as_str() {
        Some("applied") => PhotoEditStatus::Applied,
        Some("rejected") => PhotoEditStatus::Rejected,
        Some("failed") => PhotoEditStatus::Failed,
        _ => return Err(invalid()),
    };
    let error_code = if m["errorCode"].is_null() {
        None
    } else {
        Some(
            m["errorCode"]
                .as_str()
                .filter(|s| {
                    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
                })
                .ok_or_else(invalid)?
                .to_owned(),
        )
    };
    if (status == PhotoEditStatus::Applied) != error_code.is_none() {
        return Err(invalid());
    }
    let before = state(&m["before"], owned)?;
    let after = state(&m["after"], owned)?;
    if status == PhotoEditStatus::Applied
        && !before
            .as_ref()
            .zip(after.as_ref())
            .is_some_and(|(before, after)| same_document(&before.identity, &after.identity))
    {
        return Err(invalid());
    }
    Ok(PhotoReceiptLookup::Known(Box::new(PhotoReceipt {
        operation_id: operation_id.into(),
        status,
        error_code,
        before,
        after,
    })))
}
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "edit_tests.rs"]
mod tests;

#[cfg(all(test, not(any(target_os = "macos", target_os = "linux"))))]
mod unsupported_tests {
    use super::*;
    #[test]
    fn seecut_edit_and_receipt_are_unsupported_without_any_dispatch() {
        let mut host = NativeHost::new();
        let id = Uuid::nil();
        let expected = PhotoIdentity {
            host_instance_id: id,
            doc_id: 1,
            document_generation: 1,
            revision: 1,
        };
        assert!(matches!(
            host.photo_execute_after_gate(id, "fixed", &expected, PhotoEditAction::Undo),
            Err(HostError::Unsupported)
        ));
        assert!(matches!(
            host.photo_receipt_after_gate(id, "fixed"),
            Err(HostError::Unsupported)
        ));
    }
}
