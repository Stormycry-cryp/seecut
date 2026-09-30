// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The engine-owned editing session.
//!
//! Open a project folder and the engine holds the edit: every mutation is a
//! `concat_project` [`Command`], applied with undo recorded, and the new
//! state is what the window draws. The window never keeps a model of its
//! own; it renders the [`Project`] this session hands back.
//!
//! Saving reuses `projects::save_owned`'s temp-file-and-rename, so the document on
//! disk is written by exactly one code path.

use concat_project::model::VideoSettings;
use concat_project::{Command, DocumentSettings, Editor, Project};
use serde::Serialize;

use crate::ownership::WriterGuard;
use crate::projects;
use crate::projects::{CreatedProject, OwnedProjectError};

/// One open project: its folder, its settings and its undo history.
pub struct Session {
    /// The project folder, for saving.
    path: String,
    settings: DocumentSettings,
    editor: Editor,
    // Some on audited desktop platforms. None preserves native compatibility
    // elsewhere without claiming ownership protection or granting MCP writes.
    owner: Option<WriterGuard>,
}

/// What every mutating call returns: the authoritative state plus history
/// availability, so undo/redo affordances are never guessing.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EditorView {
    /// The whole project, as the engine holds it.
    pub project: Project,
    /// Whether there is something to undo.
    pub can_undo: bool,
    /// Whether there is something to redo.
    pub can_redo: bool,
    /// The settings as the session holds them - the document's own output
    /// size wins over the manifest's on open.
    pub settings: SettingsView,
    /// The id a creating command minted, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_id: Option<String>,
}

/// The session's settings, as the window shows them.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    /// Project name.
    pub name: String,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub rate_num: i64,
    /// Frame rate denominator.
    pub rate_den: i64,
}

impl Session {
    /// Opens a project folder as the editing session.
    ///
    /// A folder whose document is missing opens as an empty
    /// project rather than failing, but a *corrupt* document is an error,
    /// because silently replacing an edit with emptiness is how projects get
    /// lost. `settings` come from the manifest and seed the first timeline
    /// of a project that has no document yet; a document that loads brings
    /// every timeline's own frame with it, and those win, because that is
    /// where an edited frame was saved.
    pub fn open(path: &str, settings: DocumentSettings) -> Result<Session, String> {
        Self::open_owned(path, settings).map_err(|error| error.to_string())
    }

    /// Opens an independent writer with typed ownership/project errors. The
    /// owner is acquired before reading any mutable document state.
    pub fn open_owned(
        path: &str,
        settings: DocumentSettings,
    ) -> Result<Session, OwnedProjectError> {
        let owner = projects::acquire_project_owner(std::path::Path::new(path))?;
        let path = owner
            .as_ref()
            .map(|owner| owner.identity().target().to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        Self::load_owned(path, settings, owner)
    }

    pub(crate) fn from_created(created: CreatedProject) -> Result<Session, OwnedProjectError> {
        let (info, owner) = created.into_parts();
        let settings = settings_from_info(&info);
        Self::load_owned(info.path, settings, owner)
    }

    fn load_owned(
        path: String,
        settings: DocumentSettings,
        owner: Option<WriterGuard>,
    ) -> Result<Session, OwnedProjectError> {
        projects::validate_project_owner(&path, owner.as_ref())?;
        // Only a genuinely absent manifest seeds emptiness. A failed read or
        // JSON parse of an existing document must not prepare an empty overwrite.
        let manifest = projects::manifest_path(std::path::Path::new(&path));
        let document = match std::fs::symlink_metadata(&manifest) {
            Ok(_) => Some(projects::read_document(&path)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!("could not inspect {}: {error}", manifest.display()).into());
            }
        };
        let editor = match document {
            Some(document) => match Editor::from_document(&document) {
                Some(editor) => editor,
                // The settings-only manifest `create` writes: a project
                // closed before its first edit reopens empty, it is not
                // corrupt.
                None if projects::is_settings_only(&document) => {
                    Editor::with_video(settings.video())
                }
                None if concat_project::document_version(&document)
                    > concat_project::DOCUMENT_VERSION =>
                {
                    return Err(format!(
                        "{path} was saved by a newer Concat than this one: update to open it"
                    )
                    .into());
                }
                None => {
                    return Err(format!("{path} holds a document this build cannot read").into());
                }
            },
            // No document yet - a project created moments ago.
            None => Editor::with_video(settings.video()),
        };
        Ok(Session {
            path,
            settings,
            editor,
            owner,
        })
    }

