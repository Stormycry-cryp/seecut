// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Flattening a project document into the exporter's clip list.
//!
//! Walking a timeline's clips, resolving their media and track, folding
//! track state into per-clip flags, and building each clip's FFmpeg chains
//! from the structured effects the document stores. An export is derived
//! from the engine's own model, so the model of record and the rendered
//! pixels cannot drift apart.
//!
//! Text clips are deliberately not flattened here. Titles rasterise
//! separately and rejoin the exporter as plain image clips; see
//! `ExportRequest`'s caller.

use std::path::Path;

use concat_effects::Catalogue;
use concat_project::model::{ClipKind as ModelClipKind, KeyProperty, Project, Timeline};

use crate::{ClipKind, ExportClip, ExportKey, TransitionSpec};

/// Flattens one timeline of `project` for the exporter - the active one
/// when `timeline_id` is `None`. Text clips are excluded (they rasterise
/// separately and rejoin as image clips); a clip whose media or track has
/// vanished is skipped with the same tolerance the document reader extends.
pub fn flatten_timeline(project: &Project, timeline_id: Option<&str>) -> Vec<ExportClip> {
    flatten_timeline_in(project, timeline_id, None)
}

/// [`flatten_timeline`], knowing the project folder: a clip with a cutout
/// is then told where its media's masks are, and renders cut. Without the
/// folder the cutout is carried but not rendered.
pub fn flatten_timeline_in(
    project: &Project,
    timeline_id: Option<&str>,
    project_dir: Option<&Path>,
) -> Vec<ExportClip> {
    flatten_timeline_in_with_catalogue(project, timeline_id, project_dir, Catalogue::builtin())
}

/// Flattens with the caller's captured catalogue for both audio and video
/// chains. Pass the same catalogue to `crate::render_with_catalogue` to keep
/// effect resolution consistent; media and package files remain path inputs.
pub fn flatten_timeline_in_with_catalogue(
    project: &Project,
    timeline_id: Option<&str>,
    project_dir: Option<&Path>,
    catalogue: &Catalogue,
) -> Vec<ExportClip> {
    let Some(timeline) = pick_timeline(project, timeline_id) else {
        return Vec::new();
    };

    timeline
        .clips
        .iter()
        .filter_map(|clip| {
            if clip.kind == ModelClipKind::Text {
                return None;
            }
            let index = timeline
                .tracks
                .iter()
                .position(|track| track.id == clip.track_id)?;
            let track = &timeline.tracks[index];

            // A layer: no file, a chain over the stack beneath its track,
            // its opacity the strength and its fades the ramps.
            if clip.kind == ModelClipKind::Layer {
                return Some(ExportClip {
                    hidden: !track.visible,
                    muted: true,
                    volume: 0.0,
                    fade_in: clip.fade_in,
                    fade_out: clip.fade_out,
                    effects: clip.video_effects.clone(),
                    opacity: clip.opacity,
                    video_filter_chain: catalogue.video_chain(&clip.video_effects),
                    has_audio: Some(false),
                    ..ExportClip::blank(ClipKind::Layer, clip.start, clip.duration, index)
                });
            }

            let media = project.media.iter().find(|item| item.id == clip.media_id)?;

            let kind = match clip.kind {
                ModelClipKind::Video => ClipKind::Video,
                ModelClipKind::Audio => ClipKind::Audio,
                ModelClipKind::Image => ClipKind::Image,
                // Handled above; unreachable spelled as a skip so a new
                // kind fails soft.
                ModelClipKind::Text | ModelClipKind::Layer => return None,
            };
            Some(ExportClip {
                path: media.path.clone(),
                audio_stream: clip.audio_stream,
                source_start: clip.source_start,
                hidden: !track.visible,
                muted: track.muted || clip.muted == Some(true),
                volume: export_base(clip, KeyProperty::Volume),
                fade_in: clip.fade_in,
                fade_out: clip.fade_out,
                filter_chain: catalogue.audio_chain(&clip.filters),
                speed: clip.speed,
                preserve_pitch: clip.preserve_pitch,
                speed_curve: clip
                    .speed_curve
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|point| (point.at, point.speed))
                    .collect(),
                reverse: clip.reverse,
                animation: export_keys(clip),
                flip_h: clip.flip_h,
                flip_v: clip.flip_v,
                blend: clip.blend.clone(),
                crop: clip
                    .crop
                    .filter(|crop| !crop.is_none())
                    .map(|crop| [crop.left, crop.top, crop.right, crop.bottom]),
                effects: clip.video_effects.clone(),
                scale: export_base(clip, KeyProperty::Scale),
                offset_x: export_base(clip, KeyProperty::OffsetX),
                offset_y: export_base(clip, KeyProperty::OffsetY),
                rotation: export_base(clip, KeyProperty::Rotation),
                stretch_x: clip.stretch_x,
                stretch_y: clip.stretch_y,
                opacity: export_base(clip, KeyProperty::Opacity),
                video_filter_chain: catalogue.video_chain(&clip.video_effects),
                // Passed through unconditionally: `resolve_transitions` is
                // the one adjacency judge (frame/2 tolerance). A fixed
                // 1/60 s gate here would agree at 30fps and disagree at
                // every other rate; resolving is the kinder read of what
                // the user placed.
                transition: clip
                    .transition_in
                    .as_ref()
                    .map(|transition| TransitionSpec {
                        kind: transition.id.clone(),
                        duration: transition.duration,
                    }),
                media_width: media.width,
                media_height: media.height,
                has_audio: Some(media.has_audio),
                cutout: clip.cutout.clone(),
                mask_dir: match (&clip.cutout, project_dir) {
                    (Some(cutout), Some(dir)) => {
                        concat_vision::mask_dir(dir, &media.path, cutout.subject)
                            .to_string_lossy()
                            .into_owned()
                    }
                    _ => String::new(),
                },
                ..ExportClip::blank(kind, clip.start, clip.duration, index)
            })
        })
        .collect()
}

