//! Closed metadata reads and current regular-file backing observations.
//! No authorization, serialization for models, pixel proof or persistent file UUID.

use crate::{HostError, NativeHost, Result, Uuid};
use serde_json::{Map, Value, json};
use std::fs::File;

const MAX_STATE_BYTES: usize = 512 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_LIST: usize = 256;

/// Exact native integers. Never cast these fields through JavaScript Number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoIdentity {
    /// The owned child instance, independent of the parent SeeCut process ID.
    pub host_instance_id: Uuid,
    /// Native document ID, including values above 2^53.
    pub doc_id: u64,
    /// Current native admission and backing-binding generation.
    pub document_generation: u64,
    /// Current native revision, not an on-disk write receipt.
    pub revision: u64,
}

/// The child's bounded report about its recorded path; no filesystem proof yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoPathStatus {
    /// A complete nonempty native path was reported.
    Recorded,
    /// The document has no recorded path.
    Missing,
    /// The child rejected an empty or NUL-containing path.
    Invalid,
    /// The child refused an over-limit path without truncating it.
    TooLong,
}

/// Native backing facts. The raw path is private and omitted from Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct PhotoBacking {
    native_path: Option<String>,
    /// Whether a complete recorded path exists; this is not file verification.
    pub path_status: PhotoPathStatus,
    /// Native read-only-source flag, without inferred save permissions.
    pub source_read_only: bool,
    /// Original native value; zero recovery state cannot establish saved backing.
    pub saved_revision: u64,
}
impl std::fmt::Debug for PhotoBacking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoBacking")
            .field("path_status", &self.path_status)
            .field("source_read_only", &self.source_read_only)
            .field("saved_revision", &self.saved_revision)
            .finish_non_exhaustive()
    }
}

/// Bounded native layer metadata; names remain untrusted document data.
#[derive(Clone, PartialEq)]
pub struct PhotoLayer {
    /// Original native u64 layer ID, never a floating-point model ID.
    pub id: u64,
    /// Bounded user-controlled name, not instructions or authority.
    pub name: String,
    /// Native opacity in the inclusive range zero to one.
    pub opacity: f64,
    /// The layer's own visibility flag, not a rendered visibility proof.
    pub visible: bool,
}
impl std::fmt::Debug for PhotoLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoLayer")
            .field("id", &self.id)
            .field("opacity", &self.opacity)
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

/// Bounded active-document metadata with exact native identity and backing facts.
#[derive(Clone, PartialEq)]
pub struct PhotoDocument {
    /// Exact native child/document/generation/revision binding.
    pub identity: PhotoIdentity,
    /// Untrusted bounded document name, never used to derive a file path.
    pub name: String,
    /// Native document width and height, without a pixel or adoption claim.
    pub size: [u32; 2],
    /// Native active layer when present.
    pub active_layer: Option<u64>,
    /// At most 256 native selected IDs; consult the truncation flag.
    pub selected_layers: Vec<u64>,
    /// Whether the child omitted additional selected IDs.
    pub selection_list_truncated: bool,
    /// Presence of a native pixel-selection object, without reading its pixels.
    pub has_pixel_selection: bool,
    /// At most 256 bounded metadata entries, without claiming a full tree.
    pub layers: Vec<PhotoLayer>,
    /// Whether the layer list may omit additional entries.
    pub layer_list_may_be_truncated: bool,
    /// Current dirty fact; a clean new document need not have been saved.
    pub dirty: bool,
    /// Native history availability.
    pub can_undo: bool,
    /// Native redo availability.
    pub can_redo: bool,
    /// Native history count, preserved as u64.
    pub history_entries: u64,
    /// Raw native facts, without filesystem access or path disclosure in Debug.
    pub backing: PhotoBacking,
}
impl std::fmt::Debug for PhotoDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoDocument")
            .field("identity", &self.identity)
            .field("size", &self.size)
            .field("dirty", &self.dirty)
            .field("backing", &self.backing)
            .finish_non_exhaustive()
    }
}

/// Read-only UI-callback metadata. This is neither atomic pixels nor a snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoState {
    /// The validated owned host identity.
    pub host_instance_id: Uuid,
    /// Native frame counter, without assuming it identifies adopted pixels.
    pub frame: u64,
    /// Active document, or None when the editor has no active document.
    pub document: Option<PhotoDocument>,
    /// Bounded native busy code, without interpreting it as permission.
    pub busy: Option<String>,
}