    /// Opens the project a [`projects::ProjectInfo`] describes.
    pub fn open_info(info: &projects::ProjectInfo) -> Result<Session, String> {
        Self::open_info_owned(info).map_err(|error| error.to_string())
    }

    /// Opens the described independent writer with branchable ownership errors.
    pub fn open_info_owned(info: &projects::ProjectInfo) -> Result<Session, OwnedProjectError> {
        Self::open_owned(&info.path, settings_from_info(info))
    }

    /// Checks whether this Session already owns a target, without creating a
    /// sidecar or opening a second writer. Unrelated missing targets are false.
    pub fn matches_project(&self, path: &str) -> Result<bool, OwnedProjectError> {
        if let Some(owner) = self.owner.as_ref() {
            return Ok(owner.matches_project(path)?);
        }
        // Portable native compatibility: canonical comparison only, no claim
        // that another writer is prevented and no ownership sidecar creation.
        match std::fs::canonicalize(path) {
            Ok(target) => Ok(target
                == std::fs::canonicalize(&self.path)
                    .map_err(|error| format!("could not resolve {}: {error}", self.path))?),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(format!("could not resolve {path}: {error}").into()),
        }
    }

    /// Retains the Session's existing owner for a save worker. This clone must
    /// live until that worker finishes or is discarded; it cannot create another
    /// logical Session. None is the explicit unsupported-native compatibility path.
    pub fn writer_guard(&self) -> Option<WriterGuard> {
        self.owner.clone()
    }

    /// The project folder.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The session's settings: the project's name, and the frame and rate of
    /// the timeline being edited.
    ///
    /// Built rather than stored, because the frame is the active timeline's
    /// and the active timeline changes under a tab click. Everything that
    /// renders - the monitor, the export - asks here and gets the frame of
    /// whatever it is about to draw.
    pub fn settings(&self) -> DocumentSettings {
        let video = self.editor.project().active().video;
        DocumentSettings {
            name: self.settings.name.clone(),
            width: video.width,
            height: video.height,
            rate_num: video.rate_num,
            rate_den: video.rate_den,
        }
    }

    /// The active timeline's frame and rate.
    pub fn video(&self) -> VideoSettings {
        self.editor.project().active().video
    }

    /// Sets the active timeline's frame and rate, as an edit - undoable,
    /// and this timeline's alone. Anything a frame could not be is ignored
    /// by the command, for the reason a zero dimension is in
    /// [`Session::prepare_save`].
    pub fn set_video(&mut self, video: VideoSettings) -> Result<EditorView, String> {
        let timeline_id = self.editor.project().active_timeline_id.clone();
        self.apply(Command::SetTimelineVideo { timeline_id, video })
    }

    /// The edit as it stands.
    pub fn project(&self) -> &Project {
        self.editor.project()
    }

    /// Whether there is something to undo.
    pub fn can_undo(&self) -> bool {
        self.editor.can_undo()
    }

    /// Whether there is something to redo.
    pub fn can_redo(&self) -> bool {
        self.editor.can_redo()
    }

    /// The current state without changing anything.
    pub fn view(&self) -> EditorView {
        self.view_with(None)
    }

    fn view_with(&self, created_id: Option<String>) -> EditorView {
        let settings = self.settings();
        EditorView {
            project: self.editor.project().clone(),
            can_undo: self.editor.can_undo(),
            can_redo: self.editor.can_redo(),
            settings: SettingsView {
                name: settings.name,
                width: settings.width,
                height: settings.height,
                rate_num: settings.rate_num,
                rate_den: settings.rate_den,
            },
            created_id,
        }
    }

    /// Applies one edit command and returns the new state.
    pub fn apply(&mut self, command: Command) -> Result<EditorView, String> {
        self.apply_within(None, command)
    }

