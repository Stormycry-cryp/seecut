//! Closed Clip video export v1 wire evidence. Validation checks shape and
//! relationships; only the native worker can establish real file/decoder facts.
use crate::{AssetVersion, ClipEdit, Method, OperationQuery, Request, Response};
use serde::{Deserialize, Serialize};

/// Native export request and original proof protocol.
pub const CLIP_EXPORT_PROTOCOL_VERSION: u8 = 1;
/// Independent current-output readback protocol.
pub const CLIP_EXPORT_OUTPUT_PROTOCOL_VERSION: u8 = 1;
/// Owner-bound cancellation and registration recovery protocol.
pub const CLIP_EXPORT_CONTROL_PROTOCOL_VERSION: u8 = 1;
const SAFE: u64 = 9_007_199_254_740_991;
const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FRAMES: u64 = 864_000;
const MAX_SECONDS: f64 = 14_400.0;

/// Closed requested and decoded video standards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClipExportCodec {
    /// H264 protocol value.
    H264,
    /// Hevc protocol value.
    Hevc,
    /// Av1 protocol value.
    Av1,
}

/// Native CRF choice; preset stays veryfast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClipExportQuality {
    /// High protocol value.
    High,
    /// Balanced protocol value.
    Balanced,
    /// Small protocol value.
    Small,
}

/// Informational progress, never completion evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportPhase {
    /// Preparing protocol value.
    Preparing,
    /// Rendering protocol value.
    Rendering,
    /// Verifying protocol value.
    Verifying,
    /// Publishing protocol value.
    Publishing,
    /// Registering protocol value.
    Registering,
}

/// Closed causes only when final publication was never attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportReason {
    /// Stopped protocol value.
    Stopped,
    /// Revoked protocol value.
    Revoked,
    /// AccountChanged protocol value.
    AccountChanged,
    /// SessionChanged protocol value.
    SessionChanged,
    /// DocumentChanged protocol value.
    DocumentChanged,
    /// LeaseExpired protocol value.
    LeaseExpired,
    /// SourceChanged protocol value.
    SourceChanged,
    /// UnsupportedInput protocol value.
    UnsupportedInput,
    /// ResourceLimit protocol value.
    ResourceLimit,
    /// EngineFailure protocol value.
    EngineFailure,
    /// OutputInvalid protocol value.
    OutputInvalid,
    /// IoFailure protocol value.
    IoFailure,
}

/// Actual file and directory sync confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportDurability {
    /// Confirmed protocol value.
    Confirmed,
}

/// Known publication fact; not current file existence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportPublication {
    /// Unconfirmed protocol value.
    Unconfirmed,
    /// Confirmed protocol value.
    Confirmed,
}

/// Last unconfirmed publication or completion stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportStage {
    /// Publish protocol value.
    Publish,
    /// DirectorySync protocol value.
    DirectorySync,
    /// OutputReadback protocol value.
    OutputReadback,
    /// InputPostcheck protocol value.
    InputPostcheck,
    /// Registration protocol value.
    Registration,
    /// Completion protocol value.
    Completion,
}

/// Control only the original owner-bound export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClipExportControlAction {
    /// Cancel protocol value.
    Cancel,
    /// RetryRegistration protocol value.
    RetryRegistration,
}

/// Control handling result, not original export completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportControlResult {
    /// CancelAccepted protocol value.
    CancelAccepted,
    /// AlreadyPublishing protocol value.
    AlreadyPublishing,
    /// AlreadyTerminal protocol value.
    AlreadyTerminal,
    /// RegistrationAttempted protocol value.
    RegistrationAttempted,
}

/// A distinct actual current-file read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipExportOutputSource {
    /// FreshExport protocol value.
    FreshExport,
}

/// Trusted frozen render facts; never inferred from the output probe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportExpected {
    /// Expected width.
    pub width: u32,
    /// Expected height.
    pub height: u32,
    /// Positive exact frame rate numerator.
    pub rate_num: u64,
    /// Positive exact frame rate denominator.
    pub rate_den: u64,
    /// Exact planned video frame count.
    pub frames: u64,
    /// Frozen requested codec.
    pub codec: ClipExportCodec,
    /// True requires actual 10-bit, false actual 8-bit.
    pub ten_bit: bool,
    /// Planned presence of one or more audio streams.
    pub audio_expected: bool,
}