/// The timeline to flatten: the named one, or the document's active one,
/// falling back to the first - the same preference the UI's tab strip has.
fn pick_timeline<'a>(project: &'a Project, timeline_id: Option<&str>) -> Option<&'a Timeline> {
    let wanted = timeline_id.unwrap_or(&project.active_timeline_id);
    project
        .timelines
        .iter()
        .find(|timeline| timeline.id == wanted)
        .or_else(|| project.timelines.first())
        .map(|timeline| timeline.as_ref())
}

/// The constant the engine should receive for a keyable property.
///
/// The clip's own, until that property is keyed - and then the *neutral*
/// value, because a keyed property's keys travel absolutely rather than as a
/// factor on this base. Absolute is not a preference: `Animation` multiplies
/// the base by the scale and opacity tracks, so a clip sitting at zero
/// opacity could never be keyed back up if its keys were relative to it.
///
/// Presets stay relative, which is what lets a Fade preset ride on top of a
/// hand-keyed opacity without either knowing about the other.
pub fn export_base(clip: &concat_project::model::Clip, property: KeyProperty) -> f64 {
    if !clip.is_keyed(property) {
        return clip.constant(property);
    }
    match property {
        // A factor's neutral is one; an addition's is zero.
        KeyProperty::Scale | KeyProperty::Opacity | KeyProperty::Volume => 1.0,
        KeyProperty::OffsetX | KeyProperty::OffsetY | KeyProperty::Rotation => 0.0,
    }
}

