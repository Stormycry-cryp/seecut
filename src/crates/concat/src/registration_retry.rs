// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable registration of immutable output snapshots. Callers hold the library write lock.
use crate::personal_library::{Library, PreparedGenerated, prepare_generated};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared: Option<PreparedGenerated>,
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
/// Repeated generation-cache indexing uses source and content identity.
pub(crate) fn register(root: &Path, path: &Path, name: Option<&str>) -> Result<(), String> {
    register_output(root, path, name, None, None)
}
/// A formal export supplies one operation ID and, for canvas, its owned encoded bytes.
pub(crate) fn register_output(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: Option<&str>,
    bytes: Option<&[u8]>,
) -> Result<(), String> {
    let mut entries = load(root)?;
    let prepared = prepare_generated(root, path, name, operation, bytes)?;
    if let Some(previous) = entries
        .iter()
        .filter_map(|entry| entry.prepared.as_ref())
        .find(|p| p.registration_id == prepared.registration_id)
    {
        if previous.sha256 != prepared.sha256 {
            return Err("同一待入库输出的内容已变化，已保留原副本".into());
        }
    } else {
        entries.push(Entry {
            path: prepared.source_path.clone(),
            name: name.map(str::to_owned),
            prepared: Some(prepared.clone()),
        });
        save(root, &entries).map_err(|e| format!("无法保存重试记录：{e}，输出副本已保留"))?;
    }
    let result = Library::load(root).and_then(|mut library| {
        library.commit_generated(&prepared)?;
        Ok(())
    });
    match result {
        Ok(()) => {
            entries.retain(|entry| {
                entry
                    .prepared
                    .as_ref()
                    .is_none_or(|p| p.registration_id != prepared.registration_id)
            });
            // A stale entry replays the same registration ID after a failed journal cleanup.
            let _ = save(root, &entries);
            Ok(())
        }
        Err(error) => Err(format!("{error}；刷新资产库可重试入库")),
    }
}
/// Retry snapshots only. No generation, download or wallet request is made.
pub(crate) fn retry(root: &Path) -> Result<Outcome, String> {
    let mut entries = load(root)?;
    if entries.is_empty() {
        return Ok(Outcome::default());
    }
    // Older path-only entries can preserve only the bytes still available at migration time.
    let mut upgraded = false;
    for entry in &mut entries {
        if entry.prepared.is_none()
            && let Ok(prepared) =
                prepare_generated(root, &entry.path, entry.name.as_deref(), None, None)
        {
            entry.prepared = Some(prepared);
            upgraded = true;
        }
    }
    if upgraded {
        save(root, &entries)?;
    }
    let mut library = Library::load(root)?;
    let mut pending = Vec::new();
    let mut completed = 0;
    for entry in entries {
        if entry
            .prepared
            .as_ref()
            .is_some_and(|prepared| library.commit_generated(prepared).is_ok())
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

#[cfg(test)]
mod tests {
    use crate::{
        personal_library::{Library, prepare_generated},
        registration_retry,
    };
    use serde_json::json;
    use std::{fs, path::PathBuf};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("asset-version-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn overwritten_external_path_creates_linked_versions_and_keeps_old_bytes() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("export.png");
        fs::write(&source, b"first").unwrap();
        let mut l = Library::load(&root).unwrap();
        let first = l.register_generated(&source).unwrap().clone();
        fs::write(&source, b"second").unwrap();
        let second = l.register_generated(&source).unwrap().clone();
        assert_ne!(first.id, second.id);
        assert_ne!(first.path, second.path);
        assert_eq!(fs::read(&first.path).unwrap(), b"first");
        assert_eq!(fs::read(&second.path).unwrap(), b"second");
        assert_eq!(
            second.version.as_ref().unwrap().previous_id.as_ref(),
            Some(&first.id)
        );
        assert_eq!(l.register_generated(&source).unwrap().id, second.id);
        fs::remove_file(&source).unwrap();
        let reload = Library::load(&root).unwrap();
        assert_eq!(
            fs::read(&reload.get(&first.id).unwrap().path).unwrap(),
            b"first"
        );
    }
    #[test]
    fn formal_export_id_distinguishes_same_bytes_and_replay_is_idempotent() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("same.mp4");
        fs::write(&source, b"same").unwrap();
        for id in ["export-1", "export-1", "export-2", "export-2"] {
            registration_retry::register_output(&root, &source, Some("成片"), Some(id), None)
                .unwrap();
        }
        let mut l = Library::load(&root).unwrap();
        assert_eq!(l.items().len(), 2);
        let first = l.items()[0].clone();
        let second = l.items()[1].clone();
        assert_ne!(first.id, second.id);
        assert_eq!(first.path, second.path);
        assert_eq!(second.version.unwrap().previous_id, Some(first.id));
        assert_eq!(l.register_generated(&source).unwrap().id, second.id);
        assert_eq!(l.items().len(), 2);
    }
    #[test]
    fn failed_index_replays_original_snapshot_after_external_overwrite_and_deletion() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("export.png");
        fs::write(&source, b"first").unwrap();
        fs::create_dir_all(root.join("library.json")).unwrap();
        assert!(
            registration_retry::register_output(&root, &source, None, Some("first"), None).is_err()
        );
        fs::write(&source, b"second").unwrap();
        assert!(
            registration_retry::register_output(&root, &source, None, Some("second"), None)
                .is_err()
        );
        fs::remove_file(source).unwrap();
        fs::remove_dir(root.join("library.json")).unwrap();
        let out = registration_retry::retry(&root).unwrap();
        assert_eq!((out.completed, out.remaining), (2, 0));
        let l = Library::load(&root).unwrap();
        assert_eq!(l.items().len(), 2);
        assert_eq!(fs::read(&l.items()[0].path).unwrap(), b"first");
        assert_eq!(fs::read(&l.items()[1].path).unwrap(), b"second");
        let id = l.items()[0].id.clone();
        assert_eq!(registration_retry::retry(&root).unwrap().completed, 0);
        assert_eq!(Library::load(&root).unwrap().items()[0].id, id);
    }
    #[test]
    fn legacy_manifest_keeps_id_flags_and_snapshots_available_bytes() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("old.png");
        fs::write(&source, b"legacy").unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("library.json"),serde_json::to_vec(&json!({"items":[{"id":"stable-old","name":"Old","path":source.canonicalize().unwrap(),"source":"generated","kind":"image","created_at":1,"trashed":true,"favorite":true}]})).unwrap()).unwrap();
        let l = Library::load(&root).unwrap();
        let old = l.get("stable-old").unwrap().clone();
        assert!(old.trashed && old.favorite);
        assert!(old.version.is_some());
        fs::write(source, b"overwritten").unwrap();
        assert_eq!(fs::read(&old.path).unwrap(), b"legacy");
        assert_eq!(Library::load(&root).unwrap().items().len(), 1);
    }
    #[test]
    fn canvas_owned_bytes_survive_external_overwrite_before_worker_registration() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("canvas.png");
        fs::write(&source, b"later external file").unwrap();
        registration_retry::register_output(
            &root,
            &source,
            Some("canvas"),
            Some("canvas-1"),
            Some(b"owned PNG bytes"),
        )
        .unwrap();
        let l = Library::load(&root).unwrap();
        assert_eq!(fs::read(&l.items()[0].path).unwrap(), b"owned PNG bytes");
        assert_eq!(fs::read(source).unwrap(), b"later external file");
    }
    #[test]
    fn stale_success_journal_replay_uses_same_asset_and_rejects_changed_operation() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("export.png");
        fs::write(&source, b"old").unwrap();
        fs::create_dir_all(root.join("library.json")).unwrap();
        assert!(
            registration_retry::register_output(&root, &source, None, Some("same-op"), None)
                .is_err()
        );
        let pending = fs::read(root.join("pending-generated.json")).unwrap();
        fs::remove_dir(root.join("library.json")).unwrap();
        registration_retry::retry(&root).unwrap();
        let old = Library::load(&root).unwrap().items()[0].clone();
        fs::write(root.join("pending-generated.json"), pending).unwrap();
        registration_retry::retry(&root).unwrap();
        assert_eq!(Library::load(&root).unwrap().items().len(), 1);
        fs::write(&source, b"changed").unwrap();
        assert!(
            registration_retry::register_output(&root, &source, None, Some("same-op"), None)
                .is_err()
        );
        assert_eq!(fs::read(&old.path).unwrap(), b"old");
        assert_eq!(Library::load(&root).unwrap().items()[0].id, old.id);
    }
    #[test]
    fn corrupted_snapshot_is_not_committed_as_expected_content() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("export.png");
        fs::write(&source, b"original").unwrap();
        let prepared = prepare_generated(&root, &source, None, None, None).unwrap();
        fs::write(&prepared.path, b"corrupted").unwrap();
        let mut l = Library::load(root).unwrap();
        assert!(l.commit_generated(&prepared).is_err());
        assert!(l.items().is_empty());
    }
}
