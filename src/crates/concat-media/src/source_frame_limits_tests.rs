// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new(bytes: &[u8], extension: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "concat-source-limits-{}-{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            extension,
        ));
        std::fs::write(&path, bytes).expect("write embedded fixture");
        Self(path)
    }
    fn dynamic() -> Self {
        Self::new(
            include_bytes!("../tests/fixtures/source-dynamic-64-128.mkv"),
            "mkv",
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn limits(width: u32, height: u32) -> SourceFrameLimits {
    SourceFrameLimits {
        displayed_width: width,
        displayed_height: height,
        max_pixels: 16_777_216,
    }
}
fn software() -> DecodeOptions {
    DecodeOptions::default().in_software().scaled_to(64, 64)
}
fn strict(path: &Path, options: &DecodeOptions, limits: SourceFrameLimits) -> Decoder {
    Decoder::open_with_source_limits(
        path,
        options,
        crate::InputPolicy::SelfContainedLocal,
        limits,
    )
    .unwrap_or_else(|error| panic!("strict fixture opens: {error}"))
}
fn expect_raw_shape_error(decoder: &mut Decoder) {
    match decoder.next_frame() {
        Err(Error::Probe { detail, .. }) => {
            assert!(
                detail.contains("source frame displays as 128x128"),
                "{detail}"
            );
        }
        Err(other) => panic!("expected original frame size error, got {other}"),
        Ok(_) => panic!("expected original frame size error"),
    }
}

#[test]
fn dynamic_source_is_fully_readable_and_legacy_scaling_hides_its_change() {
    let fixture = Fixture::dynamic();
    let mut decoder = Decoder::open_with_policy(
        &fixture.0,
        &software(),
        crate::InputPolicy::SelfContainedLocal,
    )
    .expect("legacy opens");
    let mut count = 0;
    while let Some(frame) = decoder.next_frame().expect("every fixture frame decodes") {
        assert_eq!((frame.width(), frame.height()), (64, 64));
        count += 1;
    }
    assert_eq!(
        count, 8,
        "four source 64x64 frames, then four 128x128 frames"
    );
}

#[test]
fn default_legacy_normalizes_all_frames_to_probe_size_but_strict_checks_raw_size() {
    let fixture = Fixture::dynamic();
    let options = DecodeOptions::default().in_software();
    let mut legacy =
        Decoder::open_with_policy(&fixture.0, &options, crate::InputPolicy::SelfContainedLocal)
            .expect("default legacy opens");
    assert_eq!((legacy.width(), legacy.height()), (128, 128));
    let mut count = 0;
    while let Some(frame) = legacy.next_frame().expect("full default legacy decode") {
        assert_eq!((frame.width(), frame.height()), (128, 128));
        count += 1;
    }
    assert_eq!(count, 8);
    let mut guarded = strict(&fixture.0, &options, limits(128, 128));
    match guarded.next_frame() {
        Err(Error::Probe { detail, .. }) => assert_eq!(
            detail,
            "source frame displays as 64x64, expected 128x128 within 16777216 pixels",
        ),
        Err(other) => panic!("expected raw shape failure, got {other}"),
        Ok(_) => panic!("strict must reject original 64x64 first frame"),
    }
    assert_eq!(guarded.produced(), 0);
}

#[test]
fn strict_rejects_the_first_changed_raw_frame_before_scaling_and_filters() {
    let fixture = Fixture::dynamic();
    let options = software()
        .prefiltered("scale=64:64")
        .filtered("scale=64:64");
    let mut decoder = strict(&fixture.0, &options, limits(64, 64));
    for _ in 0..4 {
        assert!(
            decoder
                .next_frame()
                .expect("original dimensions accepted")
                .is_some()
        );
    }
    expect_raw_shape_error(&mut decoder);
    assert_eq!(decoder.produced(), 4);
}

#[test]
fn strict_checks_frames_before_seek_discard_and_pacer_drop() {
    let fixture = Fixture::dynamic();
    let options = software().starting_at(Rational::new(1, 5));
    let mut decoder = strict(&fixture.0, &options, limits(64, 64));
    expect_raw_shape_error(&mut decoder);
    assert_eq!(decoder.produced(), 0);

    let mut decoder = strict(
        &fixture.0,
        &software().at_rate(FrameRate::new(Rational::new(1, 1))),
        limits(64, 64),
    );
    assert!(decoder.next_frame().expect("first paced frame").is_some());
    expect_raw_shape_error(&mut decoder);
}

#[test]
fn source_limit_is_installed_in_codec_and_kept_on_seek_and_software_reopen() {
    let fixture = Fixture::dynamic();
    let limit = limits(64, 64);
    let mut decoder = strict(&fixture.0, &software(), limit);
    // SAFETY: this test only reads a scalar in the live, solely owned codec.
    assert_eq!(
        unsafe { (*decoder.decoder.as_ptr()).max_pixels },
        limit.max_pixels as i64
    );
    assert!(decoder.next_frame().expect("first frame").is_some());
    decoder.seek(Rational::ZERO).expect("seek");
    assert_eq!(
        unsafe { (*decoder.decoder.as_ptr()).max_pixels },
        limit.max_pixels as i64
    );
    decoder
        .resume_in_software(Some(Rational::ZERO), false)
        .expect("software reopen");
    assert_eq!(
        unsafe { (*decoder.decoder.as_ptr()).max_pixels },
        limit.max_pixels as i64
    );
    for _ in 0..4 {
        assert!(
            decoder
                .next_frame()
                .expect("original dimensions after reopen")
                .is_some()
        );
    }
    expect_raw_shape_error(&mut decoder);
    decoder
        .seek(Rational::new(1, 5))
        .expect("seek into changed region");
    expect_raw_shape_error(&mut decoder);
}

#[test]
fn source_limits_accept_rotation_and_allow_codec_padding() {
    let rotated = Fixture::new(
        include_bytes!("../tests/fixtures/source-rotated-64x48.mp4"),
        "mp4",
    );
    let padded = Fixture::new(
        include_bytes!("../tests/fixtures/source-padded-64x50.mp4"),
        "mp4",
    );
    for (fixture, width, height) in [(&rotated, 48, 64), (&padded, 64, 50)] {
        let mut bound = limits(width, height);
        bound.max_pixels = 64 * 64;
        let mut decoder = strict(&fixture.0, &software(), bound);
        assert_eq!(unsafe { (*decoder.decoder.as_ptr()).max_pixels }, 64 * 64);
        let mut count = 0;
        while decoder
            .next_frame()
            .expect("valid rotated or padded frame")
            .is_some()
        {
            count += 1;
        }
        assert_eq!(count, 4);
    }
}

#[test]
fn invalid_source_limits_fail_before_opening_any_input() {
    let valid = limits(64, 64);
    let invalid = [
        SourceFrameLimits {
            displayed_width: 0,
            ..valid
        },
        SourceFrameLimits {
            displayed_height: 0,
            ..valid
        },
        SourceFrameLimits {
            displayed_width: u32::MAX,
            ..valid
        },
        SourceFrameLimits {
            displayed_height: u32::MAX,
            ..valid
        },
        SourceFrameLimits {
            max_pixels: 0,
            ..valid
        },
        SourceFrameLimits {
            max_pixels: 64 * 64 - 1,
            ..valid
        },
        SourceFrameLimits {
            max_pixels: i64::MAX as u64 + 1,
            ..valid
        },
    ];
    for limit in invalid {
        let result = Decoder::open_with_source_limits(
            "/not-an-existing-input.mkv",
            &software(),
            crate::InputPolicy::SelfContainedLocal,
            limit,
        );
        assert!(matches!(result, Err(Error::Probe { .. })));
    }
}
