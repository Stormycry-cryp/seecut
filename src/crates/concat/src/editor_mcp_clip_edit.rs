//! Explicit clip targets translated to the existing native commands/history.
//!
//! Integration must authenticate the current clip workspace, obtain exact trusted
//! tool approval and E permission, and reserve the original operation receipt
//! before calling `execute` on the UI thread. This adapter cannot grant access.
use concat_editor_mcp::clip_edit::{
    ClipEdit, ClipKeyProperty, ClipText, ClipTrackFlag, ClipTrimEdge, MAX_CLIP_SECONDS,
    MAX_CLIP_TARGETS, clip_native_id,
};
use concat_project::commands::{ClipMove, ClipPatch, Command, IdMint, TrackFlag, TrimEdge};
use concat_project::model::{
    Clip, ClipKind, KeyEase, KeyProperty, Project, SpeedPoint, TextStyle, Timeline, VideoSettings,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

const MIN_DURATION: f64 = 1.0 / 60.0;
const MAX_OBJECTS: usize = 10_000;
const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PREVIEW_WORK: usize = 16 * 1024 * 1024;
// The native MCP text envelope is 64 KiB. Keep 8 KiB for authenticated
// instance/client/operation identity and the outer response representation.
const MAX_RECEIPT_BYTES: usize = 56 * 1024;

/// Immutable expected project/session and committed document binding.
pub(crate) struct ClipBinding<'a> {
    /// Opaque project identity from the native bridge, never a path.
    pub project_id: &'a str,
    /// Identity of the current opening of this document.
    pub document_session_id: &'a str,
    /// Current Studio clip-session generation.
    pub generation: u64,
    /// Expected committed document revision.
    pub revision: u64,
    /// Exact native selection epoch.
    pub selection_revision: u64,
    /// Expected current timeline as canonical `timeline:<id>`.
    pub timeline_id: &'a str,
    /// Frozen complete edit state, captured before native approval.
    pub expected_edit_state_id: &'a str,
}

/// Actual identity resolved by the trusted native bridge, not request arguments.
pub(crate) struct ClipIdentity<'a> {
    /// Actual currently active clip project identity.
    pub project_id: &'a str,
    /// Actual currently open clip document identity.
    pub document_session_id: &'a str,
}

/// Stable, path-free errors for the outer operation receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipEditError {
    /// Invalid or unbounded typed input.
    InvalidInput,
    /// Complete native state no longer matches approval.
    StaleContext,
    /// Ambiguous or noncanonical stable identities cannot be targeted safely.
    AmbiguousIdentity,
    /// Project, opening, generation or active timeline no longer matches.
    WrongProject,
    /// The observed document revision is stale.
    StaleRevision,
    /// A gesture or pending inspector commit is still active.
    Busy,
    /// An explicit target does not exist in the permitted scope.
    MissingObject,
    /// The edit would change a locked lane, including indirect effects.
    LockedTrack,
    /// The existing native command validator refused the operation.
    NativeRejected,
    /// Video freeze needs an explicit-target native extraction helper.
    UnsupportedVideoFreeze,
    /// There is no corresponding undo or redo step.
    NoHistory,
    /// The real native commit did not produce the validated change.
    CommitFailed,
    /// The complete projected receipt would exceed the native response budget.
    ReceiptTooLarge,
    /// The host has no side-effect-free peek and checked history commit hooks.
    HistoryPreviewUnavailable,
}
impl ClipEditError {
    /// Existing-style wire status without native filesystem or decoder details.
    pub(crate) fn status(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalidInput",
            Self::StaleContext => "staleContext",
            Self::AmbiguousIdentity => "unsupportedTarget",
            Self::WrongProject => "wrongProject",
            Self::StaleRevision => "staleRevision",
            Self::Busy => "busy",
            Self::MissingObject => "objectGone",
            Self::LockedTrack => "lockedTrack",
            Self::NativeRejected => "editRejected",
            Self::UnsupportedVideoFreeze => "unsupportedVideoFreeze",
            Self::NoHistory => "noHistory",
            Self::CommitFailed => "outcomeUnknown",
            Self::ReceiptTooLarge => "resourceLimit",
            Self::HistoryPreviewUnavailable => "historyPreviewUnavailable",
        }
    }
}

/// One native edit or explicit history operation; no reflected command payload.
#[derive(Debug)]
pub(crate) enum ClipPlan {
    /// One original command, potentially an atomic native batch.
    Command(Box<Command>),
    /// Undo in the current Studio's Session history.
    Undo,
    /// Redo in that same history.
    Redo,
}

/// Direction in the same currently open native Session history.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ClipHistory {
    /// Preview or commit the previous edit state.
    Undo,
    /// Preview or commit the next redo state.
    Redo,
}

type ClipApply = fn(&mut crate::studio::Studio, Command) -> Result<concat_project::Outcome, String>;
type ClipHistoryPeek = fn(&crate::studio::Studio, ClipHistory) -> Option<Project>;
type ClipHistoryCommit =
    fn(&mut crate::studio::Studio, ClipHistory) -> Result<concat_project::Outcome, String>;

/// Trusted host hooks which integration must supply. They are never request data.
/// A checked apply must return the real Session error and native Outcome, call
/// the same Studio after_change bookkeeping only when applied, and use no
/// independent Editor. History peek must not mutate revision, selection or stack.
pub(crate) struct ClipNativeHooks {
    /// Same Studio/Session Command path, returning its unswallowed result.
    pub apply: ClipApply,
    /// Exact next history project, or None when that history direction is empty.
    /// Leave absent until a genuine read-only native peek exists.
    pub peek_history: Option<ClipHistoryPeek>,
    /// Checked native history commit with normal Studio bookkeeping.
    pub history: Option<ClipHistoryCommit>,
}

fn native<'a>(value: &'a str, kind: &str) -> Result<&'a str, ClipEditError> {
    clip_native_id(value, kind).map_err(|_| ClipEditError::InvalidInput)
}
fn active<'a>(project: &'a Project, id: &str) -> Result<&'a Timeline, ClipEditError> {
    let id = native(id, "timeline")?;
    if project.active_timeline_id != id {
        return Err(ClipEditError::WrongProject);
    }
    project
        .timelines
        .iter()
        .find(|t| t.id == id)
        .map(AsRef::as_ref)
        .ok_or(ClipEditError::MissingObject)
}
fn clip<'a>(timeline: &'a Timeline, id: &str) -> Result<&'a Clip, ClipEditError> {
    timeline
        .clip(native(id, "clip")?)
        .ok_or(ClipEditError::MissingObject)
}
fn track(timeline: &Timeline, id: &str) -> Result<String, ClipEditError> {
    let id = native(id, "track")?;
    timeline.track(id).ok_or(ClipEditError::MissingObject)?;
    Ok(id.to_owned())
}
fn timeline_id(project: &Project, id: &str) -> Result<String, ClipEditError> {
    let id = native(id, "timeline")?;
    if !project.timelines.iter().any(|t| t.id == id) {
        return Err(ClipEditError::MissingObject);
    }
    Ok(id.to_owned())
}
fn property(p: ClipKeyProperty) -> KeyProperty {
    match p {
        ClipKeyProperty::Scale => KeyProperty::Scale,
        ClipKeyProperty::OffsetX => KeyProperty::OffsetX,
        ClipKeyProperty::OffsetY => KeyProperty::OffsetY,
        ClipKeyProperty::Rotation => KeyProperty::Rotation,
        ClipKeyProperty::Opacity => KeyProperty::Opacity,
        ClipKeyProperty::Volume => KeyProperty::Volume,
    }
}
fn text_style(mut style: TextStyle, patch: &ClipText) -> TextStyle {
    if let Some(v) = &patch.content {
        style.content = v.clone();
    }
    if let Some(v) = &patch.font_family {
        style.font_family = v.clone();
    }
    if let Some(v) = patch.font_size {
        style.font_size = v;
    }
    if let Some(v) = patch.font_weight {
        style.font_weight = v;
    }
    if let Some(v) = patch.italic {
        style.italic = v;
    }
    if let Some(v) = &patch.color {
        style.color = v.clone();
    }
    if let Some(v) = patch.opacity {
        style.opacity = v;
    }
    style
}
fn inside(clip: &Clip, time: f64) -> Result<(), ClipEditError> {
    if time > clip.start + MIN_DURATION && time < clip.start + clip.duration - MIN_DURATION {
        Ok(())
    } else {
        Err(ClipEditError::NativeRejected)
    }
}
fn audio(project: &Project, clip: &Clip) -> bool {
    matches!(clip.kind, ClipKind::Audio | ClipKind::Video)
        && project
            .media_by_id(&clip.media_id)
            .is_some_and(|m| m.has_audio || m.kind == concat_project::model::MediaKind::Audio)
}

