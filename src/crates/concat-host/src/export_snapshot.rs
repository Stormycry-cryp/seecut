// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Bounded, owned copies of the known path inputs of one native export.
//!
//! Rendering opts into the engine's existing strict local input policy.
//! This does not independently verify the output, guarantee retention of
//! every declared container track, or implement an Agent operation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use concat_effects::{Catalogue, manifest::Kind as EffectKind};
use concat_export::{ExportRequest, Reporter, flatten::flatten_timeline_in_with_catalogue};
use concat_project::model::{AppliedFilter, ClipKind, Project, Timeline};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::export::{ExportSpec, Progress};
use crate::titles::Titles;

/// Worker resource limits. Empty directories also consume an entry slot.
#[derive(Clone, Copy, Debug)]
pub struct FreezeLimits {
    /// Maximum copied file bytes in total.
    pub max_bytes: u64,
    /// Maximum regular files.
    pub max_files: usize,
    /// Maximum files plus directories, including absent-mask placeholders.
    pub max_entries: usize,
    /// Maximum directory depth below the owned job root.
    pub max_depth: usize,
    /// Maximum bytes in any source or staged path.
    pub max_path_bytes: usize,
    /// Maximum bytes in one path component.
    pub max_component_bytes: usize,
    /// Maximum selected clips, including hidden, muted and text clips.
    pub max_clips: usize,
    /// Maximum streaming JSON encoding size of the original Project.
    pub max_project_bytes: u64,
    /// Maximum JSON manifest size, enforced while entries are added too.
    pub max_manifest_bytes: u64,
    /// Maximum output width or height.
    pub max_dimension: u32,
    /// Maximum output pixel count.
    pub max_pixels: u64,
    /// Maximum positive frame rate.
    pub max_fps: f64,
    /// Maximum selected timeline end in seconds.
    pub max_seconds: f64,
    /// Maximum output frame count.
    pub max_frames: u64,
}

impl Default for FreezeLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024 * 1024,
            max_files: 10_000,
            max_entries: 10_000,
            max_depth: 16,
            max_path_bytes: 4096,
            max_component_bytes: 255,
            max_clips: 100_000,
            max_project_bytes: 16 * 1024 * 1024,
            max_manifest_bytes: 16 * 1024 * 1024,
            max_dimension: 8192,
            max_pixels: 16_777_216,
            max_fps: 120.0,
            max_seconds: 4.0 * 60.0 * 60.0,
            max_frames: 864_000,
        }
    }
}

/// Why a known file is used. One original file can have several uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum InputKind {
    /// A source picture or sound file.
    Media,
    /// A PNG actually returned by the title painter.
    Title,
    /// A file or directory within a complete mask tree.
    Mask,
}

/// The staged shape of one manifest entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum InputShape {
    /// An independently reopened and hashed regular file.
    File {
        /// Actual copied byte count.
        bytes: u64,
        /// SHA256 of actual copied bytes.
        sha256: String,
    },
    /// An ordinary directory, including empty ones.
    Directory {
        /// Whether this directory had no entries when copied.
        empty: bool,
    },
    /// The source mask directory was absent; the staged directory is empty.
    Absent,
}

/// One known input. Source paths are internal audit data, not model receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InputEntry {
    /// Uses of this entry.
    pub kinds: Vec<InputKind>,
    /// Original native path, retained only for internal audit.
    pub source: String,
    /// Application-generated path relative to the owned job root.
    pub relative_path: String,
    /// Expected file bytes or directory state.
    pub shape: InputShape,
}

/// The bounded, sorted manifest of enumerated inputs.
#[derive(Clone, Debug, Serialize)]
pub struct InputManifest {
    /// Exact selected timeline, with no fallback.
    pub timeline_id: String,
    /// Streaming digest of the original Project encoding.
    pub project_sha256: String,
    /// Original Project encoding byte count.
    pub project_bytes: u64,
    /// Sorted files, empty directories and absent-mask placeholders.
    pub entries: Vec<InputEntry>,
    /// Actual regular-file bytes across all copies.
    pub total_bytes: u64,
    /// Number of copied regular files.
    pub file_count: usize,
}

/// Proof only of the known copied input bytes at the verification boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputProof {
    /// Original Project digest, separate from the file manifest digest.
    pub project_sha256: String,
    /// SHA256 binding the full sorted input manifest.
    pub manifest_sha256: String,
    /// Actual copied file bytes.
    pub total_bytes: u64,
    /// Actual copied regular files.
    pub file_count: usize,
}

/// Engine render result plus input proof; no independent output verification.
#[derive(Debug)]
pub struct SnapshotRender {
    /// Ordinary path returned by the existing render engine.
    pub path: String,
    /// Known input verification, not a delivered-output receipt.
    pub inputs: InputProof,
}

/// Closed internal render outcomes. An error never promises that no output
/// file exists; published-but-unverified outputs retain their actual path.
#[derive(Debug)]
pub enum SnapshotRenderError {
    /// Input verification failed before entering the render engine.
    InputsInvalid {
        /// The input verification error.
        error: String,
    },
    /// The engine returned an error. This does not assert output absence.
    EngineFailed {
        /// The original engine error, preserved even if post-checks fail.
        error: String,
        /// The actual post-render known-input verification result.
        input_verification: Result<InputProof, String>,
    },
    /// The engine published a path, but the input post-check failed/cancelled.
    PublishedInputsUnverified {
        /// The exact path the engine returned; no output proof is implied.
        path: String,
        /// The input post-check error, including cancellation.
        error: String,
    },
}

impl std::fmt::Display for SnapshotRenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputsInvalid { error } => {
                write!(formatter, "export inputs invalid before render: {error}")
            }
            Self::EngineFailed {
                error,
                input_verification,
            } => {
                write!(formatter, "export engine failed: {error}")?;
                if let Err(after) = input_verification {
                    write!(formatter, "; input post-check: {after}")?;
                }
                Ok(())
            }
            Self::PublishedInputsUnverified { path, error } => write!(
                formatter,
                "export engine published {path}; input post-check failed: {error}"
            ),
        }
    }
}
impl std::error::Error for SnapshotRenderError {}

/// A private request, one captured effect table and an exclusively owned job.
/// Dropping it removes only its still-identical owned directory.
pub struct FrozenExport {
    request: ExportRequest,
    catalogue: Catalogue,
    stage: OwnedStage,
    manifest: InputManifest,
    manifest_sha256: String,
    stamps: BTreeMap<String, Stamp>,
    limits: FreezeLimits,
}

impl FrozenExport {
    /// Paints titles and copies every known input of exactly `timeline_id`.
    /// Call from a worker with trusted native objects and a managed parent.
    pub fn prepare(
        project: &Project,
        timeline_id: &str,
        project_dir: Option<&Path>,
        spec: &ExportSpec,
        titles: &Titles,
        catalogue: Catalogue,
        staging_parent: &Path,
        limits: FreezeLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        Self::prepare_inner(
            project,
            timeline_id,
            project_dir,
            spec,
            titles,
            catalogue,
            staging_parent,
            limits,
            cancel,
            &mut |_| {},
        )
    }

