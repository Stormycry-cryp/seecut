// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

use super::*;
use crate::{EncodeOptions, Encoder, FrameSink, InputPolicy};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "concat-source-bit-depth-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn movie(path: &Path, ten_bit: bool) {
    let mut encoder = Encoder::create(
        path,
        64,
        64,
        FrameRate::THIRTY,
        &EncodeOptions {
            hardware: false,
            ten_bit,
            preset: "ultrafast".into(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|error| panic!("required real H264 ten_bit={ten_bit} encoder failed: {error}"));
    for n in 0..3 {
        let mut frame = Frame::black(64, 64);
        frame.fill([40 + n * 30, 90, 160, 255]);
        encoder.write_frame(&frame).unwrap();
    }
    encoder.finish().unwrap();
}
fn open(path: &Path, options: &DecodeOptions) -> Decoder {
    Decoder::open_with_policy(path, options, InputPolicy::SelfContainedLocal).unwrap()
}

#[test]
fn actual_descriptors_report_depth_and_refuse_missing_hardware_or_mixed_components() {
    assert_eq!(source_component_depth(Pixel::YUV420P), Some(8));
    assert_eq!(source_component_depth(Pixel::YUV420P10LE), Some(10));
    assert_eq!(source_component_depth(Pixel::RGBA), Some(8));
    assert_eq!(source_component_depth(Pixel::None), None);
    assert_eq!(source_component_depth(Pixel::VAAPI), None);
    // Real FFmpeg RGB565 has 5, 6 and 5 component bits; it is not uniform 8-bit.
    assert_eq!(source_component_depth(Pixel::RGB565LE), None);
}

#[test]
fn ten_bit_source_remains_ten_bit_after_scale_filters_and_rgba_output() {
    let d = Scratch::new();
    let path = d.path("ten.mp4");
    movie(&path, true);
    let options = DecodeOptions::default()
        .in_software()
        .scaled_to(32, 32)
        .prefiltered("format=rgba")
        .filtered("format=rgba");
    let mut decoder = open(&path, &options);
    assert_eq!(decoder.source_bit_depth(), None);
    let mut count = 0;
    while let Some(frame) = decoder.next_frame().unwrap() {
        assert_eq!((frame.width(), frame.height()), (32, 32));
        assert_eq!(frame.pixels().len(), 32 * 32 * 4);
        assert_eq!(decoder.source_bit_depth(), Some(10));
        count += 1;
        assert!(count <= 3);
    }
    assert_eq!(count, 3);
    assert_eq!(
        decoder.source_bit_depth(),
        Some(10),
        "EOF retains last returned source fact"
    );
}

#[test]
fn looping_seek_and_software_reopen_keep_source_fact_tied_to_successful_frames() {
    let d = Scratch::new();
    let path = d.path("ten-loop.mp4");
    movie(&path, true);
    let options = DecodeOptions::default()
        .in_software()
        .scaled_to(32, 32)
        .repeating();
    let mut decoder = open(&path, &options);
    for _ in 0..8 {
        assert!(decoder.next_frame().unwrap().is_some());
        assert_eq!(decoder.source_bit_depth(), Some(10));
        assert_eq!(decoder.current.as_ref().unwrap().bit_depth, Some(10));
    }
    assert_eq!(
        decoder.produced(),
        8,
        "loop emits more frames than the three encoded sources"
    );
    decoder.seek(Rational::new(1, 30)).unwrap();
    assert_eq!(decoder.source_bit_depth(), None);
    assert!(decoder.next_frame().unwrap().is_some());
    assert_eq!(decoder.source_bit_depth(), Some(10));
    decoder
        .resume_in_software(Some(Rational::ZERO), false)
        .unwrap();
    assert_eq!(decoder.source_bit_depth(), None);
    assert!(decoder.next_frame().unwrap().is_some());
    assert_eq!(decoder.source_bit_depth(), Some(10));
}

#[test]
fn paced_real_sources_keep_current_depth_separate_from_pending_metadata() {
    let d = Scratch::new();
    let eight = d.path("eight.mp4");
    let ten = d.path("ten.mp4");
    movie(&eight, false);
    movie(&ten, true);
    // Repeating preserves the original source buffer instead of letting
    // the buffer source consume it. Stop repetition after the three real frames.
    let options = DecodeOptions::default().in_software().repeating();
    let mut decoder = open(&eight, &options);
    for _ in 0..3 {
        assert!(decoder.next_frame().unwrap().is_some());
    }
    decoder.options.looping = false;
    assert!(decoder.next_frame().unwrap().is_none());
    assert_eq!(decoder.source_bit_depth(), Some(8));
    let mut next = open(&ten, &options);
    assert!(next.next_frame().unwrap().is_some());
    assert_eq!(next.source_bit_depth(), Some(10));
    // A deterministic pacer seam using actual decoded 8/10-bit Source frames,
    // not a synthetic bit-depth value or a claim about one mixed-depth file.
    // The drained 8-bit decoder supplies EOF after the queued real 10-bit Source.
    let mut current = decoder.current.take().unwrap();
    current.pts = Some(Rational::ZERO);
    let mut pending = next.current.take().unwrap();
    pending.pts = Some(Rational::new(1, 30));
    assert_eq!(source_component_depth(current.frame.format()), Some(8));
    assert_eq!(source_component_depth(pending.frame.format()), Some(10));
    decoder.current = Some(current);
    decoder.pending = Some(pending);
    decoder.options.frame_rate = Some(FrameRate::SIXTY);
    decoder.origin = Rational::ZERO;
    decoder.tick = 0;
    decoder.source_done = false;
    decoder.last_source_bit_depth = None;
    for _ in 0..2 {
        assert!(decoder.next_frame().unwrap().is_some());
        assert_eq!(decoder.source_bit_depth(), Some(8));
        assert_eq!(decoder.pending.as_ref().unwrap().bit_depth, Some(10));
    }
    assert!(decoder.next_frame().unwrap().is_some());
    assert_eq!(decoder.source_bit_depth(), Some(10));
    assert!(decoder.pending.is_none());
    assert_eq!(decoder.current.as_ref().unwrap().bit_depth, Some(10));
}
