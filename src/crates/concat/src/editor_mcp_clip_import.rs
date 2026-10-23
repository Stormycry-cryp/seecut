// SPDX-License-Identifier: AGPL-3.0-or-later
//! One immutable library version probed off-thread, committed by the original UI operation.
use super::*;
use crate::personal_library::{AssetKind, AssetReference, Library};
use canvas_policy::OutputOriginCapture;
use concat_editor_mcp::{AssetVersion, ClipEdit};
use concat_host::media::{self, MediaSummary};
use concat_project::commands::NewMedia;
use concat_project::model::MediaKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_IMPORT_AUDIO_TRACKS: usize = 128;
static IMPORT_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Held through the UI completion. A stuck native probe holds the slot; it does
/// not spawn additional probes or pretend that a deadline killed native FFmpeg.
struct ImportSlot;
impl ImportSlot {
    fn acquire() -> Result<Self, &'static str> {
        IMPORT_IN_FLIGHT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "resourceLimit")
    }
}
impl Drop for ImportSlot {
    fn drop(&mut self) {
        IMPORT_IN_FLIGHT.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    bytes: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
impl FileStamp {
    fn read(path: &Path) -> Result<Self, &'static str> {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "assetUnavailable")?;
        if !metadata.is_file() {
            return Err("assetUnavailable");
        }
        if metadata.len() > MAX_IMPORT_BYTES {
            return Err("resourceLimit");
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

struct PreparedImport {
    item: NewMedia,
    path: PathBuf,
    stamp: FileStamp,
}
impl PreparedImport {
    fn current(&self) -> bool {
        FileStamp::read(&self.path).is_ok_and(|stamp| stamp == self.stamp)
    }
}

fn expected_kind(kind: AssetKind) -> MediaKind {
    match kind {
        AssetKind::Image => MediaKind::Image,
        AssetKind::Video => MediaKind::Video,
        AssetKind::Audio => MediaKind::Audio,
    }
}

fn prepare_import_from_root(
    root: &Path,
    asset_id: &str,
    version: &AssetVersion,
) -> Result<PreparedImport, &'static str> {
    prepare_import_with(root, asset_id, version, |path| {
        media::probe(&path.to_string_lossy())
    })
}

/// Injection is private test machinery; production always calls the original
/// media::probe, never a model-supplied decoder, path, command or project JSON.
fn prepare_import_with(
    root: &Path,
    asset_id: &str,
    version: &AssetVersion,
    probe: impl FnOnce(&Path) -> Result<MediaSummary, String>,
) -> Result<PreparedImport, &'static str> {
    concat_editor_mcp::validate_library_asset(asset_id, version).map_err(|_| "invalidInput")?;
    let library = Library::load_for_relink_bounded(
        root,
        library_assets::MAX_LIBRARY_MANIFEST,
        library_assets::MAX_LIBRARY_ASSETS,
    )
    .map_err(|e| {
        if e == "resourceLimit" {
            "resourceLimit"
        } else {
            "assetUnavailable"
        }
    })?;
    let mut matches = library
        .items()
        .iter()
        .filter(|a| a.id == asset_id && !a.trashed);
    let asset = matches.next().ok_or("assetUnavailable")?;
    if matches.next().is_some() || library_assets::fixed_version(asset).as_ref() != Some(version) {
        return Err("assetUnavailable");
    }
    let (path, _) = crate::personal_library::checked_managed_version_metadata(root, asset)
        .map_err(|_| "assetUnavailable")?;
    let stamp = FileStamp::read(&path)?;
    // Bounded variant shares the existing managed path/version checker. This is
    // the first full SHA pass, not an extra pass followed by the GUI pre-check.
    crate::personal_library::checked_managed_version_path_bounded(root, asset, MAX_IMPORT_BYTES)
        .map_err(|e| {
            if e == "resourceLimit" {
                "resourceLimit"
            } else {
                "assetUnavailable"
            }
        })?;
    let summary = probe(&path).map_err(|_| "assetUnavailable")?;
    let reference = AssetReference {
        asset_id: asset_id.into(),
        version: Some(crate::personal_library::AssetVersion {
            sha256: version.sha256.clone(),
            registration_id: version.registration_id.clone(),
            previous_id: version.previous_id.clone(),
        }),
    };
    // Second SHA pass: same before/after-probe guarantee as GUI ImportNamed,
    // with an explicit byte limit even if another actor grows the file.
    crate::personal_library::verify_asset_reference_bounded(
        &path,
        Some(&reference),
        MAX_IMPORT_BYTES,
    )
    .map_err(|e| {
        if e == "resourceLimit" {
            "resourceLimit"
        } else {
            "assetUnavailable"
        }
    })?;
    if FileStamp::read(&path)? != stamp
        || summary.path != path.to_string_lossy()
        || summary.kind != expected_kind(asset.kind)
    {
        return Err("assetUnavailable");
    }
    if summary.audio_tracks.len() > MAX_IMPORT_AUDIO_TRACKS {
        return Err("resourceLimit");
    }
    let mut item = summary.to_new_media();
    item.name = library_assets::display_name(&asset.name);
    item.extra.insert(
        "seecutLibraryReference".into(),
        serde_json::to_value(&reference).map_err(|_| "assetUnavailable")?,
    );
    let fixed = reference.version.as_ref().ok_or("assetUnavailable")?;
    item.extra.insert(
        crate::project_library_relink::BINDING_KEY.into(),
        serde_json::to_value(crate::project_library_relink::LibraryBinding {
            version: 1,
            asset_id: asset_id.into(),
            registration_id: fixed.registration_id.clone(),
            sha256: fixed.sha256.clone(),
            previous_path: item.path.clone(),
            verified_original: true,
        })
        .map_err(|_| "assetUnavailable")?,
    );
    Ok(PreparedImport { item, path, stamp })
}

struct Origin {
    request: Request,
    scope: Scope,
    permit: Permit,
    generation: u64,
    account: OutputOriginCapture,
}
impl Origin {
    fn current(&self, studio: &Studio, app: &App) -> Result<(), Response> {
        {
            let mut bridge = studio.editor_mcp.borrow_mut();
            let Some((kind, ids)) = bridge.active_ids(studio, app) else {
                return Err(Response::error("wrongProject"));
            };
            if kind != "clip"
                || bridge.scope(&ids) != self.scope
                || library_assets::output_origin(&bridge) != self.account
            {
                return Err(Response::error("notAuthorized"));
            }
            let p = self
                .request
                .clip_parameters
                .as_ref()
                .expect("frozen clip import");
            let query = concat_editor_mcp::OperationQuery {
                operation_id: p.operation_id.clone(),
                client_sequence: p.client_sequence,
            };
            if !bridge.canvas_receipts.contains_family(
                &self.scope,
                &self.request.client_id,
                &query,
                Instant::now(),
                true,
            ) {
                return Err(Response::error("outcomeUnknown"));
            }
        }
        clip_execute::final_check(
            studio,
            app,
            &self.request,
            &self.scope,
            &self.permit,
            self.generation,
        )
    }
    fn response(&self, status: &str, value: Value) -> Response {
        clip_execute::response(&self.request, &self.scope, status, value)
    }
    fn finish(&self, studio: &Studio, response: Response) {
        let p = self
            .request
            .clip_parameters
            .as_ref()
            .expect("frozen clip import");
        if !studio.editor_mcp.borrow_mut().canvas_receipts.finish(
            &self.scope,
            &self.request.client_id,
            p,
            response,
        ) {
            log::warn!("original clip import receipt unavailable at completion");
        }
    }
}

/// Called only after the original execute_clip reservation and exact permit.
/// The caller stores the returned pending/refusal in that original receipt and
/// write registry before this UI callback yields. Replays hit the cached result.
pub(super) fn queue_import(
    request: &Request,
    scope: &Scope,
    permit: &Permit,
    generation: u64,
    studio: &Studio,
    app: &App,
) -> Response {
    let p = request.clip_parameters.as_ref().expect("typed clip import");
    let ClipEdit::ImportLibrary { asset_id, version } = &p.action.edit else {
        return Response::error("invalidInput");
    };
    let origin = Origin {
        request: request.clone(),
        scope: scope.clone(),
        permit: permit.clone(),
        generation,
        account: library_assets::output_origin(&studio.editor_mcp.borrow()),
    };
    if let Err(error) = origin.current(studio, app) {
        return origin.response(&error.status, json!({}));
    }
    if let Err(error) = clip_edit::approval_preflight(studio, &p.action) {
        return origin.response(error.status(), json!({}));
    }
    let state = match clip_edit::state(studio, &[]) {
        Ok(value) => value,
        Err(e) => return origin.response(e.status(), json!({})),
    };
    let slot = match ImportSlot::acquire() {
        Ok(slot) => slot,
        Err(status) => return origin.response(status, json!({})),
    };
    let asset_id = asset_id.clone();
    let version = version.clone();
    let pending=origin.response("pending",json!({"sourceRevision":request.revision,
        "sourceDocumentStateId":state["documentStateId"],"sourceEditStateId":p.action.expected_edit_state_id,
        "selectionRevision":p.selection_revision}));
    crate::host::spawn(
        move || {
            let result = concat_host::AppDirs::locate()
                .map_err(|_| "assetUnavailable")
                .and_then(|dirs| {
                    prepare_import_from_root(
                        &dirs.data.join("personal-library"),
                        &asset_id,
                        &version,
                    )
                });
            (slot, result)
        },
        move |studio, app, _models, (_slot, result)| {
            let response = match origin.current(studio, app) {
                Err(error) => origin.response(&error.status, json!({})),
                Ok(()) => match result {
                    Err(status) => origin.response(status, json!({})),
                    Ok(prepared) => {
                        if !prepared.current() {
                            origin.finish(studio, origin.response("assetUnavailable", json!({})));
                            return;
                        }
                        let p = origin
                            .request
                            .clip_parameters
                            .as_ref()
                            .expect("frozen clip import");
                        let expected = clip_edit::ClipBinding {
                            project_id: &origin.scope.project,
                            document_session_id: &origin.scope.session,
                            generation: origin.generation,
                            revision: origin.request.revision.expect("frozen revision"),
                            selection_revision: p.selection_revision,
                            timeline_id: &p.action.timeline_id,
                            expected_edit_state_id: &p.action.expected_edit_state_id,
                        };
                        let actual = clip_edit::ClipIdentity {
                            project_id: &origin.scope.project,
                            document_session_id: &origin.scope.session,
                        };
                        let before = studio.mcp_clip_state().1;
                        let mut refusal = None;
                        let result = clip_edit::execute_import(
                            studio,
                            &expected,
                            &actual,
                            &p.action.edit,
                            &prepared.item,
                            |current| match origin.current(current, app) {
                                Err(error) => {
                                    refusal = Some(error);
                                    Err(clip_edit::ClipEditError::CommitFailed)
                                }
                                Ok(()) if !prepared.current() => {
                                    refusal = Some(Response::error("assetUnavailable"));
                                    Err(clip_edit::ClipEditError::CommitFailed)
                                }
                                Ok(()) => Ok(()),
                            },
                        );
                        match result {
                            Ok(receipt) => origin.response(
                                if receipt.changed { "ok" } else { "unchanged" },
                                serde_json::to_value(receipt).expect("bounded clip import receipt"),
                            ),
                            Err(error) => {
                                let status = if studio.mcp_clip_state().1 != before {
                                    "outcomeUnknown"
                                } else {
                                    refusal
                                        .as_ref()
                                        .map_or(error.status(), |e| e.status.as_str())
                                };
                                origin.response(status, json!({}))
                            }
                        }
                    }
                },
            };
            origin.finish(studio, response);
        },
    );
    pending
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        asset_id: String,
        version: AssetVersion,
        path: PathBuf,
    }
    impl Fixture {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let base = std::env::temp_dir().join(format!(
                "seecut-fixed-library-import-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&base).unwrap();
            let base = base.canonicalize().unwrap();
            let source = base.join(name);
            std::fs::write(&source, bytes).unwrap();
            let root = base.join("library");
            let mut library = Library::load(&root).unwrap();
            let asset = library.import(&source).unwrap().clone();
            let version = library_assets::fixed_version(&asset).unwrap();
            Self {
                base,
                root,
                asset_id: asset.id,
                version,
                path: asset.path,
            }
        }
        fn prepare(&self) -> Result<PreparedImport, &'static str> {
            prepare_import_from_root(&self.root, &self.asset_id, &self.version)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn real_image_video_audio_prepare_frozen_versions_without_manifest_mutation() {
        for (name, bytes, kind) in [
            (
                "image.png",
                include_bytes!("../tests/fixtures/agent-library/image.png").as_slice(),
                MediaKind::Image,
            ),
            (
                "video.mp4",
                include_bytes!("../tests/fixtures/agent-library/video.mp4").as_slice(),
                MediaKind::Video,
            ),
            (
                "audio.wav",
                include_bytes!("../tests/fixtures/agent-library/audio.wav").as_slice(),
                MediaKind::Audio,
            ),
        ] {
            let fixture = Fixture::new(name, bytes);
            let original =
                std::fs::read(fixture.root.join(crate::personal_library::MANIFEST_NAME)).unwrap();
            let prepared = fixture.prepare().unwrap();
            assert_eq!(prepared.item.kind, kind);
            assert!(prepared.current());
            assert_eq!(prepared.item.path, fixture.path.to_string_lossy());
            let reference = &prepared.item.extra["seecutLibraryReference"];
            assert_eq!(reference["asset_id"], fixture.asset_id);
            assert_eq!(reference["version"]["sha256"], fixture.version.sha256);
            assert_eq!(
                reference["version"]["registration_id"],
                fixture.version.registration_id
            );
            if kind == MediaKind::Image {
                assert_eq!(
                    (prepared.item.width, prepared.item.height),
                    (Some(16), Some(16))
                );
            }
            if kind == MediaKind::Video {
                assert_eq!(
                    (prepared.item.width, prepared.item.height),
                    (Some(64), Some(64))
                );
                assert!((prepared.item.duration.unwrap() - 1.0).abs() < 0.01);
                assert_eq!(prepared.item.frame_rate, Some(4.0));
                assert!(!prepared.item.has_audio);
            }
            if kind == MediaKind::Audio {
                assert!((prepared.item.duration.unwrap() - 0.25).abs() < 0.01);
                assert_eq!(prepared.item.audio_tracks.len(), 1);
                assert_eq!(prepared.item.audio_tracks[0].sample_rate, 22_050);
                assert_eq!(prepared.item.audio_tracks[0].channels, 2);
            }
            assert_eq!(
                std::fs::read(fixture.root.join(crate::personal_library::MANIFEST_NAME)).unwrap(),
                original
            );
        }
    }
    #[test]
    fn changed_fixed_version_is_refused_before_probe() {
        let fixture = Fixture::new(
            "image.png",
            include_bytes!("../tests/fixtures/agent-library/image.png"),
        );
        let mut wrong = fixture.version.clone();
        wrong.registration_id = "another-version".into();
        let result = prepare_import_with(&fixture.root, &fixture.asset_id, &wrong, |_| {
            panic!("must not probe")
        });
        assert_eq!(result.err(), Some("assetUnavailable"));
    }
    #[test]
    fn content_changed_during_real_probe_is_refused_and_never_prepared() {
        let fixture = Fixture::new(
            "image.png",
            include_bytes!("../tests/fixtures/agent-library/image.png"),
        );
        let result =
            prepare_import_with(&fixture.root, &fixture.asset_id, &fixture.version, |path| {
                let summary = media::probe(&path.to_string_lossy())?;
                std::fs::write(path, b"changed after successful native probe").unwrap();
                Ok(summary)
            });
        assert_eq!(result.err(), Some("assetUnavailable"));
    }
    #[test]
    fn probe_kind_must_agree_with_fixed_library_kind() {
        let fixture = Fixture::new(
            "image.png",
            include_bytes!("../tests/fixtures/agent-library/image.png"),
        );
        let result =
            prepare_import_with(&fixture.root, &fixture.asset_id, &fixture.version, |path| {
                let mut summary = media::probe(&path.to_string_lossy())?;
                summary.kind = MediaKind::Audio;
                Ok(summary)
            });
        assert_eq!(result.err(), Some("assetUnavailable"));
    }
    #[test]
    fn one_probe_slot_remains_held_until_completion_and_releases_after_drop() {
        let first = ImportSlot::acquire().unwrap();
        assert!(ImportSlot::acquire().is_err());
        drop(first);
        let second = ImportSlot::acquire().unwrap();
        drop(second);
    }
    #[test]
    fn prepared_media_uses_real_session_addmedia_dedup_and_same_undo_redo_history() {
        let fixture = Fixture::new(
            "audio.wav",
            include_bytes!("../tests/fixtures/agent-library/audio.wav"),
        );
        let prepared = fixture.prepare().unwrap();
        let project = fixture.base.join("project");
        std::fs::create_dir(&project).unwrap();
        let mut session = concat_host::Session::open_owned(
            &project.to_string_lossy(),
            concat_project::DocumentSettings {
                name: "library-import".into(),
                width: 640,
                height: 360,
                rate_num: 30,
                rate_den: 1,
            },
        )
        .unwrap();
        let first = session
            .apply_outcome(concat_project::Command::AddMedia {
                item: prepared.item.clone(),
            })
            .unwrap();
        assert!(first.applied);
        let id = first.created_id.unwrap();
        let reference = clip_edit::library_reference(&session.project().media[0]).unwrap();
        assert_eq!(
            reference,
            json!({"assetId":fixture.asset_id,"version":fixture.version})
        );
        assert!(reference.get("path").is_none());
        let second = session
            .apply_outcome(concat_project::Command::AddMedia {
                item: prepared.item,
            })
            .unwrap();
        assert!(!second.applied);
        assert!(second.created_id.is_none());
        assert_eq!(session.project().media.len(), 1);
        assert_eq!(session.project().media[0].id, id);
        assert!(session.history_outcome(false).applied);
        assert!(session.project().media.is_empty());
        assert!(session.history_outcome(true).applied);
        assert_eq!(session.project().media[0].id, id);
        drop(session);
    }
}
