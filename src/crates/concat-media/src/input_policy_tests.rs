//! Real linked-FFmpeg fixtures. Missing codecs are failures, never silent skips.
use crate::{
    audio::{self, AudioClip},
    input::Input,
    *,
};
use concat_core::{
    frame::Frame,
    time::{FrameRate, Rational},
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const STRICT: InputPolicy = InputPolicy::SelfContainedLocal;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "concat-strict-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory.canonicalize().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(root) = std::env::var_os("CONCAT_MEDIA_POLICY_FIXTURES") {
            let target = PathBuf::from(root).join(self.0.file_name().unwrap());
            std::fs::create_dir_all(&target).unwrap();
            for entry in std::fs::read_dir(&self.0).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() {
                    std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn movie(path: &Path) {
    let options = EncodeOptions {
        hardware: false,
        preset: "ultrafast".into(),
        crf: 16,
        ..Default::default()
    };
    let mut encoder = Encoder::create(path, 32, 16, FrameRate::THIRTY, &options)
        .expect("required H264 encoder unavailable");
    for n in 0..30 {
        let mut f = Frame::black(32, 16);
        f.fill([40 + n * 3, 80, 160, 255]);
        encoder.write_frame(&f).unwrap();
    }
    encoder.finish().unwrap();
}
fn wav(path: &Path) {
    tone(path, 1000.0);
}
fn tone(path: &Path, frequency: f64) {
    let mut data = Vec::new();
    for n in 0..48_000 {
        let value =
            ((n as f64 * frequency * std::f64::consts::TAU / 48_000.0).sin() * 12000.0) as i16;
        for _ in 0..2 {
            data.extend_from_slice(&value.to_le_bytes());
        }
    }
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16_u32.to_le_bytes());
    b.extend_from_slice(&1_u16.to_le_bytes());
    b.extend_from_slice(&2_u16.to_le_bytes());
    b.extend_from_slice(&48_000_u32.to_le_bytes());
    b.extend_from_slice(&192_000_u32.to_le_bytes());
    b.extend_from_slice(&4_u16.to_le_bytes());
    b.extend_from_slice(&16_u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend(data);
    std::fs::write(path, b).unwrap();
}
fn clip(path: &Path) -> AudioClip {
    AudioClip {
        path: path.into(),
        stream: None,
        start: 0.0,
        duration: 1.0,
        source_start: 0.0,
        speed: 1.0,
        preserve_pitch: true,
        volume: 1.0,
        volume_curve: Default::default(),
        fade_in: 0.0,
        fade_out: 0.0,
        filter_chain: String::new(),
    }
}
fn full_video(path: &Path) -> usize {
    let mut decoder =
        Decoder::open_with_policy(path, &DecodeOptions::default().in_software(), STRICT).unwrap();
    let mut count = 0;
    while let Some(frame) = decoder.next_frame().unwrap() {
        assert!(frame.width() > 0 && frame.height() > 0);
        count += 1;
        assert!(count < 1000);
    }
    count
}
#[test]
fn strict_real_mp4_audio_full_decode_seek_pace_pool_mix_and_mux() {
    let d = Scratch::new();
    let video = d.path("video.mp4");
    movie(&video);
    let source = d.path("source.wav");
    wav(&source);
    // 1000 Hz produces a 192-byte PCM period that generic FFmpeg 9 probing
    // misidentifies as MPEG-TS. Strict mode selects WAV from the primary bytes.
    for frequency in [1000.0, 997.0] {
        let tone = d.path(&format!("tone-{frequency}.bin"));
        self::tone(&tone, frequency);
        assert!(probe_with_policy(&tone, STRICT).unwrap().audio.is_some());
        let mut decoder =
            AudioDecoder::open_with_policy(&tone, &AudioOptions::default(), STRICT).unwrap();
        let mut count = 0;
        while let Some(samples) = decoder.next_i16().unwrap() {
            count += samples.len();
        }
        assert_eq!(count, 96_000);
        audio::mix_to_file_with_policy(
            &[clip(&tone)],
            1.0,
            &d.path(&format!("tone-{frequency}.m4a")),
            STRICT,
        )
        .unwrap();
    }
    for (name, prefix) in [("bad-riff.mp4", b"RIFF"), ("bad-rf64.wav", b"RF64")] {
        let invalid = d.path(name);
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(b"WAVEbroken");
        std::fs::write(&invalid, bytes).unwrap();
        assert!(probe_with_policy(&invalid, STRICT).is_err());
        assert!(
            AudioDecoder::open_with_policy(&invalid, &AudioOptions::default(), STRICT).is_err()
        );
    }
    let mixed = d.path("audio.m4a");
    audio::mix_to_file_with_policy(&[clip(&source)], 1.0, &mixed, STRICT)
        .expect("required AAC encoder and filters must work");
    let output = d.path("with-audio.mp4");
    audio::mux_with_policy(&video, &mixed, &output, STRICT).unwrap();
    let info = probe_with_policy(&output, STRICT).unwrap();
    assert_eq!(info.video.as_ref().unwrap().width, 32);
    assert!(info.audio.is_some());
    assert_eq!(full_video(&output), 30);
    let mut sound = AudioDecoder::open_with_policy(
        &output,
        &AudioOptions {
            format: SampleFormat::F32,
            ..Default::default()
        },
        STRICT,
    )
    .unwrap();
    let mut samples = 0;
    let mut peak = 0_f32;
    while let Some(values) = sound.next_f32().unwrap() {
        samples += values.len();
        for value in values {
            peak = peak.max(value.abs());
        }
    }
    assert!((samples as i64 - 96_000).abs() < 6000);
    assert!(peak > 0.1);
    let mut decoder =
        Decoder::open_with_policy(&output, &DecodeOptions::default().in_software(), STRICT)
            .unwrap();
    decoder.seek(Rational::new(1, 2)).unwrap();
    assert!(decoder.next_frame().unwrap().is_some());
    let options = DecodeOptions::default()
        .in_software()
        .at_rate(FrameRate::SIXTY);
    let mut paced = Decoder::open_with_policy(&output, &options, STRICT).unwrap();
    let mut n = 0;
    while paced.next_frame().unwrap().is_some() {
        n += 1;
        assert!(n < 100);
    }
    assert!((58..=62).contains(&n));
    let pool = ReaderPool::new_with_policy(1024 * 1024, 2, STRICT);
    for t in [Rational::ZERO, Rational::new(3, 4), Rational::new(1, 4)] {
        assert_eq!(
            pool.frame_at(&output, t, 32, 16, false, None, None)
                .unwrap()
                .width(),
            32
        );
    }
    // Existing public defaults keep their original path and need no Options fields.
    assert_eq!(probe(&output).unwrap().video.unwrap().width, 32);
    assert!(Decoder::open(&output, &DecodeOptions::default().in_software()).is_ok());
    assert!(AudioDecoder::open(&output, &AudioOptions::default()).is_ok());
}
#[test]
fn strict_png_jpeg_webp_are_single_primary_streams() {
    let d = Scratch::new();
    let png = d.path("frame.png");
    std::fs::write(&png, include_bytes!("../tests/fixtures/strict.png")).unwrap();
    let jpeg = d.path("frame.jpeg");
    std::fs::write(
        &jpeg,
        crate::jpeg(&Frame::black(32, 16), 2).expect("required mjpeg encoder unavailable"),
    )
    .unwrap();
    let webp = d.path("frame.webp");
    std::fs::write(&webp, include_bytes!("../tests/fixtures/strict.webp")).unwrap();
    for path in [&png, &jpeg, &webp] {
        assert!(probe_with_policy(path, STRICT).unwrap().video.is_some());
        assert_eq!(full_video(path), 1);
        let pool = ReaderPool::new_with_policy(1024 * 1024, 1, STRICT);
        assert!(
            pool.frame_at(path, Rational::ZERO, 16, 16, true, None, None)
                .is_ok()
        );
    }
}
#[test]
fn forged_extensions_and_sequence_demuxers_cannot_use_readable_neighbours() {
    let d = Scratch::new();
    let neighbour = d.path("neighbour.mp4");
    movie(&neighbour);
    assert_eq!(full_video(&neighbour), 30);
    let payloads = [
        "ffconcat version 1.0\nfile neighbour.mp4\n",
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nneighbour.mp4\n#EXT-X-ENDLIST\n",
        "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT1S\"><Period><AdaptationSet mimeType=\"video/mp4\"><Representation id=\"1\" bandwidth=\"1000\"><BaseURL>neighbour.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>",
    ];
    for (n, payload) in payloads.iter().enumerate() {
        for ext in ["mp4", "png", ["ffconcat", "m3u8", "mpd"][n]] {
            let path = d.path(&format!("fake-{n}.{ext}"));
            std::fs::write(&path, payload).unwrap();
            assert!(probe_with_policy(&path, STRICT).is_err(), "{path:?}");
            assert!(Decoder::open_with_policy(&path, &DecodeOptions::default(), STRICT).is_err());
            assert!(
                AudioDecoder::open_with_policy(&path, &AudioOptions::default(), STRICT).is_err()
            );
            assert!(
                audio::mix_to_file_with_policy(&[clip(&path)], 1.0, &d.path("bad.m4a"), STRICT)
                    .is_err()
            );
            assert!(
                audio::mux_with_policy(&path, &neighbour, &d.path("bad-v.mp4"), STRICT).is_err()
            );
            assert!(
                audio::mux_with_policy(&neighbour, &path, &d.path("bad-a.mp4"), STRICT).is_err()
            );
            let pool = ReaderPool::new_with_policy(1024, 1, STRICT);
            assert!(
                pool.frame_at(&path, Rational::ZERO, 32, 16, false, None, None)
                    .is_err()
            );
        }
    }
    let png = include_bytes!("../tests/fixtures/strict.png");
    for name in ["frame-001.png", "frame-002.png", "frame-%03d.png"] {
        std::fs::write(d.path(name), png).unwrap();
    }
    let pattern = d.path("frame-%03d.png");
    assert!(Input::open_with_test_image2(&pattern).is_err());
    // FFmpeg selects image2 for a sequence-shaped URL even when that literal
    // file exists. The strict closed demuxer set must reject this selection.
    assert!(Decoder::open_with_policy(&pattern, &DecodeOptions::default(), STRICT).is_err());
    // Network references are rejected by format selection; no network request is made.
    let network = d.path("network.mp4");
    std::fs::write(&network,"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nhttp://127.0.0.1:9/not-requested.ts\n#EXT-X-ENDLIST\n").unwrap();
    assert!(probe_with_policy(&network, STRICT).is_err());
}
#[test]
fn sticky_denial_survives_reads_seeks_cached_frames_and_eof() {
    let d = Scratch::new();
    let video = d.path("video.mp4");
    movie(&video);
    let sound = d.path("sound.wav");
    wav(&sound);
    let mut input = Input::open(&video, STRICT).unwrap();
    input.deny_for_test();
    assert!(matches!(
        input.read_packet(&mut ffmpeg_the_third::Packet::empty()),
        Err(Error::InputPolicy { .. })
    ));
    assert!(matches!(
        input.seek_checked(0, ..=0),
        Err(Error::InputPolicy { .. })
    ));
    let mut decoder =
        Decoder::open_with_policy(&video, &DecodeOptions::default().limited_to(0), STRICT).unwrap();
    decoder.deny_input_for_test();
    assert!(
        matches!(decoder.next_frame(), Err(Error::InputPolicy { .. })),
        "max_frames must not hide denial as EOF"
    );
    let mut audio =
        AudioDecoder::open_with_policy(&sound, &AudioOptions::default(), STRICT).unwrap();
    while audio.next_frame().unwrap().is_some() {}
    audio.deny_input_for_test();
    assert!(
        matches!(audio.next_frame(), Err(Error::InputPolicy { .. })),
        "exhausted decoder must not hide denial"
    );
}
fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(payload);
    b
}
fn replace_dref(bytes: &[u8], target: &str, relative: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        assert!(offset + 8 <= bytes.len());
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        assert!(size >= 8 && offset + size <= bytes.len());
        let kind: &[u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
        let payload = &bytes[offset + 8..offset + size];
        if kind == b"dref" {
            let mut alias = vec![0_u8; 150];
            if relative {
                alias[130..132].copy_from_slice(&1_u16.to_be_bytes());
                alias[132..134].copy_from_slice(&1_u16.to_be_bytes());
            }
            alias.extend_from_slice(&2_u16.to_be_bytes());
            alias.extend_from_slice(&(target.len() as u16).to_be_bytes());
            alias.extend_from_slice(target.as_bytes());
            if target.len() % 2 == 1 {
                alias.push(0);
            }
            alias.extend_from_slice(&[255, 255, 0, 0]);
            let mut entry = vec![0; 4];
            entry.extend(alias);
            let mut body = vec![0; 4];
            body.extend_from_slice(&1_u32.to_be_bytes());
            body.extend(atom(b"alis", &entry));
            out.extend(atom(b"dref", &body));
        } else if [b"moov", b"trak", b"mdia", b"minf", b"dinf"].contains(&kind) {
            out.extend(atom(kind, &replace_dref(payload, target, relative)));
        } else {
            out.extend_from_slice(&bytes[offset..offset + size]);
        }
        offset += size;
    }
    out
}
#[test]
fn mov_drefs_cannot_open_real_relative_or_absolute_external_tracks() {
    let d = Scratch::new();
    let neighbour = d.path("neighbour.mp4");
    movie(&neighbour);
    assert_eq!(full_video(&neighbour), 30);
    let original = std::fs::read(&neighbour).unwrap();
    for relative in [true, false] {
        let target = if relative {
            "neighbour.mp4".to_owned()
        } else {
            neighbour.to_str().unwrap().to_owned()
        };
        let path = d.path(if relative {
            "relative.mov"
        } else {
            "absolute.mov"
        });
        std::fs::write(&path, replace_dref(&original, &target, relative)).unwrap();
        // Only this cfg(test) helper enables drefs, so the actual demuxer must
        // attempt a readable external track and trigger our rejecting callback.
        assert!(
            matches!(
                Input::open_with_test_drefs(&path),
                Err(Error::InputPolicy { .. })
            ),
            "MOV dref did not reach the deny hook: {path:?}"
        );
        // Production drefs=0 can ignore the external track or reject the movie.
        // Neither result is described as preserving every original track.
        let _ = probe_with_policy(&path, STRICT);
    }
}
#[test]
fn context_primary_and_state_drop_in_order() {
    let d = Scratch::new();
    let video = d.path("video.mp4");
    movie(&video);
    let owner = Input::open(&video, STRICT).unwrap();
    let events = owner.drop_events();
    drop(owner);
    assert_eq!(
        *events.lock().unwrap(),
        ["context closed", "primary closed", "state dropped"]
    );
}
#[cfg(target_os = "macos")]
fn handles_for(path: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt;
    (0..4096)
        .filter(|&fd| {
            let mut name = [0_i8; libc::PATH_MAX as usize];
            // SAFETY: F_GETPATH writes at most PATH_MAX bytes into this live buffer.
            let result = unsafe { libc::fcntl(fd, libc::F_GETPATH, name.as_mut_ptr()) };
            result == 0
                && unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_bytes()
                    == path.as_os_str().as_bytes()
        })
        .count()
}
#[cfg(target_os = "macos")]
#[test]
fn repeated_success_and_failure_do_not_leak_primary_descriptors() {
    let d = Scratch::new();
    let good = d.path("good.mp4");
    movie(&good);
    let bad = d.path("bad.mp4");
    std::fs::write(&bad, b"ffconcat version 1.0\nfile good.mp4\n").unwrap();
    let baseline = (handles_for(&good), handles_for(&bad));
    for _ in 0..32 {
        drop(Input::open(&good, STRICT).unwrap());
        assert!(Input::open(&bad, STRICT).is_err());
        assert_eq!((handles_for(&good), handles_for(&bad)), baseline);
    }
}