    // A private chunk checkpoint also lets tests deterministically mutate
    // a source or cancel mid-copy. The public API always supplies a no-op.
    fn prepare_inner(
        project: &Project,
        timeline_id: &str,
        project_dir: Option<&Path>,
        spec: &ExportSpec,
        titles: &Titles,
        catalogue: Catalogue,
        staging_parent: &Path,
        limits: FreezeLimits,
        cancel: &AtomicBool,
        checkpoint: &mut dyn FnMut(&Path),
    ) -> Result<Self, String> {
        cancelled(cancel)?;
        let (project_bytes, project_sha256) =
            json_digest(project, limits.max_project_bytes, cancel)?;
        let timeline = validate_project(project, timeline_id, &catalogue, &limits, cancel)?;
        let stage = OwnedStage::create(staging_parent, &limits)?;
        reject_output_in_stage(&spec.output, &stage.path)?;
        let mut builder = Builder {
            stage,
            manifest: InputManifest {
                timeline_id: timeline_id.to_owned(),
                project_sha256,
                project_bytes,
                entries: Vec::new(),
                total_bytes: 0,
                file_count: 0,
            },
            stamps: BTreeMap::new(),
            files: HashMap::new(),
            masks: HashMap::new(),
            manifest_budget: 0,
            limits,
            cancel,
            checkpoint,
        };
        builder.manifest_budget =
            json_digest(&builder.manifest, limits.max_manifest_bytes, cancel)?.0;
        let video = timeline.video;
        let mut selected = project.clone();
        selected.active_timeline_id = timeline_id.to_owned();
        let expected: HashSet<&str> = timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Text)
            .map(|clip| clip.id.as_str())
            .collect();
        let mut clips =
            flatten_timeline_in_with_catalogue(project, Some(timeline_id), project_dir, &catalogue);
        let non_text = timeline.clips.len() - expected.len();
        if clips.len() != non_text {
            return Err("flatten omitted a selected non-text clip".to_owned());
        }
        for clip in &mut clips {
            cancelled(cancel)?;
            if clip.kind != concat_export::ClipKind::Layer {
                clip.path = builder.file(Path::new(&clip.path), InputKind::Media)?;
            }
            if !clip.mask_dir.is_empty() {
                clip.mask_dir = builder.mask(Path::new(&clip.mask_dir))?;
            }
        }
        let mut actual = HashSet::new();
        for source in timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Text)
        {
            cancelled(cancel)?;
            // Paint/copy one title at a time, so limits and cancellation are
            // checked before the next title can add more cache output.
            selected.active_mut().clips.clear();
            selected
                .active_mut()
                .clips
                .push(std::sync::Arc::clone(source));
            let mut painted = titles.clips(&selected, video.width, video.height);
            cancelled(cancel)?;
            if painted.len() != 1 {
                return Err(
                    "title painter omitted a selected text clip or returned extra IDs".to_owned(),
                );
            }
            let mut title = painted
                .pop()
                .ok_or_else(|| "title painter omitted a selected text clip".to_owned())?;
            if title.clip_id != source.id || !actual.insert(title.clip_id.clone()) {
                return Err("title painter returned a duplicate or unexpected clip ID".to_owned());
            }
            title.clip.effects = source.video_effects.clone();
            title.clip.video_filter_chain = catalogue.video_chain(&source.video_effects);
            title.clip.path = builder.file(Path::new(&title.clip.path), InputKind::Title)?;
            clips.push(title.clip);
        }
        if actual.iter().map(String::as_str).collect::<HashSet<_>>() != expected {
            return Err("title painter ID set differs from selected text IDs".to_owned());
        }
        builder
            .manifest
            .entries
            .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        let (_, manifest_sha256) =
            json_digest(&builder.manifest, limits.max_manifest_bytes, cancel)?;
        let frozen = Self {
            request: ExportRequest {
                output: spec.output.clone(),
                width: video.width,
                height: video.height,
                rate_num: video.rate_num,
                rate_den: video.rate_den,
                crf: spec.crf,
                preset: spec.preset.clone(),
                codec: spec.codec,
                ten_bit: spec.ten_bit,
                clips,
            },
            catalogue,
            stage: builder.stage,
            manifest: builder.manifest,
            manifest_sha256,
            stamps: builder.stamps,
            limits,
        };
        frozen.verify_inputs(cancel)?;
        Ok(frozen)
    }

    /// Read-only staged request. Its known input paths all belong to this job.
    pub fn request(&self) -> &ExportRequest {
        &self.request
    }

    /// Read-only internal audit manifest, including original source paths.
    pub fn manifest(&self) -> &InputManifest {
        &self.manifest
    }

    /// Digest of the full sorted manifest, not just the source Project.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    /// Reopens the whole staged tree, rejecting changes, extra entries or aliases.
    pub fn verify_inputs(&self, cancel: &AtomicBool) -> Result<InputProof, String> {
        cancelled(cancel)?;
        self.stage.verify_root()?;
        let expected: BTreeMap<&str, &InputEntry> = self
            .manifest
            .entries
            .iter()
            .map(|entry| (entry.relative_path.as_str(), entry))
            .collect();
        let mut seen = 0;
        self.verify_directory(&self.stage.path, "", 0, &expected, &mut seen, cancel)?;
        if seen != expected.len() {
            return Err("staged input disappeared".to_owned());
        }
        self.stage.verify_root()?;
        let (_, digest) = json_digest(&self.manifest, self.limits.max_manifest_bytes, cancel)?;
        if digest != self.manifest_sha256 {
            return Err("input manifest changed".to_owned());
        }
        Ok(InputProof {
            project_sha256: self.manifest.project_sha256.clone(),
            manifest_sha256: self.manifest_sha256.clone(),
            total_bytes: self.manifest.total_bytes,
            file_count: self.manifest.file_count,
        })
    }

    fn verify_directory(
        &self,
        dir: &Path,
        relative: &str,
        depth: usize,
        expected: &BTreeMap<&str, &InputEntry>,
        seen: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(io_error)? {
            cancelled(cancel)?;
            let entry = entry.map_err(io_error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF8 staged entry".to_owned())?;
            let rel = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            *seen += 1;
            if *seen > self.limits.max_entries {
                return Err("staged entry limit exceeded".to_owned());
            }
            let record = expected
                .get(rel.as_str())
                .ok_or_else(|| format!("unexpected staged input: {rel}"))?;
            let path = entry.path();
            validate_path(&path, &self.limits)?;
            let stamp = plain_stamp(&path)?;
            if self.stamps.get(&rel) != Some(&stamp) {
                return Err(format!("staged input replaced or changed: {rel}"));
            }
            match &record.shape {
                InputShape::File { bytes, sha256 } => {
                    let (count, digest, _) = hash_file(&path, *bytes, cancel)?;
                    if count != *bytes || &digest != sha256 {
                        return Err(format!("staged bytes changed: {rel}"));
                    }
                }
                InputShape::Directory { empty } => {
                    if !stamp.directory || depth + 1 > self.limits.max_depth {
                        return Err("staged directory depth/type mismatch".to_owned());
                    }
                    let is_empty = fs::read_dir(&path).map_err(io_error)?.next().is_none();
                    if is_empty != *empty {
                        return Err(format!("staged directory shape changed: {rel}"));
                    }
                    self.verify_directory(&path, &rel, depth + 1, expected, seen, cancel)?;
                }
                InputShape::Absent => {
                    if !stamp.directory || fs::read_dir(&path).map_err(io_error)?.next().is_some() {
                        return Err(format!("absent mask placeholder changed: {rel}"));
                    }
                }
            }
            if plain_stamp(&path)? != stamp {
                return Err(format!("staged input changed while verifying: {rel}"));
            }
        }
        Ok(())
    }

    /// Verifies known inputs around a strict local engine render.
    /// A post-render verification error can leave the published output intact.
    pub fn render(
        &self,
        cancel: &AtomicBool,
        progress: impl FnMut(Progress),
    ) -> Result<SnapshotRender, SnapshotRenderError> {
        self.render_before_publish(cancel, progress, |_| Ok(()))
    }

    /// Runs a trusted native check on the actual finished temporary output,
    /// after one last input verification and before exclusive publication.
    /// The callback runs at most once; rejection prevents publication and the engine
    /// cleans its temporary output. This hook itself grants no permission.
    /// After callback success the engine does not check cancellation again;
    /// native callers must arbitrate cancellation against their own publish
    /// claim in the callback. Post-render input errors still retain the path.
    pub fn render_before_publish(
        &self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(Progress),
        before_publish: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<SnapshotRender, SnapshotRenderError> {
        self.render_inner(cancel, &mut progress, &mut || {}, before_publish)
    }

    // A private boundary permits deterministic post-publication mutation in
    // tests. Native callers can only use the public no-op boundary.
    fn render_inner(
        &self,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
        after_render: &mut dyn FnMut(),
        before_publish: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<SnapshotRender, SnapshotRenderError> {
        self.verify_inputs(cancel)
            .map_err(|error| SnapshotRenderError::InputsInvalid { error })?;
        let engine_result = concat_export::render_with_catalogue_mode_before_publish(
            &self.request,
            &self.catalogue,
            concat_export::RenderMode::StrictLocal,
            Reporter {
                cancel,
                progress: &mut |frame, total, stage| {
                    progress(Progress {
                        frame,
                        total,
                        stage,
                    })
                },
            },
            |candidate| {
                self.verify_inputs(cancel)?;
                before_publish(candidate)
            },
        );
        after_render();
        let post_verify = self.verify_inputs(cancel);
        // Preserve both facts. In particular, an input post-check never
        // hides a published engine path or replaces the engine's own error.
        match (engine_result, post_verify) {
            (Ok(path), Ok(inputs)) => Ok(SnapshotRender { path, inputs }),
            (Ok(path), Err(error)) => {
                Err(SnapshotRenderError::PublishedInputsUnverified { path, error })
            }
            (Err(error), input_verification) => Err(SnapshotRenderError::EngineFailed {
                error,
                input_verification,
            }),
        }
    }
}

fn validate_project<'a>(
    project: &'a Project,
    id: &str,
    catalogue: &Catalogue,
    limits: &FreezeLimits,
    cancel: &AtomicBool,
) -> Result<&'a Timeline, String> {
    let mut matches = project
        .timelines
        .iter()
        .filter(|timeline| timeline.id == id);
    let timeline = matches
        .next()
        .ok_or_else(|| "selected timeline does not exist".to_owned())?;
    if matches.next().is_some() {
        return Err("selected timeline ID is ambiguous".to_owned());
    }
    if timeline.clips.is_empty() || timeline.clips.len() > limits.max_clips {
        return Err("selected clip count is empty or exceeds limit".to_owned());
    }
    let video = timeline.video;
    let fps = video.rate_num as f64 / video.rate_den as f64;
    if video.width == 0
        || video.height == 0
        || video.width > limits.max_dimension
        || video.height > limits.max_dimension
        || u64::from(video.width) * u64::from(video.height) > limits.max_pixels
        || video.rate_num <= 0
        || video.rate_den <= 0
        || !fps.is_finite()
        || fps <= 0.0
        || fps > limits.max_fps
    {
        return Err("selected output format exceeds limits".to_owned());
    }
    // The Project encoding was already bounded before these reference maps.
    // Avoid rescanning the entire media bin for every selected clip.
    let mut tracks = HashMap::<&str, usize>::new();
    for track in &timeline.tracks {
        cancelled(cancel)?;
        *tracks.entry(track.id.as_str()).or_default() += 1;
    }
    let mut media_by_id = HashMap::new();
    for media in &project.media {
        cancelled(cancel)?;
        let entry = media_by_id
            .entry(media.id.as_str())
            .or_insert((media, 0_usize));
        entry.1 += 1;
    }
    let mut ids = HashSet::new();
    let mut end = 0.0_f64;
    for clip in &timeline.clips {
        cancelled(cancel)?;
        if !ids.insert(&clip.id) {
            return Err("duplicate selected clip ID".to_owned());
        }
        if tracks.get(clip.track_id.as_str()) != Some(&1) {
            return Err(format!(
                "selected clip has missing or ambiguous track: {}",
                clip.id
            ));
        }
        if !clip.start.is_finite()
            || clip.start < 0.0
            || !clip.duration.is_finite()
            || clip.duration < 0.0
        {
            return Err("selected clip timing is invalid".to_owned());
        }
        end = end.max(clip.start + clip.duration);
        validate_effects(&clip.filters, true, catalogue)?;
        validate_effects(&clip.video_effects, false, catalogue)?;
        if !matches!(clip.kind, ClipKind::Layer | ClipKind::Text) {
            let &(media, count) = media_by_id
                .get(clip.media_id.as_str())
                .ok_or_else(|| format!("selected clip has missing media: {}", clip.id))?;
            if count != 1 || media.path.is_empty() {
                return Err("selected media is ambiguous or has an empty path".to_owned());
            }
            validate_path(Path::new(&media.path), limits)?;
        }
    }
    if !end.is_finite()
        || end <= 0.0
        || end > limits.max_seconds
        || (end * fps).ceil() > limits.max_frames as f64
    {
        return Err("selected timeline duration/frame limit exceeded".to_owned());
    }
    Ok(timeline)
}