/// Pure conversion and target preconditions. It reads no file, changes no
/// selection and never interprets an empty list as the current/all selection.
pub(crate) fn plan(
    project: &Project,
    current_timeline: &str,
    edit: &ClipEdit,
) -> Result<ClipPlan, ClipEditError> {
    edit.validate().map_err(|_| ClipEditError::InvalidInput)?;
    if serde_json::to_vec(edit)
        .map_err(|_| ClipEditError::InvalidInput)?
        .len()
        > 16 * 1024
    {
        return Err(ClipEditError::InvalidInput);
    }
    validate_identities(project)?;
    let tl = active(project, current_timeline)?;
    use ClipEdit::*;
    let command = match edit {
        PlaceMedia {
            media_id,
            track_id,
            start,
        } => {
            let media_id = native(media_id, "media")?;
            project
                .media_by_id(media_id)
                .ok_or(ClipEditError::MissingObject)?;
            Command::AddClip {
                media_id: media_id.into(),
                track_id: track(tl, track_id)?,
                start: *start,
            }
        }
        AddTrack => Command::AddTrack,
        RemoveTrack { track_id } => Command::RemoveTrack {
            track_id: track(tl, track_id)?,
        },
        SetTrackFlag {
            track_id,
            flag,
            value,
        } => Command::SetTrackFlag {
            track_id: track(tl, track_id)?,
            flag: match flag {
                ClipTrackFlag::Visible => TrackFlag::Visible,
                ClipTrackFlag::Muted => TrackFlag::Muted,
            },
            value: *value,
        },
        AddTimeline => Command::AddTimeline,
        RemoveTimeline { timeline_id: id } => Command::RemoveTimeline {
            timeline_id: timeline_id(project, id)?,
        },
        RenameTimeline {
            timeline_id: id,
            name,
        } => Command::RenameTimeline {
            timeline_id: timeline_id(project, id)?,
            name: name.clone(),
        },
        SelectTimeline { timeline_id: id } => Command::SelectTimeline {
            timeline_id: timeline_id(project, id)?,
        },
        MoveTimeline {
            timeline_id: id,
            index,
        } => {
            if *index >= project.timelines.len() {
                return Err(ClipEditError::InvalidInput);
            }
            Command::MoveTimeline {
                timeline_id: timeline_id(project, id)?,
                index: *index,
            }
        }
        SetTimelineVideo {
            timeline_id: id,
            width,
            height,
            rate_num,
            rate_den,
        } => Command::SetTimelineVideo {
            timeline_id: timeline_id(project, id)?,
            video: VideoSettings {
                width: *width,
                height: *height,
                rate_num: *rate_num,
                rate_den: *rate_den,
            },
        },
        Move { moves } => Command::MoveClips {
            moves: moves
                .iter()
                .map(|m| {
                    Ok(ClipMove {
                        clip_id: clip(tl, &m.clip_id)?.id.clone(),
                        track_id: track(tl, &m.track_id)?,
                        start: m.start,
                    })
                })
                .collect::<Result<_, ClipEditError>>()?,
        },
        Trim {
            clip_id,
            edge,
            delta,
        } => {
            let c = clip(tl, clip_id)?;
            match edge {
                ClipTrimEdge::Start
                    if c.start + delta < 0.0
                        || c.duration - delta < MIN_DURATION
                        || (!c.reverse && c.source_start + delta * c.speed < 0.0) =>
                {
                    return Err(ClipEditError::NativeRejected);
                }
                ClipTrimEdge::End if c.duration + delta < MIN_DURATION => {
                    return Err(ClipEditError::NativeRejected);
                }
                _ => {}
            }
            Command::TrimClip {
                clip_id: c.id.clone(),
                edge: match edge {
                    ClipTrimEdge::Start => TrimEdge::Start,
                    ClipTrimEdge::End => TrimEdge::End,
                },
                delta: *delta,
            }
        }
        Split { clip_ids, time } => {
            let targets = clip_ids
                .iter()
                .map(|id| {
                    let c = clip(tl, id)?;
                    inside(c, *time)?;
                    Ok(c.id.clone())
                })
                .collect::<Result<_, ClipEditError>>()?;
            Command::SplitClips {
                clip_ids: targets,
                time: *time,
            }
        }
        Delete { clip_ids } => Command::RemoveClips {
            clip_ids: clip_ids
                .iter()
                .map(|id| Ok(clip(tl, id)?.id.clone()))
                .collect::<Result<_, ClipEditError>>()?,
        },
        Merge { clip_ids } => {
            let targets = clip_ids
                .iter()
                .map(|id| Ok(clip(tl, id)?.id.clone()))
                .collect::<Result<Vec<_>, ClipEditError>>()?;
            if concat_project::why_not_merge(tl, &targets).is_some() {
                return Err(ClipEditError::NativeRejected);
            }
            Command::MergeClips { clip_ids: targets }
        }
        FreezeImage { clip_id, time } => {
            let c = clip(tl, clip_id)?;
            inside(c, *time)?;
            if c.kind == ClipKind::Video {
                return Err(ClipEditError::UnsupportedVideoFreeze);
            }
            if c.kind != ClipKind::Image {
                return Err(ClipEditError::NativeRejected);
            }
            project
                .media_by_id(&c.media_id)
                .ok_or(ClipEditError::MissingObject)?;
            Command::FreezeFrame {
                clip_id: c.id.clone(),
                time: *time,
                duration: Some(1.0),
                still: None,
            }
        }
        SetSpeed { clip_id, speed } => Command::SetClipSpeed {
            clip_id: clip(tl, clip_id)?.id.clone(),
            speed: *speed,
        },
        SetSpeedCurve { clip_id, points } => Command::SetClipSpeedCurve {
            clip_id: clip(tl, clip_id)?.id.clone(),
            curve: points.as_ref().map(|points| {
                points
                    .iter()
                    .map(|p| SpeedPoint {
                        at: p.at,
                        speed: p.speed,
                    })
                    .collect()
            }),
        },
        Transform {
            clip_id,
            transform: t,
        } => {
            let c = clip(tl, clip_id)?;
            if c.kind == ClipKind::Audio {
                return Err(ClipEditError::NativeRejected);
            }
            Command::SetClipTransform {
                clip_id: c.id.clone(),
                scale: t.scale,
                offset_x: t.offset_x,
                offset_y: t.offset_y,
                rotation: t.rotation,
                stretch_x: t.stretch_x,
                stretch_y: t.stretch_y,
            }
        }
        Update {
            clip_id,
            properties: p,
        } => {
            let c = clip(tl, clip_id)?;
            if p.fade_in.is_some_and(|v| v > c.duration)
                || p.fade_out.is_some_and(|v| v > c.duration)
            {
                return Err(ClipEditError::NativeRejected);
            }
            if (p.volume.is_some() || p.muted.is_some() || p.preserve_pitch.is_some())
                && !audio(project, c)
            {
                return Err(ClipEditError::NativeRejected);
            }
            Command::UpdateClip {
                clip_id: c.id.clone(),
                patch: ClipPatch {
                    name: p.name.clone(),
                    volume: p.volume,
                    fade_in: p.fade_in,
                    fade_out: p.fade_out,
                    opacity: p.opacity,
                    preserve_pitch: p.preserve_pitch,
                    muted: p.muted,
                    reverse: p.reverse,
                    flip_h: p.flip_h,
                    flip_v: p.flip_v,
                    ..Default::default()
                },
            }
        }
        SetAudioStream { clip_id, stream } => {
            let c = clip(tl, clip_id)?;
            if !audio(project, c) {
                return Err(ClipEditError::NativeRejected);
            }
            if let Some(index) = stream {
                let m = project
                    .media_by_id(&c.media_id)
                    .ok_or(ClipEditError::MissingObject)?;
                if !m.audio_tracks.iter().any(|t| t.index == *index) {
                    return Err(ClipEditError::MissingObject);
                }
            }
            Command::UpdateClip {
                clip_id: c.id.clone(),
                patch: ClipPatch {
                    audio_stream: Some(*stream),
                    ..Default::default()
                },
            }
        }
        DetachAudio { clip_id } => {
            let c = clip(tl, clip_id)?;
            if c.kind != ClipKind::Video
                || !audio(project, c)
                || c.muted == Some(true)
                || tl
                    .clips
                    .iter()
                    .any(|other| other.detached_from.as_deref() == Some(c.id.as_str()))
            {
                return Err(ClipEditError::NativeRejected);
            }
            Command::DetachAudio {
                clip_id: c.id.clone(),
            }
        }
        ReattachAudio { clip_id } => {
            let c = clip(tl, clip_id)?;
            let video = c.detached_from.as_deref().unwrap_or(&c.id);
            if tl.clip(video).is_none()
                || !tl
                    .clips
                    .iter()
                    .any(|c| c.detached_from.as_deref() == Some(video))
            {
                return Err(ClipEditError::NativeRejected);
            }
            Command::ReattachAudio {
                clip_id: c.id.clone(),
            }
        }
        AddText {
            track_id,
            start,
            duration,
            text,
        } => Command::AddTextClip {
            track_id: Some(track(tl, track_id)?),
            start: *start,
            style: Some(text_style(TextStyle::default(), text)),
            duration: Some(*duration),
            offset_y: None,
        },
        SetText { clip_id, text } => {
            let c = clip(tl, clip_id)?;
            if c.kind != ClipKind::Text {
                return Err(ClipEditError::NativeRejected);
            }
            let style = c.text.clone().ok_or(ClipEditError::NativeRejected)?;
            Command::UpdateClip {
                clip_id: c.id.clone(),
                patch: ClipPatch {
                    text: Some(Some(text_style(style, text))),
                    ..Default::default()
                },
            }
        }
        SetKey {
            clip_id,
            property: p,
            at,
            value,
            ease,
        } => Command::SetClipKey {
            clip_id: clip(tl, clip_id)?.id.clone(),
            property: property(*p),
            at: *at,
            value: *value,
            ease: KeyEase(*ease),
        },
        ClearKey {
            clip_id,
            property: p,
            at,
        } => Command::ClearClipKey {
            clip_id: clip(tl, clip_id)?.id.clone(),
            property: property(*p),
            at: *at,
        },
        ClearKeys {
            clip_id,
            property: p,
        } => Command::ClearClipKeys {
            clip_id: clip(tl, clip_id)?.id.clone(),
            property: property(*p),
        },
        Undo => return Ok(ClipPlan::Undo),
        Redo => return Ok(ClipPlan::Redo),
    };
    Ok(ClipPlan::Command(Box::new(command)))
}

