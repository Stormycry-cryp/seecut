// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure planning of explicit project copies bound to existing library versions.

use std::{
    fs,
    path::{Path, PathBuf},
};

use concat_project::{
    Project,
    model::{MediaItem, MediaKind},
};
use serde::{Deserialize, Serialize};

use crate::personal_library::{Asset, AssetKind, Library, checked_managed_version_path};

pub(crate) const BINDING_KEY: &str = "seecutLibraryBinding";

/// Stored in MediaItem.extra so invalid future metadata cannot drop a media row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LibraryBinding {
    pub(crate) version: u32,
    pub(crate) asset_id: String,
    pub(crate) registration_id: String,
    pub(crate) sha256: String,
    pub(crate) previous_path: String,
    pub(crate) verified_original: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RepairBinding {
    pub(crate) media_id: String,
    pub(crate) asset: Asset,
    pub(crate) binding: LibraryBinding,
}

#[derive(Clone, Debug)]
pub(crate) struct RepairPlan {
    pub(crate) expected: Project,
    pub(crate) patched: Project,
    pub(crate) bindings: Vec<RepairBinding>,
    /// The caller must confirm this before adopting or writing the planned copy.
    pub(crate) needs_confirmation: bool,
    library_root: PathBuf,
}

/// No directory search, media copy, migration, project edit or index write.
/// A missing or ambiguous target aborts the whole plan without selecting latest.
pub(crate) fn plan_repair(project: &Project, library_root: &Path) -> Result<RepairPlan, String> {
    let missing = project.missing_media();
    if missing.is_empty() {
        return Err("当前工程没有缺失素材".into());
    }
    let library = Library::load_for_relink(library_root)?;
    let library_root = library_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut patched = project.clone();
    let mut bindings = Vec::with_capacity(missing.len());
    let mut needs_confirmation = false;
    for missing_item in missing {
        let item = unique_media(project, &missing_item.id)?;
        check_missing(item)?;
        let previous = read_binding(item)?;
        let candidates = library
            .items()
            .iter()
            .filter(|asset| {
                if let Some(binding) = &previous {
                    asset.id == binding.asset_id
                } else {
                    asset.original_path.as_deref() == Some(Path::new(&item.path))
                        || asset
                            .recovery
                            .as_ref()
                            .is_some_and(|recovery| recovery.previous_path == Path::new(&item.path))
                }
            })
            .collect::<Vec<_>>();
        let asset = match candidates.as_slice() {
            [asset] => *asset,
            [] => return Err(format!("素材“{}”没有可追溯的库内版本", item.name)),
            _ => {
                return Err(format!(
                    "素材“{}”对应多个库内版本，无法确定要关联哪一个",
                    item.name
                ));
            }
        };
        let path = checked_managed_version_path(&library_root, asset)
            .map_err(|error| format!("素材“{}”无法关联：{error}", item.name))?;
        check_kind(item.kind, asset.kind)?;
        let version = asset.version.as_ref().ok_or("库内资产没有固定内容版本")?;
        let binding = if let Some(binding) = previous {
            if binding.registration_id != version.registration_id
                || binding.sha256 != version.sha256
            {
                return Err(format!("素材“{}”已记录的版本与库内版本不一致", item.name));
            }
            binding
        } else {
            // A library recovery can prove its own former hash, not the bytes
            // the old project used at this external path. Do not inherit true.
            LibraryBinding {
                version: 1,
                asset_id: asset.id.clone(),
                registration_id: version.registration_id.clone(),
                sha256: version.sha256.clone(),
                previous_path: item.path.clone(),
                verified_original: false,
            }
        };
        needs_confirmation |= !binding.verified_original;
        let target = patched
            .media
            .iter_mut()
            .find(|target| target.id == item.id)
            .ok_or("工程素材已变化")?;
        target.path = path.to_str().ok_or("受管素材路径不是有效文本")?.to_owned();
        target.extra.insert(
            BINDING_KEY.into(),
            serde_json::to_value(&binding).map_err(|error| error.to_string())?,
        );
        bindings.push(RepairBinding {
            media_id: item.id.clone(),
            asset: asset.clone(),
            binding,
        });
    }
    Ok(RepairPlan {
        expected: project.clone(),
        patched,
        bindings,
        needs_confirmation,
        library_root,
    })
}