    /// Applies one edit command as a move of `gesture`, so that a knob
    /// dragged through many values is one undo step; see
    /// `concat_project::Editor::apply_within`. `None` is a step of its own.
    pub fn apply_within(
        &mut self,
        gesture: Option<&str>,
        command: Command,
    ) -> Result<EditorView, String> {
        let outcome = self
            .editor
            .apply_within(gesture, command)
            .map_err(|error| error.to_string())?;
        Ok(self.view_with(outcome.created_id))
    }

    /// Ends the gesture in progress: the next command naming it starts a
    /// new undo step.
    pub fn end_gesture(&mut self) {
        self.editor.end_gesture();
    }

    /// Steps the history back one edit.
    pub fn undo(&mut self) -> EditorView {
        self.editor.undo();
        self.view()
    }

    /// Steps the history forward one edit.
    pub fn redo(&mut self) -> EditorView {
        self.editor.redo();
        self.view()
    }

    /// Takes a new name, if there is one, and hands back what a save must
    /// write: the folder and the document. The frame is not a parameter any
    /// more - it is the active timeline's, set through [`Session::set_video`]
    /// as an edit. The disk write is the caller's, so it can happen off the
    /// thread that owns the session; [`Session::save`] does both.
    pub fn prepare_save(&mut self, name: Option<&str>) -> (String, serde_json::Value) {
        if let Some(name) = name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                self.settings.name = trimmed.to_owned();
            }
        }
        (self.path.clone(), self.editor.to_document(&self.settings))
    }

    /// Writes the session's document to its project folder.
    pub fn save(&mut self, name: Option<&str>) -> Result<(), String> {
        self.save_owned(name).map_err(|error| error.to_string())
    }

    /// Saves through the existing owner with typed errors, never reacquiring a
    /// second independent writer against this Session's own lock.
    pub fn save_owned(&mut self, name: Option<&str>) -> Result<(), OwnedProjectError> {
        let (path, document) = self.prepare_save(name);
        projects::save_owned(&path, &document, self.owner.as_ref())
    }

    /// The document as it would be saved.
    pub fn document(&self) -> serde_json::Value {
        self.editor.to_document(&self.settings)
    }

    /// The active timeline flattened for rendering. This is what export and
    /// preview consume: the engine flattens its own session, so the pixels
    /// rendered are the model's, never a copy of it.
    pub fn flattened_clips(&self) -> Vec<concat_export::ExportClip> {
        concat_export::flatten::flatten_timeline_in(
            self.editor.project(),
            None,
            Some(std::path::Path::new(&self.path)),
        )
    }
}