fn objects(project: &Project) -> HashSet<String> {
    project
        .media
        .iter()
        .map(|m| format!("media:{}", m.id))
        .chain(project.timelines.iter().flat_map(|t| {
            std::iter::once(format!("timeline:{}", t.id))
                .chain(t.tracks.iter().map(|v| format!("track:{}", v.id)))
                .chain(t.clips.iter().map(|v| format!("clip:{}", v.id)))
        }))
        .collect()
}

/// Pure native preflight; original command errors and every indirect locked
/// lane change are refused before the current Session is touched.
pub(crate) fn preflight(
    project: &Project,
    command: &Command,
    locked: impl Fn(&str) -> bool,
) -> Result<(Project, bool), ClipEditError> {
    let (staged, applied) = preview_command(project, command)?;
    validate_transition(project, &staged, locked)?;
    Ok((staged, applied))
}

fn preview_command(project: &Project, command: &Command) -> Result<(Project, bool), ClipEditError> {
    validate_identities(project)?;
    preview_work(project, command)?;
    let mut staged = project.clone();
    let mut mint = IdMint::default();
    mint.adopt_project(project);
    let outcome = concat_project::commands::apply(&mut staged, &mut mint, command.clone())
        .map_err(|_| ClipEditError::NativeRejected)?;
    // The temporary mint is never committed or returned. Receipt budgeting
    // pads projected new identities; actual IDs only come from the Session.
    Ok((staged, outcome.applied))
}

fn validate_transition(
    before: &Project,
    after: &Project,
    locked: impl Fn(&str) -> bool,
) -> Result<(), ClipEditError> {
    validate_identities(before)?;
    validate_identities(after)?;
    let timelines = before
        .timelines
        .iter()
        .chain(&after.timelines)
        .map(|t| t.id.as_str())
        .collect::<HashSet<_>>();
    for id in timelines {
        let old = before.timelines.iter().find(|t| t.id == id);
        let new = after.timelines.iter().find(|t| t.id == id);
        let lanes = old
            .into_iter()
            .chain(new)
            .flat_map(|t| &t.tracks)
            .map(|t| t.id.as_str())
            .collect::<HashSet<_>>();
        for lane in lanes {
            if !locked(lane) {
                continue;
            }
            let old_track = old.and_then(|t| t.track(lane));
            let new_track = new.and_then(|t| t.track(lane));
            let old_clips = old
                .into_iter()
                .flat_map(|t| &t.clips)
                .filter(|c| c.track_id == lane)
                .collect::<Vec<_>>();
            let new_clips = new
                .into_iter()
                .flat_map(|t| &t.clips)
                .filter(|c| c.track_id == lane)
                .collect::<Vec<_>>();
            if old_track != new_track || old_clips != new_clips {
                return Err(ClipEditError::LockedTrack);
            }
        }
    }
    let old_objects = objects(before);
    let new_objects = objects(after);
    // A temporary mint does not know deleted IDs reserved by the real Session.
    // Each new ID can appear as identity and one relationship (active tab/lane).
    // Reserve two complete 64-byte suffixes before committing the actual mint.
    let reserved = new_objects
        .difference(&old_objects)
        .count()
        .saturating_mul(128);
    bounded_digest_with_limit("clip-v1-", after, MAX_STATE_BYTES.saturating_sub(reserved))?;
    let old_count = old_objects.len();
    let new_count = new_objects.len();
    if new_count > MAX_OBJECTS && new_count > old_count {
        return Err(ClipEditError::InvalidInput);
    }
    if after.timelines.iter().any(|t| {
        t.clips.iter().any(|c| {
            let previous = before
                .timelines
                .iter()
                .find(|p| p.id == t.id)
                .and_then(|p| p.clip(&c.id));
            previous != Some(c.as_ref())
                && (!c.start.is_finite()
                    || !c.duration.is_finite()
                    || c.start < 0.0
                    || c.duration < MIN_DURATION
                    || c.start + c.duration > MAX_CLIP_SECONDS)
        })
    }) {
        return Err(ClipEditError::InvalidInput);
    }
    Ok(())
}

/// A bounded, path-free view of a changed surviving clip.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClipResult {
    kind: ClipKind,
    detached_from: Option<String>,
    detached_audio_ids: Vec<String>,
    id: String,
    timeline_id: String,
    track_id: String,
    media_id: Option<String>,
    start: f64,
    duration: f64,
    source_start: f64,
    speed: f64,
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    rotation: f64,
    opacity: f64,
    volume: f64,
    fade_in: f64,
    fade_out: f64,
    muted: bool,
    name: String,
    stretch_x: f64,
    stretch_y: f64,
    preserve_pitch: bool,
    reverse: bool,
    flip_h: bool,
    flip_v: bool,
    audio_stream: Option<u32>,
    speed_curve: Option<Vec<SpeedPoint>>,
    keys: Vec<concat_project::model::ClipKey>,
    text: Option<TextStyle>,
}

/// Complete bounded evidence; the outer bridge adds authenticated operation identity.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClipReceipt {
    pub project_id: String,
    pub document_session_id: String,
    pub generation: u64,
    pub source_revision: u64,
    pub revision: u64,
    pub source_timeline_id: String,
    pub active_timeline_id: String,
    pub source_document_state_id: String,
    pub document_state_id: String,
    pub source_edit_state_id: String,
    pub edit_state_id: String,
    pub selection_revision: u64,
    pub selected_object_ids: Vec<String>,
    pub edit: Value,
    pub changed: bool,
    pub document_changed: bool,
    pub created_ids: Vec<String>,
    pub removed_ids: Vec<String>,
    pub readback: Value,
}

fn clip_result(tl: &Timeline, c: &Clip) -> ClipResult {
    ClipResult {
        kind: c.kind,
        detached_audio_ids: if c.kind == ClipKind::Video {
            let mut ids = tl
                .clips
                .iter()
                .filter(|other| other.detached_from.as_deref() == Some(c.id.as_str()))
                .map(|other| format!("clip:{}", other.id))
                .collect::<Vec<_>>();
            ids.sort();
            ids
        } else {
            vec![]
        },
        detached_from: c.detached_from.as_ref().map(|id| format!("clip:{id}")),
        id: format!("clip:{}", c.id),
        timeline_id: format!("timeline:{}", tl.id),
        track_id: format!("track:{}", c.track_id),
        media_id: (!c.media_id.is_empty()).then(|| format!("media:{}", c.media_id)),
        start: c.start,
        duration: c.duration,
        source_start: c.source_start,
        speed: c.speed,
        scale: c.scale,
        offset_x: c.offset_x,
        offset_y: c.offset_y,
        rotation: c.rotation,
        opacity: c.opacity,
        volume: c.volume,
        fade_in: c.fade_in,
        fade_out: c.fade_out,
        muted: c.muted == Some(true),
        name: c.name.clone(),
        stretch_x: c.stretch_x,
        stretch_y: c.stretch_y,
        preserve_pitch: c.preserve_pitch,
        reverse: c.reverse,
        flip_h: c.flip_h,
        flip_v: c.flip_v,
        audio_stream: c.audio_stream,
        speed_curve: c.speed_curve.clone(),
        keys: c.keys.clone(),
        text: c.text.clone(),
    }
}

// One nonallocating writer is shared by work budgeting and content fingerprints.
struct ModelWriter {
    bytes: usize,
    hash: Sha256,
    limit: usize,
}
impl std::io::Write for ModelWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("resourceLimit"));
        }
        self.bytes += bytes.len();
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn serialize_native(value: &impl Serialize, limit: usize) -> Result<ModelWriter, ClipEditError> {
    let mut writer = ModelWriter {
        bytes: 0,
        hash: Sha256::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| ClipEditError::ReceiptTooLarge)?;
    Ok(writer)
}
fn bounded_digest_with_limit(
    prefix: &str,
    value: &impl Serialize,
    limit: usize,
) -> Result<String, ClipEditError> {
    Ok(format!(
        "{prefix}{:x}",
        serialize_native(value, limit)?.hash.finalize()
    ))
}
fn preview_work(project: &Project, command: &Command) -> Result<(), ClipEditError> {
    if let Command::DetachAudio { clip_id } = command {
        let timeline = project.active();
        let clip = timeline.clip(clip_id).ok_or(ClipEditError::MissingObject)?;
        let media = project
            .media_by_id(&clip.media_id)
            .ok_or(ClipEditError::MissingObject)?;
        if clip.kind != ClipKind::Video
            || clip.muted == Some(true)
            || !media.has_audio
            || timeline
                .clips
                .iter()
                .any(|c| c.detached_from.as_deref() == Some(clip_id.as_str()))
        {
            return Ok(());
        }
        let streams = media.audio_tracks.len().max(1);
        let work = streams
            .saturating_mul(timeline.tracks.len().saturating_add(streams))
            .saturating_mul(timeline.clips.len().saturating_add(streams));
        if streams > MAX_CLIP_TARGETS || work > MAX_PREVIEW_WORK {
            return Err(ClipEditError::ReceiptTooLarge);
        }
        let source = serialize_native(project, MAX_STATE_BYTES)?.bytes;
        let clip_size = serialize_native(clip, MAX_STATE_BYTES)?.bytes;
        let titles = if streams > 1 {
            media.audio_tracks.iter().try_fold(0usize, |sum, t| {
                serialize_native(&t.title, MAX_STATE_BYTES).map(|s| sum.saturating_add(s.bytes))
            })?
        } else {
            0
        };
        let expansion = clip_size
            .saturating_mul(streams)
            .saturating_add(titles)
            .saturating_add(streams.saturating_mul(512));
        if expansion > MAX_STATE_BYTES.saturating_sub(source) {
            return Err(ClipEditError::ReceiptTooLarge);
        }
    }
    Ok(())
}