/// Actual complete normalized 48000 Hz stereo F32 audio stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportAudioStream {
    /// index.
    pub index: u32,
    /// codec.
    pub codec: String,
    /// source sample rate.
    pub source_sample_rate: u32,
    /// source channels.
    pub source_channels: u32,
    /// sample frames.
    pub sample_frames: u64,
    /// seconds.
    pub seconds: f64,
    /// peak.
    pub peak: f64,
    /// rms.
    pub rms: f64,
}

/// Actual full-decode facts; sample precision does not prove quality or HDR.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportSummary {
    /// byte length.
    pub byte_length: u64,
    /// sha256.
    pub sha256: String,
    /// codec.
    pub codec: ClipExportCodec,
    /// bit depth.
    pub bit_depth: u8,
    /// width.
    pub width: u32,
    /// height.
    pub height: u32,
    /// rate num.
    pub rate_num: u64,
    /// rate den.
    pub rate_den: u64,
    /// video frames.
    pub video_frames: u64,
    /// first video position seconds.
    pub first_video_position_seconds: f64,
    /// last video position seconds.
    pub last_video_position_seconds: f64,
    /// video seconds.
    pub video_seconds: f64,
    /// audio streams.
    pub audio_streams: Vec<ClipExportAudioStream>,
}

/// Existing fixed library asset version, with no path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportAsset {
    /// Existing asset ID.
    pub asset_id: String,
    /// Exact immutable version.
    pub version: AssetVersion,
}

impl ClipExportCodec {
    /// Actual standard name used by the media backend.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}
impl ClipExportQuality {
    /// Existing native CRF mapping, with the existing veryfast preset.
    pub fn crf(self) -> u8 {
        match self {
            Self::High => 16,
            Self::Balanced => 20,
            Self::Small => 26,
        }
    }
}
/// Require both native capability fields in the existing capability data object.
/// Tool advertisement alone neither grants permission nor establishes support.
pub fn capabilities_supported(data: &serde_json::Value) -> bool {
    data.get("clipExportProtocolVersion")
        .and_then(serde_json::Value::as_u64)
        == Some(1)
        && data
            .get("clipExportOutputProtocolVersion")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
}
/// Validate a display name without granting a filesystem target.
pub fn validate_export_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.len() > 256
        || name.trim() != name
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        Err("invalidExportName")
    } else {
        Ok(())
    }
}
#[cfg(feature = "stdio")]
pub(crate) fn short_side_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::json!({"type":"integer","enum":[720,1080,1440,2160]})
        .try_into()
        .expect("constant schema")
}
#[cfg(feature = "stdio")]
pub(crate) fn fps_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::json!({"type":"integer","enum":[24,30,60]})
        .try_into()
        .expect("constant schema")
}

/// Closed original proof. Validation is not evidence of native IO on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ClipExportProof {
    /// Accepted progress; not a completed output.
    Pending {
        /// Must be 1.
        version: u8,
        /// Current bounded phase.
        phase: ClipExportPhase,
        /// Actual progress only.
        frames_done: u64,
        /// Zero until known; otherwise the exact total.
        frames_total: u64,
    },
    /// A native fact that final publication was never attempted.
    NotPublished {
        /// Must be 1.
        version: u8,
        /// CAS fact, independent of whether publication was attempted.
        publish_claimed: bool,
        /// Must be false, including after a rejecting post-CAS hook.
        publish_attempted: bool,
        /// Native known reason.
        reason: ClipExportReason,
    },
    /// Fully verified and durable original output, pending library verification.
    RegistrationPending {
        /// Must be 1.
        version: u8,
        /// Original verified input-copy manifest digest.
        input_manifest_sha256: String,
        /// Original render facts.
        expected: ClipExportExpected,
        /// Actual complete decode facts.
        summary: ClipExportSummary,
        /// Native file and directory sync confirmation.
        durability: ClipExportDurability,
    },
    /// Original output plus independently verified exact managed asset/version.
    Verified {
        /// Must be 1.
        version: u8,
        /// Original verified input-copy manifest digest.
        input_manifest_sha256: String,
        /// Original render facts.
        expected: ClipExportExpected,
        /// Actual complete decode facts.
        summary: ClipExportSummary,
        /// Native file and directory sync confirmation.
        durability: ClipExportDurability,
        /// Existing fixed library asset/version.
        asset: ClipExportAsset,
    },
    /// A known original publication uncertainty; generic query failure cannot create it.
    OutcomeUnknown {
        /// Must be 1.
        version: u8,
        /// Must be true.
        publish_claimed: bool,
        /// Unconfirmed or actual confirmed historical publication.
        publication: ClipExportPublication,
        /// Actual unconfirmed stage.
        stage: ClipExportStage,
    },
}

