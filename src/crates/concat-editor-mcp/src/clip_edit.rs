//! Bounded timeline edits. No file paths, document JSON, scripts or implicit selection.
use serde::{Deserialize, Serialize};

/// Maximum timeline position or duration accepted by the editing boundary.
pub const MAX_CLIP_SECONDS: f64 = 86_400.0;
/// Maximum number of explicitly targeted clips in one atomic edit.
pub const MAX_CLIP_TARGETS: usize = 128;

/// Which edge a trim moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClipTrimEdge {
    /// Move the head and its source in-point.
    Start,
    /// Move the tail.
    End,
}

/// A persistent track flag; editor-only lane locks are not changed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClipTrackFlag {
    /// Include this track's picture in the output.
    Visible,
    /// Silence this track's sound.
    Muted,
}

/// Properties supported by the existing native keyframe command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClipKeyProperty {
    /// Picture scale.
    Scale,
    /// Horizontal frame-width offset.
    OffsetX,
    /// Vertical frame-height offset.
    OffsetY,
    /// Clockwise degrees.
    Rotation,
    /// Picture opacity.
    Opacity,
    /// Linear audio gain.
    Volume,
}

/// One explicit clip destination; IDs use the names published by `project`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipDestination {
    /// Canonical `clip:<stable-id>`.
    pub clip_id: String,
    /// Canonical `track:<stable-id>` in the active timeline.
    pub track_id: String,
    /// New timeline start in seconds.
    pub start: f64,
}

/// Optional placement values; at least one must be supplied.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipTransform {
    /// Uniform scale, 0.05 through 8.
    pub scale: Option<f64>,
    /// Horizontal offset in frame widths, -3 through 3.
    pub offset_x: Option<f64>,
    /// Vertical offset in frame heights, -3 through 3.
    pub offset_y: Option<f64>,
    /// Rotation in degrees, -180 through 180.
    pub rotation: Option<f64>,
    /// Additional width multiplier, 0.1 through 10.
    pub stretch_x: Option<f64>,
    /// Additional height multiplier, 0.1 through 10.
    pub stretch_y: Option<f64>,
}

/// Safe existing-clip updates; deliberately excludes arbitrary effect chains.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipProperties {
    /// Display name, up to 256 bytes.
    pub name: Option<String>,
    /// Linear gain, 0 through 16.
    pub volume: Option<f64>,
    /// Ramp-in seconds; cannot exceed the target clip's duration.
    pub fade_in: Option<f64>,
    /// Ramp-out seconds; cannot exceed the target clip's duration.
    pub fade_out: Option<f64>,
    /// Compositing opacity, 0 through 1.
    pub opacity: Option<f64>,
    /// Preserve voice pitch during speed changes.
    pub preserve_pitch: Option<bool>,
    /// Explicit mute state; never a toggle based on current selection.
    pub muted: Option<bool>,
    /// Explicit playback direction.
    pub reverse: Option<bool>,
    /// Mirror horizontally.
    pub flip_h: Option<bool>,
    /// Mirror vertically.
    pub flip_v: Option<bool>,
}

/// Plain title content and optional existing-model styling.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipText {
    /// Literal displayed text, up to 8192 bytes; never evaluated.
    pub content: Option<String>,
    /// Font family name only, up to 128 bytes; no font-file import.
    pub font_family: Option<String>,
    /// Cap height as a frame-height fraction, 0.01 through 1.
    pub font_size: Option<f64>,
    /// CSS weight, 100 through 900.
    pub font_weight: Option<f64>,
    /// Italic style.
    pub italic: Option<bool>,
    /// Six-digit RGB hex fill.
    pub color: Option<String>,
    /// Title opacity, 0 through 1.
    pub opacity: Option<f64>,
}

/// One strictly ordered speed-curve point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipSpeedPoint {
    /// Fraction of clip duration, 0 through 1.
    pub at: f64,
    /// Playback rate, 0.0625 through 16.
    pub speed: f64,
}