fn bounded_digest(prefix: &str, value: &impl Serialize) -> Result<String, ClipEditError> {
    bounded_digest_with_limit(prefix, value, MAX_STATE_BYTES)
}
fn digest(prefix: &str, value: &impl Serialize) -> String {
    bounded_digest(prefix, value).expect("preflighted native model serializes within its bound")
}
fn document_state_id(project: &Project) -> String {
    digest("clip-v1-", project)
}
fn bounded_project(project: &Project) -> Result<(), ClipEditError> {
    bounded_digest("clip-v1-", project).map(|_| ())
}

/// Refuse ambiguous public identities; never repair or silently pick a first match.
pub(crate) fn validate_identities(project: &Project) -> Result<(), ClipEditError> {
    bounded_project(project)?;
    let mut seen = HashSet::new();
    for id in project
        .media
        .iter()
        .map(|m| format!("media:{}", m.id))
        .chain(project.timelines.iter().flat_map(|t| {
            std::iter::once(format!("timeline:{}", t.id))
                .chain(t.tracks.iter().map(|v| format!("track:{}", v.id)))
                .chain(t.clips.iter().map(|v| format!("clip:{}", v.id)))
        }))
    {
        let namespace = id
            .split_once(':')
            .ok_or(ClipEditError::AmbiguousIdentity)?
            .0;
        native(&id, namespace).map_err(|_| ClipEditError::AmbiguousIdentity)?;
        if !seen.insert(id) {
            return Err(ClipEditError::AmbiguousIdentity);
        }
    }
    Ok(())
}
fn selected(studio: &crate::studio::Studio) -> Vec<String> {
    let mut ids = studio
        .selection
        .iter()
        .map(|id| format!("clip:{id}"))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}
fn selected_media(studio: &crate::studio::Studio) -> Vec<String> {
    let mut ids = studio
        .media
        .selected
        .iter()
        .map(|id| format!("media:{id}"))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}
fn edit_state_id(studio: &crate::studio::Studio, document: &str) -> Result<String, ClipEditError> {
    if let Some(session) = &studio.session {
        for target in [session.peek_undo(), session.peek_redo()]
            .into_iter()
            .flatten()
        {
            bounded_project(target)?;
        }
    }
    let mut locks = studio
        .lane_view
        .iter()
        .filter(|(_, v)| v.locked)
        .map(|(id, _)| format!("track:{id}"))
        .collect::<Vec<_>>();
    locks.sort();
    Ok(digest(
        "clip-edit-v1-",
        &json!({"documentStateId":document,"generation":studio.mcp_clip_state().0,"revision":studio.mcp_clip_state().1,"selectedClipIds":selected(studio),"selectedMediaIds":selected_media(studio),"lockedTrackIds":locks,"undoStateId":studio.session.as_ref().and_then(|v|v.peek_undo()).map(document_state_id),"redoStateId":studio.session.as_ref().and_then(|v|v.peek_redo()).map(document_state_id)}),
    ))
}

/// Path-free exact object readback. Full model contents are hashed, never emitted.
pub(crate) fn object_value(
    project: &Project,
    id: &str,
    locked: impl Fn(&str) -> bool,
) -> Option<Value> {
    let (namespace, suffix) = id.split_once(':')?;
    for item in &project.media {
        if namespace == "media" && suffix == item.id {
            return Some(
                json!({"id":id,"kind":item.kind,"name":item.name,"duration":item.duration,"width":item.width,"height":item.height,"hasAudio":item.has_audio,"audioTracks":item.audio_tracks,"placeholder":item.placeholder,"stateId":digest("clip-object-v1-",item)}),
            );
        }
    }
    for (order, t) in project.timelines.iter().enumerate() {
        if namespace == "timeline" && suffix == t.id {
            return Some(
                json!({"id":id,"name":t.name,"order":order,"video":t.video,"trackOrder":t.tracks.iter().map(|v|format!("track:{}",v.id)).collect::<Vec<_>>(),"stateId":digest("clip-object-v1-",t)}),
            );
        }
        for (order, track) in t.tracks.iter().enumerate() {
            if namespace == "track" && suffix == track.id {
                return Some(
                    json!({"id":id,"timelineId":format!("timeline:{}",t.id),"order":order,"visible":track.visible,"muted":track.muted,"locked":locked(&track.id),"stateId":digest("clip-object-v1-",track)}),
                );
            }
        }
        for c in &t.clips {
            if namespace == "clip" && suffix == c.id {
                let mut value = serde_json::to_value(clip_result(t, c)).ok()?;
                value["stateId"] = json!(digest("clip-object-v1-", c));
                return Some(value);
            }
        }
    }
    None
}
fn readback(
    project: &Project,
    ids: &[String],
    clips: Vec<String>,
    media: Vec<String>,
    can_undo: bool,
    can_redo: bool,
    locked: impl Fn(&str) -> bool,
) -> Value {
    let mut timelines = Vec::new();
    let mut tracks = Vec::new();
    let mut clip_rows = Vec::new();
    let mut media_rows = Vec::new();
    let mut missing = Vec::new();
    let mut unique = ids.to_vec();
    unique.sort();
    unique.dedup();
    for id in unique {
        if let Some(value) = object_value(project, &id, &locked) {
            match id.split_once(':').map(|v| v.0) {
                Some("timeline") => timelines.push(value),
                Some("track") => tracks.push(value),
                Some("clip") => clip_rows.push(value),
                Some("media") => media_rows.push(value),
                _ => {}
            }
        } else {
            missing.push(id);
        }
    }
    json!({"missingObjectIds":missing,"activeTimelineId":format!("timeline:{}",project.active_timeline_id),"timelineOrder":project.timelines.iter().map(|t|format!("timeline:{}",t.id)).collect::<Vec<_>>(),"selectedClipIds":clips,"selectedMediaIds":media,"canUndo":can_undo,"canRedo":can_redo,"timelines":timelines,"tracks":tracks,"clips":clip_rows,"media":media_rows})
}
/// The target-independent complete edit stamp, and bounded requested object details.
pub(crate) fn state(
    studio: &crate::studio::Studio,
    ids: &[String],
) -> Result<Value, ClipEditError> {
    validate_identities(studio.project())?;
    let document = document_state_id(studio.project());
    let value = json!({"version":1,"documentStateId":document,"editStateId":edit_state_id(studio,&document)?,"readback":readback(studio.project(),ids,selected(studio),selected_media(studio),studio.session.as_ref().is_some_and(|v|v.can_undo()),studio.session.as_ref().is_some_and(|v|v.can_redo()),|id|studio.locked(id))});
    if serde_json::to_vec(&value)
        .map_err(|_| ClipEditError::InvalidInput)?
        .len()
        > MAX_RECEIPT_BYTES
    {
        return Err(ClipEditError::ReceiptTooLarge);
    }
    Ok(value)
}
fn related(edit: &ClipEdit) -> Vec<String> {
    use ClipEdit::*;
    match edit {
        PlaceMedia {
            media_id, track_id, ..
        } => vec![media_id.clone(), track_id.clone()],
        RemoveTrack { track_id } | SetTrackFlag { track_id, .. } | AddText { track_id, .. } => {
            vec![track_id.clone()]
        }
        RemoveTimeline { timeline_id }
        | RenameTimeline { timeline_id, .. }
        | SelectTimeline { timeline_id }
        | MoveTimeline { timeline_id, .. }
        | SetTimelineVideo { timeline_id, .. } => vec![timeline_id.clone()],
        Move { moves } => moves
            .iter()
            .flat_map(|v| [v.clip_id.clone(), v.track_id.clone()])
            .collect(),
        Split { clip_ids, .. } | Delete { clip_ids } | Merge { clip_ids } => clip_ids.clone(),
        Trim { clip_id, .. }
        | FreezeImage { clip_id, .. }
        | SetSpeed { clip_id, .. }
        | SetSpeedCurve { clip_id, .. }
        | Transform { clip_id, .. }
        | Update { clip_id, .. }
        | SetAudioStream { clip_id, .. }
        | DetachAudio { clip_id }
        | ReattachAudio { clip_id }
        | SetText { clip_id, .. }
        | SetKey { clip_id, .. }
        | ClearKey { clip_id, .. }
        | ClearKeys { clip_id, .. } => vec![clip_id.clone()],
        AddTrack | AddTimeline | Undo | Redo => vec![],
    }
}
#[derive(PartialEq)]
enum ObjectRef<'a> {
    Media(&'a concat_project::model::MediaItem),
    Timeline(&'a Timeline, usize),
    Track(&'a concat_project::model::Track, &'a str, usize),
    Clip(&'a Clip, &'a str),
}
fn object_index(project: &Project) -> HashMap<String, ObjectRef<'_>> {
    let mut result = HashMap::new();
    for m in &project.media {
        result.insert(format!("media:{}", m.id), ObjectRef::Media(m));
    }
    for (order, t) in project.timelines.iter().enumerate() {
        result.insert(format!("timeline:{}", t.id), ObjectRef::Timeline(t, order));
        for (order, v) in t.tracks.iter().enumerate() {
            result.insert(format!("track:{}", v.id), ObjectRef::Track(v, &t.id, order));
        }
        for v in &t.clips {
            result.insert(format!("clip:{}", v.id), ObjectRef::Clip(v, &t.id));
        }
    }
    result
}
fn changed_ids(before: &Project, after: &Project) -> Vec<String> {
    let old = object_index(before);
    object_index(after)
        .into_iter()
        .filter_map(|(id, value)| (old.get(&id) != Some(&value)).then_some(id))
        .collect()
}