fn settings_from_info(info: &projects::ProjectInfo) -> DocumentSettings {
    DocumentSettings {
        name: info.name.clone(),
        width: info.width,
        height: info.height,
        rate_num: info.rate_num,
        rate_den: info.rate_den,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DocumentSettings {
        DocumentSettings {
            name: "Test".to_owned(),
            width: 1920,
            height: 1080,
            rate_num: 30,
            rate_den: 1,
        }
    }

    #[test]
    fn a_fresh_project_opens_empty_and_round_trips_a_save() {
        let scratch =
            std::env::temp_dir().join(format!("concat-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let info = projects::create(&scratch.to_string_lossy(), "Fresh", 1920, 1080, 30, 1)
            .expect("creates");

        let mut session = Session::open_info(&info).expect("opens the settings-only manifest");
        assert!(!session.can_undo());
        assert_eq!(
            (session.settings().width, session.settings().height),
            (1920, 1080),
            "the manifest's frame seeds the first timeline"
        );
        let view = session.apply(Command::AddTrack).expect("adds a track");
        assert!(view.can_undo);
        session
            .set_video(VideoSettings {
                width: 1280,
                height: 720,
                rate_num: 25,
                rate_den: 1,
            })
            .expect("sets the frame");
        session.save(Some("Renamed")).expect("saves");
        let track_count = session.project().active().tracks.len();
        drop(session);
        let reopened = Session::open(&info.path, settings()).expect("reopens");
        assert_eq!(
            reopened.settings().name,
            "Test",
            "the manifest's name is what open gets"
        );
        assert_eq!(
            (reopened.settings().width, reopened.settings().height),
            (1280, 720),
            "the document's frame wins over the manifest's"
        );
        assert_eq!(reopened.settings().rate_num, 25, "and so does its rate");
        assert_eq!(reopened.project().active().tracks.len(), track_count);

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_corrupt_document_is_refused() {
        let scratch =
            std::env::temp_dir().join(format!("concat-corrupt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        std::fs::write(scratch.join("concat.json"), br#"{"timelines": "garbage"}"#)
            .expect("writes");
        assert!(Session::open(&scratch.to_string_lossy(), settings()).is_err());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn malformed_json_is_refused_without_retaining_a_writer() {
        let scratch = projects_test_dir("malformed");
        std::fs::write(scratch.join("concat.json"), b"{broken").unwrap();
        assert!(matches!(
            Session::open_owned(&scratch.to_string_lossy(), settings()),
            Err(OwnedProjectError::Project(_))
        ));
        std::fs::write(scratch.join("concat.json"), b"{}").unwrap();
        assert!(Session::open_owned(&scratch.to_string_lossy(), settings()).is_ok());
        std::fs::remove_dir_all(scratch).unwrap();
    }

    fn projects_test_dir(label: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "concat-session-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[cfg(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))]
    #[test]
    fn creator_handoff_and_save_worker_keep_the_same_uninterrupted_owner() {
        let scratch = projects_test_dir("handoff");
        let created =
            projects::create_owned(&scratch.to_string_lossy(), "Fresh", 1920, 1080, 30, 1).unwrap();
        let info = created.info().clone();
        assert!(matches!(
            Session::open_info_owned(&info),
            Err(OwnedProjectError::Ownership(
                crate::ownership::OwnershipError::Conflict { .. }
            ))
        ));
        let mut session = created.into_session().unwrap();
        assert!(matches!(
            Session::open_info_owned(&info),
            Err(OwnedProjectError::Ownership(
                crate::ownership::OwnershipError::Conflict { .. }
            ))
        ));
        session.apply(Command::AddTrack).unwrap();
        let expected_tracks = session.project().active().tracks.len();
        session.save_owned(None).unwrap();
        let worker_owner = session.writer_guard().unwrap();
        let (path, document) = session.prepare_save(None);
        drop(session);
        assert!(matches!(
            Session::open_info_owned(&info),
            Err(OwnedProjectError::Ownership(
                crate::ownership::OwnershipError::Conflict { .. }
            ))
        ));
        let lane = projects::SaveLane::default();
        assert!(
            lane.write_owned(lane.next(), &path, &document, Some(&worker_owner))
                .unwrap()
        );
        drop(worker_owner);
        let reopened = Session::open_info_owned(&info).unwrap();
        assert_eq!(reopened.project().active().tracks.len(), expected_tracks);
        drop(reopened);
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[cfg(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))]
    #[test]
    fn a_stale_owner_cannot_save_or_hide_an_unrelated_lookup() {
        let scratch = projects_test_dir("stale");
        let created =
            projects::create_owned(&scratch.to_string_lossy(), "First", 1920, 1080, 30, 1).unwrap();
        let mut session = created.into_session().unwrap();
        let path = session.path().to_owned();
        let before = std::fs::read(std::path::Path::new(&path).join("concat.json")).unwrap();
        let lock = std::path::Path::new(&path).join(".seecut-writer.lock");
        std::fs::rename(&lock, scratch.join("old-lock")).unwrap();
        std::fs::write(&lock, b"replacement").unwrap();
        let other = scratch.join("Other");
        std::fs::create_dir(&other).unwrap();
        assert!(!session.matches_project(&other.to_string_lossy()).unwrap());
        assert!(!other.join(".seecut-writer.lock").exists());
        assert!(matches!(
            session.matches_project(&path),
            Err(OwnedProjectError::Ownership(
                crate::ownership::OwnershipError::IdentityAmbiguous { .. }
            ))
        ));
        assert!(session.save_owned(None).is_err());
        assert_eq!(
            std::fs::read(std::path::Path::new(&path).join("concat.json")).unwrap(),
            before
        );
        drop(session);
        std::fs::remove_dir_all(scratch).unwrap();
    }
}
