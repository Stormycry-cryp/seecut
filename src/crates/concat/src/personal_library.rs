// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-only storage for the user's personal media library.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::hash_map::DefaultHasher,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// The file stored below the library root.
pub const MANIFEST_NAME: &str = "library.json";

/// Serialize read-modify-write operations from the cloud worker and the clip
/// export pane. The guard must cover the load as well as the save.
pub(crate) fn with_library_write_lock<T>(operation: impl FnOnce() -> T) -> T {
    static WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = WRITE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let result = operation();
    drop(guard);
    result
}

/// A media category understood by the editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// A still image.
    Image,
    /// A video file.
    Video,
    /// An audio file.
    Audio,
}

/// How an asset entered the personal library.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetSource {
    /// Copied into the managed `imported` directory.
    Imported,
    /// Generated or downloaded media copied into immutable managed storage.
    Generated,
}

/// One entry in the personal library index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    /// Stable identifier used by library operations.
    pub id: String,
    /// User-visible name. Renaming does not move the file.
    pub name: String,
    /// Canonical path to the media file.
    pub path: PathBuf,
    /// Media category inferred from the extension.
    pub kind: AssetKind,
    /// Whether this file was imported or generated.
    pub source: AssetSource,
    /// Immutable generated-content identity and previous version, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<AssetVersion>,
    /// Canonical source path retained as provenance, separate from managed storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<PathBuf>,
    /// Explicit recovery provenance; an unknown historical file remains unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<AssetRecovery>,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at: u64,
    /// Whether the item is in the library recycle bin.
    pub trashed: bool,
    /// Whether the user pinned this item for quick access.
    #[serde(default)]
    pub favorite: bool,
    /// Optional user folder. Assets keep their stable id and media path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetVersion {
    pub sha256: String,
    pub registration_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetRecovery {
    /// First missing path, retained across subsequent recovery attempts.
    pub previous_path: PathBuf,
    pub selected_path: PathBuf,
    pub sha256: String,
    /// False stays false even after a later match against the recovered bytes.
    pub verified_original: bool,
}

/// Durable registration input. Its managed bytes survive a failed index update.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PreparedGenerated {
    pub(crate) path: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) name: String,
    pub(crate) kind: AssetKind,
    pub(crate) sha256: String,
    pub(crate) registration_id: String,
    pub(crate) operation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    items: Vec<Asset>,
    #[serde(default)]
    folders: Vec<Folder>,
}

/// A local personal media library backed by an atomic JSON index.
pub struct Library {
    root: PathBuf,
    items: Vec<Asset>,
    folders: Vec<Folder>,
}

impl Library {
    /// Reads the existing index without migration, registration retry or writes.
    /// Use for a frozen relink request, whose failure must preserve the index.
    pub fn load_for_relink(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        if !root.is_dir() {
            return Err("个人资产库目录不存在，请刷新后重试".into());
        }
        let bytes = fs::read(root.join(MANIFEST_NAME))
            .map_err(|error| format!("无法读取个人资产库索引：{error}"))?;
        let manifest = serde_json::from_slice::<Manifest>(&bytes).map_err(|error| {
            format!("个人资产库索引已损坏，请先备份或修复 library.json：{error}")
        })?;
        Ok(Self {
            root,
            items: manifest.items,
            folders: manifest.folders,
        })
    }