struct ReceiptContext<'a> {
    identity: &'a ClipIdentity<'a>,
    generation: u64,
    revision: u64,
    applied: bool,
    can_undo: bool,
    can_redo: bool,
}
fn projected_receipt(
    before: &Project,
    project: &Project,
    context: ReceiptContext<'_>,
) -> ClipReceipt {
    let ReceiptContext {
        identity,
        generation,
        revision,
        applied,
        can_undo,
        can_redo,
    } = context;
    let old = objects(before);
    let new = objects(project);
    let mut created_ids = new.difference(&old).cloned().collect::<Vec<_>>();
    created_ids.sort();
    let mut removed_ids = old.difference(&new).cloned().collect::<Vec<_>>();
    removed_ids.sort();
    ClipReceipt {
        project_id: identity.project_id.into(),
        document_session_id: identity.document_session_id.into(),
        generation,
        source_revision: revision.saturating_sub(u64::from(applied)),
        revision,
        source_timeline_id: format!("timeline:{}", before.active_timeline_id),
        active_timeline_id: format!("timeline:{}", project.active_timeline_id),
        source_document_state_id: document_state_id(before),
        document_state_id: document_state_id(project),
        source_edit_state_id: format!("clip-edit-v1-{}", "0".repeat(64)),
        edit_state_id: format!("clip-edit-v1-{}", "0".repeat(64)),
        selection_revision: 0,
        selected_object_ids: vec![],
        edit: Value::Null,
        changed: applied,
        document_changed: before != project,
        created_ids,
        removed_ids,
        readback: readback(
            project,
            &changed_ids(before, project),
            vec![],
            vec![],
            can_undo,
            can_redo,
            |_| false,
        ),
    }
}
fn receipt(
    before: &Project,
    studio: &crate::studio::Studio,
    identity: &ClipIdentity<'_>,
    applied: bool,
) -> ClipReceipt {
    let (generation, revision, _, _) = studio.mcp_clip_state();
    projected_receipt(
        before,
        studio.project(),
        ReceiptContext {
            identity,
            generation,
            revision,
            applied,
            can_undo: studio.session.as_ref().is_some_and(|v| v.can_undo()),
            can_redo: studio.session.as_ref().is_some_and(|v| v.can_redo()),
        },
    )
}

fn receipt_budget(receipt: &ClipReceipt) -> Result<(), ClipEditError> {
    // Project-clone preflight uses a temporary mint. Native deleted IDs remain
    // reserved in the real Session, so its newly minted numeric suffix can be
    // longer. Pad every canonical identity to the accepted 64-byte suffix bound
    // before serializing the complete receipt; never drop affected target IDs.
    fn pad_ids(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                if let Some((namespace, suffix)) = text.split_once(':')
                    && matches!(namespace, "media" | "clip" | "track" | "timeline")
                    && suffix.len() <= 64
                {
                    *text = format!("{namespace}:{}", "x".repeat(64));
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    pad_ids(value);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values_mut() {
                    pad_ids(value);
                }
            }
            _ => {}
        }
    }
    let mut projected = serde_json::to_value(receipt).map_err(|_| ClipEditError::InvalidInput)?;
    pad_ids(&mut projected);
    if serde_json::to_vec(&projected)
        .map_err(|_| ClipEditError::InvalidInput)?
        .len()
        > MAX_RECEIPT_BYTES
    {
        Err(ClipEditError::ReceiptTooLarge)
    } else {
        Ok(())
    }
}

/// Read-only trusted-card preflight, with the same target and complete-state binding.
pub(crate) fn approval_preflight(
    studio: &crate::studio::Studio,
    action: &concat_editor_mcp::ClipAction,
) -> Result<(), ClipEditError> {
    if studio.mcp_clip_state().3 {
        return Err(ClipEditError::Busy);
    }
    validate_identities(studio.project())?;
    if edit_state_id(studio, &document_state_id(studio.project()))? != action.expected_edit_state_id
    {
        return Err(ClipEditError::StaleContext);
    }
    match plan(studio.project(), &action.timeline_id, &action.edit)? {
        ClipPlan::Command(command) => {
            preflight(studio.project(), &command, |id| studio.locked(id))?;
        }
        ClipPlan::Undo | ClipPlan::Redo => {
            let target = studio
                .mcp_clip_peek_history(matches!(action.edit, ClipEdit::Redo))
                .ok_or(ClipEditError::NoHistory)?;
            validate_transition(studio.project(), &target, |id| studio.locked(id))?;
        }
    }
    Ok(())
}

/// Execute only after outer authorization/deduplication and modal checks, on
/// the UI thread. Hooks must preserve the same Session and exact native result.
/// No transient view-state override or independent history is permitted.
pub(crate) fn execute(
    studio: &mut crate::studio::Studio,
    expected: &ClipBinding<'_>,
    actual: &ClipIdentity<'_>,
    edit: &ClipEdit,
    hooks: &ClipNativeHooks,
    commit: impl FnOnce(&crate::studio::Studio) -> Result<(), ClipEditError>,
) -> Result<ClipReceipt, ClipEditError> {
    let (generation, revision, _, busy) = studio.mcp_clip_state();
    if expected.project_id.is_empty()
        || expected.document_session_id.is_empty()
        || expected.project_id.len() > 512
        || expected.document_session_id.len() > 512
        || expected.project_id != actual.project_id
        || expected.document_session_id != actual.document_session_id
        || expected.generation != generation
        || studio.session.is_none()
    {
        return Err(ClipEditError::WrongProject);
    }
    if busy {
        return Err(ClipEditError::Busy);
    }
    if expected.selection_revision != studio.mcp_selection_epoch() {
        return Err(ClipEditError::StaleContext);
    }
    if expected.revision != revision {
        return Err(ClipEditError::StaleRevision);
    }
    validate_identities(studio.project())?;
    if edit_state_id(studio, &document_state_id(studio.project()))?
        != expected.expected_edit_state_id
    {
        return Err(ClipEditError::StaleContext);
    }
    let planned = plan(studio.project(), expected.timeline_id, edit)?;
    let before = studio.project().clone();
    let history_direction = match &planned {
        ClipPlan::Undo => Some(ClipHistory::Undo),
        ClipPlan::Redo => Some(ClipHistory::Redo),
        _ => None,
    };
    let (staged, applied) = match &planned {
        ClipPlan::Command(command) => preflight(&before, command, |id| studio.locked(id))?,
        ClipPlan::Undo | ClipPlan::Redo => {
            let peek = hooks
                .peek_history
                .ok_or(ClipEditError::HistoryPreviewUnavailable)?;
            if hooks.history.is_none() {
                return Err(ClipEditError::HistoryPreviewUnavailable);
            }
            let direction = history_direction.ok_or(ClipEditError::InvalidInput)?;
            let staged = peek(studio, direction).ok_or(ClipEditError::NoHistory)?;
            validate_transition(&before, &staged, |id| studio.locked(id))?;
            let applied = true;
            (staged, applied)
        }
    };
    let next_revision = if applied {
        revision.checked_add(1).ok_or(ClipEditError::InvalidInput)?
    } else {
        revision
    };
    let mut preview = projected_receipt(
        &before,
        &staged,
        ReceiptContext {
            identity: actual,
            generation,
            revision: next_revision,
            applied,
            can_undo: false,
            can_redo: false,
        },
    );
    let mut ids = related(edit);
    ids.extend(changed_ids(&before, &staged));
    ids.extend(preview.removed_ids.clone());
    let selection = selected(studio)
        .into_iter()
        .filter(|id| native(id, "clip").is_ok_and(|id| staged.active().clip(id).is_some()))
        .collect::<Vec<_>>();
    preview.source_revision = revision;
    preview.source_edit_state_id = expected.expected_edit_state_id.into();
    preview.edit = serde_json::to_value(edit).map_err(|_| ClipEditError::InvalidInput)?;
    preview.selection_revision = u64::MAX;
    preview.selected_object_ids = selection.clone();
    preview.readback = readback(
        &staged,
        &ids,
        selection,
        selected_media(studio),
        false,
        false,
        |id| studio.locked(id),
    );
    receipt_budget(&preview)?;
    // No helper, history stack or revision has changed before this exact boundary.
    if !applied {
        commit(studio)?;
        let mut result = receipt(&before, studio, actual, false);
        decorate(&mut result, studio, edit, expected, &before)?;
        return Ok(result);
    }
    let outcome = commit_prepared(studio, commit, |studio| {
        match planned {
            ClipPlan::Command(command) => (hooks.apply)(studio, *command),
            ClipPlan::Undo | ClipPlan::Redo => {
                let history = hooks
                    .history
                    .ok_or(ClipEditError::HistoryPreviewUnavailable)?;
                history(
                    studio,
                    history_direction.ok_or(ClipEditError::InvalidInput)?,
                )
            }
        }
        .map_err(|_| ClipEditError::NativeRejected)
    })?;
    // This check detects a violated hook contract, never infers a native error
    // from Studio.apply's ambiguous Option. The real Session error came above.
    let (after_generation, after_revision, _, _) = studio.mcp_clip_state();
    if !outcome.applied || after_generation != generation || after_revision != next_revision {
        return Err(ClipEditError::CommitFailed);
    }
    studio.sync_mcp_selection();
    let mut result = receipt(&before, studio, actual, true);
    decorate(&mut result, studio, edit, expected, &before)?;
    Ok(result)
}

// The same final boundary is exercised with a real Session in non-GUI tests.
fn commit_prepared<T, R, E>(
    host: &mut T,
    guard: impl FnOnce(&T) -> Result<(), E>,
    apply: impl FnOnce(&mut T) -> Result<R, E>,
) -> Result<R, E> {
    guard(host)?;
    apply(host)
}

