// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The monitor: the frame at the playhead, one request at a time.
//!
//! The pane owns the picture, the busy and wanted flags that serialise
//! requests, the quality tier per timeline, and the worker that draws a
//! frame. What goes into the frame - the flattened clips with their
//! titles, a look being auditioned, a cutout being painted - is the
//! controller's to say, and the pane asks it for that list. A request
//! while one is out waits for it, and the newest wins; a frame that
//! could not be drawn says so once per project and then stops repeating
//! itself.
//!
//! The stage's gestures - pressing, dragging and releasing a picture on
//! the monitor - are not here: one [`crate::studio::Gesture`] spans the
//! stage and the lanes, so they live with the timeline's.

use std::collections::HashMap;
use std::sync::Arc;

use concat_host::preview::FrameSpec;
use concat_project::model::Project;

use crate::host::{spawn, spawn_detached};
use crate::i18n::tf;
use crate::panes::Msg;
use crate::studio::Studio;

/// Everything that can happen to the monitor.
pub enum MonitorMsg {
    /// The frame at the playhead is wanted.
    Request,
    /// The worker is done: a frame, or why not.
    Frame(Result<Picture, String>, FrameSpec, u64, Box<SourceStamp>),
    /// The quality picker: 0 full, 1 half, 2 quarter.
    QualityChanged(i32),
    /// A project opened: the monitor starts clean and may complain again.
    Opened,
    /// The project closed: nothing to show.
    Closed,
}

impl std::fmt::Debug for MonitorMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request => write!(f, "Request"),
            Self::Frame(result, spec, _, _) => write!(
                f,
                "Frame({}, {spec:?})",
                match result {
                    Ok(Picture::Sources(_)) => "sources".to_owned(),
                    Ok(Picture::Pixels(_, w, h, _)) => format!("{w}x{h} pixels"),
                    Err(error) => format!("error {error:?}"),
                }
            ),
            Self::QualityChanged(index) => write!(f, "QualityChanged({index})"),
            Self::Opened => write!(f, "Opened"),
            Self::Closed => write!(f, "Closed"),
        }
    }
}

/// A monitor frame on its way to the window.
pub enum Picture {
    /// Decoded and placed, waiting to be drawn on the window's own device -
    /// which happens back on this thread, never on the worker that decoded
    /// it. See `Monitor::texture_of`.
    Sources(concat_export::PreviewSources),
    /// Raw RGBA, to be uploaded.
    Pixels(Vec<u8>, u32, u32, f64),
}

/// Original native source, captured before the worker is dispatched.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceStamp {
    pub owner: String,
    pub session_path: String,
    pub generation: u64,
    pub revision: u64,
    pub timeline_id: String,
    pub requested_time_bits: u64,
    pub width: u32,
    pub height: u32,
    pub inputs: String,
}

struct Adopted {
    source: SourceStamp,
    frame: concat_editor_mcp::ClipPreviewFrame,
    cpu: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
}

/// The monitor's state.
#[derive(Default)]
pub struct MonitorPane {
    /// The last frame drawn.
    pub image: slint::Image,
    active_request: Option<u64>,
    sequence: u64,
    // A pane lifetime nonce prevents ABA across a fresh Studio/process. It is
    // never exposed; the native document generation binds each opening.
    owner: Option<String>,
    adopted: Option<Adopted>,
    wanted: bool,
    /// Said once per project: a monitor that cannot decode says so, and
    /// then stops repeating itself.
    failed: bool,
    /// 0 Full, 1 Half, 2 Quarter of the output size, by timeline id. Kept
    /// per timeline because the cost it trades against is the timeline's
    /// frame - a 4K cut wants the quarter setting that a 1080p cut beside
    /// it does not - and the trade is the window's, not the document's,
    /// so it is remembered here and not saved.
    quality: HashMap<String, usize>,
}

