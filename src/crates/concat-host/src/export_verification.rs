// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Independent, bounded verification of one native export's actual bytes.
//!
//! A verified output proves complete software decoding and a stable file at
//! the check boundary. It does not prove publication, library registration,
//! image quality, listening acceptance, or any historical Agent operation.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use concat_core::time::{FrameRate, Rational};
use concat_media::{
    AudioDecoder, AudioOptions, DecodeOptions, Decoder, FrameSource, InputPolicy, SampleFormat,
    SourceFrameLimits, VideoCodec, probe_with_policy,
};
use concat_project::model::VideoSettings;
use serde::Serialize;
use sha2::{Digest, Sha256};

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const STRICT: InputPolicy = InputPolicy::SelfContainedLocal;

/// Frozen native expectations; the caller binds these to its original operation.
/// Do not build this value from the output's own probe, or accept model fields.
#[derive(Clone, Copy, Debug)]
pub struct OutputExpectation {
    /// Native-normalized output dimensions and exact positive frame rate.
    pub video: VideoSettings,
    /// Exact frame count already calculated by the trusted export caller.
    pub frames: u64,
    /// Requested standard, irrespective of hardware encoder or fallback.
    pub codec: VideoCodec,
    /// Whether the actual muxed export must contain audio.
    pub audio_expected: bool,
}

/// Worker limits, checked before opening codecs and throughout bounded reads.
#[derive(Clone, Copy, Debug)]
pub struct VerificationLimits {
    /// Maximum actual file bytes.
    pub max_bytes: u64,
    /// Maximum video width or height.
    pub max_dimension: u32,
    /// Maximum displayed video pixels per frame.
    pub max_pixels: u64,
    /// Maximum exact decoded video frames.
    pub max_frames: u64,
    /// Maximum positive frame rate.
    pub max_fps: f64,
    /// Maximum video duration; audio adds AAC padding plus one video frame.
    pub max_seconds: f64,
    /// Maximum audio streams; every advertised audio stream is fully checked.
    pub max_audio_streams: usize,
}

impl Default for VerificationLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024 * 1024,
            max_dimension: 8192,
            max_pixels: 16_777_216,
            max_frames: 864_000,
            max_fps: 120.0,
            max_seconds: 4.0 * 60.0 * 60.0,
            max_audio_streams: 8,
        }
    }
}

/// Facts from complete decoding, excluding the container's duration estimate.
#[derive(Clone, Debug, Serialize)]
pub struct OutputSummary {
    /// Actual whole-file length.
    pub bytes: u64,
    /// SHA256 of the entire actual file, checked before and after decoding.
    pub sha256: String,
    /// Actual probed video standard.
    pub video_codec: String,
    /// Actual displayed width.
    pub width: u32,
    /// Actual displayed height.
    pub height: u32,
    /// Actual exact frame rate numerator in lowest terms.
    pub rate_num: i64,
    /// Actual exact frame rate denominator in lowest terms.
    pub rate_den: i64,
    /// Actual frames decoded through EOF, without pacing or repetition.
    pub video_frames: u64,
    /// Actual first presentation position in seconds, independently checked against zero.
    pub first_video_position_seconds: f64,
    /// Actual last presentation position in seconds, independently checked against CFR timing.
    pub last_video_position_seconds: f64,
    /// Actual last presentation position plus one verified video frame duration.
    pub video_seconds: f64,
    /// Each advertised audio stream, independently read through EOF.
    pub audio: Vec<AudioSummary>,
}

/// One stream normalized to stereo 48000 Hz F32, with finite samples only.
#[derive(Clone, Debug, Serialize)]
pub struct AudioSummary {
    /// Actual file stream index.
    pub stream: u32,
    /// Actual probed codec name.
    pub codec: String,
    /// Probed source sampling rate.
    pub source_rate: u32,
    /// Probed source channel count.
    pub source_channels: u32,
    /// Actual normalized sampling frames, each containing two samples.
    pub sample_frames: u64,
    /// Actual normalized sampling frames divided by 48000.
    pub seconds: f64,
    /// Maximum absolute finite sample; zero is valid silence.
    pub peak: f64,
    /// Root mean square across both normalized channels; not listening proof.
    pub rms: f64,
}

/// Opaque full-decode proof. Only actual verification constructs this type.
/// It cannot be deserialized, and no public fields can overwrite its facts.
#[derive(Debug)]
pub struct VerifiedOutput {
    path: PathBuf,
    summary: OutputSummary,
    limits: VerificationLimits,
    checked_at: SystemTime,
}

impl VerifiedOutput {
    /// Read-only facts. Mutation of an owned summary clone cannot alter proof.
    pub fn summary(&self) -> &OutputSummary {
        &self.summary
    }

    /// Native-internal actual path; never expose this as an arbitrary model capability.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Time of this actual check, with no claim about historical publication.
    pub fn checked_at(&self) -> SystemTime {
        self.checked_at
    }

    /// Independently open and hash a regular file of identical bytes.
    /// Byte identity reuses the earlier complete decode; no codec runs again.
    /// The source may already have moved: this checks `path` on its own.
    pub fn recheck_same_content(
        &self,
        path: impl AsRef<Path>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let path = path.as_ref();
        let mut checkpoint = |_: Checkpoint| {};
        let (held, bytes, sha256) =
            hash_open(path, self.limits.max_bytes, cancel, &mut checkpoint)?;
        if bytes != self.summary.bytes || sha256 != self.summary.sha256 {
            return Err("output content differs from the fully decoded output".into());
        }
        held.unchanged(path)?;
        cancelled(cancel)?;
        Ok(Self {
            path: path.to_path_buf(),
            summary: self.summary.clone(),
            limits: self.limits,
            checked_at: SystemTime::now(),
        })
    }
}