impl RepairPlan {
    /// Before creating an isolated preview, the entire source must be unchanged.
    pub(crate) fn revalidate(&self, current: &Project, root: &Path) -> Result<(), String> {
        if current != &self.expected {
            return Err("原工程已变化，请重新准备修复副本".into());
        }
        for repair in &self.bindings {
            check_missing(unique_media(current, &repair.media_id)?)?;
        }
        self.validate_assets(root)
    }

    /// Before Save As, allow normal timeline edits while freezing repaired media.
    pub(crate) fn revalidate_preview(&self, current: &Project, root: &Path) -> Result<(), String> {
        for repair in &self.bindings {
            let current = unique_media(current, &repair.media_id)?;
            let planned = unique_media(&self.patched, &repair.media_id)?;
            if current.path != planned.path
                || current.kind != planned.kind
                || read_binding(current)?.as_ref() != Some(&repair.binding)
            {
                return Err("预览工程的已关联素材发生变化，请重新准备修复副本".into());
            }
        }
        self.validate_assets(root)
    }

    fn validate_assets(&self, root: &Path) -> Result<(), String> {
        let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
        if canonical_root != self.library_root {
            return Err("个人资产库位置已变化，请重新准备修复副本".into());
        }
        let library = Library::load_for_relink(root)?;
        for repair in &self.bindings {
            let mut matches = library
                .items()
                .iter()
                .filter(|asset| asset.id == repair.asset.id);
            let current = matches.next().ok_or("已选库内资产不存在，请重新选择")?;
            if matches.next().is_some() || current != &repair.asset {
                return Err("已选库内资产记录已变化，请重新准备修复副本".into());
            }
            checked_managed_version_path(root, current)?;
        }
        Ok(())
    }
}

fn unique_media<'a>(project: &'a Project, id: &str) -> Result<&'a MediaItem, String> {
    if id.is_empty() {
        return Err("缺失素材没有稳定工程身份".into());
    }
    let mut matches = project.media.iter().filter(|item| item.id == id);
    let item = matches.next().ok_or("找不到原工程素材")?;
    if matches.next().is_some() {
        return Err("工程素材身份重复，无法安全关联".into());
    }
    Ok(item)
}

fn check_missing(item: &MediaItem) -> Result<(), String> {
    if item.placeholder || !Path::new(&item.path).is_absolute() {
        return Err("只能关联具有完整旧路径的缺失素材，不能替换模板占位项".into());
    }
    match fs::symlink_metadata(&item.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法确认原工程素材缺失：{error}")),
        Ok(_) => Err("原工程素材路径已存在，未替换任何引用".into()),
    }
}

fn read_binding(item: &MediaItem) -> Result<Option<LibraryBinding>, String> {
    let Some(value) = item.extra.get(BINDING_KEY) else {
        return Ok(None);
    };
    let binding: LibraryBinding = serde_json::from_value(value.clone())
        .map_err(|_| "工程素材的库版本绑定已损坏，不能按无指纹素材处理".to_owned())?;
    if binding.version != 1
        || binding.asset_id.is_empty()
        || binding.registration_id.is_empty()
        || binding.sha256.len() != 64
        || !binding
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || !Path::new(&binding.previous_path).is_absolute()
    {
        return Err("工程素材的库版本绑定无效或版本不受支持".into());
    }
    Ok(Some(binding))
}