    /// Opens a library, creating its directories when no index exists.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        if root.exists() && !root.is_dir() {
            return Err("个人资产库路径不是文件夹".into());
        }
        let manifest_path = root.join(MANIFEST_NAME);
        let manifest = if manifest_path.exists() {
            let bytes = fs::read(&manifest_path)
                .map_err(|error| format!("无法读取个人资产库索引：{error}"))?;
            serde_json::from_slice::<Manifest>(&bytes).map_err(|error| {
                format!("个人资产库索引已损坏，请先备份或修复 library.json：{error}")
            })?
        } else {
            Manifest::default()
        };
        fs::create_dir_all(root.join("imported"))
            .map_err(|error| format!("无法创建个人资产库目录：{error}"))?;
        let mut library = Self {
            root,
            items: manifest.items,
            folders: manifest.folders,
        };
        // Existing intents win over the current external bytes. Keep migration
        // identities separate from ordinary output registration and its queue.
        let mut migrations = crate::registration_retry::load_legacy_migrations(&library.root)?;
        let existing_count = migrations.len();
        let mut migrated = false;
        for (position, entry) in migrations.iter().enumerate() {
            if migrations[..position]
                .iter()
                .any(|previous| previous.target_asset_id == entry.target_asset_id)
            {
                return Err("旧资产迁移身份重复，已保留原记录和副本".into());
            }
            migrated |= library.apply_legacy_migration(entry)?;
        }
        let mut staged = false;
        for item in &library.items {
            if item.source == AssetSource::Generated
                && item.version.is_none()
                && !migrations
                    .iter()
                    .any(|entry| entry.target_asset_id == item.id)
            {
                let prepared = match fs::metadata(&item.path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => Err(error.to_string()),
                    Ok(_) => {
                        prepare_generated(&library.root, &item.path, Some(&item.name), None, None)
                    }
                };
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        if staged {
                            crate::registration_retry::save_legacy_migrations(
                                &library.root,
                                &migrations,
                            )?;
                        }
                        return Err(format!("旧资产“{}”入库失败：{error}", item.name));
                    }
                };
                migrations.push(crate::registration_retry::LegacyMigration {
                    target_asset_id: item.id.clone(),
                    asset_path: item.path.clone(),
                    prepared,
                });
                staged = true;
            }
        }
        if staged {
            crate::registration_retry::save_legacy_migrations(&library.root, &migrations)?;
        }
        for entry in migrations.iter().skip(existing_count) {
            migrated |= library.apply_legacy_migration(entry)?;
        }
        if migrated {
            library.save()?;
        }
        if !migrations.is_empty() {
            // A failed cleanup leaves the same intents for idempotent replay.
            crate::registration_retry::save_legacy_migrations(&library.root, &[])?;
        }
        Ok(library)
    }

    fn apply_legacy_migration(
        &mut self,
        entry: &crate::registration_retry::LegacyMigration,
    ) -> Result<bool, String> {
        let mut matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.id == entry.target_asset_id);
        let index = matches.next().map(|(index, _)| index);
        if index.is_none() || matches.next().is_some() {
            return Err("旧资产迁移目标不存在或身份重复，已保留迁移记录".into());
        }
        let path = checked_prepared_path(&self.root, &entry.prepared)?;
        let item = &mut self.items[index.expect("已验证旧资产身份")];
        let version = AssetVersion {
            sha256: entry.prepared.sha256.clone(),
            registration_id: entry.prepared.registration_id.clone(),
            previous_id: None,
        };
        if item.source != AssetSource::Generated || item.kind != entry.prepared.kind {
            return Err("旧资产迁移目标已变化，已保留原记录和副本".into());
        }
        if let Some(existing) = &item.version {
            if existing == &version && item.path == path {
                return Ok(false);
            }
            return Err("旧资产已有不同版本，已保留原记录和迁移副本".into());
        }
        if item.path != entry.asset_path {
            return Err("旧资产路径已变化，已保留原记录和迁移副本".into());
        }
        // Retain current user metadata, including any previously recorded provenance.
        item.original_path
            .get_or_insert_with(|| entry.prepared.source_path.clone());
        item.path = path;
        item.version = Some(version);
        Ok(true)
    }

    /// Returns all indexed items, including recycled items.
    pub fn items(&self) -> &[Asset] {
        &self.items
    }

    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    pub fn create_folder(&mut self, name: &str) -> Result<&Folder, String> {
        validate_name(name)?;
        if self.folders.iter().any(|folder| folder.name == name) {
            return Err("文件夹已存在".into());
        }
        self.folders.push(Folder {
            id: Uuid::new_v4().to_string(),
            name: name.to_owned(),
        });
        if let Err(error) = self.save() {
            self.folders.pop();
            return Err(error);
        }
        Ok(self.folders.last().expect("刚创建的文件夹必须存在"))
    }

    pub fn move_to_folder(
        &mut self,
        ids: &[String],
        folder_id: Option<&str>,
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        if let Some(folder_id) = folder_id
            && !self.folders.iter().any(|folder| folder.id == folder_id)
        {
            return Err("文件夹不存在，请刷新后重试".into());
        }
        for id in ids {
            let Some(asset) = self.get(id) else {
                return Err("素材不存在，请刷新后重试".into());
            };
            if asset.trashed {
                return Err("请先从回收站恢复素材".into());
            }
        }
        let previous = self.items.clone();
        let mut changed = 0;
        for item in &mut self.items {
            if ids.contains(&item.id) && item.folder_id.as_deref() != folder_id {
                item.folder_id = folder_id.map(str::to_owned);
                changed += 1;
            }
        }
        if changed == 0 {
            return Ok(0);
        }
        if let Err(error) = self.save() {
            self.items = previous;
            return Err(error);
        }
        Ok(changed)
    }

    /// Returns an item by stable identifier.
    pub fn get(&self, id: &str) -> Option<&Asset> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Copies a media file into managed storage and indexes it.
    pub fn import(&mut self, path: impl AsRef<Path>) -> Result<&Asset, String> {
        let source = checked_media(path.as_ref())?;
        // A source path is not a content identity. Re-importing changed bytes
        // creates a new version so projects referencing the older managed copy
        // keep their original media. A missing copy is never treated as a hit.
        for (index, item) in self.items.iter().enumerate().rev() {
            if item.source == AssetSource::Imported
                && item.original_path.as_ref() == Some(&source.0)
                && same_file_bytes(&source.0, &item.path)?
            {
                return Ok(&self.items[index]);
            }
        }
        let extension = source
            .0
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "文件扩展名无效".to_owned())?
            .to_ascii_lowercase();
        let id = Uuid::new_v4().to_string();
        let created_at = now_millis()?;
        let destination = self.root.join("imported").join(format!("{id}.{extension}"));
        let temporary = self.root.join("imported").join(format!(".{id}.tmp"));
        if let Err(error) = fs::copy(&source.0, &temporary) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("复制素材失败：{error}"));
        }
        match same_file_bytes(&source.0, &temporary) {
            Ok(true) => {}
            Ok(false) => {
                let _ = fs::remove_file(&temporary);
                return Err("源素材在导入过程中发生变化，请重试".into());
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        }
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("保存导入素材失败：{error}"));
        }
        let canonical_destination = match destination.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_file(&destination);
                return Err(format!("读取导入素材路径失败：{error}"));
            }
        };
        let item = Asset {
            id,
            name: display_name(&source.0),
            path: canonical_destination,
            kind: source.1,
            source: AssetSource::Imported,
            version: None,
            original_path: Some(source.0),
            recovery: None,
            created_at,
            trashed: false,
            favorite: false,
            folder_id: None,
        };
        self.items.push(item);
        if let Err(error) = self.save() {
            let item = self.items.pop().expect("刚插入的资产必须存在");
            let _ = fs::remove_file(item.path);
            return Err(error);
        }
        Ok(self.items.last().expect("刚保存的资产必须存在"))
    }

    /// Copies generated media into immutable managed storage, then indexes it.
    pub fn register_generated(&mut self, path: impl AsRef<Path>) -> Result<&Asset, String> {
        self.register_generated_with_name(path, None)
    }

    pub fn register_generated_with_name(
        &mut self,
        path: impl AsRef<Path>,
        name: Option<&str>,
    ) -> Result<&Asset, String> {
        let prepared = prepare_generated(&self.root, path.as_ref(), name, None, None)?;
        self.commit_generated(&prepared)
    }

    pub(crate) fn commit_generated(
        &mut self,
        prepared: &PreparedGenerated,
    ) -> Result<&Asset, String> {
        let path = checked_prepared_path(&self.root, prepared)?;
        for (index, item) in self.items.iter().enumerate().rev() {
            if let Some(version) = &item.version {
                if version.registration_id == prepared.registration_id {
                    if version.sha256 != prepared.sha256 {
                        return Err("同一输出的内容已变化，已保留原版本".into());
                    }
                    return Ok(&self.items[index]);
                }
                // A directory scan or repeated indexing adopts the existing content version.
                if prepared.operation.is_none()
                    && item.original_path.as_ref() == Some(&prepared.source_path)
                    && version.sha256 == prepared.sha256
                    && item.path.is_file()
                {
                    return Ok(&self.items[index]);
                }
            }
        }
        let previous_id = self
            .items
            .iter()
            .rev()
            .find(|item| {
                item.source == AssetSource::Generated
                    && item.original_path.as_ref() == Some(&prepared.source_path)
            })
            .map(|item| item.id.clone());
        self.items.push(Asset {
            id: Uuid::new_v4().to_string(),
            name: prepared.name.clone(),
            path,
            kind: prepared.kind,
            source: AssetSource::Generated,
            original_path: Some(prepared.source_path.clone()),
            recovery: None,
            version: Some(AssetVersion {
                sha256: prepared.sha256.clone(),
                registration_id: prepared.registration_id.clone(),
                previous_id,
            }),
            created_at: now_millis()?,
            trashed: false,
            favorite: false,
            folder_id: None,
        });
        if let Err(error) = self.save() {
            self.items.pop();
            return Err(error);
        }
        Ok(self.items.last().expect("刚保存的资产必须存在"))
    }

    /// Restores one frozen missing item without registering another asset.
    /// The caller holds the library write lock across load and this operation.
    /// Returned failures roll back the index and newly published file; this is
    /// not a crash-atomic transaction across the media file and JSON index.
    pub fn relink_missing(
        &mut self,
        expected: &Asset,
        selected: &Path,
        allow_unverified: bool,
    ) -> Result<&Asset, String> {
        let mut matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.id == expected.id);
        let index = matches.next().map(|(index, _)| index);
        if index.is_none() || matches.next().is_some() {
            return Err("找不到唯一的原资产，请刷新后重试".into());
        }
        let index = index.expect("已验证资产身份");
        if &self.items[index] != expected {
            return Err("资产记录已变化，请刷新后重新选择文件".into());
        }
        if crate::registration_retry::load_legacy_migrations(&self.root)?
            .iter()
            .any(|entry| entry.target_asset_id == expected.id)
        {
            return Err("此资产仍有待恢复的原副本，请先刷新完成原入库恢复".into());
        }
        let expected_hash = expected
            .version
            .as_ref()
            .map(|version| version.sha256.as_str())
            .or_else(|| {
                expected
                    .recovery
                    .as_ref()
                    .map(|recovery| recovery.sha256.as_str())
            });
        match fs::symlink_metadata(&expected.path) {
            Ok(metadata) => {
                // A repeated completed request never creates another record or
                // replaces a file, including one changed by an external writer.
                if metadata.is_file()
                    && expected.recovery.is_some()
                    && expected_hash.is_some_and(|hash| {
                        file_digest(&expected.path).is_ok_and(|actual| actual == hash)
                    })
                {
                    return Ok(&self.items[index]);
                }
                return Err("原素材路径已存在，未覆盖任何文件".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法确认原素材缺失：{error}")),
        }
        if expected_hash.is_none() && !allow_unverified {
            return Err("旧素材没有内容指纹，请先确认使用所选文件；无法验证是否为原内容".into());
        }
        let (selected_path, kind) = checked_media(selected)?;
        if kind != expected.kind {
            return Err("所选文件的素材类型与原记录不一致".into());
        }
        let root = self
            .root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let source_folder = match expected.source {
            AssetSource::Imported => "imported",
            AssetSource::Generated => "generated",
        };
        // Only a direct managed path may be restored in place. Never recreate
        // an old external source or traverse a symlinked managed directory.
        let folder_name = ["imported", "generated"]
            .into_iter()
            .find(|name| expected.path.parent() == Some(root.join(name).as_path()))
            .unwrap_or(source_folder);
        let folder = checked_recovery_folder(&root, folder_name)?;
        let extension = selected_path
            .extension()
            .and_then(|value| value.to_str())
            .ok_or("文件扩展名无效")?;
        let destination = if expected.path.parent() == Some(folder.as_path()) {
            expected.path.clone()
        } else {
            folder.join(format!("recovered-{}.{}", Uuid::new_v4(), extension))
        };
        let temporary = folder.join(format!(".recovery-{}.tmp", Uuid::new_v4()));
        let mut staged = false;
        let mut published = false;
        let mut copied_hash = String::new();
        let result = (|| {
            let mut input = File::open(&selected_path).map_err(|error| error.to_string())?;
            let mut output = File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| error.to_string())?;
            staged = true;
            std::io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
            output.sync_all().map_err(|error| error.to_string())?;
            copied_hash = file_digest(&temporary)?;
            if let Some(hash) = expected_hash {
                if copied_hash != hash {
                    return Err("所选文件与已记录的内容指纹不一致，原记录未改变".into());
                }
            } else {
                let info = concat_media::probe(&temporary)
                    .map_err(|_| "所选文件不是可读取的有效素材".to_owned())?;
                if (kind == AssetKind::Audio && info.audio.is_none())
                    || (kind != AssetKind::Audio && info.video.is_none())
                {
                    return Err("所选文件内容与素材类型不一致".into());
                }
            }
            if !same_file_bytes(&selected_path, &temporary)? {
                return Err("所选文件在复制过程中发生变化，请重新选择".into());
            }
            // hard_link publishes atomically without overwriting an existing
            // destination, unlike rename on platforms that replace the target.
            fs::hard_link(&temporary, &destination)
                .map_err(|error| format!("无法恢复素材文件，未覆盖已有文件：{error}"))?;
            published = true;
            let mut restored = expected.clone();
            restored.path = destination.clone();
            restored
                .original_path
                .get_or_insert_with(|| expected.path.clone());
            if restored.version.is_none() {
                restored.version = Some(AssetVersion {
                    sha256: copied_hash.clone(),
                    registration_id: format!("recovery:{}:{}", expected.id, copied_hash),
                    previous_id: None,
                });
            }
            restored.recovery = Some(AssetRecovery {
                previous_path: expected
                    .recovery
                    .as_ref()
                    .map(|recovery| recovery.previous_path.clone())
                    .unwrap_or_else(|| expected.path.clone()),
                selected_path: selected_path.clone(),
                sha256: copied_hash.clone(),
                verified_original: expected
                    .recovery
                    .as_ref()
                    .map(|recovery| recovery.verified_original)
                    .unwrap_or(expected_hash.is_some()),
            });
            self.items[index] = restored;
            if let Err(error) = self.save() {
                self.items[index] = expected.clone();
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = result {
            let cleanup = if published {
                remove_recovery_copy(&temporary, &destination, &copied_hash)
            } else {
                Ok(())
            };
            if staged {
                let _ = fs::remove_file(&temporary);
            }
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => format!("{error}；{cleanup}"),
            });
        }
        let _ = fs::remove_file(temporary);
        Ok(&self.items[index])
    }

    /// Changes only an item's display name.
    pub fn rename(&mut self, id: &str, name: impl Into<String>) -> Result<(), String> {
        let name = name.into();
        validate_name(&name)?;
        self.update(id, |item| item.name = name)
    }

    /// Moves an item to the library recycle bin without deleting any file.
    pub fn trash(&mut self, id: &str) -> Result<(), String> {
        self.update(id, |item| item.trashed = true)
    }

    /// Restores a recycled item without changing its file.
    pub fn restore(&mut self, id: &str) -> Result<(), String> {
        self.update(id, |item| item.trashed = false)
    }

    /// Changes an item's favorite state without moving its file.
    pub fn set_favorite(&mut self, id: &str, favorite: bool) -> Result<(), String> {
        self.update(id, |item| item.favorite = favorite)
    }

    /// Moves several indexed items into or out of the recycle bin atomically.
    pub fn set_trashed_many(&mut self, ids: &[String], trashed: bool) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        for id in ids {
            if self.get(id).is_none() {
                return Err("批量操作包含已不存在的个人资产，请刷新后重试".into());
            }
        }
        let previous = self.items.clone();
        let mut changed = 0;
        for item in &mut self.items {
            if ids.iter().any(|id| id == &item.id) && item.trashed != trashed {
                item.trashed = trashed;
                changed += 1;
            }
        }
        if let Err(error) = self.save() {
            self.items = previous;
            return Err(error);
        }
        Ok(changed)
    }

    fn update(&mut self, id: &str, change: impl FnOnce(&mut Asset)) -> Result<(), String> {
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| "找不到这个个人资产".to_owned())?;
        let previous = self.items[index].clone();
        change(&mut self.items[index]);
        if let Err(error) = self.save() {
            self.items[index] = previous;
            return Err(error);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&Manifest {
            items: self.items.clone(),
            folders: self.folders.clone(),
        })
        .map_err(|error| format!("无法生成个人资产库索引：{error}"))?;
        let temporary = self
            .root
            .join(format!(".{MANIFEST_NAME}.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = File::create(&temporary)
                .map_err(|error| format!("无法写入个人资产库索引：{error}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("无法写入个人资产库索引：{error}"))?;
            #[cfg(test)]
            if FAIL_NEXT_MANIFEST_SAVE.with(|failure| {
                let mut failure = failure.borrow_mut();
                if failure.as_ref() == Some(&self.root) {
                    failure.take();
                    true
                } else {
                    false
                }
            }) {
                return Err("test: manifest commit failed after snapshot staging".into());
            }
            fs::rename(&temporary, self.root.join(MANIFEST_NAME))
                .map_err(|error| format!("无法更新个人资产库索引：{error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_MANIFEST_SAVE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn fail_next_manifest_save(root: &Path) {
    FAIL_NEXT_MANIFEST_SAVE.with(|failure| *failure.borrow_mut() = Some(root.to_path_buf()));
}

fn checked_prepared_path(root: &Path, prepared: &PreparedGenerated) -> Result<PathBuf, String> {
    let managed = root
        .join("generated")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let path = prepared.path.canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(managed) || file_digest(&path)? != prepared.sha256 {
        return Err("待入库副本校验失败，已保留原记录".into());
    }
    Ok(path)
}

fn file_digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn checked_recovery_folder(root: &Path, name: &str) -> Result<PathBuf, String> {
    let folder = root.join(name);
    match fs::symlink_metadata(&folder) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&folder).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let metadata = fs::symlink_metadata(&folder).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("受管素材目录不是普通文件夹，未写入任何素材".into());
    }
    if folder.canonicalize().map_err(|error| error.to_string())? != folder {
        return Err("受管素材目录路径已变化，未写入任何素材".into());
    }
    Ok(folder)
}

fn remove_recovery_copy(temporary: &Path, destination: &Path, sha256: &str) -> Result<(), String> {
    // Keep an external replacement intact if it appeared after our publication.
    let metadata = fs::symlink_metadata(destination).map_err(|error| error.to_string())?;
    if !metadata.is_file() || file_digest(destination)? != sha256 {
        return Err("恢复路径被其他操作改变，已保留该文件；资产索引未改变".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let staged = fs::metadata(temporary).map_err(|error| error.to_string())?;
        if metadata.dev() != staged.dev() || metadata.ino() != staged.ino() {
            return Err("恢复路径被其他操作替换，已保留该文件；资产索引未改变".into());
        }
    }
    #[cfg(not(unix))]
    let _ = temporary;
    fs::remove_file(destination).map_err(|error| format!("无法清理本次恢复副本：{error}"))
}

/// Run on a worker. Canvas can supply its encoded bytes, avoiding an external-path overwrite race.
pub(crate) fn prepare_generated(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: Option<&str>,
    bytes: Option<&[u8]>,
) -> Result<PreparedGenerated, String> {
    let (source_path, kind) = checked_media(path)?;
    let extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .ok_or("文件扩展名无效")?
        .to_ascii_lowercase();
    let folder = root.join("generated");
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let temporary = folder.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        if let Some(bytes) = bytes {
            let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
        } else {
            fs::copy(&source_path, &temporary).map_err(|e| e.to_string())?;
            File::options()
                .write(true)
                .open(&temporary)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            if !same_file_bytes(&source_path, &temporary)? {
                return Err("源素材在入库过程中发生变化，请重试".into());
            }
        }
        let sha256 = file_digest(&temporary)?;
        let destination = folder.join(format!("{sha256}.{extension}"));
        if destination.exists() {
            if file_digest(&destination)? != sha256 {
                return Err("受管副本校验失败，已保留原文件".into());
            }
            fs::remove_file(&temporary).map_err(|e| e.to_string())?;
        } else {
            fs::rename(&temporary, &destination).map_err(|e| e.to_string())?;
        }
        let operation = operation
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let registration_id = if let Some(operation) = &operation {
            format!("operation:{operation}")
        } else {
            let encoded = serde_json::to_vec(&source_path).map_err(|e| e.to_string())?;
            format!("content:{:x}:{sha256}", Sha256::digest(encoded))
        };
        Ok(PreparedGenerated {
            path: destination.canonicalize().map_err(|e| e.to_string())?,
            source_path: source_path.clone(),
            name: name
                .filter(|v| !v.is_empty() && validate_name(v).is_ok())
                .map(str::to_owned)
                .unwrap_or_else(|| display_name(&source_path)),
            kind,
            sha256,
            registration_id,
            operation,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Builds or reuses a local JPEG thumbnail for an image or video asset.
///
/// Decoding can be expensive, so callers should invoke this on a background
/// thread. The cache key includes source metadata and changes when the source
/// file changes. Audio assets and all failures return `None`.
pub fn thumbnail(asset: &Asset, root: &Path) -> Option<PathBuf> {
    if asset.kind == AssetKind::Audio {
        return None;
    }
    let path = asset.path.canonicalize().ok()?;
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let (cache_dir, destination) = thumbnail_destination(&path, &metadata, root)?;
    if destination.is_file() {
        return Some(destination);
    }

    let info = concat_media::probe(&path).ok()?;
    let video = info.video?;
    if video.width == 0 || video.height == 0 {
        return None;
    }
    let longest = u64::from(video.width.max(video.height)).max(512);
    let width = ((u64::from(video.width) * 512 + longest / 2) / longest).max(1) as u32;
    let height = ((u64::from(video.height) * 512 + longest / 2) / longest).max(1) as u32;
    let mut options = concat_media::DecodeOptions::default()
        .scaled_to(width, height)
        .limited_to(1);
    if asset.kind == AssetKind::Video {
        options = options.nearest_keyframes();
    }
    let mut decoder = concat_media::Decoder::open(&path, &options).ok()?;
    let frame = concat_media::FrameSource::next_frame(&mut decoder).ok()??;
    let jpeg = concat_media::jpeg(&frame, 5).ok()?;

    fs::create_dir_all(&cache_dir).ok()?;
    let temporary = cache_dir.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = File::create(&temporary).ok()?;
        file.write_all(&jpeg).ok()?;
        file.sync_all().ok()?;
        fs::rename(&temporary, &destination).ok()?;
        Some(destination)
    })();
    if result.is_none() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Returns an already-generated thumbnail without decoding media on the caller's thread.
pub fn cached_thumbnail(asset: &Asset, root: &Path) -> Option<PathBuf> {
    if asset.kind == AssetKind::Audio {
        return None;
    }
    let path = asset.path.canonicalize().ok()?;
    let metadata = fs::metadata(&path).ok()?;
    let (_, destination) = thumbnail_destination(&path, &metadata, root)?;
    destination.is_file().then_some(destination)
}

fn thumbnail_destination(
    path: &Path,
    metadata: &fs::Metadata,
    root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let mut hasher = DefaultHasher::new();
    "v2-512".hash(&mut hasher);
    path.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.as_secs().hash(&mut hasher);
    modified.subsec_nanos().hash(&mut hasher);
    let cache_dir = root.join("thumbnails");
    let destination = cache_dir.join(format!("{:016x}.jpg", hasher.finish()));
    Some((cache_dir, destination))
}

fn checked_media(path: &Path) -> Result<(PathBuf, AssetKind), String> {
    if path.as_os_str().is_empty() {
        return Err("请选择素材文件".into());
    }
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取素材文件：{error}"))?;
    if !metadata.is_file() {
        return Err("素材必须是普通文件".into());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "无法识别没有扩展名的素材".to_owned())?;
    let kind = match extension.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "avif" => {
            AssetKind::Image
        }
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "mpeg" | "mpg" => AssetKind::Video,
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "aiff" | "aif" => {
            AssetKind::Audio
        }
        _ => return Err(format!("不支持 .{extension} 格式的素材")),
    };
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("无法解析素材文件路径：{error}"))?;
    Ok((canonical, kind))
}

fn display_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("未命名素材")
        .to_owned()
}

fn same_file_bytes(source: &Path, managed: &Path) -> Result<bool, String> {
    let source_size = fs::metadata(source)
        .map_err(|error| format!("无法读取源素材：{error}"))?
        .len();
    let managed_size = match fs::metadata(managed) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("无法读取已导入素材：{error}")),
    };
    if source_size != managed_size {
        return Ok(false);
    }
    let mut left = File::open(source).map_err(|error| format!("无法读取源素材：{error}"))?;
    let mut right = File::open(managed).map_err(|error| format!("无法读取已导入素材：{error}"))?;
    let mut source_chunk = [0_u8; 64 * 1024];
    let mut managed_chunk = [0_u8; 64 * 1024];
    loop {
        let count = left
            .read(&mut source_chunk)
            .map_err(|error| format!("无法读取源素材：{error}"))?;
        if count == 0 {
            return Ok(true);
        }
        right
            .read_exact(&mut managed_chunk[..count])
            .map_err(|error| format!("无法读取已导入素材：{error}"))?;
        if source_chunk[..count] != managed_chunk[..count] {
            return Ok(false);
        }
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("资产名称不能为空".into());
    }
    if trimmed != name
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return Err("资产名称不能包含路径、首尾空格或控制字符".into());
    }
    Ok(())
}