/// Current Unix file object and observation version, not a persistent project ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoFileIdentity {
    /// Current device identifier.
    pub device: u64,
    /// Current inode identifier; atomic save can replace this value.
    pub inode: u64,
    /// Current file size, without reading its bytes.
    pub bytes: u64,
    /// Modification timestamp seconds.
    pub modified_seconds: i64,
    /// Modification timestamp nanoseconds.
    pub modified_nanoseconds: i64,
    /// Metadata-change timestamp seconds.
    pub changed_seconds: i64,
    /// Metadata-change timestamp nanoseconds.
    pub changed_nanoseconds: i64,
}

/// A double-read binding retaining the verified File for this value's lifetime.
/// It proves the current recorded path/object only, not historical I/O bytes.
pub struct PhotoBackingBinding {
    identity: PhotoIdentity,
    file_identity: PhotoFileIdentity,
    history_key: String,
    _file: File,
}
impl std::fmt::Debug for PhotoBackingBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoBackingBinding")
            .field("identity", &self.identity)
            .field("file_identity", &self.file_identity)
            .finish_non_exhaustive()
    }
}
impl PhotoBackingBinding {
    /// Full native identity observed unchanged before and after filesystem checks.
    pub fn identity(&self) -> &PhotoIdentity {
        &self.identity
    }

    /// Current object/version observed at the final verification point.
    pub fn file_identity(&self) -> &PhotoFileIdentity {
        &self.file_identity
    }

    /// Trusted history-store key only: `photo:` plus strict UTF-8 canonical path.
    /// Contains a real path: never send to a model, log it, or treat it as a grant.
    /// External changes require a fresh gated double-read, not reuse of this key.
    pub fn history_key(&self) -> &str {
        &self.history_key
    }
}

impl NativeHost {
    /// Read only `seecut.state` with no retry, after existing outer authorization.
    /// Run on a host worker. The caller must check its read lease, account and
    /// Task gates before and after this call; this crate does not grant access.
    pub fn photo_state_after_gate(&mut self, id: Uuid) -> Result<PhotoState> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(HostError::Unsupported);
        }
        let running = self.instance(id)?;
        if !running.info.ready {
            return Err(HostError::NotReady);
        }
        let value = super::call(running, "seecut.state", json!({}), false)?;
        parse_state(&value, id)
    }

    /// Double-read current native state around real regular-file verification.
    /// macOS/Linux only; canonicalization and metadata may block on a network
    /// filesystem, so this is never a UI-tick operation or a hard I/O deadline.
    /// The caller retains original read/account/Task gates and rechecks them
    /// before publishing the returned binding. No new authorization is created.
    pub fn photo_backing_after_gate(&mut self, id: Uuid) -> Result<PhotoBackingBinding> {
        verify_backing(|| self.photo_state_after_gate(id))
    }
}