/// Reopen, hash, strict-probe, fully decode, and hash again without changing
/// or deleting the file. Any error leaves publication status unspecified.
/// Cancellation is checked between reads, not within one blocking codec call.
pub fn verify_output(
    path: impl AsRef<Path>,
    expected: OutputExpectation,
    limits: VerificationLimits,
    cancel: &AtomicBool,
) -> Result<VerifiedOutput, String> {
    verify_with_checkpoint(path.as_ref(), expected, limits, cancel, &mut |_| {})
}

// A private synchronous test seam. No global hooks, timers or races are needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checkpoint {
    HashChunk,
    AfterFirstHash,
    VideoFrame,
    AudioChunk,
    BeforeFinalHash,
}

fn verify_with_checkpoint(
    path: &Path,
    expected: OutputExpectation,
    limits: VerificationLimits,
    cancel: &AtomicBool,
    checkpoint: &mut impl FnMut(Checkpoint),
) -> Result<VerifiedOutput, String> {
    let (rate, _expected_seconds, max_audio_frames) = validate(expected, limits)?;
    cancelled(cancel)?;
    let (held, bytes, sha256) = hash_open(path, limits.max_bytes, cancel, checkpoint)?;
    checkpoint(Checkpoint::AfterFirstHash);
    cancelled(cancel)?;
    held.unchanged(path)?;
    let info = probe_with_policy(path, STRICT).map_err(media_error)?;
    cancelled(cancel)?;
    held.unchanged(path)?;
    let video = info.require_video().map_err(media_error)?;
    if video.codec != expected.codec.name()
        || video.width != expected.video.width
        || video.height != expected.video.height
        || video.frame_rate != rate
    {
        return Err(
            "actual video codec, dimensions or exact frame rate differ from expectation".into(),
        );
    }
    let audio_present = !info.audio_streams.is_empty();
    if audio_present != expected.audio_expected {
        return Err("actual audio presence differs from expectation".into());
    }
    if info.audio_streams.len() > limits.max_audio_streams {
        return Err("audio stream limit exceeded".into());
    }

    // No requested pacing, trimming, repetition or target-size override.
    // The media layer rejects original frame dimensions before conversion,
    // download, discard or pacing, and applies its codec pixel allocation cap.
    let mut decoder = Decoder::open_with_source_limits(
        path,
        &DecodeOptions::default().in_software(),
        STRICT,
        SourceFrameLimits {
            displayed_width: expected.video.width,
            displayed_height: expected.video.height,
            max_pixels: limits.max_pixels,
        },
    )
    .map_err(media_error)?;
    let mut frames = 0_u64;
    let mut first = None;
    let mut previous = None;
    loop {
        cancelled(cancel)?;
        let Some(frame) = decoder.next_frame().map_err(media_error)? else {
            break;
        };
        frames = frames.checked_add(1).ok_or("video frame count overflow")?;
        if frames > limits.max_frames || frames > expected.frames {
            return Err("decoded video frame limit or exact expectation exceeded".into());
        }
        if frame.width() != expected.video.width || frame.height() != expected.video.height {
            return Err("decoded video dimensions differ from expectation".into());
        }
        let position = check_video_position(
            decoder.position(),
            frames - 1,
            previous,
            rate,
            limits.max_seconds,
        )?;
        first.get_or_insert(position);
        previous = Some(position);
        checkpoint(Checkpoint::VideoFrame);
        cancelled(cancel)?;
    }
    if frames != expected.frames {
        return Err("decoded video frame count differs from exact expectation".into());
    }
    let first = first.ok_or("decoded video has no first presentation position")?;
    let last = previous.ok_or("decoded video has no last presentation position")?;
    // Checking last against (frames - 1)/fps also checks last + 1/fps
    // against frames/fps, using the same exact one-frame allowance.
    check_video_position(
        Some(last),
        expected.frames - 1,
        None,
        rate,
        limits.max_seconds,
    )?;
    let actual_video_seconds = last.as_f64() + rate.frame_duration().as_f64();
    if !actual_video_seconds.is_finite() || actual_video_seconds <= 0.0 {
        return Err("decoded video end duration is invalid".into());
    }
    drop(decoder);
    held.unchanged(path)?;

    let mut audio = Vec::with_capacity(info.audio_streams.len());
    for stream in &info.audio_streams {
        cancelled(cancel)?;
        if stream.sample_rate == 0 || stream.channels == 0 {
            return Err("audio probe has no usable sampling rate or channels".into());
        }
        let options = AudioOptions {
            format: SampleFormat::F32,
            rate: RATE,
            channels: CHANNELS,
            stream: Some(usize::try_from(stream.index).map_err(|_| "audio stream index overflow")?),
            ..Default::default()
        };
        let mut decoder =
            AudioDecoder::open_with_policy(path, &options, STRICT).map_err(media_error)?;
        let mut sample_frames = 0_u64;
        let mut peak = 0_f64;
        let mut sum_squares = 0_f64;
        loop {
            cancelled(cancel)?;
            let Some(samples) = decoder.next_f32().map_err(media_error)? else {
                break;
            };
            if samples.len() % usize::from(CHANNELS) != 0 {
                return Err("audio chunk has incomplete stereo sampling frames".into());
            }
            sample_frames = sample_frames
                .checked_add(
                    u64::try_from(samples.len() / usize::from(CHANNELS))
                        .map_err(|_| "audio count overflow")?,
                )
                .ok_or("audio count overflow")?;
            if sample_frames > max_audio_frames {
                return Err("decoded audio duration limit exceeded".into());
            }
            for value in samples {
                let value = f64::from(value);
                if !value.is_finite() {
                    return Err("decoded audio contains a non-finite sample".into());
                }
                peak = peak.max(value.abs());
                sum_squares += value * value;
            }
            if !sum_squares.is_finite() {
                return Err("decoded audio statistics overflow".into());
            }
            checkpoint(Checkpoint::AudioChunk);
            cancelled(cancel)?;
        }
        let audio_seconds = sample_frames as f64 / f64::from(RATE);
        if sample_frames == 0 || !audio_duration_matches(sample_frames, expected.frames, rate)? {
            return Err(
                "decoded audio duration differs from video beyond AAC and frame tolerance".into(),
            );
        }
        audio.push(AudioSummary {
            stream: stream.index,
            codec: stream.codec.clone(),
            source_rate: stream.sample_rate,
            source_channels: stream.channels,
            sample_frames,
            seconds: audio_seconds,
            peak,
            rms: (sum_squares / (sample_frames as f64 * f64::from(CHANNELS))).sqrt(),
        });
        held.unchanged(path)?;
    }
    checkpoint(Checkpoint::BeforeFinalHash);
    cancelled(cancel)?;
    held.unchanged(path)?;
    let (after, after_bytes, after_sha256) = hash_open(path, limits.max_bytes, cancel, checkpoint)?;
    if held.stamp != after.stamp || bytes != after_bytes || sha256 != after_sha256 {
        return Err("output changed or was replaced during verification".into());
    }
    held.unchanged(path)?;
    after.unchanged(path)?;
    cancelled(cancel)?;
    Ok(VerifiedOutput {
        path: path.to_path_buf(),
        summary: OutputSummary {
            bytes,
            sha256,
            video_codec: video.codec.clone(),
            width: video.width,
            height: video.height,
            rate_num: video.frame_rate.fps().numerator(),
            rate_den: video.frame_rate.fps().denominator(),
            video_frames: frames,
            first_video_position_seconds: first.as_f64(),
            last_video_position_seconds: last.as_f64(),
            video_seconds: actual_video_seconds,
            audio,
        },
        limits,
        checked_at: SystemTime::now(),
    })
}