/// Original clip_edit export receipt; source fields never use a later document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportData {
    /// Original native App instance.
    pub app_instance_id: String,
    /// Original authenticated client.
    pub client_id: String,
    /// Original project.
    pub project_id: String,
    /// Original document opening.
    pub document_session_id: String,
    /// Original canonical operation identity.
    pub operation_id: String,
    /// Original nonzero JS-safe sequence.
    pub client_sequence: u64,
    /// Must remain clip_edit.
    pub tool: String,
    /// Original normalized exportVideo action.
    pub edit: ClipEdit,
    /// Original safe generation.
    pub generation: u64,
    /// Original safe revision.
    pub source_revision: u64,
    /// Original safe selection revision.
    pub source_selection_revision: u64,
    /// Original canonical timeline ID.
    pub source_timeline_id: String,
    /// Original clip-edit-v1 digest.
    pub source_edit_state_id: String,
    /// Original clip-v1 digest.
    pub source_document_state_id: String,
    /// Native canonical nonnil UUID.
    pub output_id: String,
    /// Native original registration UUID.
    pub registration_id: String,
    /// Closed original export evidence.
    pub export_proof: ClipExportProof,
}

/// Separate control data under the existing outer Response with status ok.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportControlData {
    /// Must be 1.
    pub clip_export_control_protocol_version: u8,
    /// Exact requested original-export control.
    pub action: ClipExportControlAction,
    /// Original canonical operation identity.
    pub operation_id: String,
    /// Original nonzero safe sequence.
    pub client_sequence: u64,
    /// Control outcome, not original completion.
    pub result: ClipExportControlResult,
    /// Complete original Response with a closed outer shape.
    #[serde(deserialize_with = "deserialize_original")]
    pub original: Response,
}
fn deserialize_original<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Response, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Original {
        status: String,
        data: Option<serde_json::Value>,
    }
    let original = Original::deserialize(d)?;
    Ok(Response {
        status: original.status,
        data: original.data,
    })
}