impl MonitorPane {
    /// Applies one message. The studio is the rest of the window; while
    /// this runs the studio's copy of the pane is a blank it must not read.
    pub fn update(&mut self, msg: MonitorMsg, studio: &mut Studio) {
        match msg {
            MonitorMsg::Request => self.request(studio),
            MonitorMsg::Frame(result, spec, sequence, source) => {
                let source = *source;
                // A completion from an old document must not clear a newer
                // worker's busy flag or claim its source identity.
                let expected_size =
                    Self::frame_size(self.quality_of(studio.project()), studio.output_size());
                let current_source = studio.monitor_source(
                    expected_size.0,
                    expected_size.1,
                    self.owner.as_deref().unwrap_or_default(),
                );
                let Some(current) =
                    self.finish_request(sequence, &source, current_source.as_ref(), studio.playing)
                else {
                    return;
                };
                if current {
                    let picture = match result {
                        Ok(Picture::Sources(sources)) => {
                            let time = sources.rendered_time_seconds();
                            studio
                                .host
                                .monitor
                                .texture_of(&sources, spec)
                                .and_then(|texture| {
                                    slint::Image::try_from(texture)
                                        .map(|image| (image, None, time))
                                        .map_err(|error| format!("preview texture: {error}"))
                                })
                        }
                        Ok(Picture::Pixels(bytes, width, height, time)) => {
                            let buffer =
                                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                    &bytes, width, height,
                                );
                            Ok((slint::Image::from_rgba8(buffer.clone()), Some(buffer), time))
                        }
                        Err(error) => Err(error),
                    };
                    match picture {
                        Ok((image, cpu, time)) => self.adopt(image, cpu, source, time, sequence),
                        Err(error) => {
                            self.adopted = None;
                            log::warn!("preview: {error}");
                            if !self.failed {
                                self.failed = true;
                                studio.notify(&tf("Preview failed: {0}", &[&error]), true);
                            }
                        }
                    }
                }
                if self.wanted {
                    self.request(studio);
                }
            }
            MonitorMsg::QualityChanged(index) => {
                let id = studio.project().active_timeline_id.clone();
                self.quality.insert(id, (index.max(0) as usize).min(2));
                self.request(studio);
            }
            MonitorMsg::Opened | MonitorMsg::Closed => {
                self.failed = false;
                self.active_request = None;
                self.wanted = false;
                self.adopted = None;
                self.image = slint::Image::default();
            }
        }
    }

    /// None is a completion owned by an earlier worker. Some(false) belongs
    /// to this worker but its original input is no longer current.
    fn finish_request(
        &mut self,
        sequence: u64,
        source: &SourceStamp,
        current: Option<&SourceStamp>,
        playing: bool,
    ) -> Option<bool> {
        if self.active_request != Some(sequence) {
            return None;
        }
        self.active_request = None;
        Some(current.is_some_and(|current| {
            // Playback naturally advances while decoding. Keep its existing
            // display cadence, with the original time; playing is never ready.
            let mut expected = current.clone();
            if playing {
                expected.requested_time_bits = source.requested_time_bits;
            }
            &expected == source
        }))
    }

    fn adopt(
        &mut self,
        image: slint::Image,
        cpu: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
        source: SourceStamp,
        time: f64,
        sequence: u64,
    ) {
        use sha2::{Digest, Sha256};
        let owner = self
            .owner
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        let identity =
            serde_json::json!({"owner":owner,"sequence":sequence,"source":source,"time":time});
        let frame = concat_editor_mcp::ClipPreviewFrame {
            state: "ready".into(),
            version: 1,
            source: "adoptedMonitorFrame".into(),
            frame_id: format!(
                "clip-frame-v1-{:x}",
                Sha256::digest(serde_json::to_vec(&identity).expect("monitor identity"))
            ),
            generation: source.generation,
            revision: source.revision,
            timeline_id: source.timeline_id.clone(),
            rendered_frame_time_seconds: time,
            source_width: source.width,
            source_height: source.height,
        };
        let actual = image.size();
        let dimensions_match = actual.width == source.width && actual.height == source.height;
        self.image = image;
        self.adopted = frame
            .validate()
            .ok()
            .filter(|_| dimensions_match)
            .map(|()| Adopted { source, frame, cpu });
    }

    pub(crate) fn adopted_frame(&self, studio: &Studio) -> concat_editor_mcp::ClipAdoptedFrame {
        let busy = studio.mcp_clip_state().3 || studio.monitor_inputs_busy();
        let source = if !studio.playing && !busy && self.active_request.is_none() && !self.wanted {
            let size = Self::frame_size(self.quality_of(studio.project()), studio.output_size());
            studio.monitor_source(size.0, size.1, self.owner.as_deref().unwrap_or_default())
        } else {
            None
        };
        let supported = studio.host.monitor_catalogue_ready()
            && self.adopted.as_ref().is_some_and(|adopted| {
                if adopted.cpu.is_some() {
                    u64::from(adopted.frame.source_width) * u64::from(adopted.frame.source_height)
                        <= 4_194_304
                } else {
                    self.image.to_wgpu_29_texture().is_some()
                        && studio
                            .host
                            .clip_preview_reader
                            .try_borrow()
                            .is_ok_and(|reader| reader.is_some())
                }
            });
        self.state_for(source.as_ref(), studio.playing, busy, supported)
    }

    fn state_for(
        &self,
        source: Option<&SourceStamp>,
        playing: bool,
        busy: bool,
        supported: bool,
    ) -> concat_editor_mcp::ClipAdoptedFrame {
        use concat_editor_mcp::{
            ClipAdoptedFrame as State, ClipPreviewUnavailable as Unavailable,
            ClipPreviewUnavailableReason as Reason,
        };
        let reason = if playing {
            Reason::Playing
        } else if busy {
            Reason::Busy
        } else if self.adopted.is_some() && !supported {
            Reason::Unsupported
        } else if self.active_request.is_some() || self.wanted {
            Reason::Pending
        } else if let Some(adopted) = &self.adopted {
            if source != Some(&adopted.source) {
                Reason::Pending
            } else {
                return State::Ready(adopted.frame.clone());
            }
        } else {
            Reason::NoFrame
        };
        State::Unavailable(Unavailable {
            state: "unavailable".into(),
            version: 1,
            reason,
        })
    }

    /// Only retained CPU pixels from the adopted image; never renders or decodes.
    pub(crate) fn cached_cpu(&self) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
        let adopted = self.adopted.as_ref()?;
        if u64::from(adopted.frame.source_width) * u64::from(adopted.frame.source_height)
            > 4_194_304
        {
            return None;
        }
        adopted.cpu.clone()
    }

    /// The quality tier for the project's active timeline: 0 full, 1
    /// half, 2 quarter. Half until chosen.
    pub fn quality_of(&self, project: &Project) -> usize {
        self.quality
            .get(&project.active_timeline_id)
            .copied()
            .unwrap_or(1)
    }

    /// The frame's size at a quality tier: the output scaled, rounded to
    /// even dimensions, never smaller than two pixels a side.
    pub fn frame_size(quality: usize, (width, height): (u32, u32)) -> (u32, u32) {
        let scale = match quality {
            0 => 1.0,
            1 => 0.5,
            _ => 0.25,
        };
        let side = |px: u32| ((f64::from(px) * scale).round() as u32).max(2) & !1;
        (side(width), side(height))
    }

    /// Asks the engine for the frame at the playhead, one at a time.
    fn request(&mut self, studio: &mut Studio) {
        if studio.on_start || studio.session.is_none() {
            return;
        }
        if self.active_request.is_some() {
            self.wanted = true;
            return;
        }
        let (width, height) =
            Self::frame_size(self.quality_of(studio.project()), studio.output_size());
        studio.host.capture_monitor_catalogue();
        let Some((clips, settings)) = studio.preview_clips() else {
            return;
        };
        let spec = FrameSpec {
            time: f64::from(studio.playhead),
            width,
            height,
        };
        let owner = self
            .owner
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        let Some(source) = studio.monitor_source(width, height, owner) else {
            return;
        };
        let monitor = studio.host.monitor.clone();
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("monitor request sequence exhausted");
        let sequence = self.sequence;
        self.active_request = Some(sequence);
        self.wanted = false;
        spawn(
            move || {
                // On the window's device the frame stays a texture; without
                // one it comes back as pixels and is uploaded here.
                let frame = if monitor.has_gpu() {
                    monitor
                        .frame_sources(Arc::clone(&clips), &settings, spec)
                        .map(Picture::Sources)
                } else {
                    monitor
                        .frame_with_time(Arc::clone(&clips), &settings, spec)
                        .map(|(bytes, time)| Picture::Pixels(bytes, width, height, time))
                };
                // Decode-ahead for whatever comes next, on a worker of its
                // own, so the frame goes to the window without waiting for
                // it and the next frame can start meanwhile.
                {
                    let monitor = monitor.clone();
                    let settings = settings.clone();
                    spawn_detached(move || monitor.prefetch(clips, &settings, spec, 2));
                }
                frame
            },
            move |studio, _, _, result| {
                studio.handle(Msg::Monitor(MonitorMsg::Frame(
                    result,
                    spec,
                    sequence,
                    Box::new(source),
                )));
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_even_sided_and_never_vanishes() {
        assert_eq!(MonitorPane::frame_size(0, (1920, 1080)), (1920, 1080));
        assert_eq!(MonitorPane::frame_size(1, (1920, 1080)), (960, 540));
        assert_eq!(MonitorPane::frame_size(2, (1920, 1080)), (480, 270));
        // An odd output at half is rounded to even, as the encoder and the
        // chroma planes need.
        assert_eq!(MonitorPane::frame_size(1, (1001, 1001)), (500, 500));
        // Nothing gets smaller than two pixels a side, whatever the tier.
        assert_eq!(MonitorPane::frame_size(2, (1, 1)), (2, 2));
        assert_eq!(MonitorPane::frame_size(2, (0, 0)), (2, 2));
        // A tier past the picker's three is the smallest one.
        assert_eq!(MonitorPane::frame_size(9, (400, 400)), (100, 100));
    }

    #[test]
    fn quality_is_half_until_chosen_and_kept_per_timeline() {
        let mut pane = MonitorPane::default();
        let mut project = Project {
            active_timeline_id: "a".to_owned(),
            ..Project::default()
        };
        assert_eq!(pane.quality_of(&project), 1);
        pane.quality.insert("a".to_owned(), 2);
        assert_eq!(pane.quality_of(&project), 2);
        project.active_timeline_id = "b".to_owned();
        assert_eq!(pane.quality_of(&project), 1, "another timeline has its own");
    }
    fn source() -> SourceStamp {
        SourceStamp {
            owner: "pane-owner".into(),
            session_path: "/native/project".into(),
            generation: 3,
            revision: 7,
            timeline_id: "timeline:main".into(),
            requested_time_bits: 1.01f64.to_bits(),
            width: 2,
            height: 2,
            inputs: "actual-native-inputs".into(),
        }
    }

    #[test]
    fn old_source_completion_cannot_claim_new_request_or_replace_cached_pixels() {
        let mut pane = MonitorPane::default();
        let bytes: [u8; 16] = [
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 0,
        ];
        let cpu = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bytes, 2, 2);
        pane.adopt(
            slint::Image::from_rgba8(cpu.clone()),
            Some(cpu),
            source(),
            1.0,
            1,
        );
        let first_id = pane.adopted.as_ref().unwrap().frame.frame_id.clone();
        assert_eq!(pane.cached_cpu().unwrap().as_bytes(), &bytes);
        assert_eq!(
            pane.adopted
                .as_ref()
                .unwrap()
                .frame
                .rendered_frame_time_seconds,
            1.0
        );
        pane.active_request = Some(9);
        assert_eq!(
            pane.finish_request(8, &source(), Some(&source()), false),
            None
        );
        assert_eq!(pane.active_request, Some(9));
        for changed in [
            SourceStamp {
                owner: "new-pane".into(),
                ..source()
            },
            SourceStamp {
                generation: 4,
                ..source()
            },
            SourceStamp {
                session_path: "/other".into(),
                ..source()
            },
            SourceStamp {
                revision: 8,
                ..source()
            },
            SourceStamp {
                timeline_id: "timeline:other".into(),
                ..source()
            },
            SourceStamp {
                inputs: "new-audition-or-mask".into(),
                ..source()
            },
            SourceStamp {
                requested_time_bits: 2.0f64.to_bits(),
                ..source()
            },
            SourceStamp {
                width: 4,
                ..source()
            },
        ] {
            pane.active_request = Some(9);
            assert_eq!(
                pane.finish_request(9, &source(), Some(&changed), false),
                Some(false)
            );
            assert_eq!(pane.cached_cpu().unwrap().as_bytes(), &bytes);
            assert_eq!(pane.adopted.as_ref().unwrap().frame.frame_id, first_id);
        }
        pane.active_request = Some(10);
        pane.wanted = true;
        assert_eq!(
            pane.finish_request(10, &source(), Some(&source()), false),
            Some(true)
        );
        pane.wanted = false;
        pane.active_request = Some(10);
        let next_time = SourceStamp {
            requested_time_bits: 2.0f64.to_bits(),
            ..source()
        };
        assert_eq!(
            pane.finish_request(10, &source(), Some(&next_time), true),
            Some(true)
        );
        pane.active_request = Some(11);
        assert_eq!(
            pane.finish_request(11, &source(), Some(&source()), false),
            Some(true)
        );
        let cpu = pane.cached_cpu().unwrap();
        pane.adopt(
            slint::Image::from_rgba8(cpu.clone()),
            Some(cpu),
            source(),
            1.0,
            11,
        );
        assert_ne!(pane.adopted.as_ref().unwrap().frame.frame_id, first_id);
    }

    #[test]
    fn identical_dimensions_and_sources_in_different_panes_have_distinct_frame_identity() {
        let mut a = MonitorPane::default();
        let mut b = MonitorPane::default();
        for pane in [&mut a, &mut b] {
            let cpu =
                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&[0u8; 16], 2, 2);
            pane.adopt(
                slint::Image::from_rgba8(cpu.clone()),
                Some(cpu),
                source(),
                1.0,
                1,
            );
        }
        assert_ne!(
            a.adopted.unwrap().frame.frame_id,
            b.adopted.unwrap().frame.frame_id
        );
    }
    #[test]
    fn paused_idle_adopted_pixels_are_ready_and_other_states_are_explicit() {
        use concat_editor_mcp::{
            ClipAdoptedFrame as State, ClipPreviewUnavailableReason as Reason,
        };
        let mut pane = MonitorPane::default();
        let reason = |value| match value {
            State::Unavailable(value) => value.reason,
            State::Ready(_) => panic!("unexpected ready"),
        };
        assert_eq!(
            reason(pane.state_for(None, false, false, true)),
            Reason::NoFrame
        );
        let cpu = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&[0u8; 16], 2, 2);
        pane.adopt(
            slint::Image::from_rgba8(cpu.clone()),
            Some(cpu),
            source(),
            1.0,
            1,
        );
        let ready = pane.state_for(Some(&source()), false, false, true);
        assert!(matches!(ready, State::Ready(_)));
        assert_eq!(ready, pane.state_for(Some(&source()), false, false, true));
        assert_eq!(
            reason(pane.state_for(Some(&source()), true, false, true)),
            Reason::Playing
        );
        assert_eq!(
            reason(pane.state_for(Some(&source()), false, true, true)),
            Reason::Busy
        );
        assert_eq!(
            reason(pane.state_for(Some(&source()), false, false, false)),
            Reason::Unsupported
        );
        assert_eq!(
            reason(pane.state_for(None, false, false, true)),
            Reason::Pending
        );
        pane.active_request = Some(2);
        assert_eq!(
            reason(pane.state_for(Some(&source()), false, false, true)),
            Reason::Pending
        );
        pane.active_request = None;
        pane.wanted = true;
        assert_eq!(
            reason(pane.state_for(Some(&source()), false, false, true)),
            Reason::Pending
        );
    }
}