fn validate(
    expected: OutputExpectation,
    limits: VerificationLimits,
) -> Result<(FrameRate, f64, u64), String> {
    if limits.max_bytes == 0
        || limits.max_dimension == 0
        || limits.max_pixels == 0
        || limits.max_frames == 0
        || !limits.max_fps.is_finite()
        || limits.max_fps <= 0.0
        || !limits.max_seconds.is_finite()
        || limits.max_seconds <= 0.0
    {
        return Err("verification limits must be positive and finite".into());
    }
    let settings = expected.video;
    if settings.width == 0
        || settings.height == 0
        || settings.width > limits.max_dimension
        || settings.height > limits.max_dimension
        || u64::from(settings.width)
            .checked_mul(u64::from(settings.height))
            .is_none_or(|pixels| pixels > limits.max_pixels)
        || settings.rate_num <= 0
        || settings.rate_den <= 0
        || expected.frames == 0
        || expected.frames > limits.max_frames
    {
        return Err("native output expectation exceeds size, frame or rate limits".into());
    }
    // i128 is wide enough for u64 frame count times positive i64 denominator.
    let fps = Rational::checked_new(i128::from(settings.rate_num), i128::from(settings.rate_den))
        .ok_or("expected frame rate cannot be represented")?;
    if fps.as_f64() > limits.max_fps {
        return Err("expected frame rate exceeds limit".into());
    }
    let duration = Rational::checked_new(
        i128::from(expected.frames)
            .checked_mul(i128::from(settings.rate_den))
            .ok_or("expected duration multiplication overflow")?,
        i128::from(settings.rate_num),
    )
    .ok_or("expected duration cannot be represented")?;
    let seconds = duration.as_f64();
    if !seconds.is_finite() || seconds <= 0.0 || seconds > limits.max_seconds {
        return Err("expected video duration exceeds limit".into());
    }
    // Validate the exact audio comparison arithmetic before any file read.
    audio_duration_matches(0, expected.frames, FrameRate::new(fps))?;
    let max_audio_frames =
        (limits.max_seconds * f64::from(RATE) + 2048.0 + f64::from(RATE) / fps.as_f64()).floor();
    // Comparing against 2^64 (exclusive) prevents saturating float-to-int casts.
    if !max_audio_frames.is_finite()
        || !(1.0..18_446_744_073_709_551_616.0).contains(&max_audio_frames)
    {
        return Err("audio duration bound cannot be represented".into());
    }
    Ok((FrameRate::new(fps), seconds, max_audio_frames as u64))
}

// The native output is CFR. Compare the actual presentation clock with
// the frozen zero-based frame index, allowing at most one frame of time-base
// quantization, with checked arithmetic rather than float subtraction.
fn check_video_position(
    position: Option<Rational>,
    index: u64,
    previous: Option<Rational>,
    rate: FrameRate,
    max_seconds: f64,
) -> Result<Rational, String> {
    let position = position.ok_or("decoded video frame has no time position")?;
    if position.is_negative()
        || !position.as_f64().is_finite()
        || position.as_f64() > max_seconds
        || previous.is_some_and(|before| position < before)
    {
        return Err("decoded video position is invalid, backwards or beyond duration limit".into());
    }
    let clock_den = i128::from(position.denominator());
    let rate_den = i128::from(rate.fps().denominator());
    let actual = i128::from(position.numerator())
        .checked_mul(i128::from(rate.fps().numerator()))
        .ok_or("video position multiplication overflow")?;
    let expected = i128::from(index)
        .checked_mul(clock_den)
        .and_then(|value| value.checked_mul(rate_den))
        .ok_or("expected video position multiplication overflow")?;
    let allowance = clock_den
        .checked_mul(rate_den)
        .ok_or("video position allowance overflow")?;
    if (actual - expected).abs() > allowance {
        return Err(
            "decoded video position differs from frozen CFR timing by more than one frame".into(),
        );
    }
    Ok(position)
}