/// One explicit edit to the currently bound clip project and active timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ClipEdit {
    /// Probe and import one exact managed library version into the current media bin.
    /// Paths, unversioned assets and implicit import targets are not accepted.
    ImportLibrary {
        /// Stable local personal-library asset identity.
        asset_id: String,
        /// Existing immutable content and registration identity.
        version: crate::AssetVersion,
    },
    /// Place an existing bin item on a named active-timeline track.
    PlaceMedia {
        /// Existing `media:<id>`; no new media import is permitted.
        media_id: String,
        /// Existing `track:<id>`.
        track_id: String,
        /// Timeline seconds.
        start: f64,
    },
    /// Append a track through the native command and current undo stack.
    AddTrack,
    /// Remove a named active-timeline track and its clips.
    RemoveTrack {
        /// Existing `track:<id>`.
        track_id: String,
    },
    /// Set one persistent track flag.
    SetTrackFlag {
        /// Existing `track:<id>`.
        track_id: String,
        /// Picture visibility or sound mute.
        flag: ClipTrackFlag,
        /// Desired flag value.
        value: bool,
    },
    /// Add a timeline and make it active, as the native command does.
    AddTimeline,
    /// Remove a named timeline of the bound project, retaining the last one.
    RemoveTimeline {
        /// Existing `timeline:<id>`.
        timeline_id: String,
    },
    /// Rename a named timeline.
    RenameTimeline {
        /// Existing `timeline:<id>`.
        timeline_id: String,
        /// Nonempty tab label, up to 256 bytes.
        name: String,
    },
    /// Switch the active timeline by stable ID.
    SelectTimeline {
        /// Existing `timeline:<id>`.
        timeline_id: String,
    },
    /// Reorder a timeline tab; the native editor treats this as view state.
    MoveTimeline {
        /// Existing `timeline:<id>`.
        timeline_id: String,
        /// Final zero-based position; must exist in the current project.
        index: usize,
    },
    /// Set a named timeline's bounded output frame and rational frame rate.
    SetTimelineVideo {
        /// Existing `timeline:<id>`.
        timeline_id: String,
        /// Width, 16 through 8192 pixels.
        width: u32,
        /// Height, 16 through 8192 pixels.
        height: u32,
        /// Positive rate numerator, at most 240000.
        rate_num: i64,
        /// Positive denominator, at most 1001; rate at most 240 fps.
        rate_den: i64,
    },
    /// Move explicit clips atomically; an empty list never means all clips.
    Move {
        /// One through 128 unique active-timeline clips and destinations.
        moves: Vec<ClipDestination>,
    },
    /// Move one clip edge using the native in-point/keyframe semantics.
    Trim {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// Edge to move.
        edge: ClipTrimEdge,
        /// Signed timeline seconds; resulting span is validated before commit.
        delta: f64,
    },
    /// Split every named clip strictly inside its span.
    Split {
        /// Nonempty unique active-timeline clip IDs.
        clip_ids: Vec<String>,
        /// Shared timeline cut position in seconds.
        time: f64,
    },
    /// Delete only the explicit, nonempty list of active-timeline clips.
    Delete {
        /// Nonempty unique `clip:<id>` targets.
        clip_ids: Vec<String>,
    },
    /// Merge compatible, source-contiguous pieces through the native validator.
    Merge {
        /// Two through 128 unique `clip:<id>` targets.
        clip_ids: Vec<String>,
    },
    /// Insert a one-second image hold and ripple its lane. Video requires a
    /// future explicit-target native still-extraction helper and is refused.
    FreezeImage {
        /// Existing image `clip:<id>` whose media is already in this project.
        clip_id: String,
        /// Position strictly inside the clip.
        time: f64,
    },
    /// Set constant playback rate, retaining native source-span semantics.
    SetSpeed {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// Rate, 0.0625 through 16.
        speed: f64,
    },
    /// Set a bounded speed curve, or clear it without inventing a new rate.
    SetSpeedCurve {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// None clears; otherwise two through 64 strictly ordered points.
        points: Option<Vec<ClipSpeedPoint>>,
    },
    /// Set bounded picture placement through SetClipTransform.
    Transform {
        /// Existing visual `clip:<id>`.
        clip_id: String,
        /// At least one placement field.
        transform: ClipTransform,
    },
    /// Apply explicitly supported existing-clip fields.
    Update {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// Nonempty, typed property patch.
        properties: ClipProperties,
    },
    /// Select an existing audio stream of the clip's current media.
    SetAudioStream {
        /// Existing clip with playable audio.
        clip_id: String,
        /// Stream index from current bin metadata; None selects the default.
        stream: Option<u32>,
    },
    /// Detach a video's existing audio through the native command.
    DetachAudio {
        /// Existing unmuted video `clip:<id>` with audio.
        clip_id: String,
    },
    /// Reattach existing detached audio through the native command.
    ReattachAudio {
        /// Existing video or detached-audio `clip:<id>`.
        clip_id: String,
    },
    /// Add plain text on a named track as one native edit.
    AddText {
        /// Existing `track:<id>`.
        track_id: String,
        /// Timeline start in seconds.
        start: f64,
        /// Duration in seconds, at least 1/60.
        duration: f64,
        /// Nonempty content and optional title styling.
        text: ClipText,
    },
    /// Update an existing text clip; omitted style fields are preserved.
    SetText {
        /// Existing text `clip:<id>`.
        clip_id: String,
        /// Nonempty plain-text/style patch.
        text: ClipText,
    },
    /// Set one of the native clip property's keys.
    SetKey {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// Supported native property.
        property: ClipKeyProperty,
        /// Clip-relative position, 0 through 1.
        at: f64,
        /// Value within the property's normal bounds.
        value: f64,
        /// Cubic-bezier points, all 0 through 1.
        ease: [f64; 4],
    },
    /// Remove one explicit key, leaving other keys intact.
    ClearKey {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// Supported property.
        property: ClipKeyProperty,
        /// Clip-relative position, 0 through 1.
        at: f64,
    },
    /// Remove a named property's keys, leaving its constant value.
    ClearKeys {
        /// Existing `clip:<id>`.
        clip_id: String,
        /// Supported property.
        property: ClipKeyProperty,
    },
    /// Undo one step in the current native editor history. Refused until the
    /// host supplies a side-effect-free history preview and checked commit.
    Undo,
    /// Redo one step in the same history, with the same preview/budget checks.
    Redo,
}