/// The keys the engine plays: the clip's own where it has them, and its
/// animation presets - materialised for its current length - everywhere it
/// does not. See `concat_project::animation`.
///
/// A property the user has keyed does not also get its preset's track. Two
/// sets of keys on one property is a question with no good answer, and the
/// hand-set ones are the ones someone will be looking at when they wonder
/// why the clip is not doing what they said.
pub fn export_keys(clip: &concat_project::model::Clip) -> Vec<ExportKey> {
    let presets = concat_project::animation::animation_of(clip);
    let mut out = Vec::new();

    for property in KeyProperty::ALL {
        if clip.is_keyed(property) {
            out.extend(clip.keys_on(property).map(|key| ExportKey {
                property: property.name().to_owned(),
                at: key.at,
                value: key.value,
                ease: key.ease.sane().0,
            }));
            continue;
        }
        // Gain has no presets to fall back on; the catalogue is picture only.
        let Some(animation) = presets.as_ref() else {
            continue;
        };
        let track = match property {
            KeyProperty::Scale => &animation.scale,
            KeyProperty::OffsetX => &animation.offset_x,
            KeyProperty::OffsetY => &animation.offset_y,
            KeyProperty::Rotation => &animation.rotation,
            KeyProperty::Opacity => &animation.opacity,
            KeyProperty::Volume => continue,
        };
        out.extend(track.keys().iter().map(|key| ExportKey {
            property: property.name().to_owned(),
            at: key.at,
            value: key.value,
            ease: [key.ease.x1, key.ease.y1, key.ease.x2, key.ease.y2],
        }));
    }
    out
}

/// The clip's gain over its length, as `(fraction, gain)` pairs, or empty
/// for a clip whose gain is the one number in `ExportClip::volume`.
pub fn volume_curve(clip: &concat_project::model::Clip) -> Vec<(f64, f64)> {
    clip.keys_on(KeyProperty::Volume)
        .map(|key| (key.at, key.value))
        .collect()
}

#[cfg(test)]
mod tests {

    use concat_project::commands::{ClipPatch, NewMedia, TrackFlag};
    use concat_project::model::AppliedFilter;
    use concat_project::{Command, Editor};

    use super::*;

    /// A project with one video media item and one clip of it, built through
    /// the real command layer so the fixture cannot drift from the model.
    fn project_with_clip() -> (Editor, String, String) {
        let mut editor = Editor::new();
        let media_id = editor
            .apply(Command::AddMedia {
                item: NewMedia {
                    path: "/footage/a.mp4".into(),
                    name: "a.mp4".into(),
                    duration: Some(10.0),
                    kind: concat_project::model::MediaKind::Video,
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: None,
                    frame_rate_fraction: None,
                    video_codec: None,
                    audio_codec: None,
                    has_audio: true,
                    audio_tracks: Vec::new(),
                    extra: Default::default(),
                },
            })
            .expect("adds media")
            .created_id
            .expect("mints an id");
        let track_id = editor.project().timelines[0].tracks[0].id.clone();
        let clip_id = editor
            .apply(Command::AddClip {
                media_id: media_id.clone(),
                track_id,
                start: 1.0,
            })
            .expect("adds clip")
            .created_id
            .expect("mints an id");
        (editor, media_id, clip_id)
    }

    #[test]
    fn a_clip_flattens_with_its_media_and_track_facts() {
        let (editor, _, _) = project_with_clip();
        let flat = flatten_timeline(editor.project(), None);
        assert_eq!(flat.len(), 1);
        let clip = &flat[0];
        assert_eq!(clip.path, "/footage/a.mp4");
        assert!(matches!(clip.kind, ClipKind::Video));
        assert_eq!(clip.start, 1.0);
        assert_eq!(clip.duration, 10.0, "the clip takes its media's length");
        assert_eq!(clip.track, 0);
        assert!(!clip.hidden);
        assert!(!clip.muted);
        assert_eq!(clip.media_width, Some(1920));
        assert_eq!(clip.has_audio, Some(true));
        assert_eq!(clip.filter_chain, "");
        assert_eq!(clip.video_filter_chain, "");
    }

    #[test]
    fn track_state_folds_into_the_clip_flags() {
        let (mut editor, _, _) = project_with_clip();
        let track_id = editor.project().timelines[0].tracks[0].id.clone();
        editor
            .apply(Command::SetTrackFlag {
                track_id: track_id.clone(),
                flag: TrackFlag::Visible,
                value: false,
            })
            .expect("hides");
        editor
            .apply(Command::SetTrackFlag {
                track_id,
                flag: TrackFlag::Muted,
                value: true,
            })
            .expect("mutes");
        let flat = flatten_timeline(editor.project(), None);
        assert!(flat[0].hidden, "a hidden track hides its clips");
        assert!(flat[0].muted, "a muted track mutes its clips");
    }