/// Independent successful current output readback, never a generic pending receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipExportOutputData {
    /// Must be 1.
    pub clip_export_output_protocol_version: u8,
    /// Must be freshExport.
    pub source: ClipExportOutputSource,
    /// Original native App instance.
    pub app_instance_id: String,
    /// Original authenticated client.
    pub client_id: String,
    /// Original project.
    pub project_id: String,
    /// Original document opening.
    pub document_session_id: String,
    /// Original canonical operation identity.
    pub operation_id: String,
    /// Original nonzero JS-safe sequence.
    pub client_sequence: u64,
    /// Exact original output UUID.
    pub output_id: String,
    /// Exact original registration UUID.
    pub registration_id: String,
    /// Exact original input manifest SHA256.
    pub input_manifest_sha256: String,
    /// Exact original fixed version, freshly content checked.
    pub asset: ClipExportAsset,
    /// Original full-decode facts, byte identity freshly checked.
    pub summary: ClipExportSummary,
    /// Actual current check time, safe nonnegative milliseconds.
    pub checked_at_unix_ms: u64,
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn hash_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(sha256)
}
fn identity(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}
fn source_identity(
    app: &str,
    client: &str,
    project: &str,
    session: &str,
    operation: &str,
    sequence: u64,
) -> bool {
    identity(app, 128)
        && identity(client, 64)
        && identity(project, 512)
        && identity(session, 64)
        && sequence > 0
        && sequence <= SAFE
        && crate::operation_id(client, session, sequence).is_ok_and(|id| id == operation)
}
fn video_shape(
    width: u32,
    height: u32,
    num: u64,
    den: u64,
    frames: u64,
) -> Result<(f64, f64), &'static str> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || u64::from(width) * u64::from(height) > 16_777_216
        || num == 0
        || den == 0
        || num > SAFE
        || den > SAFE
        || frames == 0
        || frames > MAX_FRAMES
    {
        return Err("invalidClipExportVideo");
    }
    let fps = num as f64 / den as f64;
    let seconds = frames as f64 / fps;
    if !fps.is_finite()
        || fps <= 0.0
        || fps > 120.0
        || !seconds.is_finite()
        || seconds > MAX_SECONDS
    {
        return Err("invalidClipExportVideo");
    }
    Ok((fps, seconds))
}
impl ClipExportExpected {
    /// Validate bounded frozen render facts, without deriving them from output.
    pub fn validate(&self) -> Result<(), &'static str> {
        video_shape(
            self.width,
            self.height,
            self.rate_num,
            self.rate_den,
            self.frames,
        )
        .map(|_| ())
    }
    fn matches_edit(&self, edit: &ClipEdit) -> bool {
        matches!(edit, ClipEdit::ExportVideo {codec,short_side,fps,ten_bit,..}
            if self.codec == *codec && self.ten_bit == *ten_bit
                && self.width.min(self.height) == *short_side
                && u128::from(self.rate_num) == u128::from(*fps) * u128::from(self.rate_den))
    }
}
impl ClipExportSummary {
    /// Check bounded actual facts. This does not establish that decoding happened.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        let (fps, _) = video_shape(
            self.width,
            self.height,
            self.rate_num,
            self.rate_den,
            self.video_frames,
        )?;
        let period = 1.0 / fps;
        if self.byte_length == 0
            || self.byte_length > MAX_BYTES
            || !sha256(&self.sha256)
            || !matches!(self.bit_depth, 8 | 10)
            || self.audio_streams.len() > 8
            || ![
                self.first_video_position_seconds,
                self.last_video_position_seconds,
                self.video_seconds,
            ]
            .into_iter()
            .all(f64::is_finite)
            || self.first_video_position_seconds < 0.0
            || self.first_video_position_seconds > period + 1e-9
            || self.last_video_position_seconds < self.first_video_position_seconds
            || self.last_video_position_seconds > MAX_SECONDS
            || (self.last_video_position_seconds - (self.video_frames - 1) as f64 / fps).abs()
                > period + 1e-9
            || self.video_seconds <= 0.0
            || (self.video_seconds - (self.last_video_position_seconds + period)).abs() > 1e-9
        {
            return Err("invalidClipExportSummary");
        }
        let mut indexes = std::collections::HashSet::new();
        for audio in &self.audio_streams {
            if !indexes.insert(audio.index)
                || !identity(&audio.codec, 64)
                || audio.source_sample_rate == 0
                || audio.source_channels == 0
                || audio.sample_frames == 0
                || audio.sample_frames > SAFE
                || ![audio.seconds, audio.peak, audio.rms]
                    .into_iter()
                    .all(f64::is_finite)
                || audio.seconds <= 0.0
                || audio.peak < 0.0
                || audio.rms < 0.0
                || audio.rms > audio.peak + 1e-9
                || (audio.seconds - audio.sample_frames as f64 / 48_000.0).abs() > 1e-9
                || audio.sample_frames as f64
                    > (MAX_SECONDS + 2048.0 / 48_000.0 + period) * 48_000.0
            {
                return Err("invalidClipExportAudio");
            }
        }
        Ok(())
    }
    /// Cross-check every actual fact against the original trusted expectations.
    pub fn validate_for_expected(&self, expected: &ClipExportExpected) -> Result<(), &'static str> {
        expected.validate()?;
        self.validate_shape()?;
        let expected_depth = if expected.ten_bit { 10 } else { 8 };
        if self.width != expected.width
            || self.height != expected.height
            || self.codec != expected.codec
            || self.bit_depth != expected_depth
            || self.video_frames != expected.frames
            || u128::from(self.rate_num) * u128::from(expected.rate_den)
                != u128::from(expected.rate_num) * u128::from(self.rate_den)
            || self.audio_streams.is_empty() == expected.audio_expected
        {
            return Err("invalidClipExportExpectedMismatch");
        }
        // Exact integer sample-frame comparison, allowing AAC padding and one video frame.
        let scaled_expected = u128::from(expected.frames) * u128::from(expected.rate_den) * 48_000;
        let tolerance =
            2048 * u128::from(expected.rate_num) + 48_000 * u128::from(expected.rate_den);
        for audio in &self.audio_streams {
            let actual = u128::from(audio.sample_frames) * u128::from(expected.rate_num);
            if actual.abs_diff(scaled_expected) > tolerance {
                return Err("invalidClipExportAudioDuration");
            }
        }
        Ok(())
    }
}
impl ClipExportAsset {
    fn validate(&self, sha: &str, registration: &str) -> Result<(), &'static str> {
        crate::validate_library_asset(&self.asset_id, &self.version)
            .map_err(|_| "invalidClipExportAsset")?;
        if self.version.sha256 != sha
            || self.version.registration_id != format!("operation:{registration}")
        {
            return Err("invalidClipExportAssetVersion");
        }
        Ok(())
    }
}
impl ClipExportProof {
    /// Check exact proof/status and internal cross-field relationships only.
    pub fn validate_for_status(&self, status: &str) -> Result<(), &'static str> {
        let valid = match self {
            Self::Pending {
                version,
                phase,
                frames_done,
                frames_total,
            } => {
                *version == 1
                    && status == "pending"
                    && *frames_total <= MAX_FRAMES
                    && frames_done <= frames_total
                    && (*frames_total > 0 || *phase == ClipExportPhase::Preparing)
            }
            Self::NotPublished {
                version,
                publish_attempted,
                ..
            } => *version == 1 && status == "exportNotPublished" && !publish_attempted,
            Self::RegistrationPending {
                version,
                input_manifest_sha256,
                expected,
                summary,
                ..
            } => {
                *version == 1
                    && status == "partial"
                    && sha256(input_manifest_sha256)
                    && summary.validate_for_expected(expected).is_ok()
            }
            Self::Verified {
                version,
                input_manifest_sha256,
                expected,
                summary,
                asset,
                ..
            } => {
                *version == 1
                    && status == "ok"
                    && sha256(input_manifest_sha256)
                    && summary.validate_for_expected(expected).is_ok()
                    && crate::validate_library_asset(&asset.asset_id, &asset.version).is_ok()
                    && asset.version.sha256 == summary.sha256
            }
            Self::OutcomeUnknown {
                version,
                publish_claimed,
                ..
            } => *version == 1 && status == "outcomeUnknown" && *publish_claimed,
        };
        if valid {
            Ok(())
        } else {
            Err("invalidClipExportProof")
        }
    }
}
impl ClipExportData {
    /// Validate immutable original scope, source and proof. Not native IO evidence.
    pub fn validate_for_status(&self, status: &str) -> Result<(), &'static str> {
        self.export_proof.validate_for_status(status)?;
        if !source_identity(
            &self.app_instance_id,
            &self.client_id,
            &self.project_id,
            &self.document_session_id,
            &self.operation_id,
            self.client_sequence,
        ) || self.tool != "clip_edit"
            || !matches!(self.edit, ClipEdit::ExportVideo { .. })
            || self.edit.validate().is_err()
            || [
                self.generation,
                self.source_revision,
                self.source_selection_revision,
            ]
            .into_iter()
            .any(|v| v > SAFE)
            || crate::clip_edit::clip_native_id(&self.source_timeline_id, "timeline").is_err()
            || !hash_id(&self.source_edit_state_id, "clip-edit-v1-")
            || !hash_id(&self.source_document_state_id, "clip-v1-")
            || !uuid(&self.output_id)
            || !uuid(&self.registration_id)
        {
            return Err("invalidClipExportSource");
        }
        match &self.export_proof {
            ClipExportProof::RegistrationPending { expected, .. }
            | ClipExportProof::Verified { expected, .. }
                if !expected.matches_edit(&self.edit) =>
            {
                return Err("invalidClipExportFrozenOptions");
            }
            _ => {}
        }
        if let ClipExportProof::Verified { summary, asset, .. } = &self.export_proof {
            asset.validate(&summary.sha256, &self.registration_id)?;
        }
        Ok(())
    }
    /// Match the original receipt to the authenticated query; ordinary current
    /// revision changes do not replace the frozen source fields.
    pub fn validate_for_request(
        &self,
        status: &str,
        request: &Request,
    ) -> Result<(), &'static str> {
        self.validate_for_status(status)?;
        let query = request
            .operation_query
            .as_ref()
            .ok_or("invalidClipExportQuery")?;
        if self.app_instance_id != request.instance_id
            || self.client_id != request.client_id
            || request.project_id.as_deref() != Some(self.project_id.as_str())
            || request.document_session_id.as_deref() != Some(self.document_session_id.as_str())
            || self.operation_id != query.operation_id
            || self.client_sequence != query.client_sequence
        {
            return Err("invalidClipExportQueryIdentity");
        }
        Ok(())
    }
}
impl ClipExportControlData {
    /// Control success does not replace or mutate the original export status.
    pub fn validate(&self) -> Result<(), &'static str> {
        let original: ClipExportData = serde_json::from_value(
            self.original
                .data
                .clone()
                .ok_or("invalidClipExportControlOriginal")?,
        )
        .map_err(|_| "invalidClipExportControlOriginal")?;
        original.validate_for_status(&self.original.status)?;
        // An accepted cancellation requires the original publication CAS not
        // to have been won. Pending progress can lag that fence, so it does
        // not mechanically constrain an alreadyPublishing result.
        if self.result == ClipExportControlResult::CancelAccepted
            && !matches!(
                &original.export_proof,
                ClipExportProof::Pending { .. }
                    | ClipExportProof::NotPublished {
                        publish_claimed: false,
                        ..
                    }
            )
        {
            return Err("invalidClipExportCancellation");
        }
        if self.clip_export_control_protocol_version != 1
            || self.client_sequence > SAFE
            || self.operation_id != original.operation_id
            || self.client_sequence != original.client_sequence
            || !matches!(
                (self.action, self.result),
                (
                    ClipExportControlAction::Cancel,
                    ClipExportControlResult::CancelAccepted
                        | ClipExportControlResult::AlreadyPublishing
                        | ClipExportControlResult::AlreadyTerminal
                ) | (
                    ClipExportControlAction::RetryRegistration,
                    ClipExportControlResult::RegistrationAttempted
                )
            )
        {
            return Err("invalidClipExportControl");
        }
        Ok(())
    }
    /// Bind the control action and untouched original receipt to the exact caller.
    pub fn validate_for_request(&self, request: &Request) -> Result<(), &'static str> {
        self.validate()?;
        crate::validate_request_shape(request).map_err(|_| "invalidClipExportControlQuery")?;
        if request.method != Method::ClipExportControl
            || request.clip_export_control_action != Some(self.action)
        {
            return Err("invalidClipExportControlAction");
        }
        let original: ClipExportData = serde_json::from_value(
            self.original
                .data
                .clone()
                .ok_or("invalidClipExportControlOriginal")?,
        )
        .map_err(|_| "invalidClipExportControlOriginal")?;
        original.validate_for_request(&self.original.status, request)
    }
}
impl ClipExportOutputData {
    /// Validate only fresh-read shape and fixed managed content relationships.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.summary.validate_shape()?;
        if self.clip_export_output_protocol_version != 1
            || self.checked_at_unix_ms > SAFE
            || !source_identity(
                &self.app_instance_id,
                &self.client_id,
                &self.project_id,
                &self.document_session_id,
                &self.operation_id,
                self.client_sequence,
            )
            || !uuid(&self.output_id)
            || !uuid(&self.registration_id)
            || !sha256(&self.input_manifest_sha256)
        {
            return Err("invalidClipExportOutput");
        }
        self.asset
            .validate(&self.summary.sha256, &self.registration_id)
    }
    /// Bind a successful independent read to the exact authenticated original query.
    pub fn validate_for_request(&self, request: &Request) -> Result<(), &'static str> {
        self.validate()?;
        crate::validate_request_shape(request).map_err(|_| "invalidClipExportOutputQuery")?;
        let query = request
            .operation_query
            .as_ref()
            .ok_or("invalidClipExportOutputQuery")?;
        if request.method != Method::ClipExportOutput
            || self.app_instance_id != request.instance_id
            || self.client_id != request.client_id
            || request.project_id.as_deref() != Some(self.project_id.as_str())
            || request.document_session_id.as_deref() != Some(self.document_session_id.as_str())
            || self.operation_id != query.operation_id
            || self.client_sequence != query.client_sequence
        {
            return Err("invalidClipExportOutputQueryIdentity");
        }
        Ok(())
    }
    /// Compare a fresh readback with the original verified export, never a
    /// registrationPending or historical detached result.
    pub fn matches_original(&self, original: &ClipExportData) -> Result<(), &'static str> {
        self.validate()?;
        original.validate_for_status("ok")?;
        let ClipExportProof::Verified {
            input_manifest_sha256,
            summary,
            asset,
            ..
        } = &original.export_proof
        else {
            return Err("invalidClipExportOutputOriginal");
        };
        if self.app_instance_id != original.app_instance_id
            || self.client_id != original.client_id
            || self.project_id != original.project_id
            || self.document_session_id != original.document_session_id
            || self.operation_id != original.operation_id
            || self.client_sequence != original.client_sequence
            || self.output_id != original.output_id
            || self.registration_id != original.registration_id
            || self.input_manifest_sha256 != *input_manifest_sha256
            || self.summary != *summary
            || self.asset != *asset
        {
            return Err("invalidClipExportOutputIdentity");
        }
        Ok(())
    }
}

