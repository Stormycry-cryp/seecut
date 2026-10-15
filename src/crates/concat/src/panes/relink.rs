// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Missing media and the explicit, library-backed repair preview.
use crate::studio::Studio;
use crate::ui::{MissingMediaItem, RelinkData};
use concat_project::Command;
use concat_project::model::MissingMedia;
use slint::VecModel;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub(crate) struct FolderRelinkPlan {
    pub commands: Vec<Command>,
    pub ambiguous: usize,
    pub total: usize,
}

pub(crate) fn folder_relink_allowed(busy: bool, preview: bool) -> bool {
    !busy && !preview
}

/// The original explicit folder search, planned as one undoable edit.
/// Cancellation and repair-state rejection do not scan or change a project.
pub(crate) fn plan_folder_relink(
    items: &[MissingMedia],
    folder: Option<&Path>,
    busy: bool,
    preview: bool,
) -> Option<FolderRelinkPlan> {
    if !folder_relink_allowed(busy, preview) {
        return None;
    }
    let folder = folder?;
    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut stack = vec![folder.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file()
                && let Some(name) = path.file_name().and_then(|name| name.to_str())
            {
                index.entry(name.to_owned()).or_default().push(path);
            }
        }
    }
    let mut plan = FolderRelinkPlan {
        commands: Vec::new(),
        ambiguous: 0,
        total: items.len(),
    };
    for item in items {
        let Some(name) = Path::new(&item.path)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        match index.get(name).map(Vec::as_slice) {
            Some([path]) => plan.commands.push(Command::UpdateMediaPath {
                media_id: item.id.clone(),
                new_path: path.to_string_lossy().into_owned(),
            }),
            Some(paths) if paths.len() > 1 => plan.ambiguous += 1,
            _ => {}
        }
    }
    Some(plan)
}

/// Messages produced when a project opens with missing media.
#[derive(Clone, Debug)]
pub enum RelinkMsg {
    /// Display the missing references from the current project.
    Show(Vec<MissingMedia>),
}

/// The explicit library repair sheet state.
#[derive(Default)]
pub struct RelinkPane {
    /// Whether the sheet is visible.
    pub open: bool,
    /// Missing references, unchanged until a preview is adopted.
    pub items: Vec<MissingMedia>,
    /// A worker is checking or preparing this frozen request.
    pub busy: bool,
    /// The matched historical references lack original-content proof.
    pub confirm: bool,
    /// A short failure that leaves the current project unchanged.
    pub error: String,
}

impl RelinkPane {
    /// Updates the pane while Studio temporarily holds it outside its fields.
    pub fn update(&mut self, msg: RelinkMsg, _studio: &mut Studio) {
        match msg {
            RelinkMsg::Show(items) => {
                *self = Self {
                    open: true,
                    items,
                    ..Self::default()
                };
            }
        }
    }