fn invalid() -> HostError {
    HostError::ReadFailed("invalid bounded Photo metadata")
}
fn object<'a>(v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let map = v.as_object().ok_or_else(invalid)?;
    if map.len() != keys.len() || keys.iter().any(|key| !map.contains_key(*key)) {
        return Err(invalid());
    }
    Ok(map)
}
fn integer(v: &Value) -> Result<u64> {
    v.as_u64().ok_or_else(invalid)
}
fn positive(v: &Value) -> Result<u64> {
    integer(v).and_then(|n| if n == 0 { Err(invalid()) } else { Ok(n) })
}
fn boolean(v: &Value) -> Result<bool> {
    v.as_bool().ok_or_else(invalid)
}
fn text(v: &Value, max: usize) -> Result<String> {
    let s = v.as_str().ok_or_else(invalid)?;
    if s.len() > max || s.contains('\0') {
        return Err(invalid());
    }
    Ok(s.into())
}
fn optional_id(v: &Value) -> Result<Option<u64>> {
    if v.is_null() {
        Ok(None)
    } else {
        positive(v).map(Some)
    }
}
fn list(v: &Value) -> Result<&Vec<Value>> {
    v.as_array()
        .filter(|a| a.len() <= MAX_LIST)
        .ok_or_else(invalid)
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
fn backing(v: &Value) -> Result<PhotoBacking> {
    let m = object(
        v,
        &[
            "nativePath",
            "pathStatus",
            "sourceReadOnly",
            "savedRevision",
        ],
    )?;
    let status = match m["pathStatus"].as_str() {
        Some("recorded") => PhotoPathStatus::Recorded,
        Some("missing") => PhotoPathStatus::Missing,
        Some("invalid") => PhotoPathStatus::Invalid,
        Some("tooLong") => PhotoPathStatus::TooLong,
        _ => return Err(invalid()),
    };
    let native_path = if status == PhotoPathStatus::Recorded {
        let s = text(&m["nativePath"], MAX_PATH_BYTES)?;
        if s.is_empty() {
            return Err(invalid());
        }
        Some(s)
    } else {
        if !m["nativePath"].is_null() {
            return Err(invalid());
        }
        None
    };
    Ok(PhotoBacking {
        native_path,
        path_status: status,
        source_read_only: boolean(&m["sourceReadOnly"])?,
        saved_revision: integer(&m["savedRevision"])?,
    })
}
fn layer(v: &Value) -> Result<PhotoLayer> {
    let m = object(v, &["id", "name", "opacity", "visible"])?;
    let opacity = m["opacity"]
        .as_f64()
        .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
        .ok_or_else(invalid)?;
    Ok(PhotoLayer {
        id: positive(&m["id"])?,
        name: text(&m["name"], 1024)?,
        opacity,
        visible: boolean(&m["visible"])?,
    })
}
fn document(v: &Value, owned: Uuid) -> Result<PhotoDocument> {
    let m = object(
        v,
        &[
            "identity",
            "name",
            "size",
            "activeLayer",
            "selectedLayers",
            "selectionListTruncated",
            "hasPixelSelection",
            "selectionBounds",
            "selectionBoundsMeasurement",
            "layers",
            "layerListMayBeTruncated",
            "target",
            "dirty",
            "canUndo",
            "canRedo",
            "historyEntries",
            "backing",
        ],
    )?;
    if !m["selectionBounds"].is_null()
        || m["selectionBoundsMeasurement"] != "notSampled"
        || !m["target"].is_null()
    {
        return Err(invalid());
    }
    let size = m["size"]
        .as_array()
        .filter(|s| s.len() == 2)
        .ok_or_else(invalid)?;
    let size = [
        u32::try_from(positive(&size[0])?).map_err(|_| invalid())?,
        u32::try_from(positive(&size[1])?).map_err(|_| invalid())?,
    ];
    let selected_layers = list(&m["selectedLayers"])?
        .iter()
        .map(positive)
        .collect::<Result<Vec<_>>>()?;
    let layers = list(&m["layers"])?
        .iter()
        .map(layer)
        .collect::<Result<Vec<_>>>()?;
    if selected_layers
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != selected_layers.len()
        || layers
            .iter()
            .map(|l| l.id)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != layers.len()
    {
        return Err(invalid());
    }
    Ok(PhotoDocument {
        identity: identity(&m["identity"], owned)?,
        name: text(&m["name"], 1024)?,
        size,
        active_layer: optional_id(&m["activeLayer"])?,
        selected_layers,
        selection_list_truncated: boolean(&m["selectionListTruncated"])?,
        has_pixel_selection: boolean(&m["hasPixelSelection"])?,
        layers,
        layer_list_may_be_truncated: boolean(&m["layerListMayBeTruncated"])?,
        dirty: boolean(&m["dirty"])?,
        can_undo: boolean(&m["canUndo"])?,
        can_redo: boolean(&m["canRedo"])?,
        history_entries: integer(&m["historyEntries"])?,
        backing: backing(&m["backing"])?,
    })
}
fn parse_state(v: &Value, owned: Uuid) -> Result<PhotoState> {
    // Bound serialization without allocating another unbounded copy of the reply.
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(b.len());
            if self.0 > MAX_STATE_BYTES {
                return Err(std::io::Error::other("metadata budget"));
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(0), v).map_err(|_| invalid())?;
    let m = object(
        v,
        &[
            "kind",
            "atomicPixels",
            "frame",
            "hostInstanceId",
            "document",
            "view",
            "geometrySource",
            "lastLaidOutCanvasRect",
            "busy",
            "receiptCache",
        ],
    )?;
    if m["kind"] != "uiCallbackMetadata"
        || m["atomicPixels"] != false
        || m["hostInstanceId"].as_str() != Some(owned.to_string().as_str())
        || m["geometrySource"] != "lastLaidOutFrame"
        || !(m["view"].is_null() || m["view"].is_object())
    {
        return Err(invalid());
    }
    let rect = object(&m["lastLaidOutCanvasRect"], &["min", "max"])?;
    for key in ["min", "max"] {
        let a = rect[key]
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(invalid)?;
        if a.iter()
            .any(|v| !v.is_null() && !v.as_f64().is_some_and(f64::is_finite))
        {
            return Err(invalid());
        }
    }
    let cache = object(
        &m["receiptCache"],
        &[
            "storage",
            "restartRecovery",
            "capacity",
            "entries",
            "eviction",
        ],
    )?;
    if cache["storage"] != "memoryOnly"
        || cache["restartRecovery"] != false
        || cache["eviction"] != false
        || integer(&cache["capacity"])? != 1024
        || integer(&cache["entries"])? > 1024
    {
        return Err(invalid());
    }
    Ok(PhotoState {
        host_instance_id: owned,
        frame: integer(&m["frame"])?,
        document: if m["document"].is_null() {
            None
        } else {
            Some(document(&m["document"], owned)?)
        },
        busy: if m["busy"].is_null() {
            None
        } else {
            Some(text(&m["busy"], 64)?)
        },
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_regular(path: &std::path::Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let failed = |_| HostError::ReadFailed("Photo backing file unavailable");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .map_err(failed)?;
    if !file.metadata().map_err(failed)?.is_file() {
        return Err(HostError::ReadFailed("Photo backing is not a regular file"));
    }
    Ok(file)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_backing(
    mut read_state: impl FnMut() -> Result<PhotoState>,
) -> Result<PhotoBackingBinding> {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    let failed = || HostError::ReadFailed("Photo backing is unavailable or changed");
    let io = |_| failed(); // Never retain OS errors containing a native path.
    let first = read_state()?;
    let doc = first.document.as_ref().ok_or_else(failed)?;
    let backing = &doc.backing;
    if backing.source_read_only
        || backing.saved_revision == 0
        || backing.path_status != PhotoPathStatus::Recorded
    {
        return Err(failed());
    }
    let raw = Path::new(backing.native_path.as_deref().ok_or_else(failed)?);
    let native_file =
        |p: &Path| p.is_absolute() && p.extension() == Some(std::ffi::OsStr::new("pcraft"));
    if !native_file(raw) {
        return Err(failed());
    }
    let before = fs::metadata(raw).map_err(io)?;
    let canonical = fs::canonicalize(raw).map_err(io)?;
    if !native_file(&canonical) {
        return Err(failed());
    }
    let canonical_text = canonical.to_str().ok_or_else(failed)?;
    if canonical_text.contains('\0') || "photo:".len() + canonical_text.len() > MAX_PATH_BYTES {
        return Err(failed());
    }
    let path_before = fs::symlink_metadata(&canonical).map_err(io)?;
    if !before.is_file() || !path_before.is_file() {
        return Err(failed());
    }
    // O_NONBLOCK prevents a racing FIFO replacement from blocking open. This
    // does not promise a hard deadline for network filesystems or metadata I/O.
    let file = open_regular(&canonical)?;
    fn stamp(m: &fs::Metadata) -> PhotoFileIdentity {
        PhotoFileIdentity {
            device: m.dev(),
            inode: m.ino(),
            bytes: m.len(),
            modified_seconds: m.mtime(),
            modified_nanoseconds: m.mtime_nsec(),
            changed_seconds: m.ctime(),
            changed_nanoseconds: m.ctime_nsec(),
        }
    }
    let held = file.metadata().map_err(io)?;
    let observed = stamp(&held);
    if !held.is_file() || stamp(&before) != observed || stamp(&path_before) != observed {
        return Err(failed());
    }
    let second = read_state()?;
    let after_doc = second.document.as_ref().ok_or_else(failed)?;
    if first.host_instance_id != second.host_instance_id
        || doc.identity != after_doc.identity
        || backing != &after_doc.backing
    {
        return Err(failed());
    }
    // Check the recorded alias, canonical pathname and held descriptor again at
    // the final publication point. A held descriptor alone would hide replacement.
    if fs::canonicalize(raw).map_err(io)? != canonical {
        return Err(failed());
    }
    let raw_after = fs::metadata(raw).map_err(io)?;
    let path_after = fs::symlink_metadata(&canonical).map_err(io)?;
    let held_after = file.metadata().map_err(io)?;
    if !raw_after.is_file()
        || !path_after.is_file()
        || !held_after.is_file()
        || stamp(&raw_after) != observed
        || stamp(&path_after) != observed
        || stamp(&held_after) != observed
    {
        return Err(failed());
    }
    Ok(PhotoBackingBinding {
        identity: doc.identity.clone(),
        file_identity: observed,
        history_key: format!("photo:{canonical_text}"),
        _file: file,
    })
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn verify_backing(_: impl FnMut() -> Result<PhotoState>) -> Result<PhotoBackingBinding> {
    Err(HostError::Unsupported)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "photo_tests.rs"]
mod tests;