/// Validate a canonical ID from the existing `project` result, returning its
/// native suffix. No path characters, empty IDs, aliases, or extra namespaces.
pub fn clip_native_id<'a>(id: &'a str, namespace: &str) -> Result<&'a str, &'static str> {
    let (prefix, suffix) = id.split_once(':').ok_or("invalidObjectId")?;
    if prefix != namespace
        || suffix.is_empty()
        || suffix.len() > 64
        || !suffix
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("invalidObjectId");
    }
    Ok(suffix)
}

fn range(value: f64, low: f64, high: f64) -> Result<(), &'static str> {
    if value.is_finite() && (low..=high).contains(&value) {
        Ok(())
    } else {
        Err("invalidRange")
    }
}
fn optional(value: Option<f64>, low: f64, high: f64) -> Result<(), &'static str> {
    value.map_or(Ok(()), |v| range(v, low, high))
}
fn label(value: &str, limit: usize) -> Result<(), &'static str> {
    if !value.trim().is_empty() && value.len() <= limit && !value.contains('\0') {
        Ok(())
    } else {
        Err("invalidText")
    }
}
fn ids(values: &[String], min: usize) -> Result<(), &'static str> {
    if !(min..=MAX_CLIP_TARGETS).contains(&values.len()) {
        return Err("invalidTargets");
    }
    let mut seen = std::collections::HashSet::new();
    for id in values {
        clip_native_id(id, "clip")?;
        if !seen.insert(id) {
            return Err("duplicateTarget");
        }
    }
    Ok(())
}
impl ClipText {
    fn validate(&self) -> Result<(), &'static str> {
        if *self == Self::default() {
            return Err("emptyPatch");
        }
        if let Some(v) = &self.content {
            label(v, 8192)?;
        }
        if let Some(v) = &self.font_family {
            label(v, 128)?;
            if v.contains(['/', '\\', ':']) {
                return Err("invalidFontFamily");
            }
        }
        if let Some(v) = &self.color
            && (v.len() != 7
                || !v.starts_with('#')
                || !v.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit))
        {
            return Err("invalidColor");
        }
        optional(self.font_size, 0.01, 1.0)?;
        optional(self.font_weight, 100.0, 900.0)?;
        optional(self.opacity, 0.0, 1.0)
    }
}
impl ClipEdit {
    /// Validate shape, finite ranges and target-list bounds before any UI work.
    /// Existence, active timeline, lane locks and history are checked by the host.
    pub fn validate(&self) -> Result<(), &'static str> {
        use ClipEdit::*;
        match self {
            ImportLibrary { asset_id, version } => {
                crate::validate_library_asset(asset_id, version)
                    .map_err(|_| "invalidLibraryVersion")?;
            }
            PlaceMedia {
                media_id,
                track_id,
                start,
            } => {
                clip_native_id(media_id, "media")?;
                clip_native_id(track_id, "track")?;
                range(*start, 0.0, MAX_CLIP_SECONDS)?;
            }
            RemoveTrack { track_id } | SetTrackFlag { track_id, .. } => {
                clip_native_id(track_id, "track")?;
            }
            RemoveTimeline { timeline_id }
            | SelectTimeline { timeline_id }
            | MoveTimeline { timeline_id, .. } => {
                clip_native_id(timeline_id, "timeline")?;
            }
            RenameTimeline { timeline_id, name } => {
                clip_native_id(timeline_id, "timeline")?;
                label(name, 256)?;
            }
            SetTimelineVideo {
                timeline_id,
                width,
                height,
                rate_num,
                rate_den,
            } => {
                clip_native_id(timeline_id, "timeline")?;
                if !(16..=8192).contains(width)
                    || !(16..=8192).contains(height)
                    || !(1..=240_000).contains(rate_num)
                    || !(1..=1001).contains(rate_den)
                {
                    return Err("invalidVideoSettings");
                }
                range(*rate_num as f64 / *rate_den as f64, 1.0, 240.0)?;
            }
            Move { moves } => {
                ids(
                    &moves.iter().map(|m| m.clip_id.clone()).collect::<Vec<_>>(),
                    1,
                )?;
                for m in moves {
                    clip_native_id(&m.track_id, "track")?;
                    range(m.start, 0.0, MAX_CLIP_SECONDS)?;
                }
            }
            Trim { clip_id, delta, .. } => {
                clip_native_id(clip_id, "clip")?;
                range(*delta, -MAX_CLIP_SECONDS, MAX_CLIP_SECONDS)?;
            }
            Split { clip_ids, time } => {
                ids(clip_ids, 1)?;
                range(*time, 0.0, MAX_CLIP_SECONDS)?;
            }
            Delete { clip_ids } => ids(clip_ids, 1)?,
            Merge { clip_ids } => ids(clip_ids, 2)?,
            FreezeImage { clip_id, time } => {
                clip_native_id(clip_id, "clip")?;
                range(*time, 0.0, MAX_CLIP_SECONDS)?;
            }
            SetSpeed { clip_id, speed } => {
                clip_native_id(clip_id, "clip")?;
                range(*speed, 0.0625, 16.0)?;
            }
            SetSpeedCurve { clip_id, points } => {
                clip_native_id(clip_id, "clip")?;
                if let Some(points) = points {
                    if !(2..=64).contains(&points.len()) {
                        return Err("invalidSpeedCurve");
                    }
                    for p in points {
                        range(p.at, 0.0, 1.0)?;
                        range(p.speed, 0.0625, 16.0)?;
                    }
                    if points.windows(2).any(|w| w[0].at >= w[1].at) {
                        return Err("invalidSpeedCurve");
                    }
                }
            }
            Transform {
                clip_id,
                transform: t,
            } => {
                clip_native_id(clip_id, "clip")?;
                if *t == ClipTransform::default() {
                    return Err("emptyPatch");
                }
                optional(t.scale, 0.05, 8.0)?;
                optional(t.offset_x, -3.0, 3.0)?;
                optional(t.offset_y, -3.0, 3.0)?;
                optional(t.rotation, -180.0, 180.0)?;
                optional(t.stretch_x, 0.1, 10.0)?;
                optional(t.stretch_y, 0.1, 10.0)?;
            }
            Update {
                clip_id,
                properties: p,
            } => {
                clip_native_id(clip_id, "clip")?;
                if *p == ClipProperties::default() {
                    return Err("emptyPatch");
                }
                if let Some(v) = &p.name {
                    label(v, 256)?;
                }
                optional(p.volume, 0.0, 16.0)?;
                optional(p.fade_in, 0.0, MAX_CLIP_SECONDS)?;
                optional(p.fade_out, 0.0, MAX_CLIP_SECONDS)?;
                optional(p.opacity, 0.0, 1.0)?;
            }
            SetAudioStream { clip_id, .. }
            | DetachAudio { clip_id }
            | ReattachAudio { clip_id }
            | ClearKeys { clip_id, .. } => {
                clip_native_id(clip_id, "clip")?;
            }
            AddText {
                track_id,
                start,
                duration,
                text,
            } => {
                clip_native_id(track_id, "track")?;
                range(*start, 0.0, MAX_CLIP_SECONDS)?;
                range(*duration, 1.0 / 60.0, MAX_CLIP_SECONDS)?;
                text.validate()?;
                label(text.content.as_deref().ok_or("missingText")?, 8192)?;
            }
            SetText { clip_id, text } => {
                clip_native_id(clip_id, "clip")?;
                text.validate()?;
            }
            SetKey {
                clip_id,
                property,
                at,
                value,
                ease,
            } => {
                clip_native_id(clip_id, "clip")?;
                range(*at, 0.0, 1.0)?;
                let (low, high) = match property {
                    ClipKeyProperty::Scale => (0.05, 8.0),
                    ClipKeyProperty::OffsetX | ClipKeyProperty::OffsetY => (-3.0, 3.0),
                    ClipKeyProperty::Rotation => (-180.0, 180.0),
                    ClipKeyProperty::Opacity => (0.0, 1.0),
                    ClipKeyProperty::Volume => (0.0, 16.0),
                };
                range(*value, low, high)?;
                for v in ease {
                    range(*v, 0.0, 1.0)?;
                }
            }
            ClearKey { clip_id, at, .. } => {
                clip_native_id(clip_id, "clip")?;
                range(*at, 0.0, 1.0)?;
            }
            AddTrack | AddTimeline | Undo | Redo => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_fields_paths_and_implicit_targets_are_refused() {
        assert!(serde_json::from_value::<ClipEdit>(json!({"kind":"placeMedia","mediaId":"media:m1","trackId":"track:T1","start":0,"path":"/private/input.mov"})).is_err());
        assert!(ClipEdit::Delete { clip_ids: vec![] }.validate().is_err());
        assert!(clip_native_id("clip:../c1", "clip").is_err());
        assert!(clip_native_id("track:T1", "clip").is_err());
        assert!(
            ClipEdit::Delete {
                clip_ids: vec!["clip:c1".into(), "clip:c1".into()]
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn finite_ranges_and_nonempty_patches_are_enforced() {
        assert!(
            ClipEdit::SetSpeed {
                clip_id: "clip:c1".into(),
                speed: f64::NAN
            }
            .validate()
            .is_err()
        );
        assert!(
            ClipEdit::Transform {
                clip_id: "clip:c1".into(),
                transform: ClipTransform::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ClipEdit::SetSpeedCurve {
                clip_id: "clip:c1".into(),
                points: Some(vec![
                    ClipSpeedPoint {
                        at: 0.5,
                        speed: 1.0
                    },
                    ClipSpeedPoint {
                        at: 0.5,
                        speed: 2.0
                    }
                ])
            }
            .validate()
            .is_err()
        );
        assert!(
            ClipEdit::Update {
                clip_id: "clip:c1".into(),
                properties: ClipProperties {
                    muted: Some(false),
                    ..Default::default()
                }
            }
            .validate()
            .is_ok()
        );
    }
}
