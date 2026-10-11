// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pending registration for already-written outputs. Callers hold the existing library write lock.
use crate::personal_library::Library;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;
const PENDING: &str = "pending-generated.json";
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    name: Option<String>,
}
#[derive(Default, Debug)]
pub(crate) struct Outcome {
    pub(crate) completed: usize,
    pub(crate) remaining: usize,
}
fn load(root: &Path) -> Result<Vec<Entry>, String> {
    match fs::read(root.join(PENDING)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("待入库记录已损坏：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(format!("无法读取待入库记录：{e}")),
    }
}
fn save(root: &Path, entries: &[Entry]) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let temporary = root.join(format!(".pending-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let bytes = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
        let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, root.join(PENDING)).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
/// Register only; never submit a generation, download or wallet operation.
pub(crate) fn register(root: &Path, path: &Path, name: Option<&str>) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("导出文件已不可用：{e}"))?;
    if !canonical.is_file() {
        return Err("导出文件已不可用".into());
    }
    let result = Library::load(root).and_then(|mut library| {
        library.register_generated_with_name(&canonical, name)?;
        Ok(())
    });
    match result {
        Ok(()) => {
            // A stale journal entry is harmless: the existing canonical-path identity makes replay idempotent.
            if let Ok(mut entries) = load(root) {
                let previous = entries.len();
                entries.retain(|entry| entry.path != canonical);
                if entries.len() != previous {
                    let _ = save(root, &entries);
                }
            }
            Ok(())
        }
        Err(error) => {
            let mut entries =
                load(root).map_err(|journal| format!("{error}；{journal}，原文件保留"))?;
            if !entries.iter().any(|entry| entry.path == canonical) {
                entries.push(Entry {
                    path: canonical,
                    name: name.map(str::to_owned),
                });
                save(root, &entries).map_err(|journal| {
                    format!("{error}；无法保存重试记录：{journal}，原文件保留")
                })?;
            }
            Err(format!("{error}；刷新资产库可重试入库"))
        }
    }
}
/// Replays only pending local index writes. Failed entries remain durable for a later refresh.
pub(crate) fn retry(root: &Path) -> Result<Outcome, String> {
    let entries = load(root)?;
    if entries.is_empty() {
        return Ok(Outcome::default());
    }
    let mut library = Library::load(root)?;
    let mut pending = Vec::new();
    let mut completed = 0;
    for entry in entries {
        if library
            .register_generated_with_name(&entry.path, entry.name.as_deref())
            .is_ok()
        {
            completed += 1;
        } else {
            pending.push(entry);
        }
    }
    if completed > 0 {
        save(root, &pending)?;
    }
    Ok(Outcome {
        completed,
        remaining: pending.len(),
    })
}
