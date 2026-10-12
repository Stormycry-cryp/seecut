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
const LEGACY_PENDING: &str = "pending-legacy-generated.json";

/// A migration changes one existing asset; it must never create a new identity.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LegacyMigration {
    pub(crate) target_asset_id: String,
    pub(crate) asset_path: PathBuf,
    pub(crate) prepared: PreparedGenerated,
}

pub(crate) fn load_legacy_migrations(root: &Path) -> Result<Vec<LegacyMigration>, String> {
    match fs::read(root.join(LEGACY_PENDING)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("旧资产迁移记录已损坏：{e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(format!("无法读取旧资产迁移记录：{e}")),
    }
}

pub(crate) fn save_legacy_migrations(
    root: &Path,
    entries: &[LegacyMigration],
) -> Result<(), String> {
    save_json(root, LEGACY_PENDING, entries)
}
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
    save_json(root, PENDING, entries)
}
fn save_json<T: Serialize + ?Sized>(root: &Path, name: &str, entries: &T) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let temporary = root.join(format!(".pending-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let bytes = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
        let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, root.join(name)).map_err(|e| e.to_string())
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
    register_output_with_provenance(root, path, name, operation, bytes, None)
}

pub(crate) fn register_output_with_provenance(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: Option<&str>,
    bytes: Option<&[u8]>,
    provenance: Option<crate::personal_library::OutputProvenance>,
) -> Result<(), String> {
    let mut entries = load(root)?;
    let mut prepared = prepare_generated(root, path, name, operation, bytes)?;
    prepared.provenance = provenance;
    if let Some(previous) = entries
        .iter()
        .filter_map(|entry| entry.prepared.as_ref())
        .find(|p| p.registration_id == prepared.registration_id)
    {
        if previous.sha256 != prepared.sha256 || previous.provenance != prepared.provenance {
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
    let legacy_count = load_legacy_migrations(root)?.len();
    if entries.is_empty() && legacy_count == 0 {
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
    let mut completed = legacy_count;
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
        personal_library::{Library, fail_next_manifest_save, prepare_generated},
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
    fn legacy_fixture(t: &Temp, ids: &[&str]) -> (PathBuf, PathBuf, serde_json::Value) {
        let root = t.0.join("library");
        let source = t.0.join("legacy.png");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"first migration snapshot").unwrap();
        let source = source.canonicalize().unwrap();
        let items = ids
            .iter()
            .map(|id| {
                json!({
                    "id": id, "name": format!("Old {id}"), "path": source.canonicalize().unwrap(),
                    "original_path": t.0.join("original-provenance.png"), "kind": "image",
                    "source": "generated", "created_at": 7, "trashed": true, "favorite": true,
                    "folder_id": "old-folder"
                })
            })
            .collect::<Vec<_>>();
        let manifest = json!({"items": items, "folders": [{"id":"old-folder","name":"Old"}]});
        fs::write(
            root.join("library.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        (root, source, manifest)
    }

    fn fail_migration_commit(root: &std::path::Path) {
        fail_next_manifest_save(root);
        let error = Library::load(root)
            .err()
            .expect("injected index commit must fail");
        assert_eq!(error, "test: manifest commit failed after snapshot staging");
        let pending = registration_retry::load_legacy_migrations(root).unwrap();
        assert!(!pending.is_empty());
        for entry in pending {
            assert_eq!(
                fs::read(entry.prepared.path).unwrap(),
                b"first migration snapshot"
            );
        }
    }

    #[test]
    fn legacy_migration_commit_failure_recovers_first_bytes_and_current_metadata() {
        let t = Temp::new();
        let (root, source, mut manifest) = legacy_fixture(&t, &["stable-old"]);
        let before = fs::read(root.join("library.json")).unwrap();
        fail_migration_commit(&root);
        assert_eq!(fs::read(root.join("library.json")).unwrap(), before);
        let journal = fs::read(root.join(registration_retry::LEGACY_PENDING)).unwrap();
        fs::write(&source, b"overwritten later").unwrap();
        manifest["items"][0]["name"] = json!("Renamed while pending");
        manifest["items"][0]["favorite"] = json!(false);
        manifest["items"][0]["trashed"] = json!(false);
        manifest["items"][0]["folder_id"] = json!("new-folder");
        manifest["items"][0]["created_at"] = json!(11);
        manifest["folders"] = json!([{"id":"new-folder","name":"New"}]);
        fs::write(
            root.join("library.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::remove_file(&source).unwrap();
        let outcome = registration_retry::retry(&root).unwrap();
        assert_eq!((outcome.completed, outcome.remaining), (1, 0));
        let library = Library::load(&root).unwrap();
        assert_eq!(library.items().len(), 1);
        let item = library.get("stable-old").unwrap();
        assert_eq!(fs::read(&item.path).unwrap(), b"first migration snapshot");
        let actual = serde_json::to_value(item).unwrap();
        for (key, expected) in manifest["items"][0].as_object().unwrap() {
            if key != "path" {
                assert_eq!(&actual[key], expected, "metadata field {key}");
            }
        }
        assert_eq!(library.folders()[0].id, "new-folder");
        // Cleanup can fail after a successful commit: replay must be read-only.
        let committed = fs::read(root.join("library.json")).unwrap();
        fs::write(root.join(registration_retry::LEGACY_PENDING), journal).unwrap();
        let replay = Library::load(&root).unwrap();
        assert_eq!(replay.items(), library.items());
        assert_eq!(fs::read(root.join("library.json")).unwrap(), committed);
        assert!(
            registration_retry::load_legacy_migrations(&root)
                .unwrap()
                .is_empty()
        );
        assert_eq!(registration_retry::retry(&root).unwrap().completed, 0);
    }

    #[test]
    fn legacy_migration_corrupt_snapshot_keeps_old_index_and_intent() {
        let t = Temp::new();
        let (root, source, _) = legacy_fixture(&t, &["stable-old"]);
        fail_migration_commit(&root);
        let before = fs::read(root.join("library.json")).unwrap();
        let journal = fs::read(root.join(registration_retry::LEGACY_PENDING)).unwrap();
        let pending = registration_retry::load_legacy_migrations(&root).unwrap();
        fs::write(&pending[0].prepared.path, b"damaged").unwrap();
        fs::write(&source, b"different external bytes").unwrap();
        assert!(Library::load(&root).is_err());
        assert!(registration_retry::retry(&root).is_err());
        assert_eq!(fs::read(root.join("library.json")).unwrap(), before);
        assert_eq!(
            fs::read(root.join(registration_retry::LEGACY_PENDING)).unwrap(),
            journal
        );
        assert_eq!(fs::read(&source).unwrap(), b"different external bytes");
    }

    #[test]
    fn legacy_migration_wrong_target_or_different_version_is_not_replaced() {
        for different_version in [false, true] {
            let t = Temp::new();
            let (root, source, mut manifest) = legacy_fixture(&t, &["stable-old"]);
            fail_migration_commit(&root);
            let journal = fs::read(root.join(registration_retry::LEGACY_PENDING)).unwrap();
            if different_version {
                fs::write(&source, b"another committed version").unwrap();
                let other =
                    prepare_generated(&root, &source, None, Some("different-version"), None)
                        .unwrap();
                manifest["items"][0]["path"] = json!(other.path);
                manifest["items"][0]["version"] = json!({
                    "sha256":other.sha256,"registration_id":other.registration_id
                });
            } else {
                manifest["items"][0]["id"] = json!("different-target");
            }
            let before = serde_json::to_vec(&manifest).unwrap();
            fs::write(root.join("library.json"), &before).unwrap();
            assert!(Library::load(&root).is_err());
            assert!(registration_retry::retry(&root).is_err());
            assert_eq!(fs::read(root.join("library.json")).unwrap(), before);
            assert_eq!(
                fs::read(root.join(registration_retry::LEGACY_PENDING)).unwrap(),
                journal
            );
        }
    }

    #[test]
    fn legacy_migrations_and_output_retry_keep_separate_identities_and_journals() {
        let t = Temp::new();
        let (root, source, _) = legacy_fixture(&t, &["old-one", "old-two"]);
        fail_migration_commit(&root);
        assert_eq!(
            registration_retry::load_legacy_migrations(&root)
                .unwrap()
                .len(),
            2
        );
        fs::write(&source, b"new formal output").unwrap();
        fail_next_manifest_save(&root);
        assert!(
            registration_retry::register_output(&root, &source, None, Some("new-output"), None)
                .is_err()
        );
        assert_eq!(
            registration_retry::load_legacy_migrations(&root)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(registration_retry::load(&root).unwrap().len(), 1);
        fs::remove_file(&source).unwrap();
        let outcome = registration_retry::retry(&root).unwrap();
        assert_eq!((outcome.completed, outcome.remaining), (3, 0));
        let library = Library::load(&root).unwrap();
        assert_eq!(library.items().len(), 3);
        for id in ["old-one", "old-two"] {
            assert_eq!(
                fs::read(&library.get(id).unwrap().path).unwrap(),
                b"first migration snapshot"
            );
        }
        assert_eq!(
            fs::read(&library.items()[2].path).unwrap(),
            b"new formal output"
        );
        assert!(
            registration_retry::load_legacy_migrations(&root)
                .unwrap()
                .is_empty()
        );
        assert!(registration_retry::load(&root).unwrap().is_empty());
        assert_eq!(registration_retry::retry(&root).unwrap().completed, 0);
    }

    #[test]
    fn first_legacy_prepare_failure_is_not_reported_as_available() {
        for corrupt_cas in [false, true] {
            let t = Temp::new();
            let (root, source, _) = legacy_fixture(&t, &["stable-old"]);
            let before = fs::read(root.join("library.json")).unwrap();
            if corrupt_cas {
                let prepared = prepare_generated(&root, &source, None, None, None).unwrap();
                fs::write(prepared.path, b"corrupt existing CAS").unwrap();
            } else {
                fs::write(root.join("generated"), b"not a directory").unwrap();
            }
            let error = Library::load(&root)
                .err()
                .expect("present source failure must be visible");
            assert!(error.contains("Old stable-old"));
            assert_eq!(fs::read(root.join("library.json")).unwrap(), before);
            assert_eq!(fs::read(&source).unwrap(), b"first migration snapshot");
            assert!(
                registration_retry::load_legacy_migrations(&root)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn later_legacy_prepare_failure_persists_earlier_snapshot_before_returning() {
        let t = Temp::new();
        let (root, source, mut manifest) = legacy_fixture(&t, &["first-old", "second-old"]);
        let second_source = t.0.join("second.png");
        fs::write(&second_source, b"second source bytes").unwrap();
        manifest["items"][1]["path"] = json!(second_source.canonicalize().unwrap());
        let before = serde_json::to_vec(&manifest).unwrap();
        fs::write(root.join("library.json"), &before).unwrap();
        let second = prepare_generated(&root, &second_source, None, None, None).unwrap();
        fs::write(&second.path, b"corrupt second CAS").unwrap();
        let error = Library::load(&root)
            .err()
            .expect("second prepare must fail");
        assert!(error.contains("Old second-old"));
        assert_eq!(fs::read(root.join("library.json")).unwrap(), before);
        let pending = registration_retry::load_legacy_migrations(&root).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].target_asset_id, "first-old");
        assert_eq!(
            fs::read(&pending[0].prepared.path).unwrap(),
            b"first migration snapshot"
        );
        fs::write(&source, b"overwritten while repair pending").unwrap();
        fs::remove_file(&source).unwrap();
        fs::remove_file(&second.path).unwrap();
        let library = Library::load(&root).unwrap();
        assert_eq!(library.items().len(), 2);
        assert_eq!(
            fs::read(&library.get("first-old").unwrap().path).unwrap(),
            b"first migration snapshot"
        );
        assert_eq!(
            fs::read(&library.get("second-old").unwrap().path).unwrap(),
            b"second source bytes"
        );
        assert!(
            registration_retry::load_legacy_migrations(&root)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_legacy_source_stays_indexed_until_it_can_be_snapshotted() {
        let t = Temp::new();
        let (root, source, _) = legacy_fixture(&t, &["stable-old"]);
        fs::remove_file(&source).unwrap();
        let library = Library::load(&root).unwrap();
        let old = library.get("stable-old").unwrap().clone();
        assert!(old.version.is_none());
        assert_eq!(old.path, source);
        assert!(
            registration_retry::load_legacy_migrations(&root)
                .unwrap()
                .is_empty()
        );
        fs::write(&source, b"source becomes available").unwrap();
        let library = Library::load(&root).unwrap();
        let recovered = library.get("stable-old").unwrap();
        assert!(recovered.version.is_some());
        assert_eq!(
            fs::read(&recovered.path).unwrap(),
            b"source becomes available"
        );
        assert_eq!(recovered.id, old.id);
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