fn check_kind(media: MediaKind, asset: AssetKind) -> Result<(), String> {
    if matches!(
        (media, asset),
        (MediaKind::Image, AssetKind::Image)
            | (MediaKind::Video, AssetKind::Video)
            | (MediaKind::Audio, AssetKind::Audio)
    ) {
        Ok(())
    } else {
        Err("工程素材类型与库内版本不一致".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::personal_library::{AssetRecovery, MANIFEST_NAME};
    use concat_project::{
        DocumentSettings,
        model::{Clip, ClipKind},
    };
    use serde_json::json;
    use std::sync::Arc;

    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
        asset: Asset,
        project: Project,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir()
                .join(format!("seecut-project-relink-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&directory).unwrap();
            let directory = directory.canonicalize().unwrap();
            let root = directory.join("library");
            let external = directory.join("old-external.png");
            fs::write(&external, b"fixed library image bytes").unwrap();
            let mut library = Library::load(&root).unwrap();
            let asset = library.register_generated(&external).unwrap().clone();
            fs::remove_file(&external).unwrap();
            let mut project = Project::new();
            project.media.push(MediaItem {
                id: "m7".into(),
                path: external.to_str().unwrap().into(),
                name: "原工程素材".into(),
                kind: MediaKind::Image,
                width: Some(16),
                height: Some(16),
                extra: serde_json::from_value(json!({"futureMedia": {"keep": true}})).unwrap(),
                ..Default::default()
            });
            let mut clip = Clip::blank("c9", "T1", ClipKind::Image, "原剪辑", 0.0, 5.0);
            clip.media_id = "m7".into();
            clip.extra
                .insert("futureClip".into(), json!({"keep": true}));
            project.active_mut().clips.push(Arc::new(clip));
            project
                .extra
                .insert("futureProject".into(), json!({"keep": true}));
            Self {
                directory,
                root,
                asset,
                project,
            }
        }

        fn save_assets(&self, assets: &[Asset]) {
            fs::write(
                self.root.join(MANIFEST_NAME),
                serde_json::to_vec(&json!({"items": assets,"folders":[]})).unwrap(),
            )
            .unwrap();
        }

        fn binding(&self, verified_original: bool) -> LibraryBinding {
            let version = self.asset.version.as_ref().unwrap();
            LibraryBinding {
                version: 1,
                asset_id: self.asset.id.clone(),
                registration_id: version.registration_id.clone(),
                sha256: version.sha256.clone(),
                previous_path: self.project.media[0].path.clone(),
                verified_original,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn project_library_relink_plan_is_read_only_and_preserves_identity_and_unknown_fields() {
        let fixture = Fixture::new();
        let source = fixture.project.clone();
        let before = fs::read(fixture.root.join(MANIFEST_NAME)).unwrap();
        let media_before = fs::read(&fixture.asset.path).unwrap();
        let plan = plan_repair(&source, &fixture.root).unwrap();
        assert_eq!(source, fixture.project);
        assert_eq!(plan.expected, source);
        assert_eq!(plan.patched.timelines, source.timelines);
        assert_eq!(plan.patched.fonts, source.fonts);
        assert_eq!(plan.patched.extra, source.extra);
        assert_eq!(plan.patched.active_timeline_id, source.active_timeline_id);
        let mut restored_original = plan.patched.media[0].clone();
        restored_original.path = source.media[0].path.clone();
        restored_original.extra.remove(BINDING_KEY);
        assert_eq!(restored_original, source.media[0]);
        assert_eq!(plan.bindings.len(), 1);
        assert!(plan.needs_confirmation);
        assert!(!plan.bindings[0].binding.verified_original);
        assert_eq!(
            plan.patched.media[0].path,
            fixture.asset.path.to_str().unwrap()
        );
        assert_eq!(fs::read(fixture.root.join(MANIFEST_NAME)).unwrap(), before);
        assert_eq!(fs::read(&fixture.asset.path).unwrap(), media_before);
        assert!(!Path::new(&source.media[0].path).exists());
        assert!(!fixture.root.join("pending-generated.json").exists());
        assert!(!fixture.root.join("pending-legacy-generated.json").exists());
        plan.revalidate(&source, &fixture.root).unwrap();
    }

    #[test]
    fn project_library_relink_does_not_inherit_library_recovery_proof_and_roundtrips() {
        let fixture = Fixture::new();
        let mut asset = fixture.asset.clone();
        asset.original_path = None;
        asset.recovery = Some(AssetRecovery {
            previous_path: fixture.project.media[0].path.clone().into(),
            selected_path: fixture.directory.join("selected.png"),
            sha256: asset.version.as_ref().unwrap().sha256.clone(),
            verified_original: true,
        });
        fixture.save_assets(&[asset]);
        let plan = plan_repair(&fixture.project, &fixture.root).unwrap();
        assert!(plan.needs_confirmation);
        assert!(!plan.bindings[0].binding.verified_original);
        let settings = DocumentSettings {
            name: "修复副本".into(),
            width: 1920,
            height: 1080,
            rate_num: 30,
            rate_den: 1,
        };
        let saved = concat_project::to_document(&settings, &plan.patched);
        assert_eq!(saved["media"][0][BINDING_KEY]["asset_id"], fixture.asset.id);
        assert_eq!(saved["media"][0][BINDING_KEY]["verified_original"], false);
        let reopened = concat_project::from_document(&saved).unwrap();
        assert_eq!(reopened.media[0], plan.patched.media[0]);
        assert_eq!(reopened.active().clips[0].id, "c9");
        assert_eq!(reopened.active().clips[0].media_id, "m7");
        assert_eq!(reopened.active().clips[0].extra["futureClip"]["keep"], true);
        plan.revalidate_preview(&reopened, &fixture.root).unwrap();
    }

    #[test]
    fn project_library_relink_existing_binding_matches_exact_version_and_keeps_false() {
        let fixture = Fixture::new();
        for verified_original in [true, false] {
            let mut source = fixture.project.clone();
            let mut binding = fixture.binding(verified_original);
            binding.previous_path = fixture
                .directory
                .join("first-ever-missing.png")
                .to_str()
                .unwrap()
                .into();
            source.media[0]
                .extra
                .insert(BINDING_KEY.into(), serde_json::to_value(&binding).unwrap());
            let plan = plan_repair(&source, &fixture.root).unwrap();
            assert_eq!(plan.bindings[0].binding, binding);
            assert_eq!(plan.needs_confirmation, !verified_original);
            assert_eq!(
                plan.patched.media[0].extra[BINDING_KEY],
                source.media[0].extra[BINDING_KEY]
            );
        }
    }

    #[test]
    fn project_library_relink_rejects_wrong_hash_kind_and_ambiguous_versions_without_writes() {
        let fixture = Fixture::new();
        let before = fs::read(fixture.root.join(MANIFEST_NAME)).unwrap();
        fs::write(&fixture.asset.path, b"changed managed bytes").unwrap();
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
        assert_eq!(fs::read(fixture.root.join(MANIFEST_NAME)).unwrap(), before);
        fs::write(&fixture.asset.path, b"fixed library image bytes").unwrap();
        let mut different_kind = fixture.project.clone();
        different_kind.media[0].kind = MediaKind::Audio;
        assert!(plan_repair(&different_kind, &fixture.root).is_err());
        let mut another_version = fixture.asset.clone();
        another_version.id = "another-asset-id".into();
        fixture.save_assets(&[fixture.asset.clone(), another_version]);
        let ambiguous_index = fs::read(fixture.root.join(MANIFEST_NAME)).unwrap();
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
        assert_eq!(
            fs::read(fixture.root.join(MANIFEST_NAME)).unwrap(),
            ambiguous_index
        );
    }

    #[test]
    fn project_library_relink_rejects_invalid_or_mismatched_binding_without_downgrade() {
        let fixture = Fixture::new();
        let valid = serde_json::to_value(fixture.binding(true)).unwrap();
        let cases = [
            serde_json::Value::Null,
            json!({"version": 1}),
            {
                let mut value = valid.clone();
                value["version"] = json!(2);
                value
            },
            {
                let mut value = valid.clone();
                value["asset_id"] = json!("unknown-id");
                value
            },
            {
                let mut value = valid.clone();
                value["registration_id"] = json!("other-version");
                value
            },
            {
                let mut value = valid;
                value["sha256"] = json!("0".repeat(64));
                value
            },
        ];
        for value in cases {
            let mut source = fixture.project.clone();
            source.media[0].extra.insert(BINDING_KEY.into(), value);
            let original = source.clone();
            assert!(plan_repair(&source, &fixture.root).is_err());
            assert_eq!(source, original);
            assert_eq!(source.media.len(), 1);
            assert_eq!(source.active().clips.len(), 1);
        }
    }

    #[test]
    fn project_library_relink_requires_exact_old_path_fixed_version_and_untrashed_asset() {
        let fixture = Fixture::new();
        let mut asset = fixture.asset.clone();
        asset.id.clear();
        fixture.save_assets(&[asset]);
        let original_index = fs::read(fixture.root.join(MANIFEST_NAME)).unwrap();
        assert!(
            plan_repair(&fixture.project, &fixture.root)
                .unwrap_err()
                .contains("稳定身份")
        );
        assert_eq!(
            fs::read(fixture.root.join(MANIFEST_NAME)).unwrap(),
            original_index
        );
        let mut asset = fixture.asset.clone();
        asset.original_path = Some(
            fixture
                .directory
                .join("other-folder")
                .join("old-external.png"),
        );
        fixture.save_assets(&[asset]);
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
        let mut asset = fixture.asset.clone();
        asset.version = None;
        fixture.save_assets(&[asset]);
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
        let mut asset = fixture.asset.clone();
        asset.trashed = true;
        fixture.save_assets(&[asset]);
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
    }

    #[test]
    fn project_library_relink_missing_batch_is_all_or_nothing() {
        let fixture = Fixture::new();
        let mut source = fixture.project.clone();
        let mut unmatched = source.media[0].clone();
        unmatched.id = "m8".into();
        unmatched.path = fixture
            .directory
            .join("unrelated.png")
            .to_str()
            .unwrap()
            .into();
        source.media.push(unmatched);
        let original = source.clone();
        assert!(plan_repair(&source, &fixture.root).is_err());
        assert_eq!(source, original);
        assert!(!source.media[0].extra.contains_key(BINDING_KEY));
    }

    #[test]
    fn project_library_relink_revalidation_rejects_project_library_and_file_changes() {
        let fixture = Fixture::new();
        let plan = plan_repair(&fixture.project, &fixture.root).unwrap();
        let mut changed = fixture.project.clone();
        changed.extra.insert("another-edit".into(), json!(true));
        assert!(plan.revalidate(&changed, &fixture.root).is_err());
        fs::write(&fixture.project.media[0].path, b"external appeared").unwrap();
        assert!(plan.revalidate(&fixture.project, &fixture.root).is_err());
        fs::remove_file(&fixture.project.media[0].path).unwrap();
        let mut asset = fixture.asset.clone();
        asset.name = "changed-library-name".into();
        fixture.save_assets(&[asset]);
        assert!(plan.revalidate(&fixture.project, &fixture.root).is_err());
        fixture.save_assets(std::slice::from_ref(&fixture.asset));
        fs::write(&fixture.asset.path, b"changed managed bytes").unwrap();
        assert!(plan.revalidate(&fixture.project, &fixture.root).is_err());
        assert!(
            plan.revalidate_preview(&plan.patched, &fixture.root)
                .is_err()
        );
    }

    #[test]
    fn project_library_relink_preview_accepts_timeline_edits_but_freezes_media_binding() {
        let fixture = Fixture::new();
        let plan = plan_repair(&fixture.project, &fixture.root).unwrap();
        let mut preview = plan.patched.clone();
        preview.active_mut().name = "用户编辑过的时间线".into();
        preview.media[0].name = "用户重新命名".into();
        preview.media[0]
            .extra
            .insert("additionalMetadata".into(), json!(true));
        plan.revalidate_preview(&preview, &fixture.root).unwrap();
        let mut changed_path = preview.clone();
        changed_path.media[0].path = fixture.directory.join("other.png").to_str().unwrap().into();
        assert!(
            plan.revalidate_preview(&changed_path, &fixture.root)
                .is_err()
        );
        let mut changed_kind = preview.clone();
        changed_kind.media[0].kind = MediaKind::Video;
        assert!(
            plan.revalidate_preview(&changed_kind, &fixture.root)
                .is_err()
        );
        let mut changed_binding = preview.clone();
        changed_binding.media[0].extra.get_mut(BINDING_KEY).unwrap()["verified_original"] =
            json!(true);
        assert!(
            plan.revalidate_preview(&changed_binding, &fixture.root)
                .is_err()
        );
        preview.media.push(preview.media[0].clone());
        assert!(plan.revalidate_preview(&preview, &fixture.root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn project_library_relink_rejects_symlink_or_external_managed_file() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = fixture.directory.join("outside-copy.png");
        fs::rename(&fixture.asset.path, &outside).unwrap();
        symlink(&outside, &fixture.asset.path).unwrap();
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
        let mut asset = fixture.asset.clone();
        asset.path = outside;
        fixture.save_assets(&[asset]);
        assert!(plan_repair(&fixture.project, &fixture.root).is_err());
    }
}