// Compare sample duration against exact frames/fps in integer units. This
// accepts the exact AAC-plus-one-frame boundary without float subtraction loss.
fn audio_duration_matches(samples: u64, frames: u64, rate: FrameRate) -> Result<bool, String> {
    let num = i128::from(rate.fps().numerator());
    let den = i128::from(rate.fps().denominator());
    let actual = i128::from(samples)
        .checked_mul(num)
        .ok_or("audio duration overflow")?;
    let expected = i128::from(frames)
        .checked_mul(den)
        .and_then(|value| value.checked_mul(i128::from(RATE)))
        .ok_or("expected audio duration overflow")?;
    let allowance = 2048_i128 * num + i128::from(RATE) * den;
    Ok((actual - expected).abs() <= allowance)
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    identity: (u32, u64),
}

impl Stamp {
    fn of(file: &File) -> Result<Self, String> {
        let meta = file.metadata().map_err(io_error)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("output handle is not a regular file".into());
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: meta.len(),
            modified: meta.modified().map_err(io_error)?,
            created: meta.created().ok(),
            #[cfg(unix)]
            device: meta.dev(),
            #[cfg(unix)]
            inode: meta.ino(),
            #[cfg(unix)]
            changed: (meta.ctime(), meta.ctime_nsec()),
            #[cfg(windows)]
            identity: windows_identity(file)?,
        })
    }
}

struct HeldFile {
    file: File,
    stamp: Stamp,
}

impl HeldFile {
    fn unchanged(&self, path: &Path) -> Result<(), String> {
        let current = open_regular(path)?;
        if Stamp::of(&self.file)? != self.stamp || current.stamp != self.stamp {
            return Err("output changed, disappeared or was replaced".into());
        }
        Ok(())
    }
}

fn same_metadata(left: &Metadata, right: &Metadata) -> bool {
    if !left.is_file()
        || !right.is_file()
        || left.file_type().is_symlink()
        || right.file_type().is_symlink()
        || left.len() != right.len()
        || left.modified().ok() != right.modified().ok()
        || left.created().ok() != right.created().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn open_regular(path: &Path) -> Result<HeldFile, String> {
    if !path.is_absolute() {
        return Err("output path must be absolute".into());
    }
    // Check the entry before open too; nonblocking/no-follow additionally
    // prevent a FIFO or symlink substitution from hanging or following it.
    let before = fs::symlink_metadata(path).map_err(io_error)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err("output must be a regular file, without a terminal symlink".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(io_error)?;
    let opened = file.metadata().map_err(io_error)?;
    let after = fs::symlink_metadata(path).map_err(io_error)?;
    if !same_metadata(&before, &opened) || !same_metadata(&opened, &after) {
        return Err("output changed while opening its regular file".into());
    }
    let stamp = Stamp::of(&file)?;
    Ok(HeldFile { file, stamp })
}

fn hash_open(
    path: &Path,
    limit: u64,
    cancel: &AtomicBool,
    checkpoint: &mut impl FnMut(Checkpoint),
) -> Result<(HeldFile, u64, String), String> {
    cancelled(cancel)?;
    let mut held = open_regular(path)?;
    if held.stamp.len == 0 || held.stamp.len > limit {
        return Err("output file is empty or exceeds byte limit".into());
    }
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let read = held.file.read(&mut chunk).map_err(io_error)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or("output byte count overflow")?;
        if bytes > limit {
            return Err("output grew beyond byte limit".into());
        }
        hash.update(&chunk[..read]);
        checkpoint(Checkpoint::HashChunk);
        cancelled(cancel)?;
    }
    held.unchanged(path)?;
    if bytes != held.stamp.len {
        return Err("output read length differs from opened file length".into());
    }
    cancelled(cancel)?;
    Ok((held, bytes, format!("{:x}", hash.finalize())))
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("output verification cancelled".into())
    } else {
        Ok(())
    }
}
fn media_error(error: concat_media::Error) -> String {
    format!("output media check failed: {error}")
}
fn io_error(error: std::io::Error) -> String {
    format!("output file check failed: {error}")
}