    /// Publishes the sheet into Slint.
    pub fn data(&self, folder_enabled: bool) -> RelinkData {
        RelinkData {
            open: self.open,
            busy: self.busy,
            confirm: self.confirm,
            error: self.error.as_str().into(),
            folder_enabled,
            items: slint::ModelRc::new(VecModel::from(
                self.items
                    .iter()
                    .map(|item| MissingMediaItem {
                        id: item.id.clone().into(),
                        name: item.name.clone().into(),
                        path: item.path.clone().into(),
                    })
                    .collect::<Vec<_>>(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_project::{
        DocumentSettings, Editor, Project,
        model::{MediaItem, MediaKind},
    };
    use serde_json::json;
    use std::fs;

    struct Fixture {
        _directory: tempfile::TempDir,
        folder: PathBuf,
        source: PathBuf,
        settings: DocumentSettings,
        editor: Editor,
    }

    impl Fixture {
        fn new(names: &[&str]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let folder = directory.path().join("selected");
            fs::create_dir_all(&folder).unwrap();
            let mut project = Project::new();
            for (index, name) in names.iter().enumerate() {
                project.media.push(MediaItem {
                    id: format!("m{}", index + 1),
                    name: format!("素材 {}", index + 1),
                    path: directory
                        .path()
                        .join("missing")
                        .join(name)
                        .to_str()
                        .unwrap()
                        .into(),
                    kind: MediaKind::Image,
                    extra: serde_json::from_value(json!({"futureMedia": index})).unwrap(),
                    ..Default::default()
                });
            }
            project.extra.insert("futureProject".into(), json!(true));
            let settings = DocumentSettings {
                name: "手动重连测试".into(),
                width: 1920,
                height: 1080,
                rate_num: 30,
                rate_den: 1,
            };
            let document = concat_project::to_document(&settings, &project);
            let source = directory.path().join("concat.json");
            fs::write(&source, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
            let editor = Editor::from_document(&document).unwrap();
            Self {
                _directory: directory,
                folder,
                source,
                settings,
                editor,
            }
        }

        fn add_file(&self, relative: &str) -> PathBuf {
            let path = self.folder.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"isolated test media").unwrap();
            path
        }

        fn missing(&self) -> Vec<MissingMedia> {
            self.editor.project().missing_media()
        }
    }

    #[test]
    fn folder_relink_unique_recursive_matches_are_one_undoable_edit() {
        let mut fixture = Fixture::new(&["one.png", "two.png"]);
        let one = fixture.add_file("one.png");
        let two = fixture.add_file("nested/two.png");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            fixture.folder.join("nested"),
            fixture.folder.join("nested-alias"),
        )
        .unwrap();
        let before = fixture.editor.project().clone();
        let document_before = fixture.editor.to_document(&fixture.settings);
        let plan =
            plan_folder_relink(&fixture.missing(), Some(&fixture.folder), false, false).unwrap();
        assert_eq!(plan.total, 2);
        assert_eq!(plan.ambiguous, 0);
        assert_eq!(plan.commands.len(), 2);
        assert!(
            fixture
                .editor
                .apply(Command::Batch {
                    commands: plan.commands
                })
                .unwrap()
                .applied
        );
        assert_eq!(
            fixture.editor.project().media[0].path,
            one.to_str().unwrap()
        );
        assert_eq!(
            fixture.editor.project().media[1].path,
            two.to_str().unwrap()
        );
        assert!(fixture.editor.project().missing_media().is_empty());
        assert_eq!(
            fixture.editor.project().media[0].extra,
            before.media[0].extra
        );
        assert_eq!(fixture.editor.project().extra, before.extra);
        assert!(fixture.editor.undo());
        assert_eq!(fixture.editor.project(), &before);
        assert_eq!(
            fixture.editor.to_document(&fixture.settings),
            document_before
        );
        assert!(!fixture.editor.can_undo(), "the entire batch is one edit");
        assert!(fixture.editor.redo());
        assert_eq!(
            fixture.editor.project().media[0].path,
            one.to_str().unwrap()
        );
        assert_eq!(
            fixture.editor.project().media[1].path,
            two.to_str().unwrap()
        );
    }

    #[test]
    fn folder_relink_skips_ambiguous_basename_but_keeps_unique_match() {
        let mut fixture = Fixture::new(&["duplicate.png", "unique.png", "absent.png"]);
        fixture.add_file("a/duplicate.png");
        fixture.add_file("b/duplicate.png");
        let unique = fixture.add_file("unique.png");
        let before = fixture.editor.project().clone();
        let plan =
            plan_folder_relink(&fixture.missing(), Some(&fixture.folder), false, false).unwrap();
        assert_eq!(plan.total, 3);
        assert_eq!(plan.ambiguous, 1);
        assert_eq!(plan.commands.len(), 1);
        assert!(matches!(
            &plan.commands[0],
            Command::UpdateMediaPath { media_id, new_path }
                if media_id == "m2" && new_path == unique.to_str().unwrap()
        ));
        fixture
            .editor
            .apply(Command::Batch {
                commands: plan.commands,
            })
            .unwrap();
        assert_eq!(fixture.editor.project().media[0], before.media[0]);
        assert_eq!(fixture.editor.project().media[2], before.media[2]);
        assert_eq!(fixture.editor.project().missing_media().len(), 2);
        assert!(fixture.editor.undo());
        assert_eq!(fixture.editor.project(), &before);
    }

    #[test]
    fn folder_relink_only_ambiguous_items_have_no_commands() {
        let fixture = Fixture::new(&["duplicate.png", "duplicate.png"]);
        fixture.add_file("duplicate.png");
        fixture.add_file("nested/duplicate.png");
        let before = fixture.editor.project().clone();
        let plan =
            plan_folder_relink(&fixture.missing(), Some(&fixture.folder), false, false).unwrap();
        assert_eq!(plan.total, 2);
        assert_eq!(plan.ambiguous, 2);
        assert!(plan.commands.is_empty());
        assert_eq!(fixture.editor.project(), &before);
        assert!(!fixture.editor.can_undo());
    }

    #[test]
    fn folder_relink_cancelled_picker_leaves_source_and_history_unchanged() {
        let fixture = Fixture::new(&["one.png"]);
        fixture.add_file("one.png");
        let before = fixture.editor.project().clone();
        let source_before = fs::read(&fixture.source).unwrap();
        assert!(plan_folder_relink(&fixture.missing(), None, false, false).is_none());
        assert_eq!(fixture.editor.project(), &before);
        assert_eq!(fs::read(&fixture.source).unwrap(), source_before);
        assert!(!fixture.editor.can_undo());
        assert!(!fixture.editor.can_redo());
    }

    #[test]
    fn folder_relink_busy_or_preview_rejects_manual_search() {
        let fixture = Fixture::new(&["one.png"]);
        fixture.add_file("one.png");
        let source_before = fs::read(&fixture.source).unwrap();
        let before = fixture.editor.project().clone();
        for (busy, preview) in [(true, false), (false, true), (true, true)] {
            assert!(!folder_relink_allowed(busy, preview));
            assert!(
                plan_folder_relink(&fixture.missing(), Some(&fixture.folder), busy, preview)
                    .is_none()
            );
        }
        assert!(folder_relink_allowed(false, false));
        assert!(
            plan_folder_relink(&fixture.missing(), Some(&fixture.folder), false, false).is_some()
        );
        assert_eq!(fixture.editor.project(), &before);
        assert_eq!(fs::read(&fixture.source).unwrap(), source_before);
        assert!(!fixture.editor.can_undo());
    }
}