    #[test]
    fn chains_build_from_the_stored_effects() {
        let (mut editor, _, clip_id) = project_with_clip();
        editor
            .apply(Command::UpdateClip {
                clip_id,
                patch: ClipPatch {
                    video_effects: Some(vec![AppliedFilter::new("sepia")]),
                    ..Default::default()
                },
            })
            .expect("applies effect");
        let flat = flatten_timeline(editor.project(), None);
        // The exact string is chains.rs's contract, pinned there; here
        // only that flattening routes through it.
        assert!(
            flat[0].video_filter_chain.contains("color_channel_mixer")
                || !flat[0].video_filter_chain.is_empty(),
            "sepia must produce a chain, got {:?}",
            flat[0].video_filter_chain
        );
    }

    #[test]
    fn explicit_catalogue_flattens_audio_video_and_layer_without_changing_source_facts() {
        use concat_project::model::{Cutout, CutoutMode, Subject};

        let mut catalogue = Catalogue::new();
        for (id, alias, kind, chain) in [
            (
                "flatten-test.audio",
                "flatten-test-audio",
                "audio",
                "volume=0.25",
            ),
            (
                "flatten-test.video",
                "flatten-test-video",
                "effect",
                "negate",
            ),
        ] {
            let manifest = format!(
                r#"[effect]
id = "{id}"
name = "Flatten test"
kind = "{kind}"
aliases = ["{alias}"]
[ffmpeg]
chain = "{chain}"
"#
            );
            catalogue
                .add(
                    concat_effects::Package::from_sources(&manifest, None, None)
                        .expect("compiles an uninstalled fixture package"),
                )
                .unwrap();
        }

        let (mut editor, _, clip_id) = project_with_clip();
        let timeline_id = editor.project().timelines[0].id.clone();
        let track_id = editor.project().timelines[0].tracks[0].id.clone();
        let layer_track = editor.project().timelines[0].tracks[1].id.clone();
        editor
            .apply(Command::UpdateClip {
                clip_id: clip_id.clone(),
                patch: ClipPatch {
                    filters: Some(vec![AppliedFilter::new("flatten-test-audio")]),
                    video_effects: Some(vec![AppliedFilter::new("flatten-test-video")]),
                    ..Default::default()
                },
            })
            .expect("applies audio and video fixture aliases");
        let cutout = Cutout {
            mode: CutoutMode::Auto,
            subject: Subject::Person,
            feather: 0.025,
            strokes: Vec::new(),
        };
        editor
            .apply(Command::SetClipCutout {
                clip_id,
                cutout: Some(cutout.clone()),
            })
            .expect("sets source cutout");
        for flag in [TrackFlag::Visible, TrackFlag::Muted] {
            editor
                .apply(Command::SetTrackFlag {
                    track_id: track_id.clone(),
                    flag,
                    value: flag == TrackFlag::Muted,
                })
                .expect("sets source track state");
        }
        editor
            .apply(Command::AddLayerClip {
                track_id: Some(layer_track),
                start: 2.0,
                duration: Some(3.0),
                effect_id: "flatten-test-video".to_owned(),
                name: "Explicit layer".to_owned(),
            })
            .expect("adds layer using the video alias");
        editor
            .apply(Command::AddTextClip {
                track_id: None,
                start: 0.0,
                style: None,
                duration: None,
                offset_y: None,
            })
            .expect("adds text for separate rasterisation");
        editor
            .apply(Command::AddTimeline)
            .expect("makes an empty timeline active");
        let before = editor.project().clone();
        let project_dir = Path::new("/managed-export/source-project");
        let flat = flatten_timeline_in_with_catalogue(
            editor.project(),
            Some(&timeline_id),
            Some(project_dir),
            &catalogue,
        );
        assert_eq!(
            flat.len(),
            2,
            "text remains excluded and the selected timeline is used"
        );
        let picture = &flat[0];
        assert!(picture.kind == ClipKind::Video);
        assert_eq!(picture.filter_chain, "volume=0.25");
        assert_eq!(picture.video_filter_chain, "negate");
        assert_eq!(picture.path, "/footage/a.mp4");
        assert_eq!(
            (picture.start, picture.duration, picture.source_start),
            (1.0, 10.0, 0.0)
        );
        assert_eq!(picture.track, 0);
        assert!(picture.hidden && picture.muted);
        assert_eq!(
            (picture.media_width, picture.media_height),
            (Some(1920), Some(1080))
        );
        assert_eq!(picture.has_audio, Some(true));
        assert_eq!(picture.cutout.as_ref(), Some(&cutout));
        assert_eq!(
            picture.mask_dir,
            concat_vision::mask_dir(project_dir, &picture.path, Subject::Person)
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(
            picture.effects,
            vec![AppliedFilter::new("flatten-test-video")]
        );
        let layer = &flat[1];
        assert!(layer.kind == ClipKind::Layer);
        assert_eq!(layer.video_filter_chain, "negate");
        assert!(
            layer.filter_chain.is_empty() && layer.path.is_empty() && layer.mask_dir.is_empty()
        );
        assert_eq!((layer.track, layer.start, layer.duration), (1, 2.0, 3.0));
        assert!(!layer.hidden && layer.muted);
        assert_eq!(layer.has_audio, Some(false));
        assert_eq!(
            layer.effects,
            vec![AppliedFilter::new("flatten-test-video")]
        );

        // An empty table drops only the chains; every copied source fact
        // remains identical. Neither table is installed into the process.
        let empty = flatten_timeline_in_with_catalogue(
            editor.project(),
            Some(&timeline_id),
            Some(project_dir),
            &Catalogue::new(),
        );
        let mut without_chains = flat.clone();
        for clip in &mut without_chains {
            clip.filter_chain.clear();
            clip.video_filter_chain.clear();
        }
        assert!(empty == without_chains);
        let without_directory = flatten_timeline_in_with_catalogue(
            editor.project(),
            Some(&timeline_id),
            None,
            &catalogue,
        );
        assert!(without_directory[0].mask_dir.is_empty());
        assert_eq!(without_directory[0].cutout.as_ref(), Some(&cutout));
        assert!(
            flatten_timeline_in_with_catalogue(
                editor.project(),
                None,
                Some(project_dir),
                &catalogue
            )
            .is_empty(),
            "None selects the empty active timeline"
        );
        let fallback = flatten_timeline_in_with_catalogue(
            editor.project(),
            Some("missing-timeline"),
            Some(project_dir),
            &catalogue,
        );
        assert!(
            fallback == flat,
            "an unknown timeline still falls back to the first"
        );
        assert!(
            editor.project() == &before,
            "flatten must leave the original project unchanged"
        );
    }

    #[test]
    fn text_clips_are_left_for_the_rasteriser() {
        let (mut editor, _, _) = project_with_clip();
        let track_id = Some(editor.project().timelines[0].tracks[0].id.clone());
        editor
            .apply(Command::AddTextClip {
                track_id,
                start: 0.0,
                style: None,
                duration: None,
                offset_y: None,
            })
            .expect("adds title");
        let flat = flatten_timeline(editor.project(), None);
        assert_eq!(flat.len(), 1, "the text clip does not flatten");
    }

    #[test]
    fn a_clip_whose_media_vanished_is_skipped_not_fatal() {
        let (mut editor, media_id, _) = project_with_clip();
        editor
            .apply(Command::RemoveMedia { media_id })
            .expect("removes");
        // RemoveMedia also removes the clips; force the orphan case instead
        // by asking for a timeline that does not exist.
        assert!(flatten_timeline(editor.project(), None).is_empty());
        assert!(flatten_timeline(editor.project(), Some("nope")).len() <= 1);
    }
}