fn validate_effects(
    filters: &[AppliedFilter],
    audio: bool,
    catalogue: &Catalogue,
) -> Result<(), String> {
    for (index, link) in filters.iter().filter(|link| link.enabled).enumerate() {
        let package = catalogue
            .get(&link.id)
            .ok_or_else(|| format!("unknown required effect: {}", link.id))?;
        if package.folder.is_some() || package.manifest.lut.is_some() {
            return Err(format!(
                "external effect directory or LUT is not supported: {}",
                link.id
            ));
        }
        let correct_kind = if audio {
            package.kind() == EffectKind::Audio
        } else {
            matches!(package.kind(), EffectKind::Effect | EffectKind::Filter)
        };
        if !correct_kind {
            return Err(format!("required effect has the wrong kind: {}", link.id));
        }
        let fragment = package
            .ffmpeg_fragment(&link.params, index)
            .map_err(|error| format!("cannot construct required effect {}: {error}", link.id))?;
        if fragment
            .as_ref()
            .is_none_or(|fragment| fragment.trim().is_empty())
        {
            return Err(format!("required effect has no CPU backend: {}", link.id));
        }
    }
    Ok(())
}

struct Builder<'a> {
    stage: OwnedStage,
    manifest: InputManifest,
    stamps: BTreeMap<String, Stamp>,
    files: HashMap<PathBuf, usize>,
    masks: HashMap<PathBuf, String>,
    manifest_budget: u64,
    limits: FreezeLimits,
    cancel: &'a AtomicBool,
    checkpoint: &'a mut dyn FnMut(&Path),
}

impl Builder<'_> {
    fn record(&mut self, entry: InputEntry, stamp: Stamp) -> Result<(), String> {
        if self.manifest.entries.len() >= self.limits.max_entries {
            return Err("input entry limit exceeded".to_owned());
        }
        let cost = json_digest(&entry, self.limits.max_manifest_bytes, self.cancel)?.0 + 64;
        self.manifest_budget = self
            .manifest_budget
            .checked_add(cost)
            .ok_or_else(|| "manifest size overflow".to_owned())?;
        if self.manifest_budget > self.limits.max_manifest_bytes {
            return Err("input manifest size limit exceeded".to_owned());
        }
        self.stamps.insert(entry.relative_path.clone(), stamp);
        self.manifest.entries.push(entry);
        Ok(())
    }

    fn file(&mut self, source: &Path, kind: InputKind) -> Result<String, String> {
        validate_path(source, &self.limits)?;
        if let Some(&index) = self.files.get(source) {
            let entry = &mut self.manifest.entries[index];
            if !entry.kinds.contains(&kind) {
                entry.kinds.push(kind);
            }
            return Ok(self
                .stage
                .path
                .join(&entry.relative_path)
                .to_string_lossy()
                .into_owned());
        }
        let extension = safe_extension(source);
        let relative = format!("input-{:06}.{extension}", self.manifest.file_count);
        let index = self.manifest.entries.len();
        self.copy_file(source, &relative, kind)?;
        self.files.insert(source.to_path_buf(), index);
        Ok(self
            .stage
            .path
            .join(relative)
            .to_string_lossy()
            .into_owned())
    }

    fn copy_file(&mut self, source: &Path, relative: &str, kind: InputKind) -> Result<(), String> {
        cancelled(self.cancel)?;
        if self.manifest.file_count >= self.limits.max_files {
            return Err("input file count limit exceeded".to_owned());
        }
        // Reserve entry/manifest space before opening or allocating another file.
        if self.manifest.entries.len() >= self.limits.max_entries {
            return Err("input entry limit exceeded".to_owned());
        }
        validate_path(source, &self.limits)?;
        let target = self.stage.path.join(relative);
        validate_path(&target, &self.limits)?;
        let (mut input, before) = open_regular(source)?;
        let remaining = self
            .limits
            .max_bytes
            .saturating_sub(self.manifest.total_bytes);
        if before.len > remaining {
            return Err("input total byte limit exceeded".to_owned());
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(io_error)?;
        let mut hasher = Sha256::new();
        let mut count = 0_u64;
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            cancelled(self.cancel)?;
            let read = input.read(&mut chunk).map_err(io_error)?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .ok_or_else(|| "input byte overflow".to_owned())?;
            if count > remaining {
                return Err("input total byte limit exceeded while copying".to_owned());
            }
            output.write_all(&chunk[..read]).map_err(io_error)?;
            hasher.update(&chunk[..read]);
            (self.checkpoint)(source);
        }
        cancelled(self.cancel)?;
        output.flush().map_err(io_error)?;
        drop(output);
        verify_open_source(source, &input, &before)?;
        let digest = format!("{:x}", hasher.finalize());
        let (source_count, source_digest, source_stamp) =
            hash_file(source, remaining, self.cancel)?;
        if source_stamp != before || source_count != count || source_digest != digest {
            return Err("source changed while copying/reopening".to_owned());
        }
        let (copied_count, copied_digest, target_stamp) =
            hash_file(&target, remaining, self.cancel)?;
        if copied_count != count || copied_digest != digest {
            return Err("copied input digest mismatch".to_owned());
        }
        self.record(
            InputEntry {
                kinds: vec![kind],
                source: path_text(source)?,
                relative_path: relative.to_owned(),
                shape: InputShape::File {
                    bytes: count,
                    sha256: digest,
                },
            },
            target_stamp,
        )?;
        self.manifest.file_count += 1;
        self.manifest.total_bytes += count;
        Ok(())
    }

    fn mask(&mut self, source: &Path) -> Result<String, String> {
        validate_path(source, &self.limits)?;
        if let Some(relative) = self.masks.get(source) {
            return Ok(self
                .stage
                .path
                .join(relative)
                .to_string_lossy()
                .into_owned());
        }
        let relative = format!("mask-{:06}", self.masks.len());
        self.copy_directory(source, &relative, 1, true)?;
        self.masks.insert(source.to_path_buf(), relative.clone());
        Ok(self
            .stage
            .path
            .join(relative)
            .to_string_lossy()
            .into_owned())
    }

    fn copy_directory(
        &mut self,
        source: &Path,
        relative: &str,
        depth: usize,
        allow_absent: bool,
    ) -> Result<(), String> {
        cancelled(self.cancel)?;
        if depth > self.limits.max_depth {
            return Err("input directory depth limit exceeded".to_owned());
        }
        if self.manifest.entries.len() >= self.limits.max_entries {
            return Err("input entry limit exceeded".to_owned());
        }
        validate_path(source, &self.limits)?;
        let target = self.stage.path.join(relative);
        validate_path(&target, &self.limits)?;
        let before = match fs::symlink_metadata(source) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Some(Stamp::of(&meta)),
            Ok(_) => return Err("mask source is not a plain directory".to_owned()),
            Err(error) if allow_absent && error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error(error)),
        };
        fs::create_dir(&target).map_err(io_error)?;
        let index = self.manifest.entries.len();
        self.record(
            InputEntry {
                kinds: vec![InputKind::Mask],
                source: path_text(source)?,
                relative_path: relative.to_owned(),
                shape: if before.is_some() {
                    InputShape::Directory { empty: true }
                } else {
                    InputShape::Absent
                },
            },
            plain_stamp(&target)?,
        )?;
        if let Some(before) = before {
            let mut empty = true;
            for entry in fs::read_dir(source).map_err(io_error)? {
                cancelled(self.cancel)?;
                let entry = entry.map_err(io_error)?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "non-UTF8 mask component".to_owned())?;
                let child = entry.path();
                validate_path(&child, &self.limits)?;
                let child_relative = format!("{relative}/{name}");
                let meta = fs::symlink_metadata(&child).map_err(io_error)?;
                if meta.file_type().is_symlink() {
                    return Err("mask tree contains a symlink".to_owned());
                }
                if meta.is_dir() {
                    self.copy_directory(&child, &child_relative, depth + 1, false)?;
                } else if meta.is_file() {
                    self.copy_file(&child, &child_relative, InputKind::Mask)?;
                } else {
                    return Err("mask tree contains a special file".to_owned());
                }
                empty = false;
            }
            if plain_stamp(source)? != before {
                return Err("mask source directory changed while copying".to_owned());
            }
            self.manifest.entries[index].shape = InputShape::Directory { empty };
            self.stamps
                .insert(relative.to_owned(), plain_stamp(&target)?);
        } else if fs::symlink_metadata(source).is_ok() {
            return Err("absent source mask appeared while preparing".to_owned());
        }
        Ok(())
    }
}

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

