// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-only storage for the user's personal media library.

use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// The file stored below the library root.
pub const MANIFEST_NAME: &str = "library.json";

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
    /// Registered from an existing generated or downloaded file.
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
    /// Canonical source path for import de-duplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<PathBuf>,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at: u64,
    /// Whether the item is in the library recycle bin.
    pub trashed: bool,
    /// Whether the user pinned this item for quick access.
    #[serde(default)]
    pub favorite: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    items: Vec<Asset>,
}

/// A local personal media library backed by an atomic JSON index.
pub struct Library {
    root: PathBuf,
    items: Vec<Asset>,
}

impl Library {
    /// Opens a library, creating its directories when no index exists.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        if root.exists() && !root.is_dir() {
            return Err("个人资产库路径不是文件夹".into());
        }
        let manifest_path = root.join(MANIFEST_NAME);
        let items = if manifest_path.exists() {
            let bytes = fs::read(&manifest_path)
                .map_err(|error| format!("无法读取个人资产库索引：{error}"))?;
            serde_json::from_slice::<Manifest>(&bytes)
                .map_err(|error| {
                    format!("个人资产库索引已损坏，请先备份或修复 library.json：{error}")
                })?
                .items
        } else {
            Vec::new()
        };
        fs::create_dir_all(root.join("imported"))
            .map_err(|error| format!("无法创建个人资产库目录：{error}"))?;
        Ok(Self { root, items })
    }

    /// Returns all indexed items, including recycled items.
    pub fn items(&self) -> &[Asset] {
        &self.items
    }

    /// Returns an item by stable identifier.
    pub fn get(&self, id: &str) -> Option<&Asset> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Copies a media file into managed storage and indexes it.
    pub fn import(&mut self, path: impl AsRef<Path>) -> Result<&Asset, String> {
        let source = checked_media(path.as_ref())?;
        if let Some(index) = self.items.iter().position(|item| {
            item.source == AssetSource::Imported && item.original_path.as_ref() == Some(&source.0)
        }) {
            return Ok(&self.items[index]);
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
            original_path: Some(source.0),
            created_at,
            trashed: false,
            favorite: false,
        };
        self.items.push(item);
        if let Err(error) = self.save() {
            let item = self.items.pop().expect("刚插入的资产必须存在");
            let _ = fs::remove_file(item.path);
            return Err(error);
        }
        Ok(self.items.last().expect("刚保存的资产必须存在"))
    }

    /// Indexes an existing generated or downloaded media file without copying it.
    pub fn register_generated(&mut self, path: impl AsRef<Path>) -> Result<&Asset, String> {
        let checked = checked_media(path.as_ref())?;
        if let Some(index) = self.items.iter().position(|item| item.path == checked.0) {
            return Ok(&self.items[index]);
        }
        let created_at = now_millis()?;
        self.items.push(Asset {
            id: Uuid::new_v4().to_string(),
            name: display_name(&checked.0),
            path: checked.0,
            kind: checked.1,
            source: AssetSource::Generated,
            original_path: None,
            created_at,
            trashed: false,
            favorite: false,
        });
        if let Err(error) = self.save() {
            self.items.pop();
            return Err(error);
        }
        Ok(self.items.last().expect("刚保存的资产必须存在"))
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
            fs::rename(&temporary, self.root.join(MANIFEST_NAME))
                .map_err(|error| format!("无法更新个人资产库索引：{error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
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
            original_path: None,
            created_at: 0,
            trashed: false,
            favorite: false,
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