pub(crate) fn validate_request(request: &Request) -> Result<(), String> {
    if !matches!(
        request.method,
        Method::ClipExportControl | Method::ClipExportOutput
    ) || request.app_scope.is_some()
        || request.app_page.is_some()
        || request.app_control_token.is_some()
        || request.app_navigate.is_some()
        || request.app_operation_query.is_some()
        || request.read_source.is_some()
        || request.revision.is_some()
        || request.context_revision.is_some()
        || request.offset.is_some()
        || request.limit.is_some()
        || request.max_edge.is_some()
        || request.move_parameters.is_some()
        || request.canvas_parameters.is_some()
        || request.clip_parameters.is_some()
        || request.object_id.is_some()
        || request.object_ids.is_some()
        || request.library_query.is_some()
        || request.library_assets_query.is_some()
        || request.flow.is_some()
        || request.flow_workspace.is_some()
        || request
            .project_id
            .as_deref()
            .is_none_or(|s| !identity(s, 512))
        || request
            .document_session_id
            .as_deref()
            .is_none_or(|s| !identity(s, 64))
    {
        return Err("invalidInput: clip export query fields".into());
    }
    let OperationQuery {
        operation_id,
        client_sequence,
    } = request
        .operation_query
        .as_ref()
        .ok_or("invalidInput: clip export original identity")?;
    if *client_sequence > SAFE
        || *operation_id
            != crate::operation_id(
                &request.client_id,
                request.document_session_id.as_deref().unwrap(),
                *client_sequence,
            )?
        || (request.method == Method::ClipExportControl)
            != request.clip_export_control_action.is_some()
    {
        return Err("invalidInput: clip export control identity".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "clip_export_tests.rs"]
mod tests;