// Stable Win32 file identity, without an external crate or unstable MetadataExt.
#[cfg(windows)]
fn windows_identity(file: &File) -> Result<(u32, u64), String> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Information {
        attributes: u32,
        creation: [u32; 2],
        access: [u32; 2],
        write: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut Information)
        -> i32;
    }
    let mut info = Information::default();
    // SAFETY: `file` owns a live handle and `info` has the Win32 layout and size.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    Ok((
        info.volume,
        (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_core::frame::Frame;
    use concat_media::{
        EncodeOptions, Encoder, FrameSink,
        audio::{self, AudioClip},
    };
    use std::io::Write;
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "concat-output-verification-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory.canonicalize().unwrap())
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn expectation() -> OutputExpectation {
        OutputExpectation {
            video: VideoSettings {
                width: 64,
                height: 64,
                rate_num: 30,
                rate_den: 1,
            },
            frames: 30,
            codec: VideoCodec::H264,
            audio_expected: false,
        }
    }
    fn movie(path: &Path, codec: VideoCodec) {
        try_movie(path, codec)
            .unwrap_or_else(|error| panic!("required real {codec:?} fixture failed: {error}"));
    }
    fn try_movie(path: &Path, codec: VideoCodec) -> concat_media::Result<()> {
        let options = EncodeOptions {
            codec,
            hardware: false,
            preset: "ultrafast".into(),
            crf: 20,
            ..Default::default()
        };
        let mut encoder = Encoder::create(path, 64, 64, FrameRate::THIRTY, &options)?;
        for n in 0..30 {
            let mut frame = Frame::black(64, 64);
            frame.fill([20 + n * 3, 60, 160, 255]);
            encoder.write_frame(&frame)?;
        }
        encoder.finish()
    }
    fn wav(path: &Path, seconds: u32, silent: bool) {
        // Small deterministic source; production audio decoding stays streaming.
        let mut data = Vec::new();
        for n in 0..(48_000 * seconds) {
            let value = if silent {
                0
            } else {
                ((f64::from(n) * 997.0 * std::f64::consts::TAU / 48_000.0).sin() * 12000.0) as i16
            };
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut file = File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + data.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&48_000_u32.to_le_bytes()).unwrap();
        file.write_all(&192_000_u32.to_le_bytes()).unwrap();
        file.write_all(&4_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
        file.write_all(&data).unwrap();
    }
    fn with_audio(d: &Scratch, name: &str, seconds: u32, silent: bool) -> PathBuf {
        let video = d.path(&format!("{name}-video.mp4"));
        movie(&video, VideoCodec::H264);
        let source = d.path(&format!("{name}-source.wav"));
        wav(&source, seconds, silent);
        let mixed = d.path(&format!("{name}-mixed.m4a"));
        let clip = AudioClip {
            path: source,
            stream: None,
            start: 0.0,
            duration: f64::from(seconds),
            source_start: 0.0,
            speed: 1.0,
            preserve_pitch: true,
            volume: 1.0,
            volume_curve: Default::default(),
            fade_in: 0.0,
            fade_out: 0.0,
            filter_chain: String::new(),
        };
        audio::mix_to_file_with_policy(&[clip], f64::from(seconds), &mixed, STRICT)
            .expect("required real AAC encoder and mix must work; never skip");
        let output = d.path(&format!("{name}.mp4"));
        audio::mux_with_policy(&video, &mixed, &output, STRICT).unwrap();
        output
    }
    fn stored_fixture(d: &Scratch, name: &str, bytes: &[u8]) -> PathBuf {
        let path = d.path(name);
        fs::write(&path, bytes).unwrap();
        path
    }
    // These assertions exercise the real decoders separately from the verifier,
    // so a malformed fixture cannot masquerade as a duration rejection.
    fn independently_decode_fixture(path: &Path, audio_streams: usize) -> Vec<u64> {
        let info = probe_with_policy(path, STRICT).unwrap();
        assert_eq!(info.audio_streams.len(), audio_streams);
        let mut video =
            Decoder::open_with_policy(path, &DecodeOptions::default().in_software(), STRICT)
                .unwrap();
        let mut count = 0;
        while let Some(frame) = video.next_frame().unwrap() {
            assert_eq!((frame.width(), frame.height()), (64, 64));
            count += 1;
            assert!(count <= 30);
        }
        assert_eq!(count, 30);
        let mut audio_counts = Vec::new();
        for stream in info.audio_streams {
            assert_eq!(stream.codec, "aac");
            let mut decoder = AudioDecoder::open_with_policy(
                path,
                &AudioOptions {
                    format: SampleFormat::F32,
                    stream: Some(stream.index as usize),
                    ..Default::default()
                },
                STRICT,
            )
            .unwrap();
            let mut frames = 0_u64;
            while let Some(chunk) = decoder.next_f32().unwrap() {
                assert_eq!(chunk.len() % 2, 0);
                assert!(chunk.iter().all(|value| value.is_finite()));
                frames += (chunk.len() / 2) as u64;
                assert!(frames < 200_000);
            }
            audio_counts.push(frames);
        }
        audio_counts
    }
    fn verify(path: &Path, expected: OutputExpectation) -> Result<VerifiedOutput, String> {
        verify_output(
            path,
            expected,
            VerificationLimits::default(),
            &AtomicBool::new(false),
        )
    }
    fn assert_rejected(path: &Path, expected: OutputExpectation, limits: VerificationLimits) {
        assert!(verify_output(path, expected, limits, &AtomicBool::new(false)).is_err());
        assert!(path.exists(), "verification errors must not delete outputs");
    }

    #[test]
    fn source_frame_guard_rejects_sizes_that_old_decoder_would_normalize() {
        let d = Scratch::new();
        let path = stored_fixture(
            &d,
            "source-dynamic-64-128.mkv",
            include_bytes!("../../concat-media/tests/fixtures/source-dynamic-64-128.mkv"),
        );
        // Establish the old path's actual behavior independently: the
        // container reports 128, and conversion hides the first four 64 frames.
        let info = probe_with_policy(&path, STRICT).unwrap();
        let video = info.require_video().unwrap();
        assert_eq!(video.codec, "h264");
        assert_eq!((video.width, video.height), (128, 128));
        assert_eq!(video.frame_rate, FrameRate::THIRTY);
        assert!(info.audio_streams.is_empty());
        let mut old =
            Decoder::open_with_policy(&path, &DecodeOptions::default().in_software(), STRICT)
                .unwrap();
        let mut count = 0;
        while let Some(frame) = old.next_frame().unwrap() {
            assert_eq!((frame.width(), frame.height()), (128, 128));
            count += 1;
            assert!(count <= 8);
        }
        assert_eq!(count, 8);
        drop(old);
        let expected = OutputExpectation {
            video: VideoSettings {
                width: 128,
                height: 128,
                rate_num: 30,
                rate_den: 1,
            },
            frames: 8,
            codec: VideoCodec::H264,
            audio_expected: false,
        };
        let error = verify(&path, expected).unwrap_err();
        assert!(error.starts_with("output media check failed:"), "{error}");
        assert!(
            error.contains("source frame displays as 64x64, expected 128x128"),
            "{error}"
        );
        assert!(
            path.exists(),
            "a frame guard failure must not delete output"
        );
    }

    #[test]
    fn real_h264_all_frames_and_independently_opened_copy() {
        let d = Scratch::new();
        let path = d.path("video.mp4");
        movie(&path, VideoCodec::H264);
        let output = verify(&path, expectation()).unwrap();
        let summary = output.summary();
        assert_eq!(summary.video_codec, "h264");
        assert_eq!((summary.width, summary.height), (64, 64));
        assert_eq!((summary.rate_num, summary.rate_den), (30, 1));
        assert_eq!(summary.video_frames, 30);
        assert_eq!(summary.first_video_position_seconds, 0.0);
        assert!((summary.last_video_position_seconds - 29.0 / 30.0).abs() < 0.000001);
        assert!((summary.video_seconds - 1.0).abs() < 0.000001);
        assert!(summary.audio.is_empty());
        assert_eq!(summary.bytes, fs::metadata(&path).unwrap().len());
        assert_eq!(
            summary.sha256,
            format!("{:x}", Sha256::digest(fs::read(&path).unwrap()))
        );
        let copy = d.path("independent-copy.mp4");
        fs::copy(&path, &copy).unwrap();
        assert_ne!(
            open_regular(&path).unwrap().stamp,
            open_regular(&copy).unwrap().stamp
        );
        // A moved original does not invalidate separately checked identical bytes.
        fs::rename(&path, d.path("original-moved.mp4")).unwrap();
        let copied = output
            .recheck_same_content(&copy, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(copied.path(), copy);
        assert_eq!(copied.summary().sha256, summary.sha256);
        assert!(copied.checked_at() >= output.checked_at());
        OpenOptions::new()
            .append(true)
            .open(&copy)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        assert!(
            output
                .recheck_same_content(&copy, &AtomicBool::new(false))
                .is_err()
        );
        assert!(
            output
                .recheck_same_content(d.path("missing.mp4"), &AtomicBool::new(false))
                .is_err()
        );
    }

    #[test]
    fn real_aac_mix_full_decode_accepts_tone_and_normal_silence() {
        let d = Scratch::new();
        for silent in [false, true] {
            let path = with_audio(&d, if silent { "silent" } else { "tone" }, 1, silent);
            let mut expected = expectation();
            expected.audio_expected = true;
            let output = verify(&path, expected).unwrap();
            let audio = &output.summary().audio;
            assert_eq!(audio.len(), 1);
            let audio = &audio[0];
            assert_eq!(audio.codec, "aac");
            assert_eq!(audio.source_rate, 48_000);
            assert_eq!(audio.source_channels, 2);
            assert!(audio.sample_frames >= 48_000 - 2048);
            assert!((audio.seconds - 1.0).abs() <= 2048.0 / 48_000.0 + 1.0 / 30.0);
            assert!(audio.peak.is_finite() && audio.rms.is_finite());
            if silent {
                assert!(audio.peak < 0.00001)
            } else {
                assert!(audio.peak > 0.1 && audio.rms > 0.05)
            }
        }
    }

    #[test]
    fn real_output_rejects_each_independent_wrong_expectation() {
        let d = Scratch::new();
        let path = d.path("video.mp4");
        movie(&path, VideoCodec::H264);
        for change in 0..8 {
            let mut expected = expectation();
            match change {
                0 => expected.video.width = 62,
                1 => expected.video.height = 62,
                2 => expected.video.rate_num = 25,
                3 => expected.video.rate_den = 2,
                4 => expected.frames = 29,
                5 => expected.frames = 31,
                6 => expected.codec = VideoCodec::Hevc,
                _ => expected.audio_expected = true,
            }
            assert_rejected(&path, expected, VerificationLimits::default());
        }
        let path = with_audio(&d, "tone", 1, false);
        assert_rejected(&path, expectation(), VerificationLimits::default());
    }

    #[test]
    fn invalid_expectations_and_limits_fail_before_codec_or_rational_panics() {
        let d = Scratch::new();
        let path = d.path("not-even-opened.mp4");
        for change in 0..10 {
            let mut expected = expectation();
            match change {
                0 => expected.video.rate_num = 0,
                1 => expected.video.rate_den = 0,
                2 => expected.video.rate_num = i64::MIN,
                3 => expected.video.rate_den = i64::MIN,
                4 => expected.video.rate_num = i64::MAX,
                5 => expected.video.rate_den = i64::MAX,
                6 => expected.video.width = 0,
                7 => expected.video.height = u32::MAX,
                8 => expected.frames = 0,
                _ => expected.frames = u64::MAX,
            }
            assert!(verify(&path, expected).is_err());
        }
        let limits = VerificationLimits::default();
        for change in 0..8 {
            let mut bad = limits;
            match change {
                0 => bad.max_bytes = 0,
                1 => bad.max_dimension = 0,
                2 => bad.max_pixels = 0,
                3 => bad.max_frames = 0,
                4 => bad.max_fps = f64::NAN,
                5 => bad.max_fps = f64::INFINITY,
                6 => bad.max_seconds = f64::NAN,
                _ => bad.max_seconds = -1.0,
            }
            assert!(validate(expectation(), bad).is_err());
        }
        let mut expected = expectation();
        expected.video.rate_num = 1;
        expected.video.rate_den = i64::MAX;
        expected.frames = u64::MAX;
        assert!(
            validate(
                expected,
                VerificationLimits {
                    max_frames: u64::MAX,
                    max_seconds: f64::MAX,
                    ..limits
                }
            )
            .is_err()
        );
        // Equal rational rates remain equal after safe normalization.
        let mut equivalent = expectation();
        equivalent.video.rate_num = 60;
        equivalent.video.rate_den = 2;
        assert_eq!(validate(equivalent, limits).unwrap().0, FrameRate::THIRTY);
    }

    #[test]
    fn actual_cfr_positions_reject_shift_gaps_backwards_and_overflow() {
        let rate = FrameRate::THIRTY;
        let check = |position, index, previous| {
            check_video_position(position, index, previous, rate, 14_400.0)
        };
        assert!(check(Some(Rational::ZERO), 0, None).is_ok());
        assert!(check(Some(Rational::new(1, 30)), 0, None).is_ok());
        assert!(check(Some(Rational::new(1001, 30_000)), 0, None).is_err());
        assert!(check(Some(Rational::from_int(1)), 0, None).is_err());
        assert!(check(Some(Rational::from_int(1)), 5, Some(Rational::new(4, 30))).is_err());
        assert!(check(Some(Rational::new(4, 30)), 5, Some(Rational::new(5, 30))).is_err());
        assert!(check(Some(Rational::new(-1, 30)), 0, None).is_err());
        assert!(check(None, 0, None).is_err());
        assert!(check(Some(Rational::from_int(14_401)), 0, None).is_err());
        // The last-position + one-frame check allows precisely one frame,
        // and cannot accept a distant end merely because frame count matches.
        assert!(check(Some(Rational::ONE), 29, None).is_ok());
        assert!(check(Some(Rational::new(1001, 1000)), 29, None).is_err());
        assert!(
            check_video_position(
                Some(Rational::new(1, i64::MAX)),
                u64::MAX,
                None,
                FrameRate::new(Rational::new(i64::MAX - 1, i64::MAX)),
                14_400.0
            )
            .is_err()
        );
    }

    #[test]
    fn exact_audio_tolerance_boundary_is_inclusive_without_float_rounding() {
        assert!(audio_duration_matches(48_000 + 2048 + 1600, 30, FrameRate::THIRTY).unwrap());
        assert!(!audio_duration_matches(48_000 + 2048 + 1601, 30, FrameRate::THIRTY).unwrap());
        assert!(audio_duration_matches(48_000 - 2048 - 1600, 30, FrameRate::THIRTY).unwrap());
        assert!(!audio_duration_matches(48_000 - 2048 - 1601, 30, FrameRate::THIRTY).unwrap());
        assert!(audio_duration_matches(48_048 + 2048 + 1601, 30, FrameRate::NTSC_30).unwrap());
        assert!(!audio_duration_matches(48_048 + 2048 + 1602, 30, FrameRate::NTSC_30).unwrap());
    }

    #[test]
    fn real_output_enforces_bytes_frames_pixels_dimensions_rate_and_duration_limits() {
        let d = Scratch::new();
        let path = d.path("video.mp4");
        movie(&path, VideoCodec::H264);
        for change in 0..6 {
            let mut limits = VerificationLimits::default();
            match change {
                0 => limits.max_bytes = fs::metadata(&path).unwrap().len() - 1,
                1 => limits.max_frames = 29,
                2 => limits.max_pixels = 64 * 64 - 1,
                3 => limits.max_dimension = 63,
                4 => limits.max_fps = 29.99,
                _ => limits.max_seconds = 0.99,
            }
            assert_rejected(&path, expectation(), limits);
        }
        let path = with_audio(&d, "tone", 1, false);
        let mut expected = expectation();
        expected.audio_expected = true;
        assert_rejected(
            &path,
            expected,
            VerificationLimits {
                max_audio_streams: 0,
                ..Default::default()
            },
        );
    }

    #[test]
    fn real_aac_duration_mismatch_and_streaming_audio_limit_fail() {
        let d = Scratch::new();
        let mut expected = expectation();
        expected.audio_expected = true;
        let path = stored_fixture(
            &d,
            "long-audio.mp4",
            include_bytes!("../tests/fixtures/output-long-audio.mp4"),
        );
        let audio = independently_decode_fixture(&path, 1);
        assert!((audio[0] as f64 / 48_000.0 - 2.0).abs() < 0.05);
        let error = verify(&path, expected).unwrap_err();
        assert!(error.contains("audio duration differs"), "{error}");
        let error = verify_output(
            &path,
            expected,
            VerificationLimits {
                max_seconds: 1.0,
                ..Default::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("audio duration limit"), "{error}");
        assert!(path.exists());
    }

    #[test]
    fn every_real_audio_stream_is_fully_decoded_including_a_short_second_stream() {
        let d = Scratch::new();
        let mut expected = expectation();
        expected.audio_expected = true;
        let path = stored_fixture(
            &d,
            "short-second.mp4",
            include_bytes!("../tests/fixtures/output-short-second-audio.mp4"),
        );
        let audio = independently_decode_fixture(&path, 2);
        assert!((audio[0] as f64 / 48_000.0 - 1.0).abs() < 0.05);
        assert!((audio[1] as f64 / 48_000.0 - 0.2).abs() < 0.05);
        let error = verify(&path, expected).unwrap_err();
        assert!(error.contains("audio duration differs"), "{error}");
        assert!(path.exists());
    }

    #[test]
    fn corrupt_truncated_empty_missing_relative_and_directory_files_fail() {
        let d = Scratch::new();
        let path = d.path("truncated.mp4");
        movie(&path, VideoCodec::H264);
        let length = fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(length / 3)
            .unwrap();
        assert_rejected(&path, expectation(), VerificationLimits::default());
        for (name, bytes) in [
            ("corrupt.mp4", b"not a media file".as_slice()),
            ("empty.mp4", b"".as_slice()),
        ] {
            let file = d.path(name);
            fs::write(&file, bytes).unwrap();
            assert_rejected(&file, expectation(), VerificationLimits::default());
        }
        assert!(verify(&d.path("missing.mp4"), expectation()).is_err());
        assert!(verify(Path::new("relative.mp4"), expectation()).is_err());
        assert!(verify(&d.0, expectation()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_symlinks_fifo_and_socket_are_rejected_without_waiting_for_readers() {
        use std::os::unix::{fs::symlink, net::UnixListener};
        let d = Scratch::new();
        let path = d.path("video.mp4");
        movie(&path, VideoCodec::H264);
        let link = d.path("link.mp4");
        symlink(&path, &link).unwrap();
        assert_rejected(&link, expectation(), VerificationLimits::default());
        let output = verify(&path, expectation()).unwrap();
        assert!(
            output
                .recheck_same_content(&link, &AtomicBool::new(false))
                .is_err()
        );
        let socket = d.path("socket.mp4");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert_rejected(&socket, expectation(), VerificationLimits::default());
        #[cfg(target_os = "macos")]
        type Mode = u16;
        #[cfg(not(target_os = "macos"))]
        type Mode = u32;
        unsafe extern "C" {
            fn mkfifo(path: *const std::ffi::c_char, mode: Mode) -> i32;
        }
        use std::os::unix::ffi::OsStrExt;
        let fifo = d.path("fifo.mp4");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is terminated, alive, and is a new test-owned entry.
        assert_eq!(unsafe { mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_rejected(&fifo, expectation(), VerificationLimits::default());
    }

    #[test]
    fn deterministic_cancel_during_hash_video_audio_and_postcheck_never_constructs_proof() {
        let d = Scratch::new();
        let path = with_audio(&d, "tone", 1, false);
        let mut expected = expectation();
        expected.audio_expected = true;
        for target in [
            Checkpoint::HashChunk,
            Checkpoint::AfterFirstHash,
            Checkpoint::VideoFrame,
            Checkpoint::AudioChunk,
            Checkpoint::BeforeFinalHash,
        ] {
            let cancel = AtomicBool::new(false);
            let mut reached = false;
            let result = verify_with_checkpoint(
                &path,
                expected,
                VerificationLimits::default(),
                &cancel,
                &mut |point| {
                    if point == target {
                        reached = true;
                        cancel.store(true, Ordering::Release);
                    }
                },
            );
            assert!(reached, "checkpoint not exercised: {target:?}");
            assert!(result.unwrap_err().contains("cancelled"));
            assert!(path.exists());
        }
        assert!(
            verify_output(
                &path,
                expected,
                VerificationLimits::default(),
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .contains("cancelled")
        );
        let proof = verify(&path, expected).unwrap();
        assert!(
            proof
                .recheck_same_content(&path, &AtomicBool::new(true))
                .unwrap_err()
                .contains("cancelled")
        );
    }

    #[test]
    fn deterministic_in_place_changes_during_hash_decode_or_postcheck_fail() {
        let d = Scratch::new();
        let original = with_audio(&d, "tone", 1, false);
        let mut expected = expectation();
        expected.audio_expected = true;
        for target in [
            Checkpoint::HashChunk,
            Checkpoint::VideoFrame,
            Checkpoint::AudioChunk,
            Checkpoint::BeforeFinalHash,
        ] {
            let path = d.path(&format!("change-{target:?}.mp4"));
            fs::copy(&original, &path).unwrap();
            let mut reached = false;
            let result = verify_with_checkpoint(
                &path,
                expected,
                VerificationLimits::default(),
                &AtomicBool::new(false),
                &mut |point| {
                    if point == target && !reached {
                        reached = true;
                        OpenOptions::new()
                            .append(true)
                            .open(&path)
                            .unwrap()
                            .write_all(b"test change")
                            .unwrap();
                    }
                },
            );
            assert!(reached, "checkpoint not exercised: {target:?}");
            assert!(result.is_err(), "changed file must not produce proof");
            assert!(path.exists());
        }
    }

    #[test]
    fn original_path_replacement_with_identical_bytes_and_disappearance_fail() {
        let d = Scratch::new();
        let original = d.path("original.mp4");
        movie(&original, VideoCodec::H264);
        for disappear in [false, true] {
            let path = d.path(if disappear {
                "disappear.mp4"
            } else {
                "replace.mp4"
            });
            fs::copy(&original, &path).unwrap();
            let mut reached = false;
            let result = verify_with_checkpoint(
                &path,
                expectation(),
                VerificationLimits::default(),
                &AtomicBool::new(false),
                &mut |point| {
                    if point == Checkpoint::AfterFirstHash && !reached {
                        reached = true;
                        fs::rename(
                            &path,
                            d.path(if disappear {
                                "moved-away.mp4"
                            } else {
                                "replaced-old.mp4"
                            }),
                        )
                        .unwrap();
                        if !disappear {
                            fs::copy(&original, &path).unwrap();
                        }
                    }
                },
            );
            assert!(reached);
            assert!(result.is_err());
            assert_eq!(path.exists(), !disappear);
        }
    }

    #[test]
    fn change_in_final_hash_pass_is_rejected() {
        let d = Scratch::new();
        let path = d.path("video.mp4");
        movie(&path, VideoCodec::H264);
        let mut final_hash = false;
        let mut changed = false;
        let result = verify_with_checkpoint(
            &path,
            expectation(),
            VerificationLimits::default(),
            &AtomicBool::new(false),
            &mut |point| {
                if point == Checkpoint::BeforeFinalHash {
                    final_hash = true;
                }
                if final_hash && point == Checkpoint::HashChunk && !changed {
                    changed = true;
                    OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap()
                        .write_all(b"changed")
                        .unwrap();
                }
            },
        );
        assert!(changed);
        assert!(result.is_err());
        assert!(path.exists());
    }

    #[test]
    fn optional_real_hevc_and_av1_capabilities_are_reported_explicitly() {
        let d = Scratch::new();
        for codec in [VideoCodec::Hevc, VideoCodec::Av1] {
            let path = d.path(&format!("{}.mp4", codec.name()));
            match try_movie(&path, codec) {
                Err(concat_media::Error::Missing {
                    what: "encoder",
                    name,
                }) => {
                    eprintln!(
                        "OUTPUT_VERIFICATION_CODEC {codec:?}: software encoder {name} unavailable; no full-decode evidence"
                    );
                    continue;
                }
                Err(error) => panic!("real optional {codec:?} encode failed: {error}"),
                Ok(()) => {}
            }
            let mut expected = expectation();
            expected.codec = codec;
            let output = verify(&path, expected).unwrap();
            assert_eq!(output.summary().video_codec, codec.name());
            assert_eq!(output.summary().video_frames, 30);
            eprintln!(
                "OUTPUT_VERIFICATION_CODEC {codec:?}: real encode and complete software decode passed"
            );
        }
    }
}
