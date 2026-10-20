//! Read-only bounded pixels from the currently adopted native Monitor frame.
//! Wire checks do not establish GPU capture or PNG content authenticity.
use crate::{Method, Request};
use serde::{Deserialize, Serialize};

/// The sole supported adopted Monitor frame protocol.
pub const CLIP_PREVIEW_PROTOCOL_VERSION: u8 = 1;
const SAFE: u64 = 9_007_199_254_740_991;
/// Bounded PNG below the existing 1MiB IPC frame after base64 encoding.
pub const CLIP_PREVIEW_MAX_BYTES: usize = 524_288;
fn hash(v: &str, prefix: &str) -> bool {
    v.strip_prefix(prefix).is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
/// Exact observed opaque frame identity, never a time or file query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipPreviewQuery {
    /// Opaque identity from current Context's ready adoptedFrame.
    pub frame_id: String,
}
/// Metadata attached by native to the actual adopted, quantised frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipPreviewFrame {
    /// Fixed ready state.
    pub state: String,
    /// Fixed protocol version 1.
    pub version: u8,
    /// Fixed adoptedMonitorFrame source.
    pub source: String,
    /// Opaque identity of the actual adopted result.
    pub frame_id: String,
    /// Original native document generation.
    pub generation: u64,
    /// Original adopted document revision, not a replacement current revision.
    pub revision: u64,
    /// Original timeline identity.
    pub timeline_id: String,
    /// Actual quantised PreviewSources time; never inferred from playhead.
    pub rendered_frame_time_seconds: f64,
    /// Actual adopted source width, at most 16384.
    pub source_width: u32,
    /// Actual adopted source height, at most 16384.
    pub source_height: u32,
}
impl ClipPreviewFrame {
    /// Validate only the closed metadata values; native establishes provenance.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.state == "ready"
            && self.version == 1
            && self.source == "adoptedMonitorFrame"
            && hash(&self.frame_id, "clip-frame-v1-")
            && self.generation <= SAFE
            && self.revision <= SAFE
            && crate::clip_edit::clip_native_id(&self.timeline_id, "timeline").is_ok()
            && self.rendered_frame_time_seconds.is_finite()
            && self.rendered_frame_time_seconds >= 0.0
            && (1..=16384).contains(&self.source_width)
            && (1..=16384).contains(&self.source_height)
        {
            Ok(())
        } else {
            Err("invalidClipPreviewFrame")
        }
    }
}
/// Closed reason why the current adopted frame cannot be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipPreviewUnavailableReason {
    /// Playback is active.
    Playing,
    /// Native editor is busy.
    Busy,
    /// Latest requested result has not been adopted.
    Pending,
    /// No adopted frame is available.
    NoFrame,
    /// Native frame capture is unavailable.
    Unsupported,
}
impl ClipPreviewUnavailable {
    /// Validate fixed unavailable state/version before using Context metadata.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.state == "unavailable" && self.version == 1 {
            Ok(())
        } else {
            Err("invalidClipPreviewUnavailable")
        }
    }
}

/// Context's current ready frame or explicit lack of one. Ready serializes to
/// exactly ClipPreviewFrame, so Context and success use the same source fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClipAdoptedFrame {
    /// Original adopted frame metadata.
    Ready(ClipPreviewFrame),
    /// Explicitly unavailable frame; never attach old ready metadata here.
    Unavailable(ClipPreviewUnavailable),
}
impl ClipAdoptedFrame {
    /// Shared Context metadata checks for ready and unavailable variants.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Ready(frame) => frame.validate(),
            Self::Unavailable(value) => value.validate(),
        }
    }
}