fn decorate(
    result: &mut ClipReceipt,
    studio: &crate::studio::Studio,
    edit: &ClipEdit,
    expected: &ClipBinding<'_>,
    before: &Project,
) -> Result<(), ClipEditError> {
    let mut ids = related(edit);
    ids.extend(changed_ids(before, studio.project()));
    ids.extend(result.removed_ids.clone());
    let value = state(studio, &ids)?;
    result.source_revision = expected.revision;
    result.source_edit_state_id = expected.expected_edit_state_id.into();
    result.edit_state_id = value["editStateId"]
        .as_str()
        .ok_or(ClipEditError::CommitFailed)?
        .into();
    result.selection_revision = studio.mcp_selection_epoch();
    result.selected_object_ids = selected(studio);
    result.edit = serde_json::to_value(edit).map_err(|_| ClipEditError::InvalidInput)?;
    result.readback = value["readback"].clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_project::Editor;
    use concat_project::model::{MediaItem, MediaKind};

    fn editor_with_media(kind: MediaKind, has_audio: bool) -> Editor {
        let mut p = Project::new();
        p.media.push(MediaItem {
            id: "m1".into(),
            kind,
            has_audio,
            duration: Some(10.0),
            name: "fixture".to_owned(),
            ..Default::default()
        });
        Editor::from_document(&concat_project::to_document(
            &concat_project::DocumentSettings {
                name: "fixture".to_owned(),
                width: 1920,
                height: 1080,
                rate_num: 30,
                rate_den: 1,
            },
            &p,
        ))
        .unwrap()
    }
    fn editor() -> Editor {
        editor_with_media(MediaKind::Image, false)
    }
    fn apply(editor: &mut Editor, edit: ClipEdit) -> concat_project::Outcome {
        let current = format!("timeline:{}", editor.project().active_timeline_id);
        let ClipPlan::Command(command) = plan(editor.project(), &current, &edit).unwrap() else {
            panic!("expected native command");
        };
        preflight(editor.project(), &command, |_| false).unwrap();
        editor.apply(*command).unwrap()
    }

    #[test]
    fn oversized_native_state_is_refused_without_a_second_document_allocation_or_mutation() {
        for text in [false, true] {
            let mut before = Project::new();
            if text {
                let mut clip = Clip::blank("c1", "T1", ClipKind::Text, "fixture", 0.0, 1.0);
                clip.text = Some(TextStyle {
                    content: "x".repeat(MAX_STATE_BYTES + 1),
                    ..Default::default()
                });
                std::sync::Arc::make_mut(&mut before.timelines[0])
                    .clips
                    .push(std::sync::Arc::new(clip));
            } else {
                before.extra.insert(
                    "legacyPayload".into(),
                    Value::String("x".repeat(MAX_STATE_BYTES + 1)),
                );
            }
            let original_tracks = before.active().tracks.len();
            assert_eq!(
                validate_identities(&before),
                Err(ClipEditError::ReceiptTooLarge)
            );
            assert_eq!(
                preflight(&before, &Command::AddTrack, |_| false).err(),
                Some(ClipEditError::ReceiptTooLarge)
            );
            assert_eq!(before.active().tracks.len(), original_tracks);
        }
    }

    #[test]
    fn detach_preview_work_and_replication_are_bounded_before_temporary_apply() {
        use concat_project::model::{AudioTrack, Track};
        let mut e = editor_with_media(MediaKind::Video, true);
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let command = Command::DetachAudio {
            clip_id: id.clone(),
        };
        assert!(preview_work(e.project(), &command).is_ok());
        let mut large = e.project().clone();
        large.media[0].audio_tracks = (0..8)
            .map(|index| AudioTrack {
                index,
                codec: "aac".into(),
                channels: 2,
                sample_rate: 48000,
                title: String::new(),
                language: String::new(),
            })
            .collect();
        let timeline = std::sync::Arc::make_mut(&mut large.timelines[0]);
        for index in 100..2100 {
            let track = format!("t{index}");
            timeline.tracks.push(Track {
                id: track.clone(),
                visible: true,
                muted: false,
                extra: Default::default(),
            });
            let mut c = Clip::blank(
                format!("c{index}"),
                track,
                ClipKind::Video,
                "fixture",
                0.0,
                1.0,
            );
            c.media_id = "m1".into();
            timeline.clips.push(std::sync::Arc::new(c));
        }
        assert!(bounded_project(&large).is_ok());
        assert_eq!(
            preview_work(&large, &command),
            Err(ClipEditError::ReceiptTooLarge)
        );
        assert_eq!(
            preflight(&large, &command, |_| false).err(),
            Some(ClipEditError::ReceiptTooLarge)
        );
        let mut copied = e.project().clone();
        copied.media[0].audio_tracks = (0..20)
            .map(|index| AudioTrack {
                index,
                codec: "aac".into(),
                channels: 2,
                sample_rate: 48000,
                title: String::new(),
                language: String::new(),
            })
            .collect();
        std::sync::Arc::make_mut(&mut copied.timelines[0])
            .clip_mut(&id)
            .unwrap()
            .extra
            .insert(
                "legacyPayload".into(),
                Value::String("x".repeat(1024 * 1024)),
            );
        assert!(bounded_project(&copied).is_ok());
        assert_eq!(
            preview_work(&copied, &command),
            Err(ClipEditError::ReceiptTooLarge)
        );
        assert_eq!(copied.active().clips.len(), 1);
    }
    #[test]
    fn state_budget_reserves_real_session_mint_width_before_commit() {
        let mut before = Project::new();
        let (small_after, _) = preview_command(&before, &Command::AddTrack).unwrap();
        let overhead = serde_json::to_vec(&small_after).unwrap().len() + "legacyPayload".len() + 6;
        before.extra.insert(
            "legacyPayload".into(),
            Value::String("x".repeat(MAX_STATE_BYTES - overhead - 64)),
        );
        let (staged, _) = preview_command(&before, &Command::AddTrack).unwrap();
        assert!(bounded_project(&staged).is_ok());
        assert_eq!(
            preflight(&before, &Command::AddTrack, |_| false).err(),
            Some(ClipEditError::ReceiptTooLarge)
        );
        assert_eq!(before.active().tracks.len(), 4);
    }
    #[test]
    fn remaining_typed_edit_families_reach_original_native_commands_and_readback() {
        use concat_editor_mcp::clip_edit::{
            ClipDestination, ClipProperties, ClipSpeedPoint, ClipTransform,
        };
        let mut e = editor();
        let clip = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let id = format!("clip:{clip}");
        let track = apply(&mut e, ClipEdit::AddTrack).created_id.unwrap();
        let track_id = format!("track:{track}");
        apply(
            &mut e,
            ClipEdit::SetTrackFlag {
                track_id: track_id.clone(),
                flag: ClipTrackFlag::Muted,
                value: true,
            },
        );
        assert!(e.project().active().track(&track).unwrap().muted);
        apply(
            &mut e,
            ClipEdit::Move {
                moves: vec![ClipDestination {
                    clip_id: id.clone(),
                    track_id: track_id.clone(),
                    start: 2.0,
                }],
            },
        );
        apply(
            &mut e,
            ClipEdit::Trim {
                clip_id: id.clone(),
                edge: ClipTrimEdge::Start,
                delta: 0.5,
            },
        );
        apply(
            &mut e,
            ClipEdit::Trim {
                clip_id: id.clone(),
                edge: ClipTrimEdge::End,
                delta: -0.5,
            },
        );
        apply(
            &mut e,
            ClipEdit::SetSpeedCurve {
                clip_id: id.clone(),
                points: Some(vec![
                    ClipSpeedPoint {
                        at: 0.0,
                        speed: 1.0,
                    },
                    ClipSpeedPoint {
                        at: 1.0,
                        speed: 2.0,
                    },
                ]),
            },
        );
        apply(
            &mut e,
            ClipEdit::SetSpeedCurve {
                clip_id: id.clone(),
                points: None,
            },
        );
        apply(
            &mut e,
            ClipEdit::Transform {
                clip_id: id.clone(),
                transform: ClipTransform {
                    scale: Some(1.2),
                    offset_x: Some(0.4),
                    rotation: Some(-180.0),
                    stretch_x: Some(0.9),
                    ..Default::default()
                },
            },
        );
        apply(
            &mut e,
            ClipEdit::Update {
                clip_id: id.clone(),
                properties: ClipProperties {
                    name: Some("renamed".into()),
                    opacity: Some(0.3),
                    reverse: Some(true),
                    flip_h: Some(true),
                    ..Default::default()
                },
            },
        );
        assert_eq!(e.project().active().clip(&clip).unwrap().name, "renamed");
        apply(
            &mut e,
            ClipEdit::SetKey {
                clip_id: id.clone(),
                property: ClipKeyProperty::Opacity,
                at: 0.5,
                value: 0.4,
                ease: [0.0, 0.0, 1.0, 1.0],
            },
        );
        apply(
            &mut e,
            ClipEdit::ClearKey {
                clip_id: id.clone(),
                property: ClipKeyProperty::Opacity,
                at: 0.5,
            },
        );
        apply(
            &mut e,
            ClipEdit::SetKey {
                clip_id: id.clone(),
                property: ClipKeyProperty::Opacity,
                at: 0.5,
                value: 0.4,
                ease: [0.0, 0.0, 1.0, 1.0],
            },
        );
        apply(
            &mut e,
            ClipEdit::ClearKeys {
                clip_id: id.clone(),
                property: ClipKeyProperty::Opacity,
            },
        );
        assert!(e.project().active().clip(&clip).unwrap().keys.is_empty());
        apply(
            &mut e,
            ClipEdit::SetTimelineVideo {
                timeline_id: "timeline:TL1".into(),
                width: 1440,
                height: 1080,
                rate_num: 25,
                rate_den: 1,
            },
        );
        apply(
            &mut e,
            ClipEdit::RenameTimeline {
                timeline_id: "timeline:TL1".into(),
                name: " Main ".into(),
            },
        );
        assert_eq!(e.project().active().name, "Main");
        let other = apply(&mut e, ClipEdit::AddTimeline).created_id.unwrap();
        apply(
            &mut e,
            ClipEdit::SelectTimeline {
                timeline_id: "timeline:TL1".into(),
            },
        );
        apply(
            &mut e,
            ClipEdit::RemoveTimeline {
                timeline_id: format!("timeline:{other}"),
            },
        );
        apply(&mut e, ClipEdit::Delete { clip_ids: vec![id] });
        apply(&mut e, ClipEdit::RemoveTrack { track_id });
        assert!(e.project().active().clip(&clip).is_none());
        let mut audio = editor_with_media(MediaKind::Video, true);
        let id = apply(
            &mut audio,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        assert!(
            !apply(
                &mut audio,
                ClipEdit::SetAudioStream {
                    clip_id: format!("clip:{id}"),
                    stream: None
                }
            )
            .applied
        );
    }
    #[test]
    fn duplicate_public_identities_are_refused_before_any_native_command() {
        let mut e = editor();
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let mut duplicate = e.project().clone();
        let t = std::sync::Arc::make_mut(&mut duplicate.timelines[0]);
        t.clips.push(t.clips[0].clone());
        let before = duplicate.clone();
        assert_eq!(
            plan(
                &duplicate,
                "timeline:TL1",
                &ClipEdit::Delete {
                    clip_ids: vec![format!("clip:{id}")]
                }
            )
            .err(),
            Some(ClipEditError::AmbiguousIdentity)
        );
        assert_eq!(duplicate, before);
        let mut duplicate = e.project().clone();
        duplicate.media.push(duplicate.media[0].clone());
        assert_eq!(
            validate_identities(&duplicate),
            Err(ClipEditError::AmbiguousIdentity)
        );
        let mut duplicate = e.project().clone();
        let mut other = (*duplicate.timelines[0]).clone();
        other.id = "tl2".into();
        duplicate.timelines.push(std::sync::Arc::new(other));
        assert_eq!(
            validate_identities(&duplicate),
            Err(ClipEditError::AmbiguousIdentity)
        );
    }
    #[test]
    fn readback_proves_order_audio_relations_missing_ids_and_never_returns_bin_paths() {
        let mut e = editor_with_media(MediaKind::Video, true);
        let video = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let sound = apply(
            &mut e,
            ClipEdit::DetachAudio {
                clip_id: format!("clip:{video}"),
            },
        )
        .created_id
        .unwrap();
        let row = object_value(e.project(), &format!("clip:{sound}"), |_| false).unwrap();
        assert_eq!(row["kind"], "audio");
        assert_eq!(row["detachedFrom"], format!("clip:{video}"));
        let ids = vec!["media:m1".into(), "clip:missing".into()];
        let state = readback(e.project(), &ids, vec![], vec![], true, false, |_| false);
        assert_eq!(state["missingObjectIds"], json!(["clip:missing"]));
        assert!(state["media"][0].get("path").is_none());
        let first = e.project().active_timeline_id.clone();
        let second = apply(&mut e, ClipEdit::AddTimeline).created_id.unwrap();
        apply(
            &mut e,
            ClipEdit::MoveTimeline {
                timeline_id: format!("timeline:{second}"),
                index: 0,
            },
        );
        let state = readback(
            e.project(),
            &[format!("timeline:{second}")],
            vec![],
            vec![],
            true,
            false,
            |_| false,
        );
        assert_eq!(
            state["timelineOrder"],
            json!([format!("timeline:{second}"), format!("timeline:{first}")])
        );
        assert_eq!(state["timelines"][0]["order"], 0);
    }
    #[test]
    fn reattach_and_timeline_removal_cannot_indirectly_change_locked_tracks() {
        let mut e = editor_with_media(MediaKind::Video, true);
        let video = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let sound = apply(
            &mut e,
            ClipEdit::DetachAudio {
                clip_id: format!("clip:{video}"),
            },
        )
        .created_id
        .unwrap();
        let sound_track = e.project().active().clip(&sound).unwrap().track_id.clone();
        let ClipPlan::Command(command) = plan(
            e.project(),
            "timeline:TL1",
            &ClipEdit::ReattachAudio {
                clip_id: format!("clip:{video}"),
            },
        )
        .unwrap() else {
            panic!()
        };
        let before = e.project().clone();
        assert_eq!(
            preflight(e.project(), &command, |id| id == sound_track).err(),
            Some(ClipEditError::LockedTrack)
        );
        assert_eq!(e.project(), &before);
        apply(&mut e, ClipEdit::AddTimeline);
        let ClipPlan::Command(command) = plan(
            e.project(),
            &format!("timeline:{}", e.project().active_timeline_id),
            &ClipEdit::RemoveTimeline {
                timeline_id: "timeline:TL1".into(),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            preflight(e.project(), &command, |id| id == sound_track).err(),
            Some(ClipEditError::LockedTrack)
        );
    }
    fn native_session() -> (std::path::PathBuf, concat_host::Session) {
        let path =
            std::env::temp_dir().join(format!("clip-native-edit-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let s = concat_host::Session::open_owned(
            &path.to_string_lossy(),
            concat_project::DocumentSettings {
                name: "fixture".to_owned(),
                width: 1920,
                height: 1080,
                rate_num: 30,
                rate_den: 1,
            },
        )
        .unwrap();
        (path, s)
    }
    #[test]
    fn real_session_checked_commit_refuses_revoked_owner_after_preparation_without_history() {
        let (path, mut session) = native_session();
        let before = session.project().clone();
        let staged = preview_command(&before, &Command::AddTrack).unwrap();
        assert!(staged.1);
        let owner = session.writer_guard().unwrap();
        let lock = path.join(".seecut-writer.lock");
        std::fs::rename(&lock, path.join("retained-original-lock")).unwrap();
        let result = commit_prepared(
            &mut session,
            |_| owner.validate().map_err(|_| ClipEditError::CommitFailed),
            |s| {
                s.apply_outcome(Command::AddTrack)
                    .map_err(|_| ClipEditError::NativeRejected)
            },
        );
        assert_eq!(result.err(), Some(ClipEditError::CommitFailed));
        assert_eq!(session.project(), &before);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
        drop(session);
        drop(owner);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn real_session_checked_commit_refuses_revoked_lease_after_preparation_without_history() {
        use super::super::write_registry::{Scope, WriteRegistry};
        use std::time::Instant;
        let (path, mut session) = native_session();
        let before = session.project().clone();
        let scope = Scope {
            instance: "i".into(),
            project: "p".into(),
            session: "s".into(),
        };
        let mut writes = WriteRegistry::<ClipEdit, Value>::new();
        writes.bind_scope(Some(scope.clone()));
        writes
            .trusted_grant_session(&scope, "a", "t".into(), Instant::now())
            .unwrap();
        preview_command(&before, &Command::AddTrack).unwrap();
        writes.revoke();
        let result = commit_prepared(
            &mut session,
            |_| {
                writes
                    .authorize(&scope, &scope, "a", "t", Instant::now())
                    .map_err(|_| ClipEditError::CommitFailed)
            },
            |s| {
                s.apply_outcome(Command::AddTrack)
                    .map_err(|_| ClipEditError::NativeRejected)
            },
        );
        assert_eq!(result.err(), Some(ClipEditError::CommitFailed));
        assert_eq!(session.project(), &before);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn clip_original_ledger_replays_receipt_without_reapplying_real_session_history() {
        use super::super::canvas_policy::Receipts;
        use super::super::write_registry::{Begin, OperationKey, Scope, WriteRegistry};
        use concat_editor_mcp::{ClipAction, ClipParameters, OperationQuery, Response};
        use std::time::Instant;
        let (path, mut session) = native_session();
        let initial = session.project().clone();
        let now = Instant::now();
        let scope = Scope {
            instance: "i".into(),
            project: "p".into(),
            session: "s".into(),
        };
        let p = ClipParameters {
            operation_id: concat_editor_mcp::operation_id("a", "s", 1).unwrap(),
            client_sequence: 1,
            selection_revision: 0,
            action: ClipAction {
                timeline_id: "timeline:TL1".into(),
                expected_edit_state_id: format!("clip-edit-v1-{}", "a".repeat(64)),
                edit: ClipEdit::AddTrack,
            },
        };
        let key = OperationKey {
            client: "a".into(),
            document_session: "s".into(),
            client_sequence: 1,
        };
        let mut writes = WriteRegistry::<ClipParameters, Response>::new();
        writes.bind_scope(Some(scope.clone()));
        writes
            .trusted_grant_session(&scope, "a", "t".into(), now)
            .unwrap();
        let mut receipts = Receipts::default();
        assert!(matches!(
            writes.begin(&scope, &scope, &key, "t", &p, now),
            Ok(Begin::New)
        ));
        assert!(receipts.reserve(scope.clone(), "a".into(), &p, now));
        let query = OperationQuery {
            operation_id: p.operation_id.clone(),
            client_sequence: 1,
        };
        assert_eq!(
            receipts.query_family(&scope, "a", &query, now, true).status,
            "outcomeUnknown"
        );
        commit_prepared(
            &mut session,
            |s| {
                writes
                    .authorize(&scope, &scope, "a", "t", now)
                    .map_err(|_| ClipEditError::CommitFailed)?;
                s.writer_guard()
                    .unwrap()
                    .validate()
                    .map_err(|_| ClipEditError::CommitFailed)
            },
            |s| {
                s.apply_outcome(Command::AddTrack)
                    .map_err(|_| ClipEditError::NativeRejected)
            },
        )
        .unwrap();
        let after = session.project().clone();
        let r = Response::ok(
            json!({"tool":"clip_edit","operationId":p.operation_id,"documentStateId":document_state_id(&after)}),
        );
        assert!(receipts.finish(&scope, "a", &p, r.clone()));
        writes.finish(&scope, &key, r.clone(), now).unwrap();
        let cached = writes
            .cached(&scope, &scope, &key, "t", &p, now)
            .unwrap()
            .unwrap();
        assert_eq!(cached.data, r.data);
        assert_eq!(
            receipts.query_family(&scope, "a", &query, now, true).data,
            r.data
        );
        assert_eq!(session.project(), &after);
        assert!(session.history_outcome(false).applied);
        assert_eq!(session.project(), &initial);
        assert!(!session.history_outcome(false).applied);
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn real_session_history_peek_is_read_only_and_commits_same_original_stack() {
        let (path, mut session) = native_session();
        let before = session.project().clone();
        let outcome = commit_prepared(
            &mut session,
            |_| Ok::<_, ClipEditError>(()),
            |s| {
                s.apply_outcome(Command::AddTrack)
                    .map_err(|_| ClipEditError::NativeRejected)
            },
        )
        .unwrap();
        assert!(outcome.applied);
        assert_eq!(session.peek_undo(), Some(&before));
        let after = session.project().clone();
        assert_eq!(session.project(), &after);
        assert!(session.history_outcome(false).applied);
        assert_eq!(session.project(), &before);
        assert_eq!(session.peek_redo(), Some(&after));
        assert!(session.history_outcome(true).applied);
        assert_eq!(session.project(), &after);
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn placement_split_merge_and_history_use_native_semantics() {
        let mut e = editor();
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let original = e.project().clone();
        apply(
            &mut e,
            ClipEdit::Split {
                clip_ids: vec![format!("clip:{id}")],
                time: 2.0,
            },
        );
        assert_eq!(e.project().active().clips.len(), 2);
        let ids = e
            .project()
            .active()
            .clips
            .iter()
            .map(|c| format!("clip:{}", c.id))
            .collect();
        apply(&mut e, ClipEdit::Merge { clip_ids: ids });
        assert_eq!(e.project().active().clips.len(), 1);
        assert_eq!(e.project().active().clips[0].duration, 5.0);
        assert!(e.undo());
        assert_eq!(e.project().active().clips.len(), 2);
        assert!(e.undo());
        assert_eq!(e.project(), &original);
        assert!(e.redo());
        assert_eq!(e.project().active().clips.len(), 2);
    }

    #[test]
    fn unknown_targets_and_wrong_timeline_do_not_fall_back_to_selection() {
        let e = editor();
        assert!(matches!(
            plan(
                e.project(),
                "timeline:TL1",
                &ClipEdit::Delete {
                    clip_ids: vec!["clip:missing".into()]
                }
            ),
            Err(ClipEditError::MissingObject)
        ));
        assert!(matches!(
            plan(e.project(), "timeline:missing", &ClipEdit::AddTrack),
            Err(ClipEditError::WrongProject)
        ));
        assert!(
            plan(
                e.project(),
                "timeline:TL1",
                &ClipEdit::Delete { clip_ids: vec![] }
            )
            .is_err()
        );
    }

    #[test]
    fn locked_destination_and_indirect_deletion_are_rejected() {
        let mut e = editor();
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let ClipPlan::Command(c) = plan(
            e.project(),
            "timeline:TL1",
            &ClipEdit::Delete {
                clip_ids: vec![format!("clip:{id}")],
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            preflight(e.project(), &c, |id| id == "T1"),
            Err(ClipEditError::LockedTrack)
        );
        let ClipPlan::Command(c) = plan(
            e.project(),
            "timeline:TL1",
            &ClipEdit::RemoveTrack {
                track_id: "track:T1".into(),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            preflight(e.project(), &c, |id| id == "T1"),
            Err(ClipEditError::LockedTrack)
        );
    }

    #[test]
    fn image_freeze_uses_existing_media_and_native_ripple() {
        let mut e = editor();
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let result = apply(
            &mut e,
            ClipEdit::FreezeImage {
                clip_id: format!("clip:{id}"),
                time: 2.0,
            },
        );
        assert!(result.created_id.is_some());
        assert_eq!(e.project().media.len(), 1);
        assert_eq!(e.project().active().clips.len(), 3);
        assert!(e.undo());
        assert_eq!(e.project().active().clips.len(), 1);
    }

    #[test]
    fn text_patch_preserves_unspecified_style_and_speed_retains_source_span() {
        let mut e = editor();
        let id = apply(
            &mut e,
            ClipEdit::AddText {
                track_id: "track:T1".into(),
                start: 0.0,
                duration: 4.0,
                text: ClipText {
                    content: Some("before".into()),
                    font_size: Some(0.2),
                    ..Default::default()
                },
            },
        )
        .created_id
        .unwrap();
        apply(
            &mut e,
            ClipEdit::SetText {
                clip_id: format!("clip:{id}"),
                text: ClipText {
                    content: Some("after".into()),
                    ..Default::default()
                },
            },
        );
        let c = e.project().active().clip(&id).unwrap();
        assert_eq!(c.text.as_ref().unwrap().font_size, 0.2);
        assert_eq!(c.text.as_ref().unwrap().content, "after");
        apply(
            &mut e,
            ClipEdit::SetSpeed {
                clip_id: format!("clip:{id}"),
                speed: 2.0,
            },
        );
        assert_eq!(e.project().active().clip(&id).unwrap().duration, 2.0);
    }

    #[test]
    fn audio_detach_and_reattach_share_the_native_history() {
        let mut e = editor_with_media(MediaKind::Video, true);
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let sound = apply(
            &mut e,
            ClipEdit::DetachAudio {
                clip_id: format!("clip:{id}"),
            },
        )
        .created_id
        .unwrap();
        assert_eq!(e.project().active().clip(&id).unwrap().muted, Some(true));
        assert_eq!(
            e.project()
                .active()
                .clip(&sound)
                .unwrap()
                .detached_from
                .as_deref(),
            Some(id.as_str())
        );
        apply(
            &mut e,
            ClipEdit::ReattachAudio {
                clip_id: format!("clip:{sound}"),
            },
        );
        assert_eq!(e.project().active().clips.len(), 1);
        assert_eq!(e.project().active().clip(&id).unwrap().muted, None);
        assert!(e.undo());
        assert_eq!(e.project().active().clips.len(), 2);
    }

    #[test]
    fn no_change_is_not_confused_with_a_native_error() {
        let mut e = editor();
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        let ClipPlan::Command(command) = plan(
            e.project(),
            "timeline:TL1",
            &ClipEdit::SetSpeed {
                clip_id: format!("clip:{id}"),
                speed: 1.0,
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            preflight(e.project(), &command, |_| false).map(|(_, applied)| applied),
            Ok(false)
        );
        assert_eq!(
            preflight(
                e.project(),
                &Command::MergeClips { clip_ids: vec![id] },
                |_| false
            ),
            Err(ClipEditError::NativeRejected)
        );
    }

    #[test]
    fn video_freeze_is_refused_without_touching_view_state() {
        let mut e = editor_with_media(MediaKind::Video, true);
        let id = apply(
            &mut e,
            ClipEdit::PlaceMedia {
                media_id: "media:m1".into(),
                track_id: "track:T1".into(),
                start: 0.0,
            },
        )
        .created_id
        .unwrap();
        assert!(matches!(
            plan(
                e.project(),
                "timeline:TL1",
                &ClipEdit::FreezeImage {
                    clip_id: format!("clip:{id}"),
                    time: 2.0,
                }
            ),
            Err(ClipEditError::UnsupportedVideoFreeze)
        ));
    }

    #[test]
    fn complete_large_delete_receipt_is_refused_without_truncating_ids() {
        let mut before = Project::new();
        let timeline = std::sync::Arc::make_mut(&mut before.timelines[0]);
        for index in 1..=1200 {
            timeline.clips.push(std::sync::Arc::new(Clip::blank(
                format!("c{index}"),
                "T1".to_owned(),
                ClipKind::Image,
                "fixture".to_owned(),
                0.0,
                1.0,
            )));
        }
        let (after, applied) = preflight(
            &before,
            &Command::RemoveTrack {
                track_id: "T1".to_owned(),
            },
            |_| false,
        )
        .unwrap();
        assert!(applied);
        let identity = ClipIdentity {
            project_id: "project",
            document_session_id: "session",
        };
        let receipt = projected_receipt(
            &before,
            &after,
            ReceiptContext {
                identity: &identity,
                generation: 1,
                revision: 2,
                applied: true,
                can_undo: true,
                can_redo: false,
            },
        );
        assert_eq!(receipt.removed_ids.len(), 1201);
        assert_eq!(
            receipt_budget(&receipt),
            Err(ClipEditError::ReceiptTooLarge)
        );
        assert_eq!(before.active().clips.len(), 1200);
    }

    #[test]
    fn history_transition_cannot_restore_or_remove_locked_lane_contents() {
        let before = Project::new();
        let mut after = before.clone();
        std::sync::Arc::make_mut(&mut after.timelines[0])
            .clips
            .push(std::sync::Arc::new(Clip::blank(
                "c1".to_owned(),
                "T1".to_owned(),
                ClipKind::Text,
                "fixture".to_owned(),
                0.0,
                1.0,
            )));
        assert_eq!(
            validate_transition(&before, &after, |id| id == "T1"),
            Err(ClipEditError::LockedTrack)
        );
        assert_eq!(
            validate_transition(&after, &before, |id| id == "T1"),
            Err(ClipEditError::LockedTrack)
        );
    }
}