struct OwnedStage {
    path: PathBuf,
    identity: Stamp,
}

impl OwnedStage {
    fn create(parent: &Path, limits: &FreezeLimits) -> Result<Self, String> {
        let parent = fs::canonicalize(parent).map_err(io_error)?;
        validate_path(&parent, limits)?;
        let parent_stamp = plain_stamp(&parent)?;
        if !parent_stamp.directory {
            return Err("staging parent is not a directory".to_owned());
        }
        for _ in 0..100 {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let instant = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos();
            let path = parent.join(format!(
                ".clip-inputs-{}-{instant}-{sequence}",
                std::process::id()
            ));
            validate_path(&path, limits)?;
            match fs::create_dir(&path) {
                Ok(()) => {
                    let stage = Self {
                        identity: plain_stamp(&path)?,
                        path,
                    };
                    if !plain_stamp(&parent)?.same_object(&parent_stamp) {
                        return Err("staging parent was replaced".to_owned());
                    }
                    return Ok(stage);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        Err("could not allocate an exclusive staging directory".to_owned())
    }

    fn verify_root(&self) -> Result<(), String> {
        let current = plain_stamp(&self.path)?;
        if !current.directory || !current.same_object(&self.identity) {
            return Err("owned staging root was replaced".to_owned());
        }
        Ok(())
    }
}

impl Drop for OwnedStage {
    fn drop(&mut self) {
        if self.verify_root().is_ok() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    directory: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            directory: meta.is_dir(),
            #[cfg(unix)]
            device: meta.dev(),
            #[cfg(unix)]
            inode: meta.ino(),
            #[cfg(unix)]
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
    fn same_object(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device
                && self.inode == other.inode
                && self.directory == other.directory
        }
        #[cfg(not(unix))]
        {
            self.created.is_some()
                && self.created == other.created
                && self.directory == other.directory
        }
    }
}

fn plain_stamp(path: &Path) -> Result<Stamp, String> {
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if meta.file_type().is_symlink() || !(meta.is_dir() || meta.is_file()) {
        return Err(format!(
            "input is a symlink or special file: {}",
            path.display()
        ));
    }
    Ok(Stamp::of(&meta))
}

fn open_regular(path: &Path) -> Result<(File, Stamp), String> {
    let before = plain_stamp(path)?;
    if before.directory {
        return Err("expected a regular input file".to_owned());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    // No new dependency: these are the platform's stable open(2) constants.
    // Nonblocking also prevents a FIFO substitution from blocking this worker.
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0100 | 0x0004);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000 | 0x0800);
    }
    let file = options.open(path).map_err(io_error)?;
    verify_open_source(path, &file, &before)?;
    Ok((file, before))
}

fn verify_open_source(path: &Path, file: &File, before: &Stamp) -> Result<(), String> {
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() || Stamp::of(&meta) != *before || plain_stamp(path)? != *before {
        return Err(format!(
            "source changed or was replaced: {}",
            path.display()
        ));
    }
    Ok(())
}

fn hash_file(path: &Path, limit: u64, cancel: &AtomicBool) -> Result<(u64, String, Stamp), String> {
    cancelled(cancel)?;
    let (mut file, before) = open_regular(path)?;
    if before.len > limit {
        return Err("file byte limit exceeded".to_owned());
    }
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let read = file.read(&mut chunk).map_err(io_error)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or_else(|| "file size overflow".to_owned())?;
        if count > limit {
            return Err("file grew beyond byte limit".to_owned());
        }
        hasher.update(&chunk[..read]);
    }
    cancelled(cancel)?;
    verify_open_source(path, &file, &before)?;
    Ok((count, format!("{:x}", hasher.finalize()), before))
}

struct JsonDigest<'a> {
    hasher: Sha256,
    bytes: u64,
    limit: u64,
    cancel: &'a AtomicBool,
}
impl Write for JsonDigest<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("input preparation cancelled"));
        }
        let count = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("JSON size overflow"))?;
        if count > self.limit {
            return Err(io::Error::other("JSON byte limit exceeded"));
        }
        self.hasher.update(bytes);
        self.bytes = count;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_digest(
    value: &impl Serialize,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<(u64, String), String> {
    let mut writer = JsonDigest {
        hasher: Sha256::new(),
        bytes: 0,
        limit,
        cancel,
    };
    serde_json::to_writer(&mut writer, value).map_err(|error| error.to_string())?;
    Ok((writer.bytes, format!("{:x}", writer.hasher.finalize())))
}

fn validate_path(path: &Path, limits: &FreezeLimits) -> Result<(), String> {
    if path.as_os_str().len() > limits.max_path_bytes {
        return Err("input path byte limit exceeded".to_owned());
    }
    for component in path.components() {
        if component == Component::ParentDir {
            return Err("input path traversal is not supported".to_owned());
        }
        if component.as_os_str().len() > limits.max_component_bytes {
            return Err("input component byte limit exceeded".to_owned());
        }
    }
    path_text(path)?;
    Ok(())
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "input path is not UTF8".to_owned())
}

fn safe_extension(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi" | "wav" | "mp3" | "m4a" | "aac" | "flac"
        | "ogg" | "opus" | "aif" | "aiff" | "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif"
        | "tiff" | "gif" => extension,
        _ => "bin".to_owned(),
    }
}