/// Explicit closed Context unavailable shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipPreviewUnavailable {
    /// Fixed unavailable state.
    pub state: String,
    /// Protocol version 1.
    pub version: u8,
    /// Closed native reason.
    pub reason: ClipPreviewUnavailableReason,
}
/// Original IPC success payload. Stdio removes base64 from both text and
/// structured content and emits exactly one image; runtime validates actual PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipPreviewData {
    /// Protocol version 1.
    pub version: u8,
    /// Fixed clip_preview tool.
    pub tool: String,
    /// Fixed adoptedMonitorFrame source.
    pub source: String,
    /// Original native instance.
    pub app_instance_id: String,
    /// Original read client.
    pub client_id: String,
    /// Original project.
    pub project_id: String,
    /// Original document opening.
    pub document_session_id: String,
    /// Frozen document version.
    pub revision: u64,
    /// Frozen visible context version.
    pub context_revision: u64,
    /// Frozen selection version.
    pub selection_revision: u64,
    /// Actual native view fingerprint including adoptedFrame metadata.
    pub view_state_id: String,
    /// Exactly the current Context's ready adoptedFrame.
    pub frame: ClipPreviewFrame,
    /// Current native playhead at capture; may differ from quantised frame time.
    pub playhead_seconds: f64,
    /// Actual PNG width.
    pub width: u32,
    /// Actual PNG height.
    pub height: u32,
    /// Fixed image/png.
    pub mime_type: String,
    /// Actual PNG byte count, within the existing image budget.
    pub byte_length: usize,
    /// Actual PNG SHA256; actual bytes are checked by the private runtime.
    pub png_sha256: String,
    /// Capture instant in Unix milliseconds.
    pub captured_at_unix_ms: u64,
    /// Must be false: this is not a viewport or OS screenshot.
    pub viewport_capture: bool,
    /// Must be false: Slint controls and selection overlays are excluded.
    pub includes_overlays: bool,
    /// IPC-only PNG bytes, removed before text is exposed to the model.
    pub base64: String,
}
/// Integer floor whole-frame thumbnail dimensions without upscaling.
pub fn clip_preview_dimensions(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    if !(1..=16384).contains(&width)
        || !(1..=16384).contains(&height)
        || !(1..=256).contains(&max_edge)
    {
        return None;
    }
    let long = width.max(height);
    let edge = long.min(max_edge);
    Some((
        ((u64::from(width) * u64::from(edge)) / u64::from(long)).max(1) as u32,
        ((u64::from(height) * u64::from(edge)) / u64::from(long)).max(1) as u32,
    ))
}
impl ClipPreviewData {
    /// Metadata/resource validation only, never claims actual PNG decoding/hash.
    pub fn validate_for_request(&self, r: &Request) -> Result<(), &'static str> {
        self.frame.validate()?;
        if r.method != Method::ClipPreview
            || self.version != 1
            || self.tool != "clip_preview"
            || self.source != "adoptedMonitorFrame"
            || self.app_instance_id != r.instance_id
            || self.client_id != r.client_id
            || Some(&self.project_id) != r.project_id.as_ref()
            || Some(&self.document_session_id) != r.document_session_id.as_ref()
            || Some(self.revision) != r.revision
            || Some(self.context_revision) != r.context_revision
            || ![
                self.revision,
                self.context_revision,
                self.selection_revision,
                self.captured_at_unix_ms,
            ]
            .into_iter()
            .all(|v| v <= SAFE)
            || self.frame.revision != self.revision
            || !hash(&self.view_state_id, "view-v1-")
            || r.clip_preview_query
                .as_ref()
                .is_none_or(|q| q.frame_id != self.frame.frame_id)
            || !self.playhead_seconds.is_finite()
            || self.playhead_seconds < 0.0
            || clip_preview_dimensions(
                self.frame.source_width,
                self.frame.source_height,
                r.max_edge.unwrap_or(256),
            ) != Some((self.width, self.height))
            || self.mime_type != "image/png"
            || self.viewport_capture
            || self.includes_overlays
            || !(1..=CLIP_PREVIEW_MAX_BYTES).contains(&self.byte_length)
            || !hash(&self.png_sha256, "")
            || !base64_shape(&self.base64, self.byte_length)
        {
            Err("invalidClipPreview")
        } else {
            Ok(())
        }
    }
}
fn base64_shape(v: &str, bytes: usize) -> bool {
    if v.is_empty()
        || v.len() > CLIP_PREVIEW_MAX_BYTES.div_ceil(3) * 4
        || !v.len().is_multiple_of(4)
    {
        return false;
    }
    let padding = v.bytes().rev().take_while(|&c| c == b'=').count();
    padding <= 2
        && v[..v.len() - padding]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
        && v.len() / 4 * 3 - padding == bytes
}
pub(crate) fn validate_request(r: &Request) -> Result<(), String> {
    let valid = r.method == Method::ClipPreview
        && r.project_id.as_deref().is_some_and(|v| !v.is_empty())
        && r.document_session_id
            .as_deref()
            .is_some_and(|v| !v.is_empty())
        && r.revision.is_some_and(|v| v <= SAFE)
        && r.context_revision.is_some_and(|v| v <= SAFE)
        && r.clip_preview_query
            .as_ref()
            .is_some_and(|q| hash(&q.frame_id, "clip-frame-v1-"))
        && (1..=256).contains(&r.max_edge.unwrap_or(256))
        && r.app_scope.is_none()
        && r.app_page.is_none()
        && r.app_control_token.is_none()
        && r.app_navigate.is_none()
        && r.app_operation_query.is_none()
        && r.move_parameters.is_none()
        && r.canvas_parameters.is_none()
        && r.clip_parameters.is_none()
        && r.clip_export_control_action.is_none()
        && r.offset.is_none()
        && r.limit.is_none()
        && r.object_id.is_none()
        && r.object_ids.is_none()
        && r.read_source.is_none()
        && r.library_query.is_none()
        && r.library_assets_query.is_none()
        && r.operation_query.is_none()
        && r.flow.is_none()
        && r.flow_workspace.is_none();
    if valid {
        Ok(())
    } else {
        Err("invalidInput: Clip preview fields".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn request() -> Request {
        serde_json::from_value(json!({"instanceId":"i","clientId":"c","clientToken":"read","method":"clipPreview","projectId":"p","documentSessionId":"s","revision":2,"contextRevision":3,"clipPreviewQuery":{"frameId":format!("clip-frame-v1-{}","a".repeat(64))}})).unwrap()
    }
    #[test]
    fn closed_read_query_is_not_write_or_operation() {
        let r = request();
        assert!(!r.method.is_write());
        assert_eq!(r.method.tool_name(), "clip_preview");
        assert!(crate::validate_request_shape(&r).is_ok());
        let original = serde_json::to_value(r).unwrap();
        for key in ["path", "time", "source", "operationId", "clientSequence"] {
            let mut v = original.clone();
            v["clipPreviewQuery"][key] = json!(0);
            assert!(serde_json::from_value::<Request>(v).is_err());
        }
        for method in ["preview", "project", "clipOperation", "clipEdit"] {
            let mut v = original.clone();
            v["method"] = json!(method);
            let r = serde_json::from_value(v).unwrap();
            assert!(crate::validate_request_shape(&r).is_err());
        }
        for edge in [0, 257, u32::MAX] {
            let mut r = request();
            r.max_edge = Some(edge);
            assert!(crate::validate_request_shape(&r).is_err());
        }
    }
    #[test]
    fn dimensions_are_whole_frame_floor_and_never_upscale() {
        assert_eq!(clip_preview_dimensions(1920, 1080, 256), Some((256, 144)));
        assert_eq!(clip_preview_dimensions(1000, 333, 256), Some((256, 85)));
        assert_eq!(clip_preview_dimensions(3, 2, 256), Some((3, 2)));
        assert_eq!(clip_preview_dimensions(1, 16384, 1), Some((1, 1)));
        assert_eq!(clip_preview_dimensions(16385, 1, 256), None);
    }
    #[test]
    fn context_unavailable_state_and_version_are_fixed() {
        for state in ["unavailable", "ready", "pending"] {
            for version in [0, 1, 2] {
                let value = ClipAdoptedFrame::Unavailable(ClipPreviewUnavailable {
                    state: state.into(),
                    version,
                    reason: ClipPreviewUnavailableReason::Pending,
                });
                assert_eq!(
                    value.validate().is_ok(),
                    state == "unavailable" && version == 1
                );
            }
        }
        assert!(
            serde_json::from_value::<ClipAdoptedFrame>(
                json!({"state":"unavailable","version":1,"reason":"pending","frameId":"old"})
            )
            .is_err()
        );
    }
    #[test]
    fn base64_shape_is_bounded_and_byte_counted() {
        assert!(base64_shape("AA==", 1));
        assert!(base64_shape("AAA=", 2));
        assert!(!base64_shape("AAA=", 3));
        assert!(!base64_shape("A===", 0));
        assert!(!base64_shape("a/b_", 3));
    }
}