fn now_millis() -> Result<u64, String> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时间无效，无法登记素材".to_owned())?
        .as_millis();
    u64::try_from(value).map_err(|_| "系统时间超出支持范围".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RelinkTemp(PathBuf);

    impl RelinkTemp {
        fn new() -> Self {
            let path = temp_root("relink");
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for RelinkTemp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn relink_fixture() -> (RelinkTemp, Library, Asset, PathBuf) {
        let temporary = RelinkTemp::new();
        let selected = temporary.0.join("selected.png");
        fs::write(&selected, b"original immutable output").unwrap();
        let mut library = Library::load(temporary.0.join("library")).unwrap();
        let id = library.register_generated(&selected).unwrap().id.clone();
        library.rename(&id, "保留名称").unwrap();
        library.set_favorite(&id, true).unwrap();
        let folder = library.create_folder("保留目录").unwrap().id.clone();
        library
            .move_to_folder(std::slice::from_ref(&id), Some(&folder))
            .unwrap();
        let expected = library.get(&id).unwrap().clone();
        fs::remove_file(&expected.path).unwrap();
        (temporary, library, expected, selected)
    }

    fn relink_image(path: &Path) {
        let frame = concat_core::frame::Frame::black(16, 16);
        fs::write(path, concat_media::jpeg(&frame, 5).unwrap()).unwrap();
    }

    fn relink_assert_unchanged(library: &Library, expected: &Asset, manifest: &[u8]) {
        assert_eq!(library.items(), std::slice::from_ref(expected));
        assert_eq!(
            fs::read(library.root.join(MANIFEST_NAME)).unwrap(),
            manifest
        );
        assert!(!expected.path.exists());
        for name in ["imported", "generated"] {
            if let Ok(entries) = fs::read_dir(library.root.join(name)) {
                assert!(entries.into_iter().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".recovery-")
                }));
            }
        }
    }

    #[test]
    fn relink_matching_hash_restores_original_path_identity_and_metadata_once() {
        let (_temporary, mut library, expected, selected) = relink_fixture();
        let recovered = library
            .relink_missing(&expected, &selected, false)
            .unwrap()
            .clone();
        let mut preserved = recovered.clone();
        preserved.recovery = None;
        assert_eq!(preserved, expected);
        assert_eq!(
            fs::read(&recovered.path).unwrap(),
            fs::read(&selected).unwrap()
        );
        let recovery = recovered.recovery.as_ref().unwrap();
        assert_eq!(recovery.previous_path, expected.path);
        assert_eq!(recovery.selected_path, selected.canonicalize().unwrap());
        assert!(recovery.verified_original);
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        assert_eq!(
            library
                .relink_missing(&recovered, &selected, false)
                .unwrap(),
            &recovered
        );
        assert_eq!(library.items().len(), 1);
        assert_eq!(
            fs::read(library.root.join(MANIFEST_NAME)).unwrap(),
            manifest
        );
        let reopened = Library::load(&library.root).unwrap();
        assert_eq!(reopened.items(), std::slice::from_ref(&recovered));
    }

    #[test]
    fn relink_wrong_hash_kind_and_cancelled_selection_keep_original_record() {
        let (temporary, mut library, expected, selected) = relink_fixture();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        fs::write(&selected, b"wrong content").unwrap();
        assert!(library.relink_missing(&expected, &selected, false).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);
        let audio = temporary.0.join("different.wav");
        fs::write(&audio, b"original immutable output").unwrap();
        assert!(library.relink_missing(&expected, &audio, false).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);
        assert!(
            library
                .relink_missing(&expected, Path::new(""), false)
                .is_err()
        );
        relink_assert_unchanged(&library, &expected, &manifest);
    }

    #[test]
    fn relink_unverified_requires_confirmation_probes_snapshot_and_retains_provenance() {
        let (temporary, mut library, mut expected, _) = relink_fixture();
        expected.path = temporary.0.join("old-external-missing.jpg");
        expected.version = None;
        library.items[0] = expected.clone();
        library.save().unwrap();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        let selected = temporary.0.join("replacement.jpg");
        relink_image(&selected);
        assert!(library.relink_missing(&expected, &selected, false).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);
        let invalid = temporary.0.join("invalid.jpg");
        fs::write(&invalid, b"not a media file").unwrap();
        assert!(library.relink_missing(&expected, &invalid, true).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);

        let restored = library
            .relink_missing(&expected, &selected, true)
            .unwrap()
            .clone();
        assert_eq!(restored.id, expected.id);
        assert_eq!(restored.name, expected.name);
        assert_eq!(restored.source, expected.source);
        assert_eq!(restored.original_path, expected.original_path);
        assert_eq!(restored.folder_id, expected.folder_id);
        assert_eq!(restored.created_at, expected.created_at);
        assert_eq!(restored.favorite, expected.favorite);
        assert_eq!(restored.trashed, expected.trashed);
        assert_eq!(
            restored.path.parent(),
            Some(library.root.join("generated").as_path())
        );
        assert!(!expected.path.exists());
        assert!(!restored.recovery.as_ref().unwrap().verified_original);
        assert_eq!(
            restored.recovery.as_ref().unwrap().previous_path,
            expected.path
        );
        assert_eq!(
            restored.version.as_ref().unwrap().sha256,
            file_digest(&selected).unwrap()
        );

        // A second recovery matches the recorded replacement, not proven
        // historical content, and preserves the first external missing path.
        fs::remove_file(&restored.path).unwrap();
        let restored_again = library
            .relink_missing(&restored, &selected, false)
            .unwrap()
            .clone();
        assert_eq!(restored_again.path, restored.path);
        assert_eq!(restored_again.version, restored.version);
        assert!(!restored_again.recovery.as_ref().unwrap().verified_original);
        assert_eq!(
            restored_again.recovery.as_ref().unwrap().previous_path,
            expected.path
        );
        let reopened = Library::load(&library.root).unwrap();
        assert_eq!(reopened.items(), std::slice::from_ref(&restored_again));
    }

    #[test]
    fn relink_imported_without_hash_keeps_id_source_and_original_managed_path() {
        let temporary = RelinkTemp::new();
        let selected = temporary.0.join("selected.jpg");
        relink_image(&selected);
        let mut library = Library::load(temporary.0.join("library")).unwrap();
        let expected = library.import(&selected).unwrap().clone();
        fs::remove_file(&expected.path).unwrap();
        let restored = library.relink_missing(&expected, &selected, true).unwrap();
        assert_eq!(restored.id, expected.id);
        assert_eq!(restored.path, expected.path);
        assert_eq!(restored.source, AssetSource::Imported);
        assert_eq!(restored.original_path, expected.original_path);
        assert!(restored.version.is_some());
        assert!(!restored.recovery.as_ref().unwrap().verified_original);
    }

    #[test]
    fn relink_manifest_failure_rolls_back_memory_index_and_new_file() {
        let (_temporary, mut library, expected, selected) = relink_fixture();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        fail_next_manifest_save(&library.root);
        assert!(library.relink_missing(&expected, &selected, false).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);
        let restored = library.relink_missing(&expected, &selected, false).unwrap();
        assert_eq!(restored.path, expected.path);
        assert_eq!(restored.id, expected.id);
        assert!(restored.path.is_file());
        assert_eq!(library.items().len(), 1);
    }

    #[test]
    fn relink_stale_expected_and_existing_file_are_not_overwritten() {
        let (_temporary, mut library, expected, selected) = relink_fixture();
        library.rename(&expected.id, "后来修改的名称").unwrap();
        let updated = library.get(&expected.id).unwrap().clone();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        assert!(library.relink_missing(&expected, &selected, false).is_err());
        relink_assert_unchanged(&library, &updated, &manifest);
        fs::write(&updated.path, b"another existing file").unwrap();
        assert!(library.relink_missing(&updated, &selected, false).is_err());
        assert_eq!(fs::read(&updated.path).unwrap(), b"another existing file");
        assert_eq!(library.items(), std::slice::from_ref(&updated));
        assert_eq!(
            fs::read(library.root.join(MANIFEST_NAME)).unwrap(),
            manifest
        );
    }

    #[cfg(unix)]
    #[test]
    fn relink_rejects_symlinked_managed_directory() {
        use std::os::unix::fs::symlink;
        let (temporary, mut library, expected, selected) = relink_fixture();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        let external = temporary.0.join("external-directory");
        fs::create_dir(&external).unwrap();
        fs::remove_dir(library.root.join("generated")).unwrap();
        symlink(&external, library.root.join("generated")).unwrap();
        assert!(library.relink_missing(&expected, &selected, false).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);
        assert_eq!(fs::read_dir(&external).unwrap().count(), 0);
    }

    #[test]
    fn relink_loader_does_not_migrate_retry_or_create_directories() {
        let (temporary, mut library, mut expected, selected) = relink_fixture();
        expected.path = selected.clone();
        expected.version = None;
        library.items[0] = expected.clone();
        library.save().unwrap();
        fs::remove_dir(library.root.join("generated")).unwrap();
        fs::remove_dir(library.root.join("imported")).unwrap();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        let mut reopened = Library::load_for_relink(&library.root).unwrap();
        assert_eq!(reopened.items(), std::slice::from_ref(&expected));
        assert_eq!(
            fs::read(library.root.join(MANIFEST_NAME)).unwrap(),
            manifest
        );
        assert!(!library.root.join("generated").exists());
        assert!(!library.root.join("imported").exists());
        assert!(reopened.relink_missing(&expected, &selected, true).is_err());
        assert_eq!(
            fs::read(library.root.join(MANIFEST_NAME)).unwrap(),
            manifest
        );
        let absent = temporary.0.join("absent-library");
        assert!(Library::load_for_relink(&absent).is_err());
        assert!(!absent.exists());
    }

    #[test]
    fn relink_preserves_existing_legacy_migration_intent() {
        let (_temporary, mut library, expected, selected) = relink_fixture();
        let prepared = prepare_generated(&library.root, &selected, None, None, None).unwrap();
        fs::remove_file(&prepared.path).unwrap();
        let entries = vec![crate::registration_retry::LegacyMigration {
            target_asset_id: expected.id.clone(),
            asset_path: expected.path.clone(),
            prepared,
        }];
        crate::registration_retry::save_legacy_migrations(&library.root, &entries).unwrap();
        let manifest = fs::read(library.root.join(MANIFEST_NAME)).unwrap();
        let journal = fs::read(library.root.join("pending-legacy-generated.json")).unwrap();
        assert!(library.relink_missing(&expected, &selected, false).is_err());
        relink_assert_unchanged(&library, &expected, &manifest);
        assert_eq!(
            fs::read(library.root.join("pending-legacy-generated.json")).unwrap(),
            journal
        );
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "seecut-personal-library-{label}-{}",
            Uuid::new_v4()
        ))
    }

    #[test]
    fn import_keeps_source_and_deduplicates() {
        let root = temp_root("import");
        let source_dir = temp_root("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("photo.JPG");
        fs::write(&source, b"image").unwrap();
        let mut library = Library::load(&root).unwrap();
        let first_id = library.import(&source).unwrap().id.clone();
        let managed = library.get(&first_id).unwrap().path.clone();
        assert!(source.exists());
        assert_eq!(fs::read(&managed).unwrap(), b"image");
        assert_eq!(library.import(&source).unwrap().id, first_id);
        assert_eq!(library.items().len(), 1);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(source_dir).unwrap();
    }

    #[test]
    fn changed_source_import_creates_version_and_missing_copy_can_be_reimported() {
        let root = temp_root("source-version");
        let source_dir = temp_root("source-version-media");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("photo.png");
        fs::write(&source, b"first version").unwrap();
        let mut library = Library::load(&root).unwrap();
        let first = library.import(&source).unwrap().clone();
        fs::write(&source, b"second version").unwrap();
        let second = library.import(&source).unwrap().clone();
        assert_ne!(first.id, second.id);
        assert_eq!(fs::read(&first.path).unwrap(), b"first version");
        assert_eq!(fs::read(&second.path).unwrap(), b"second version");
        assert_eq!(library.import(&source).unwrap().id, second.id);
        fs::remove_file(&second.path).unwrap();
        let recovered = library.import(&source).unwrap().clone();
        assert_ne!(recovered.id, second.id);
        assert_eq!(fs::read(&recovered.path).unwrap(), b"second version");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(source_dir).unwrap();
    }

    #[test]
    fn corrupt_index_is_preserved() {
        let root = temp_root("corrupt");
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join(MANIFEST_NAME);
        fs::write(&manifest, b"not json").unwrap();
        assert!(Library::load(&root).is_err());
        assert_eq!(fs::read(&manifest).unwrap(), b"not json");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generated_deduplicates_and_recycles_without_deleting() {
        let root = temp_root("generated");
        let media_dir = temp_root("media");
        fs::create_dir_all(&media_dir).unwrap();
        let media = media_dir.join("clip.mp4");
        fs::write(&media, b"video").unwrap();
        let mut library = Library::load(&root).unwrap();
        let id = library.register_generated(&media).unwrap().id.clone();
        assert_eq!(library.register_generated(&media).unwrap().id, id);
        library.trash(&id).unwrap();
        assert!(library.get(&id).unwrap().trashed);
        assert!(media.exists());
        library.restore(&id).unwrap();
        assert!(!library.get(&id).unwrap().trashed);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(media_dir).unwrap();
    }

    #[test]
    fn generated_registration_can_keep_a_readable_display_name() {
        let root = temp_root("generated-name");
        let media_dir = temp_root("generated-name-media");
        fs::create_dir_all(&media_dir).unwrap();
        let media = media_dir.join("opaque-square.png");
        fs::write(&media, b"image").unwrap();
        let mut library = Library::load(&root).unwrap();

        let asset = library
            .register_generated_with_name(&media, Some("opaque-square"))
            .unwrap();
        assert_eq!(asset.name, "opaque-square");
        assert_ne!(asset.path, media.canonicalize().unwrap());
        assert_eq!(asset.original_path, Some(media.canonicalize().unwrap()));
        assert_eq!(fs::read(&asset.path).unwrap(), b"image");

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(media_dir).unwrap();
    }

    #[test]
    fn old_manifest_defaults_favorite_and_batch_recycle_is_atomic() {
        let root = temp_root("compat-batch");
        fs::create_dir_all(root.join("imported")).unwrap();
        let media = root.join("imported/photo.png");
        fs::write(&media, b"image").unwrap();
        let canonical = media.canonicalize().unwrap();
        fs::write(
            root.join(MANIFEST_NAME),
            serde_json::to_vec(&serde_json::json!({"items":[{
                "id":"old","name":"photo","path":canonical,"kind":"image",
                "source":"imported","created_at":1,"trashed":false
            }]}))
            .unwrap(),
        )
        .unwrap();
        let mut library = Library::load(&root).unwrap();
        assert!(!library.get("old").unwrap().favorite);
        library.set_favorite("old", true).unwrap();
        assert!(library.get("old").unwrap().favorite);
        assert!(
            library
                .set_trashed_many(&["old".into(), "missing".into()], true)
                .is_err()
        );
        assert!(!library.get("old").unwrap().trashed);
        assert_eq!(library.set_trashed_many(&["old".into()], true).unwrap(), 1);
        assert!(library.get("old").unwrap().trashed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_move_persists_ids_skips_noop_and_rolls_back_failed_save() {
        let root = temp_root("folder-move");
        let media_dir = temp_root("folder-move-media");
        fs::create_dir_all(&media_dir).unwrap();
        let first_path = media_dir.join("first.png");
        let second_path = media_dir.join("second.png");
        fs::write(&first_path, b"first").unwrap();
        fs::write(&second_path, b"second").unwrap();
        let mut library = Library::load(&root).unwrap();
        let first = library.register_generated(&first_path).unwrap().id.clone();
        let second = library.register_generated(&second_path).unwrap().id.clone();
        let folder = library.create_folder("作品").unwrap().id.clone();
        let ids = vec![first.clone(), second.clone()];
        assert_eq!(library.move_to_folder(&ids, Some(&folder)).unwrap(), 2);
        let before_noop = fs::read(root.join(MANIFEST_NAME)).unwrap();
        assert_eq!(library.move_to_folder(&ids, Some(&folder)).unwrap(), 0);
        assert_eq!(fs::read(root.join(MANIFEST_NAME)).unwrap(), before_noop);
        drop(library);

        let mut library = Library::load(&root).unwrap();
        assert_eq!(
            library.get(&first).unwrap().folder_id.as_deref(),
            Some(folder.as_str())
        );
        assert_eq!(
            library.get(&second).unwrap().folder_id.as_deref(),
            Some(folder.as_str())
        );
        assert_eq!(library.items().len(), 2);
        assert_eq!(
            library
                .move_to_folder(std::slice::from_ref(&first), None)
                .unwrap(),
            1
        );
        assert!(library.get(&first).unwrap().folder_id.is_none());
        library.trash(&first).unwrap();
        assert!(
            library
                .move_to_folder(std::slice::from_ref(&first), Some(&folder))
                .is_err()
        );
        assert!(library.get(&first).unwrap().folder_id.is_none());

        // With storage unavailable, the in-memory category must also stay put.
        fs::remove_dir_all(&root).unwrap();
        assert!(
            library
                .move_to_folder(std::slice::from_ref(&second), None)
                .is_err()
        );
        assert_eq!(
            library.get(&second).unwrap().folder_id.as_deref(),
            Some(folder.as_str())
        );
        assert!(first_path.is_file() && second_path.is_file());
        fs::remove_dir_all(media_dir).unwrap();
    }

    #[test]
    fn rejects_unknown_media_and_path_like_names() {
        let root = temp_root("invalid");
        let media_dir = temp_root("invalid-media");
        fs::create_dir_all(&media_dir).unwrap();
        let media = media_dir.join("notes.txt");
        fs::write(&media, b"text").unwrap();
        let mut library = Library::load(&root).unwrap();
        assert!(library.import(&media).is_err());
        assert!(library.items().is_empty());
        let image = media_dir.join("image.png");
        fs::write(&image, b"image").unwrap();
        let id = library.register_generated(&image).unwrap().id.clone();
        assert!(library.rename(&id, "../bad").is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(media_dir).unwrap();
    }

    #[test]
    fn thumbnail_is_small_cached_and_invalidated_by_source_changes() {
        let root = temp_root("thumbnail");
        let media_dir = temp_root("thumbnail-media");
        fs::create_dir_all(&media_dir).unwrap();
        let source = media_dir.join("wide.jpg");
        let frame = concat_core::frame::Frame::black(320, 100);
        fs::write(&source, concat_media::jpeg(&frame, 5).unwrap()).unwrap();
        let asset = Asset {
            id: "thumbnail-test".into(),
            name: "wide".into(),
            path: source.canonicalize().unwrap(),
            kind: AssetKind::Image,
            source: AssetSource::Generated,
            version: None,
            original_path: None,
            recovery: None,
            created_at: 0,
            trashed: false,
            favorite: false,
            folder_id: None,
        };

        let first = thumbnail(&asset, &root).unwrap();
        assert_eq!(thumbnail(&asset, &root).unwrap(), first);
        let info = concat_media::probe(&first).unwrap();
        let video = info.video.unwrap();
        assert_eq!((video.width, video.height), (320, 100));

        let larger = concat_core::frame::Frame::black(321, 100);
        fs::write(&source, concat_media::jpeg(&larger, 5).unwrap()).unwrap();
        let second = thumbnail(&asset, &root).unwrap();
        assert_ne!(second, first);

        let mut audio = asset;
        audio.kind = AssetKind::Audio;
        assert_eq!(thumbnail(&audio, &root), None);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(media_dir).unwrap();
    }
}