fn reject_output_in_stage(output: &str, stage: &Path) -> Result<(), String> {
    let output = Path::new(output);
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(io_error)?;
    if parent.starts_with(stage) {
        return Err("output may not be inside the input staging directory".to_owned());
    }
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("input preparation cancelled".to_owned())
    } else {
        Ok(())
    }
}
fn io_error(error: io::Error) -> String {
    format!("export input I/O: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirs::AppDirs;
    use concat_core::{frame::Frame, time::FrameRate};
    use concat_media::{
        AudioDecoder, AudioOptions, DecodeOptions, Decoder, EncodeOptions, Encoder, FrameSink,
        FrameSource, SampleFormat,
    };
    use concat_project::commands::{ClipPatch, NewMedia};
    use concat_project::model::{Cutout, CutoutMode, MediaKind, Subject, TextStyle, VideoSettings};
    use concat_project::{Command, Editor};
    use std::sync::Arc;

    struct Fixture {
        _root: OwnedStage,
        parent: PathBuf,
        project_dir: PathBuf,
        source: PathBuf,
        editor: Editor,
        titles: Titles,
        dirs: AppDirs,
    }

    fn add_media(
        editor: &mut Editor,
        path: &Path,
        kind: MediaKind,
        has_audio: bool,
        track: usize,
    ) -> String {
        let media_id = editor
            .apply(Command::AddMedia {
                item: NewMedia {
                    path: path.to_string_lossy().into_owned(),
                    name: "fixture".into(),
                    duration: Some(1.0),
                    kind,
                    width: (kind != MediaKind::Audio).then_some(64),
                    height: (kind != MediaKind::Audio).then_some(64),
                    frame_rate: Some(30.0),
                    frame_rate_fraction: Some("30/1".into()),
                    video_codec: None,
                    audio_codec: None,
                    has_audio,
                    audio_tracks: Vec::new(),
                    extra: Default::default(),
                },
            })
            .unwrap()
            .created_id
            .unwrap();
        let track_id = editor.project().active().tracks[track].id.clone();
        editor
            .apply(Command::AddClip {
                media_id,
                track_id,
                start: 0.0,
            })
            .unwrap()
            .created_id
            .unwrap()
    }

    impl Fixture {
        fn new() -> Self {
            let root = OwnedStage::create(&std::env::temp_dir(), &FreezeLimits::default()).unwrap();
            let parent = root.path.join("jobs");
            fs::create_dir(&parent).unwrap();
            let project_dir = root.path.join("project");
            fs::create_dir(&project_dir).unwrap();
            let source = root.path.join("source.mp4");
            fs::write(&source, vec![19; 128 * 1024]).unwrap();
            let dirs = AppDirs {
                config: root.path.join("config"),
                data: root.path.join("data"),
            };
            let titles = Titles::new(&dirs);
            let mut editor = Editor::with_video(VideoSettings {
                width: 64,
                height: 64,
                rate_num: 30,
                rate_den: 1,
            });
            add_media(&mut editor, &source, MediaKind::Video, false, 0);
            Self {
                _root: root,
                parent,
                project_dir,
                source,
                editor,
                titles,
                dirs,
            }
        }
        fn spec(&self) -> ExportSpec {
            ExportSpec {
                output: self
                    ._root
                    .path
                    .join("output.mp4")
                    .to_string_lossy()
                    .into_owned(),
                crf: 18,
                preset: "ultrafast".into(),
                ..ExportSpec::default()
            }
        }
        fn prepare(
            &self,
            project: &Project,
            catalogue: Catalogue,
            limits: FreezeLimits,
            cancel: &AtomicBool,
        ) -> Result<FrozenExport, String> {
            FrozenExport::prepare(
                project,
                &project.active_timeline_id,
                Some(&self.project_dir),
                &self.spec(),
                &self.titles,
                catalogue,
                &self.parent,
                limits,
                cancel,
            )
        }
        fn add_title(&mut self) -> String {
            let track_id = Some(self.editor.project().active().tracks[2].id.clone());
            self.editor
                .apply(Command::AddTextClip {
                    track_id,
                    start: 0.0,
                    style: Some(TextStyle {
                        content: "FROZEN".into(),
                        font_size: 0.25,
                        color: "#00ff00".into(),
                        shadow: false,
                        ..TextStyle::default()
                    }),
                    duration: Some(1.0),
                    offset_y: None,
                })
                .unwrap()
                .created_id
                .unwrap()
        }
        fn cutout_project(&self) -> (Project, PathBuf) {
            let mut project = self.editor.project().clone();
            let clip = project.active_mut().clip_at_mut(0);
            clip.cutout = Some(Cutout {
                mode: CutoutMode::Auto,
                subject: Subject::Person,
                feather: 0.0,
                strokes: Vec::new(),
            });
            let mask =
                concat_vision::mask_dir(&self.project_dir, &clip_path(&project), Subject::Person);
            (project, mask)
        }
        fn jobs(&self) -> usize {
            fs::read_dir(&self.parent).unwrap().count()
        }
    }

    fn clip_path(project: &Project) -> String {
        project.media[0].path.clone()
    }
    fn error(result: Result<FrozenExport, String>) -> String {
        result.err().expect("preparation must fail")
    }
    fn fresh_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn in_memory_catalogue() -> Catalogue {
        let mut result = Catalogue::new();
        for (id, kind, chain, alias) in [
            (
                "snapshot-test.negate",
                "effect",
                "negate",
                "snapshot-negate",
            ),
            ("snapshot-test.gain", "audio", "volume=0.5", "snapshot-gain"),
        ] {
            let manifest = format!(
                r#"[effect]
id = "{id}"
name = "Snapshot test"
kind = "{kind}"
aliases = ["{alias}"]
[ffmpeg]
chain = "{chain}"
"#
            );
            result
                .add(concat_effects::Package::from_sources(&manifest, None, None).unwrap())
                .unwrap();
        }
        result
    }

    #[test]
    fn snapshot_worker_types_support_native_handoff_without_unsafe_traits() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send::<FrozenExport>();
        assert_send_sync::<Titles>();
        assert_send_sync::<Arc<Titles>>();
        assert_send_sync::<Project>();
        assert_send_sync::<Catalogue>();
    }

    #[test]
    fn snapshot_deduplicates_all_selected_media_and_titles_without_touching_project() {
        let mut fixture = Fixture::new();
        fixture.add_title();
        let mut project = fixture.editor.project().clone();
        let mut second = (*project.active().clips[0]).clone();
        second.id = "second".into();
        second.start = 1.0;
        project.active_mut().clips.push(Arc::new(second));
        let mut title = (*project.active().clips[1]).clone();
        title.id = "duplicate-title".into();
        title.start = 1.0;
        project.active_mut().clips.push(Arc::new(title));
        project.active_mut().tracks[0].visible = false;
        project.active_mut().tracks[0].muted = true;
        // A missing file not referenced by this timeline is not copied.
        let mut unused = project.media[0].clone();
        unused.id = "unused".into();
        unused.path = "/missing/unreferenced.mp4".into();
        project.media.push(unused);
        let before = project.clone();
        let frozen = fixture
            .prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        assert!(project == before);
        assert_eq!(frozen.request().clips.len(), 4);
        assert_eq!(
            frozen.manifest().file_count,
            2,
            "one media and one title PNG despite four clips"
        );
        assert!(frozen.request().clips[0].hidden && frozen.request().clips[0].muted);
        assert_eq!(
            frozen.request().clips[0].path,
            frozen.request().clips[1].path
        );
        assert_eq!(
            frozen.request().clips[2].path,
            frozen.request().clips[3].path
        );
        assert!(
            frozen
                .manifest()
                .entries
                .iter()
                .any(|entry| entry.kinds.contains(&InputKind::Title))
        );
        for clip in &frozen.request().clips {
            assert!(Path::new(&clip.path).starts_with(&frozen.stage.path));
        }
        fs::write(&fixture.source, b"changed after the copy").unwrap();
        for entry in frozen
            .manifest()
            .entries
            .iter()
            .filter(|entry| entry.kinds.contains(&InputKind::Title))
        {
            fs::write(&entry.source, b"changed title cache after the copy").unwrap();
        }
        frozen.verify_inputs(&fresh_cancel()).unwrap();
        let job = frozen.stage.path.clone();
        drop(frozen);
        assert!(!job.exists());
        assert!(fixture.parent.exists());
        assert_eq!(fixture.jobs(), 0);
    }

    #[test]
    fn snapshot_copies_complete_mask_tree_and_preserves_absent_state() {
        let fixture = Fixture::new();
        let (mut project, mask) = fixture.cutout_project();
        fs::create_dir_all(mask.join("strokes/0123456789abcdef/empty")).unwrap();
        fs::write(mask.join("000000000.png"), b"main png").unwrap();
        fs::write(mask.join("model"), b"model identity").unwrap();
        fs::write(
            mask.join("strokes/0123456789abcdef/000000500.png"),
            b"region png",
        )
        .unwrap();
        fs::write(mask.join("strokes/0123456789abcdef/model"), b"region model").unwrap();
        let mut duplicate = (*project.active().clips[0]).clone();
        duplicate.id = "mask-duplicate".into();
        project.active_mut().clips.push(Arc::new(duplicate));
        let frozen = fixture
            .prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        assert_eq!(
            frozen.request().clips[0].mask_dir,
            frozen.request().clips[1].mask_dir
        );
        let staged = Path::new(&frozen.request().clips[0].mask_dir);
        assert_eq!(
            fs::read(staged.join("strokes/0123456789abcdef/000000500.png")).unwrap(),
            b"region png"
        );
        assert!(
            frozen
                .manifest()
                .entries
                .iter()
                .any(|entry| entry.relative_path.ends_with("/empty")
                    && entry.shape == InputShape::Directory { empty: true })
        );
        fs::remove_dir_all(&mask).unwrap();
        frozen.verify_inputs(&fresh_cancel()).unwrap();
        let absent = fixture
            .prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        let absent_path = Path::new(&absent.request().clips[0].mask_dir);
        assert!(absent_path.is_dir());
        assert_eq!(fs::read_dir(absent_path).unwrap().count(), 0);
        assert!(
            absent
                .manifest()
                .entries
                .iter()
                .any(|entry| entry.shape == InputShape::Absent)
        );
        fs::create_dir_all(&mask).unwrap();
        fs::write(mask.join("000000000.png"), b"late masks").unwrap();
        absent.verify_inputs(&fresh_cancel()).unwrap();
        fs::write(absent_path.join("unexpected.png"), b"not absent").unwrap();
        assert!(absent.verify_inputs(&fresh_cancel()).is_err());
    }

    #[test]
    fn snapshot_rejects_precise_timeline_missing_references_and_title_omissions() {
        let mut fixture = Fixture::new();
        let project = fixture.editor.project().clone();
        let cancel = fresh_cancel();
        assert!(
            error(FrozenExport::prepare(
                &project,
                "missing",
                None,
                &fixture.spec(),
                &fixture.titles,
                Catalogue::new(),
                &fixture.parent,
                FreezeLimits::default(),
                &cancel
            ))
            .contains("timeline")
        );
        let mut broken = project.clone();
        broken.active_mut().clip_at_mut(0).track_id = "missing".into();
        assert!(
            error(fixture.prepare(&broken, Catalogue::new(), FreezeLimits::default(), &cancel))
                .contains("track")
        );
        let mut broken = project.clone();
        broken.active_mut().clip_at_mut(0).media_id = "missing".into();
        assert!(
            error(fixture.prepare(&broken, Catalogue::new(), FreezeLimits::default(), &cancel))
                .contains("media")
        );
        let mut broken = project.clone();
        broken.media[0].path.clear();
        assert!(
            error(fixture.prepare(&broken, Catalogue::new(), FreezeLimits::default(), &cancel))
                .contains("empty path")
        );
        fixture.add_title();
        let bad_data = fixture._root.path.join("not-a-directory");
        fs::write(&bad_data, b"file").unwrap();
        let blocked_titles = Titles::new(&AppDirs {
            data: bad_data,
            config: fixture.dirs.config.clone(),
        });
        assert!(
            error(FrozenExport::prepare(
                fixture.editor.project(),
                &fixture.editor.project().active_timeline_id,
                None,
                &fixture.spec(),
                &blocked_titles,
                Catalogue::new(),
                &fixture.parent,
                FreezeLimits::default(),
                &cancel
            ))
            .contains("omitted")
        );
        // The original chosen timeline stays usable when a different one is active.
        let selected = fixture.editor.project().active_timeline_id.clone();
        fixture.editor.apply(Command::AddTimeline).unwrap();
        let before = fixture.editor.project().clone();
        let frozen = FrozenExport::prepare(
            &before,
            &selected,
            None,
            &fixture.spec(),
            &fixture.titles,
            Catalogue::new(),
            &fixture.parent,
            FreezeLimits::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(frozen.request().clips.len(), 2);
        assert!(fixture.editor.project() == &before);
    }

    #[test]
    fn snapshot_rejects_unknown_directory_lut_and_wrong_kind_effects_but_ignores_disabled() {
        let fixture = Fixture::new();
        let mut project = fixture.editor.project().clone();
        project.active_mut().clip_at_mut(0).filters = vec![AppliedFilter::new("not-installed")];
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("unknown")
        );
        project.active_mut().clip_at_mut(0).filters[0].enabled = false;
        fixture
            .prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        let mut package = in_memory_catalogue()
            .get("snapshot-test.negate")
            .unwrap()
            .clone();
        project.active_mut().clip_at_mut(0).video_effects =
            vec![AppliedFilter::new("snapshot-negate")];
        package.folder = Some(fixture._root.path.clone());
        let mut catalogue = Catalogue::new();
        catalogue.add(package.clone()).unwrap();
        assert!(
            error(fixture.prepare(
                &project,
                catalogue,
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("directory or LUT")
        );
        package.folder = None;
        package.manifest.lut = Some(concat_effects::manifest::LutTable {
            file: "look.cube".into(),
        });
        let mut catalogue = Catalogue::new();
        catalogue.add(package).unwrap();
        assert!(
            error(fixture.prepare(
                &project,
                catalogue,
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("directory or LUT")
        );
        project.active_mut().clip_at_mut(0).video_effects =
            vec![AppliedFilter::new("snapshot-gain")];
        assert!(
            error(fixture.prepare(
                &project,
                in_memory_catalogue(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("wrong kind")
        );
    }

    #[test]
    fn snapshot_rejects_actual_expression_errors_and_missing_cpu_backends() {
        let fixture = Fixture::new();
        let mut project = fixture.editor.project().clone();
        let manifest = r#"[effect]
id = "snapshot-test.expression"
name = "Expression fixture"
kind = "effect"
[[param]]
key = "amount"
label = "Amount"
min = 0
max = 1
default = 0.5
[ffmpeg]
chain = "eq=brightness={clamp(amount, 0, amount)}"
"#;
        let mut catalogue = Catalogue::new();
        catalogue
            .add(concat_effects::Package::from_sources(manifest, None, None).unwrap())
            .unwrap();
        let mut effect = AppliedFilter::new("snapshot-test.expression");
        effect.params.insert("amount".into(), -1.0);
        project.active_mut().clip_at_mut(0).video_effects = vec![effect];
        assert!(
            catalogue
                .video_chain(&project.active().clips[0].video_effects)
                .is_empty(),
            "the legacy composer silently drops this error"
        );
        assert!(
            error(fixture.prepare(
                &project,
                catalogue,
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("cannot construct")
        );

        let manifest = r#"[effect]
id = "snapshot-test.gpu-only"
name = "GPU only"
kind = "effect"
[wgsl]
entry = "effect.wgsl"
"#;
        let mut catalogue = Catalogue::new();
        catalogue
            .add(
                concat_effects::Package::from_sources(
                    manifest,
                    None,
                    Some("fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }"),
                )
                .unwrap(),
            )
            .unwrap();
        project.active_mut().clip_at_mut(0).video_effects =
            vec![AppliedFilter::new("snapshot-test.gpu-only")];
        assert!(
            error(fixture.prepare(
                &project,
                catalogue,
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("no CPU backend")
        );
        // A package with neither backend is rejected even before it can
        // enter a captured Catalogue. Exercise that actual earlier gate.
        let no_backend = concat_effects::Package::from_sources(
            r#"[effect]
id = "snapshot-test.no-audio-backend"
name = "No backend"
kind = "audio"
"#,
            None,
            None,
        )
        .unwrap_err();
        assert!(no_backend.to_string().contains("no backend"));
        assert_eq!(fixture.jobs(), 0);
    }

    #[test]
    fn snapshot_limits_are_enforced_during_streaming_and_cleanup_is_owned() {
        let fixture = Fixture::new();
        let project = fixture.editor.project();
        let cancel = fresh_cancel();
        let other = fixture.parent.join("other-job");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("keep"), b"other").unwrap();
        for limits in [
            FreezeLimits {
                max_bytes: 4,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_files: 0,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_entries: 0,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_clips: 0,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_project_bytes: 8,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_manifest_bytes: 8,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_component_bytes: 5,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_path_bytes: 5,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_dimension: 32,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_pixels: 1024,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_fps: 24.0,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_seconds: 0.5,
                ..FreezeLimits::default()
            },
            FreezeLimits {
                max_frames: 29,
                ..FreezeLimits::default()
            },
        ] {
            assert!(
                fixture
                    .prepare(project, Catalogue::new(), limits, &cancel)
                    .is_err()
            );
            assert_eq!(fixture.jobs(), 1);
            assert_eq!(fs::read(other.join("keep")).unwrap(), b"other");
        }
        let (project, mask) = fixture.cutout_project();
        fs::create_dir_all(mask.join("a/b/c")).unwrap();
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits {
                    max_depth: 1,
                    ..FreezeLimits::default()
                },
                &cancel
            ))
            .contains("depth")
        );
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits {
                    max_entries: 3,
                    ..FreezeLimits::default()
                },
                &cancel
            ))
            .contains("entry")
        );
        assert_eq!(fixture.jobs(), 1);
    }

    #[test]
    fn snapshot_copy_checkpoints_detect_source_changes_and_mid_copy_cancel_without_sleep() {
        let fixture = Fixture::new();
        let project = fixture.editor.project();
        let cancel = fresh_cancel();
        let mut changed = false;
        let mut change = |path: &Path| {
            if path == fixture.source && !changed {
                fs::write(path, vec![23; 128 * 1024]).unwrap();
                changed = true;
            }
        };
        let result = FrozenExport::prepare_inner(
            project,
            &project.active_timeline_id,
            None,
            &fixture.spec(),
            &fixture.titles,
            Catalogue::new(),
            &fixture.parent,
            FreezeLimits::default(),
            &cancel,
            &mut change,
        );
        assert!(error(result).contains("changed"));
        assert!(changed);
        assert_eq!(fixture.jobs(), 0);
        let mut cancel_on_chunk = |_: &Path| cancel.store(true, Ordering::Relaxed);
        let result = FrozenExport::prepare_inner(
            project,
            &project.active_timeline_id,
            None,
            &fixture.spec(),
            &fixture.titles,
            Catalogue::new(),
            &fixture.parent,
            FreezeLimits::default(),
            &cancel,
            &mut cancel_on_chunk,
        );
        assert!(error(result).contains("cancelled"));
        assert_eq!(fixture.jobs(), 0);
        assert!(
            fixture
                .prepare(project, Catalogue::new(), FreezeLimits::default(), &cancel)
                .is_err()
        );
    }

    #[test]
    fn snapshot_verification_rejects_added_missing_changed_and_replaced_copies() {
        let fixture = Fixture::new();
        let project = fixture.editor.project();
        for mode in ["added", "missing", "changed", "replaced"] {
            let frozen = fixture
                .prepare(
                    project,
                    Catalogue::new(),
                    FreezeLimits::default(),
                    &fresh_cancel(),
                )
                .unwrap();
            let input = Path::new(&frozen.request().clips[0].path);
            match mode {
                "added" => fs::write(frozen.stage.path.join("extra"), b"extra").unwrap(),
                "missing" => fs::remove_file(input).unwrap(),
                "changed" => fs::write(input, b"modified").unwrap(),
                _ => {
                    let bytes = fs::read(input).unwrap();
                    fs::remove_file(input).unwrap();
                    fs::write(input, bytes).unwrap();
                }
            }
            assert!(frozen.verify_inputs(&fresh_cancel()).is_err(), "{mode}");
            assert!(matches!(
                frozen.render(&fresh_cancel(), |_| {}),
                Err(SnapshotRenderError::InputsInvalid { .. })
            ));
            assert!(
                !Path::new(&fixture.spec().output).exists(),
                "pre-render verification must fail first"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_source_and_mask_symlinks_special_files_and_path_traversal() {
        use std::os::unix::{fs::symlink, net::UnixListener};
        // sockaddr_un limits the spelling used by bind, even though the
        // filesystem accepts our longer mask path. Bind through a unique
        // short alias, then remove it before testing the actual socket entry.
        let bind_socket = |path: &Path| {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let instant = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let alias = Path::new("/tmp").join(format!(
                ".sc-socket-{}-{instant:x}-{sequence:x}",
                std::process::id()
            ));
            symlink(path.parent().unwrap(), &alias).unwrap();
            let result = UnixListener::bind(alias.join(path.file_name().unwrap()));
            fs::remove_file(alias).unwrap();
            result.unwrap()
        };
        let fixture = Fixture::new();
        let mut project = fixture.editor.project().clone();
        let alias = fixture._root.path.join("alias.mp4");
        symlink(&fixture.source, &alias).unwrap();
        project.media[0].path = alias.to_string_lossy().into_owned();
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("symlink")
        );
        let socket = fixture._root.path.join("socket");
        let _listener = bind_socket(&socket);
        project.media[0].path = socket.to_string_lossy().into_owned();
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("special")
        );
        project.media[0].path = fixture
            ._root
            .path
            .join("project/../source.mp4")
            .to_string_lossy()
            .into_owned();
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("traversal")
        );
        let (project, mask) = fixture.cutout_project();
        fs::create_dir_all(&mask).unwrap();
        symlink(&fixture.source, mask.join("000000000.png")).unwrap();
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("symlink")
        );
        fs::remove_file(mask.join("000000000.png")).unwrap();
        let _socket = bind_socket(&mask.join("special"));
        assert!(
            error(fixture.prepare(
                &project,
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel()
            ))
            .contains("special")
        );
        assert_eq!(fixture.jobs(), 0);
    }

    #[test]
    fn snapshot_drop_will_not_clean_a_replaced_root() {
        let fixture = Fixture::new();
        let frozen = fixture
            .prepare(
                fixture.editor.project(),
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        let original = frozen.stage.path.clone();
        let moved = fixture._root.path.join("moved-owned-root");
        fs::rename(&original, &moved).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("keep"), b"replacement").unwrap();
        assert!(frozen.verify_inputs(&fresh_cancel()).is_err());
        drop(frozen);
        assert_eq!(fs::read(original.join("keep")).unwrap(), b"replacement");
        assert!(moved.is_dir());
    }

    fn encode_red(path: &Path) {
        let mut encoder = Encoder::create(
            path,
            64,
            64,
            FrameRate::THIRTY,
            &EncodeOptions {
                hardware: false,
                preset: "ultrafast".into(),
                ..EncodeOptions::default()
            },
        )
        .unwrap();
        let mut frame = Frame::black(64, 64);
        frame.fill([200, 30, 30, 255]);
        for _ in 0..30 {
            encoder.write_frame(&frame).unwrap();
        }
        encoder.finish().unwrap();
    }

    fn tone(path: &Path) {
        let mut pcm = Vec::new();
        for index in 0..48_000 {
            let value = (0.5
                * (2.0 * std::f64::consts::PI * 1000.0 * f64::from(index) / 48_000.0).sin()
                * 32767.0) as i16;
            pcm.extend_from_slice(&value.to_le_bytes());
            pcm.extend_from_slice(&value.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&48_000_u32.to_le_bytes());
        wav.extend_from_slice(&192_000_u32.to_le_bytes());
        wav.extend_from_slice(&4_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        fs::write(path, wav).unwrap();
    }

    #[test]
    fn snapshot_real_render_uses_copied_media_title_masks_and_audio_after_sources_change() {
        let mut fixture = Fixture::new();
        encode_red(&fixture.source);
        let audio = fixture._root.path.join("tone.wav");
        tone(&audio);
        let audio_clip = add_media(&mut fixture.editor, &audio, MediaKind::Audio, true, 1);
        let title_id = fixture.add_title();
        let video_id = fixture.editor.project().active().clips[0].id.clone();
        for (clip_id, audio_effect) in [
            (video_id.clone(), false),
            (audio_clip, true),
            (title_id, false),
        ] {
            fixture
                .editor
                .apply(Command::UpdateClip {
                    clip_id,
                    patch: if audio_effect {
                        ClipPatch {
                            filters: Some(vec![AppliedFilter::new("snapshot-gain")]),
                            ..ClipPatch::default()
                        }
                    } else {
                        ClipPatch {
                            video_effects: Some(vec![AppliedFilter::new("snapshot-negate")]),
                            ..ClipPatch::default()
                        }
                    },
                })
                .unwrap();
        }
        fixture
            .editor
            .apply(Command::SetClipCutout {
                clip_id: video_id,
                cutout: Some(Cutout {
                    mode: CutoutMode::Auto,
                    subject: Subject::Person,
                    feather: 0.0,
                    strokes: Vec::new(),
                }),
            })
            .unwrap();
        let mask_dir = concat_vision::mask_dir(
            &fixture.project_dir,
            &fixture.source.to_string_lossy(),
            Subject::Person,
        );
        fs::create_dir_all(mask_dir.join("strokes/0123456789abcdef")).unwrap();
        let mut mask = concat_vision::Mask::filled(8, 8, 255);
        for row in mask.bytes_mut().chunks_exact_mut(8) {
            row[4..].fill(0);
        }
        fs::write(mask_dir.join("000000000.png"), mask.to_png()).unwrap();
        fs::write(mask_dir.join("model"), b"fixture").unwrap();
        fs::write(
            mask_dir.join("strokes/0123456789abcdef/000000000.png"),
            concat_vision::Mask::filled(8, 8, 255).to_png(),
        )
        .unwrap();
        let frozen = fixture
            .prepare(
                fixture.editor.project(),
                in_memory_catalogue(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        assert_eq!(
            frozen
                .request()
                .clips
                .iter()
                .filter(|clip| !clip.filter_chain.is_empty())
                .count(),
            1
        );
        assert!(
            frozen
                .request()
                .clips
                .iter()
                .filter(|clip| clip.kind == concat_export::ClipKind::Image)
                .all(|clip| clip.video_filter_chain == "negate")
        );
        fs::write(&fixture.source, b"source video changed after prepare").unwrap();
        fs::write(&audio, b"source sound changed after prepare").unwrap();
        for title in frozen
            .manifest()
            .entries
            .iter()
            .filter(|entry| entry.kinds.contains(&InputKind::Title))
        {
            fs::write(&title.source, b"source title changed after prepare").unwrap();
        }
        fs::write(
            mask_dir.join("000000000.png"),
            concat_vision::Mask::filled(8, 8, 255).to_png(),
        )
        .unwrap();
        fs::write(
            mask_dir.join("strokes/0123456789abcdef/000000000.png"),
            b"changed source region",
        )
        .unwrap();
        let output = frozen.render(&fresh_cancel(), |_| {}).unwrap();
        assert_eq!(output.inputs.manifest_sha256, frozen.manifest_sha256());
        let path = Path::new(&output.path);
        let mut decoder = Decoder::open(path, &DecodeOptions::default().in_software()).unwrap();
        let mut frames = 0;
        let mut magenta = 0;
        while let Some(frame) = decoder.next_frame().unwrap() {
            assert_eq!((frame.width(), frame.height()), (64, 64));
            let left = frame.pixel(8, 4).unwrap();
            let right = frame.pixel(58, 4).unwrap();
            assert!(
                left[0] < 100 && left[1] > 140 && left[2] > 140,
                "copied red plus negate at frame {frames}: {left:?}"
            );
            assert!(
                right[0] < 35 && right[1] < 35 && right[2] < 35,
                "copied half-mask at frame {frames}: {right:?}"
            );
            magenta += frame
                .pixels()
                .chunks_exact(4)
                .filter(|p| p[0] > 150 && p[1] < 110 && p[2] > 150)
                .count();
            frames += 1;
        }
        assert_eq!(frames, 30);
        assert!(
            magenta > 30,
            "copied green title with explicit negate must remain magenta"
        );
        let mut decoder = AudioDecoder::open(
            path,
            &AudioOptions {
                rate: 48_000,
                channels: 2,
                format: SampleFormat::F32,
                ..AudioOptions::default()
            },
        )
        .unwrap();
        let mut samples = 0_u64;
        let mut energy = 0.0;
        while let Some(chunk) = decoder.next_f32().unwrap() {
            samples += chunk.len() as u64;
            energy += chunk.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
        }
        let seconds = samples as f64 / 96_000.0;
        let rms = (energy / samples as f64).sqrt();
        assert!(
            (seconds - 1.0).abs() < 0.05,
            "full sound duration: {seconds}"
        );
        assert!(
            rms > 0.1 && rms < 0.24,
            "copied tone with explicit 0.5 gain RMS: {rms}"
        );
        let stage = frozen.stage.path.clone();
        drop(frozen);
        assert!(!stage.exists());
        assert!(path.is_file(), "input cleanup must retain rendered output");
    }

    #[test]
    fn snapshot_real_render_cancel_and_existing_output_keep_inputs_and_other_files() {
        let fixture = Fixture::new();
        encode_red(&fixture.source);
        let frozen = fixture
            .prepare(
                fixture.editor.project(),
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        let cancel = fresh_cancel();
        let mut saw_frame = false;
        let error = frozen
            .render(&cancel, |progress| {
                if progress.stage == "rendering" {
                    saw_frame = true;
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .unwrap_err();
        assert!(saw_frame);
        match error {
            SnapshotRenderError::EngineFailed {
                error,
                input_verification,
            } => {
                assert!(error.contains("export cancelled"));
                assert!(input_verification.unwrap_err().contains("cancelled"));
            }
            other => panic!("expected original engine cancellation plus post-check: {other:?}"),
        }
        let output = PathBuf::from(&fixture.spec().output);
        assert!(!output.exists());
        frozen.verify_inputs(&fresh_cancel()).unwrap();
        fs::write(&output, b"previous finished output").unwrap();
        let error = frozen.render(&fresh_cancel(), |_| {}).unwrap_err();
        match error {
            SnapshotRenderError::EngineFailed {
                error,
                input_verification,
            } => {
                assert!(error.contains("already exists"));
                assert!(input_verification.is_ok());
            }
            other => panic!("expected engine output conflict with valid inputs: {other:?}"),
        }
        assert_eq!(fs::read(&output).unwrap(), b"previous finished output");
        let stage = frozen.stage.path.clone();
        drop(frozen);
        assert!(!stage.exists());
        assert_eq!(fs::read(&output).unwrap(), b"previous finished output");
    }

    #[test]
    fn snapshot_post_render_input_failure_retains_already_published_output() {
        let fixture = Fixture::new();
        encode_red(&fixture.source);
        let frozen = fixture
            .prepare(
                fixture.editor.project(),
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        let input = frozen.request().clips[0].path.clone();
        let mut changed = false;
        let error = frozen
            .render_inner(
                &fresh_cancel(),
                &mut |_| {},
                &mut || {
                    assert!(
                        Path::new(&fixture.spec().output).is_file(),
                        "engine publication must complete first"
                    );
                    fs::write(&input, b"changed staged input after publication").unwrap();
                    changed = true;
                },
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(changed);
        let returned = match error {
            SnapshotRenderError::PublishedInputsUnverified { path, error } => {
                assert!(error.contains("staged input"), "{error}");
                path
            }
            other => panic!("published output path must survive post-check failure: {other:?}"),
        };
        let output = PathBuf::from(&fixture.spec().output);
        assert_eq!(returned, output.to_string_lossy().into_owned());
        let mut decoder = Decoder::open(
            Path::new(&returned),
            &DecodeOptions::default().in_software(),
        )
        .unwrap();
        let mut count = 0;
        while decoder.next_frame().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(
            count, 30,
            "the path carried by the error is an actual complete export"
        );
        assert!(output.is_file(), "the engine may already have published");
        drop(frozen);
        assert!(
            output.is_file(),
            "post-render verification must not remove output"
        );
    }
    #[test]
    fn snapshot_post_render_cancel_retains_published_path_and_decodable_file() {
        let fixture = Fixture::new();
        encode_red(&fixture.source);
        let frozen = fixture
            .prepare(
                fixture.editor.project(),
                Catalogue::new(),
                FreezeLimits::default(),
                &fresh_cancel(),
            )
            .unwrap();
        let cancel = fresh_cancel();
        let error = frozen
            .render_inner(
                &cancel,
                &mut |_| {},
                &mut || {
                    assert!(Path::new(&fixture.spec().output).is_file());
                    cancel.store(true, Ordering::Relaxed);
                },
                |_| Ok(()),
            )
            .unwrap_err();
        let path = match error {
            SnapshotRenderError::PublishedInputsUnverified { path, error } => {
                assert!(error.contains("cancelled"));
                path
            }
            other => panic!("published path must survive cancelled post-check: {other:?}"),
        };
        assert_eq!(path, fixture.spec().output);
        let mut decoder =
            Decoder::open(Path::new(&path), &DecodeOptions::default().in_software()).unwrap();
        let mut frames = 0;
        while decoder.next_frame().unwrap().is_some() {
            frames += 1;
        }
        assert_eq!(frames, 30);
        drop(frozen);
        assert!(Path::new(&path).is_file());
    }
    fn assert_no_snapshot_publish_intermediates(fixture: &Fixture) {
        assert!(
            fs::read_dir(&fixture._root.path).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".concat-export-")
            }),
            "strict publication failures must clean renderer intermediates"
        );
    }

    #[test]
    fn snapshot_strict_render_rejects_manifest_media_and_invalid_layer_before_hook() {
        for invalid_layer in [false, true] {
            let mut fixture = Fixture::new();
            let mut catalogue = Catalogue::new();
            if invalid_layer {
                encode_red(&fixture.source);
                catalogue
                    .add(
                        concat_effects::Package::from_sources(
                            r#"[effect]
id = "snapshot-strict.invalid-layer"
name = "Unavailable layer filter"
kind = "effect"
[ffmpeg]
chain = "snapshot_strict_filter_does_not_exist"
"#,
                            None,
                            None,
                        )
                        .unwrap(),
                    )
                    .unwrap();
                fixture
                    .editor
                    .apply(Command::AddLayerClip {
                        track_id: Some(fixture.editor.project().active().tracks[1].id.clone()),
                        start: 0.0,
                        duration: Some(1.0),
                        effect_id: "snapshot-strict.invalid-layer".into(),
                        name: "Invalid layer".into(),
                    })
                    .unwrap();
            } else {
                // A real, ordinary file whose .mp4 extension cannot turn an
                // ffconcat manifest into a permitted self-contained input.
                fs::write(
                    &fixture.source,
                    b"ffconcat version 1.0\nfile 'not-opened.mp4'\n",
                )
                .unwrap();
            }
            let cancel = fresh_cancel();
            let frozen = fixture
                .prepare(
                    fixture.editor.project(),
                    catalogue,
                    FreezeLimits::default(),
                    &cancel,
                )
                .unwrap();
            frozen.verify_inputs(&cancel).unwrap();
            let mut calls = 0;
            let error = frozen
                .render_before_publish(
                    &cancel,
                    |_| {},
                    |_| {
                        calls += 1;
                        Ok(())
                    },
                )
                .unwrap_err();
            match error {
                SnapshotRenderError::EngineFailed {
                    error,
                    input_verification,
                } => {
                    assert!(input_verification.is_ok(), "the copied bytes remain intact");
                    if invalid_layer {
                        assert!(error.contains("layer effect failed"), "{error}");
                    } else {
                        assert!(
                            error.contains("strict input") || error.contains("input policy"),
                            "{error}"
                        );
                    }
                }
                other => {
                    panic!("strict engine rejection must preserve the original failure: {other:?}")
                }
            }
            assert_eq!(calls, 0);
            assert!(!Path::new(&fixture.spec().output).exists());
            assert_no_snapshot_publish_intermediates(&fixture);
            drop(frozen);
            assert_eq!(fixture.jobs(), 0);
        }
    }

    #[test]
    fn snapshot_before_publish_checks_actual_finished_candidate_and_honours_rejection() {
        for accept in [true, false] {
            let mut fixture = Fixture::new();
            encode_red(&fixture.source);
            if !accept {
                // Exercise the muxed candidate as well as the silent one.
                let audio = fixture._root.path.join("publish-tone.wav");
                tone(&audio);
                add_media(&mut fixture.editor, &audio, MediaKind::Audio, true, 1);
            }
            let cancel = fresh_cancel();
            let frozen = fixture
                .prepare(
                    fixture.editor.project(),
                    Catalogue::new(),
                    FreezeLimits::default(),
                    &cancel,
                )
                .unwrap();
            let expected_proof = frozen.verify_inputs(&cancel).unwrap();
            let output = PathBuf::from(&fixture.spec().output);
            let mut calls = 0;
            let mut finished_candidate = PathBuf::new();
            let once_token = String::from("trusted native hook token");
            let result = frozen.render_before_publish(
                &cancel,
                |_| {},
                |candidate| {
                    // Moving this token also checks that a genuine FnOnce is
                    // accepted; this is not narrowed to a reusable callback.
                    drop(once_token);
                    calls += 1;
                    assert!(!output.exists(), "the hook must run before publication");
                    assert_ne!(candidate, output.as_path());
                    assert!(candidate.is_file());
                    assert_eq!(frozen.verify_inputs(&cancel).unwrap(), expected_proof);
                    let mut decoder =
                        Decoder::open(candidate, &DecodeOptions::default().in_software()).unwrap();
                    let mut frames = 0;
                    while let Some(frame) = decoder.next_frame().unwrap() {
                        assert_eq!((frame.width(), frame.height()), (64, 64));
                        let red = frame.pixel(32, 32).unwrap();
                        assert!(
                            red[0] > 140 && red[1] < 100,
                            "actual finished candidate pixels: {red:?}"
                        );
                        frames += 1;
                    }
                    assert_eq!(frames, 30);
                    if !accept {
                        let mut decoder = AudioDecoder::open(
                            candidate,
                            &AudioOptions {
                                rate: 48_000,
                                channels: 2,
                                format: SampleFormat::F32,
                                ..AudioOptions::default()
                            },
                        )
                        .unwrap();
                        let mut count = 0;
                        while let Some(chunk) = decoder.next_f32().unwrap() {
                            count += chunk.len();
                        }
                        assert!(
                            (count as f64 / 96_000.0 - 1.0).abs() < 0.05,
                            "muxed candidate contains complete audio"
                        );
                    }
                    finished_candidate = candidate.to_path_buf();
                    if accept {
                        Ok(())
                    } else {
                        Err("native publish declined fixture".into())
                    }
                },
            );
            assert_eq!(calls, 1);
            assert!(
                !finished_candidate.exists(),
                "the renderer retires the temporary candidate"
            );
            if accept {
                let rendered = result.unwrap();
                assert_eq!(rendered.path, output.to_string_lossy().into_owned());
                assert_eq!(rendered.inputs, expected_proof);
                assert!(output.is_file());
            } else {
                match result.unwrap_err() {
                    SnapshotRenderError::EngineFailed {
                        error,
                        input_verification,
                    } => {
                        assert_eq!(error, "native publish declined fixture");
                        assert!(input_verification.is_ok());
                    }
                    other => panic!("hook rejection must remain the engine outcome: {other:?}"),
                }
                assert!(!output.exists());
            }
            assert_no_snapshot_publish_intermediates(&fixture);
        }
    }

    #[test]
    fn snapshot_last_input_verification_blocks_external_hook_and_publication() {
        let fixture = Fixture::new();
        encode_red(&fixture.source);
        let cancel = fresh_cancel();
        let frozen = fixture
            .prepare(
                fixture.editor.project(),
                Catalogue::new(),
                FreezeLimits::default(),
                &cancel,
            )
            .unwrap();
        let mut changed = false;
        let mut calls = 0;
        let error = frozen
            .render_before_publish(
                &cancel,
                |progress| {
                    if progress.stage == "rendering" && !changed {
                        // Do not disturb the decoder. The complete render can finish,
                        // but the last input-tree check must reject this extra entry.
                        fs::write(
                            frozen.stage.path.join("extra-before-publish"),
                            b"changed staged tree",
                        )
                        .unwrap();
                        changed = true;
                    }
                },
                |_| {
                    calls += 1;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(changed);
        assert_eq!(
            calls, 0,
            "last input check precedes the trusted native hook"
        );
        match error {
            SnapshotRenderError::EngineFailed {
                error,
                input_verification,
            } => {
                assert!(error.contains("unexpected staged input"), "{error}");
                assert!(
                    input_verification
                        .unwrap_err()
                        .contains("unexpected staged input")
                );
            }
            other => panic!("pre-publication input error must remain an engine failure: {other:?}"),
        }
        assert!(!Path::new(&fixture.spec().output).exists());
        assert_no_snapshot_publish_intermediates(&fixture);
    }
}
