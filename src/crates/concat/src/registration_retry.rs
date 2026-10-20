// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable registration of immutable output snapshots. Callers hold the library write lock.
use crate::personal_library::{Library, PreparedGenerated, prepare_generated};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;
const PENDING: &str = "pending-generated.json";
const LEGACY_PENDING: &str = "pending-legacy-generated.json";

/// Shared result receipt, referencing the existing library instead of creating another identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GenerationIdentity {
    pub(crate) scope: String,
    pub(crate) task: String,
    pub(crate) output: String,
}
impl GenerationIdentity {
    fn operation(&self) -> String {
        format!("generation:{}:{}:{}", self.scope, self.task, self.output)
    }
    fn filename(&self) -> String {
        format!("{}.json", seecut_flow_core::digest(self))
    }
}
#[derive(Serialize, Deserialize)]
struct GenerationReceipt {
    format: u32,
    identity: GenerationIdentity,
    reference: crate::personal_library::AssetReference,
}
fn generation_claim(
    root: &Path,
    asset: &crate::personal_library::Asset,
) -> Result<Option<GenerationIdentity>, String> {
    let reference = crate::personal_library::AssetReference {
        asset_id: asset.id.clone(),
        version: asset.version.clone(),
    };
    let path = root
        .join("generation-claims")
        .join(format!("{}.json", seecut_flow_core::digest(&reference)));
    match fs::read(path) {
        Ok(bytes) => {
            let receipt: GenerationReceipt =
                serde_json::from_slice(&bytes).map_err(|e| format!("输出归属已损坏: {e}"))?;
            if receipt.format != 1 || receipt.reference != reference {
                return Err("输出归属版本不匹配".into());
            }
            Ok(Some(receipt.identity))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("无法读取输出归属: {e}")),
    }
}
pub(crate) fn generation_asset(
    root: &Path,
    identity: &GenerationIdentity,
    sha: &str,
) -> Result<Option<crate::personal_library::Asset>, String> {
    let library = Library::load(root)?;
    let file = root.join("generation-receipts").join(identity.filename());
    match fs::read(file) {
        Ok(bytes) => {
            let receipt: GenerationReceipt =
                serde_json::from_slice(&bytes).map_err(|e| format!("输出收据已损坏: {e}"))?;
            if receipt.format != 1 || receipt.identity != *identity {
                return Err("输出收据身份不匹配".into());
            }
            let asset = library
                .get(&receipt.reference.asset_id)
                .filter(|a| {
                    a.version == receipt.reference.version
                        && a.version.as_ref().is_some_and(|v| v.sha256 == sha)
                })
                .ok_or("输出收据版本已变化")?;
            crate::personal_library::checked_managed_version_path(root, asset)?;
            Ok(Some(asset.clone()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            for asset in library
                .items()
                .iter()
                .filter(|a| a.version.as_ref().is_some_and(|v| v.sha256 == sha))
            {
                if asset.version.as_ref().is_some_and(|v| {
                    v.registration_id == format!("operation:{}", identity.operation())
                }) || generation_claim(root, asset)?.as_ref() == Some(identity)
                {
                    crate::personal_library::checked_managed_version_path(root, asset)?;
                    return Ok(Some(asset.clone()));
                }
            }
            Ok(None)
        }
        Err(e) => Err(format!("无法读取输出收据: {e}")),
    }
}
/// Both the independent generator and Flow call this under the existing library lock.
pub(crate) fn register_generation_output(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    identity: &GenerationIdentity,
    sha: &str,
    provenance: Option<crate::personal_library::OutputProvenance>,
) -> Result<crate::personal_library::Asset, String> {
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("输出缺少可信摘要".into());
    }
    let mut asset = generation_asset(root, identity, sha)?;
    if asset.is_none() {
        let reference = crate::personal_library::AssetReference {
            asset_id: String::new(),
            version: Some(crate::personal_library::AssetVersion {
                sha256: sha.into(),
                registration_id: String::new(),
                previous_id: None,
            }),
        };
        crate::personal_library::verify_asset_reference(path, Some(&reference))?;
        let library = Library::load(root)?;
        // Adopt a previous generator's exact verified cache registration, including trash state.
        for previous in library.items().iter().filter(|a| {
            a.source == crate::personal_library::AssetSource::Generated
                && a.original_path.as_deref() == Some(path)
                && a.version.as_ref().is_some_and(|v| {
                    v.sha256 == sha && !v.registration_id.starts_with("operation:generation:")
                })
        }) {
            if generation_claim(root, previous)?.is_none_or(|claim| claim == *identity) {
                asset = Some(previous.clone());
                break;
            }
        }
        if asset.is_none() {
            register_output_with_provenance(
                root,
                path,
                name,
                Some(&identity.operation()),
                None,
                provenance.clone(),
            )?;
            asset = generation_asset(root, identity, sha)?;
        }
    }
    let mut asset = asset.ok_or("输出入库收据未完成")?;
    crate::personal_library::checked_managed_version_path(root, &asset)?;
    let reference = crate::personal_library::AssetReference {
        asset_id: asset.id.clone(),
        version: asset.version.clone(),
    };
    if generation_claim(root, &asset)?.is_some_and(|claim| claim != *identity) {
        return Err("输出已归属其他任务或账户".into());
    }
    save_json(
        &root.join("generation-claims"),
        &format!("{}.json", seecut_flow_core::digest(&reference)),
        &GenerationReceipt {
            format: 1,
            identity: identity.clone(),
            reference,
        },
    )?;
    if let Some(provenance) = provenance {
        let mut library = Library::load(root)?;
        library.annotate_output(
            &asset.id,
            asset.version.as_ref().ok_or("输出缺少版本")?,
            provenance,
        )?;
        asset = library.get(&asset.id).ok_or("输出记录不可用")?.clone();
    }
    let receipt = GenerationReceipt {
        format: 1,
        identity: identity.clone(),
        reference: crate::personal_library::AssetReference {
            asset_id: asset.id.clone(),
            version: asset.version.clone(),
        },
    };
    save_json(
        &root.join("generation-receipts"),
        &identity.filename(),
        &receipt,
    )?;
    Ok(asset)
}

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
    let entries = load(root)?;
    let mut prepared = prepare_generated(root, path, name, operation, bytes)?;
    prepared.provenance = provenance;
    register_prepared_output(root, name, &prepared, entries)
}

/// The shared durable path for both ordinary and fully verified outputs.
/// Callers finish any extra content checks before entering this function.
fn register_prepared_output(
    root: &Path,
    name: Option<&str>,
    prepared: &PreparedGenerated,
    mut entries: Vec<Entry>,
) -> Result<(), String> {
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
        library.commit_generated(prepared)?;
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

/// Registers the bytes of an actual fully decoded native video output.
/// The caller supplies its captured original identity and holds the library
/// write lock. This does not acquire that lock again or authorize any caller.
/// An error means this call did not establish a verified managed reference;
/// it does not imply that the original operation has no registered asset.
pub(crate) fn register_verified_video_output(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: &str,
    proof: &concat_host::export_verification::VerifiedOutput,
    provenance: Option<crate::personal_library::OutputProvenance>,
    cancel: &AtomicBool,
) -> Result<crate::personal_library::AssetReference, String> {
    register_verified_video_output_with_commit_check(
        root,
        path,
        name,
        operation,
        proof,
        provenance,
        cancel,
        || Ok(()),
    )
}

/// 在实际受管副本核验完成后、任何登记写入前执行本次授权提交检查。
pub(crate) fn register_verified_video_output_with_commit_check(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: &str,
    proof: &concat_host::export_verification::VerifiedOutput,
    provenance: Option<crate::personal_library::OutputProvenance>,
    cancel: &AtomicBool,
    before_commit: impl FnOnce() -> Result<(), String>,
) -> Result<crate::personal_library::AssetReference, String> {
    register_verified_video_with_checkpoint_and_commit_check(
        root,
        path,
        name,
        operation,
        proof,
        provenance,
        cancel,
        &mut |_, _| {},
        before_commit,
    )
}

// Synchronous per-call test boundaries; no global hooks or background races.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VideoRegistrationCheckpoint {
    SourceRechecked,
    Prepared,
    Registered,
    ManagedPathChecked,
}

fn video_registration_not_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("视频登记核验已取消；请查询原操作状态".into());
    }
    Ok(())
}

#[cfg(test)]
fn register_verified_video_with_checkpoint(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: &str,
    proof: &concat_host::export_verification::VerifiedOutput,
    provenance: Option<crate::personal_library::OutputProvenance>,
    cancel: &AtomicBool,
    checkpoint: &mut impl FnMut(VideoRegistrationCheckpoint, &Path),
) -> Result<crate::personal_library::AssetReference, String> {
    register_verified_video_with_checkpoint_and_commit_check(
        root,
        path,
        name,
        operation,
        proof,
        provenance,
        cancel,
        checkpoint,
        || Ok(()),
    )
}

fn register_verified_video_with_checkpoint_and_commit_check(
    root: &Path,
    path: &Path,
    name: Option<&str>,
    operation: &str,
    proof: &concat_host::export_verification::VerifiedOutput,
    provenance: Option<crate::personal_library::OutputProvenance>,
    cancel: &AtomicBool,
    checkpoint: &mut impl FnMut(VideoRegistrationCheckpoint, &Path),
    before_commit: impl FnOnce() -> Result<(), String>,
) -> Result<crate::personal_library::AssetReference, String> {
    if operation.is_empty() {
        return Err("视频登记缺少原操作身份".into());
    }
    video_registration_not_cancelled(cancel)?;
    proof.recheck_same_content(path, cancel)?;
    checkpoint(VideoRegistrationCheckpoint::SourceRechecked, path);
    video_registration_not_cancelled(cancel)?;

    let entries = load(root)?;
    // The existing path streams the source into managed storage. Its returned
    // SHA describes the actual copy, not the earlier source-path observation.
    let mut prepared = prepare_generated(root, path, name, Some(operation), None)?;
    prepared.provenance = provenance;
    checkpoint(VideoRegistrationCheckpoint::Prepared, &prepared.path);
    video_registration_not_cancelled(cancel)?;
    let summary = proof.summary();
    let metadata = fs::symlink_metadata(&prepared.path).map_err(|error| error.to_string())?;
    if prepared.sha256 != summary.sha256
        || !metadata.is_file()
        || metadata.len() != summary.bytes
        || prepared.kind != crate::personal_library::AssetKind::Video
    {
        // prepare_generated may reuse a content-addressed file. It is unsafe
        // to delete it here; another operation may already own that file.
        return Err("视频入库副本与完整解码核验结果不一致；本次未写入待登记记录或索引".into());
    }
    // The copied file may change after prepare_generated calculated its SHA.
    // Reopen and hash those actual bytes before recording any recovery intent.
    proof.recheck_same_content(&prepared.path, cancel)?;
    video_registration_not_cancelled(cancel)?;
    before_commit()?;
    register_prepared_output(root, name, &prepared, entries)?;

    // From here on registration may already be durable, including a replay.
    // Preserve that fact even when cancellation or the managed check fails.
    checkpoint(VideoRegistrationCheckpoint::Registered, &prepared.path);
    let checked = (|| {
        video_registration_not_cancelled(cancel)?;
        let reference = registered_operation(root, &prepared.registration_id, &summary.sha256)?
            .ok_or("原视频登记没有可用的固定素材版本")?;
        let version = reference.version.as_ref().ok_or("视频登记未返回固定版本")?;
        if version.registration_id != prepared.registration_id || version.sha256 != summary.sha256 {
            return Err("原视频登记版本与核验结果不一致".into());
        }
        let library = Library::load_for_relink(root)?;
        let asset = library
            .get(&reference.asset_id)
            .filter(|asset| !asset.trashed && asset.version.as_ref() == Some(version))
            .ok_or("原视频素材已移除或固定版本已变化")?;
        let managed = crate::personal_library::checked_managed_version_path(root, asset)?;
        checkpoint(VideoRegistrationCheckpoint::ManagedPathChecked, &managed);
        proof.recheck_same_content(&managed, cancel)?;
        Ok(reference)
    })();
    checked.map_err(|error: String| {
        format!("已执行原视频登记，但本次未取得受管副本核验成功；请查询原操作状态：{error}")
    })
}

/// Canvas repeats reuse a live account/content version. Each explicit export
/// keeps its own registration identity so a trashed version stays trashed.
/// Use the frozen owned PNG bytes, never re-read an external file.
pub(crate) fn register_canvas_output(
    root: &Path,
    path: &Path,
    name: &str,
    scope: &str,
    export_id: &str,
    bytes: &[u8],
    provenance: crate::personal_library::OutputProvenance,
) -> Result<crate::personal_library::AssetReference, String> {
    use sha2::{Digest, Sha256};
    let sha = format!("{:x}", Sha256::digest(bytes));
    let content_operation = format!("canvas:{scope}:{sha}");
    let operation = format!("{content_operation}:export:{export_id}");
    let expected = format!("operation:{operation}");
    let legacy = format!("operation:{content_operation}");
    let family = format!("{legacy}:export:");
    let library = Library::load(root)?;
    if let Some(asset) = library.items().iter().rev().find(|asset| {
        !asset.trashed
            && asset.version.as_ref().is_some_and(|v| {
                v.sha256 == sha
                    && (v.registration_id == legacy || v.registration_id.starts_with(&family))
            })
    }) {
        crate::personal_library::checked_managed_version_path(root, asset)?;
        return Ok(crate::personal_library::AssetReference {
            asset_id: asset.id.clone(),
            version: asset.version.clone(),
        });
    }
    // Replaying a deleted operation must not resurrect it. A new explicit
    // export has a fresh export_id and may create a new live version.
    if library.items().iter().any(|asset| {
        asset.trashed
            && asset
                .version
                .as_ref()
                .is_some_and(|v| v.registration_id == expected)
    }) {
        return Err("该导出已被移入回收站；请重新导出".into());
    }
    drop(library);
    register_output_with_provenance(
        root,
        path,
        Some(name),
        Some(&operation),
        Some(bytes),
        Some(provenance),
    )?;
    registered_operation(root, &expected, &sha)?.ok_or_else(|| "画布导出登记回执不可用".into())
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

/// Retry one original prepared output only. No migration of other records,
/// source decoding, composition or export is performed.
pub(crate) fn retry_operation(
    root: &Path,
    registration_id: &str,
    sha256: &str,
) -> Result<crate::personal_library::AssetReference, String> {
    let expected = format!("operation:{registration_id}");
    if let Some(asset) = registered_operation(root, &expected, sha256)? {
        return Ok(asset);
    }
    let mut entries = load(root)?;
    let prepared = entries
        .iter()
        .filter_map(|entry| entry.prepared.as_ref())
        .find(|p| p.registration_id == expected && p.sha256 == sha256)
        .ok_or("原输出的冻结登记记录不可用")?
        .clone();
    let mut library = Library::load(root)?;
    library.commit_generated(&prepared)?;
    entries.retain(|entry| {
        entry
            .prepared
            .as_ref()
            .is_none_or(|p| p.registration_id != expected)
    });
    let _ = save(root, &entries);
    registered_operation(root, &expected, sha256)?.ok_or_else(|| "原输出登记回执不可用".into())
}

pub(crate) fn registered_operation(
    root: &Path,
    registration_id: &str,
    sha256: &str,
) -> Result<Option<crate::personal_library::AssetReference>, String> {
    // A failed first registration can have a durable prepared record before
    // the first library index exists. Absence proves no registered receipt;
    // corrupt or inaccessible existing indices must still fail closed.
    match fs::symlink_metadata(root.join(crate::personal_library::MANIFEST_NAME)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取个人资产库索引：{error}")),
        Ok(_) => {}
    }
    let library = Library::load_for_relink(root)?;
    let Some(asset) = library.items().iter().find(|asset| {
        !asset.trashed
            && asset
                .version
                .as_ref()
                .is_some_and(|v| v.registration_id == registration_id && v.sha256 == sha256)
    }) else {
        return Ok(None);
    };
    crate::personal_library::checked_managed_version_path(root, asset)?;
    Ok(Some(crate::personal_library::AssetReference {
        asset_id: asset.id.clone(),
        version: asset.version.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use crate::{
        personal_library::{Library, fail_next_manifest_save, prepare_generated},
        registration_retry,
    };
    use serde_json::json;
    use std::{fs, path::PathBuf};

    #[test]
    fn f3_canvas_content_registration_reuses_identity_and_keeps_scopes_and_old_assets() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("canvas.png");
        fs::write(&source, b"external file is not the owned snapshot").unwrap();
        let provenance = crate::personal_library::OutputProvenance {
            kind: "canvas_export".into(),
            project_path: None,
            document_sha256: "synthetic".into(),
            inputs: vec![],
            parameters: json!({}),
        };
        let register = |scope, bytes: &[u8]| {
            registration_retry::register_canvas_output(
                &root,
                &source,
                "canvas",
                scope,
                "test-export",
                bytes,
                provenance.clone(),
            )
            .unwrap()
        };
        let first = register("account-a", b"first owned PNG");
        assert_eq!(first, register("account-a", b"first owned PNG"));
        let other_account = register("account-b", b"first owned PNG");
        assert_ne!(first.asset_id, other_account.asset_id);
        let changed = register("account-a", b"changed owned PNG");
        assert_ne!(first.asset_id, changed.asset_id);
        let library = Library::load(&root).unwrap();
        assert_eq!(library.items().len(), 3);
        assert_eq!(
            fs::read(&library.get(&first.asset_id).unwrap().path).unwrap(),
            b"first owned PNG"
        );
        assert_eq!(
            fs::read(&library.get(&changed.asset_id).unwrap().path).unwrap(),
            b"changed owned PNG"
        );
        drop(library);
        assert_eq!(first, register("account-a", b"first owned PNG"));
        assert_eq!(Library::load(&root).unwrap().items().len(), 3);
    }

    #[test]
    fn f3_canvas_reexport_after_trash_keeps_deleted_record_and_retries_new_identity() {
        let t = Temp::new();
        let root = t.0.join("library");
        let source = t.0.join("canvas.png");
        let bytes = b"owned canvas PNG";
        fs::write(&source, bytes).unwrap();
        let provenance = crate::personal_library::OutputProvenance {
            kind: "canvas_export".into(),
            project_path: None,
            document_sha256: "synthetic".into(),
            inputs: vec![],
            parameters: json!({}),
        };
        let register = |id| {
            registration_retry::register_canvas_output(
                &root,
                &source,
                "canvas",
                "account-a",
                id,
                bytes,
                provenance.clone(),
            )
        };
        use sha2::{Digest, Sha256};
        let sha = format!("{:x}", Sha256::digest(bytes));
        let legacy_operation = format!("canvas:account-a:{sha}");
        registration_retry::register_output_with_provenance(
            &root,
            &source,
            Some("canvas"),
            Some(&legacy_operation),
            Some(bytes),
            Some(provenance.clone()),
        )
        .unwrap();
        let first = register("first").unwrap();
        assert_eq!(
            first.version.as_ref().unwrap().registration_id,
            format!("operation:{legacy_operation}")
        );
        let mut library = Library::load(&root).unwrap();
        library.trash(&first.asset_id).unwrap();
        let deleted = library.get(&first.asset_id).unwrap().clone();
        drop(library);
        fail_next_manifest_save(&root);
        assert!(register("second").is_err());
        let prepared = super::load(&root).unwrap().pop().unwrap().prepared.unwrap();
        let operation = prepared.registration_id.strip_prefix("operation:").unwrap();
        let second =
            registration_retry::retry_operation(&root, operation, &prepared.sha256).unwrap();
        assert_ne!(first.asset_id, second.asset_id);
        assert_eq!(
            second,
            registration_retry::retry_operation(&root, operation, &prepared.sha256).unwrap()
        );
        assert_eq!(second, register("third").unwrap());
        assert_eq!(registration_retry::retry(&root).unwrap().remaining, 0);
        let library = Library::load(&root).unwrap();
        assert_eq!(library.items().len(), 2);
        let old = library.get(&first.asset_id).unwrap();
        assert!(old.trashed);
        assert_eq!(old.version, deleted.version);
        assert_eq!(old.path, deleted.path);
        assert_eq!(fs::read(&old.path).unwrap(), bytes);
        assert!(!library.get(&second.asset_id).unwrap().trashed);
        drop(library);
        let mut library = Library::load(&root).unwrap();
        library.trash(&second.asset_id).unwrap();
        drop(library);
        assert!(
            register("second").is_err(),
            "retry cannot restore a deleted export"
        );
        let third = register("fourth").unwrap();
        assert_ne!(third.asset_id, second.asset_id);
        assert_eq!(third, register("fifth").unwrap());
        let library = Library::load(&root).unwrap();
        assert_eq!(library.items().len(), 3);
        assert_eq!(library.items().iter().filter(|a| a.trashed).count(), 2);
    }

    #[test]
    fn mcp_canvas_registration_retry_uses_only_one_original_frozen_snapshot() {
        use sha2::{Digest, Sha256};
        let temp = Temp::new();
        let root = temp.0.join("library");
        let source = temp.0.join("original.png");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 0, 255])
                .unwrap();
        }
        fs::write(&source, &bytes).unwrap();
        Library::load(&root).unwrap();
        fail_next_manifest_save(&root);
        assert!(
            registration_retry::register_output(
                &root,
                &source,
                Some("Original"),
                Some("one"),
                Some(&bytes)
            )
            .is_err()
        );
        fail_next_manifest_save(&root);
        assert!(
            registration_retry::register_output(
                &root,
                &source,
                Some("Other"),
                Some("two"),
                Some(&bytes)
            )
            .is_err()
        );
        assert_eq!(registration_retry::load(&root).unwrap().len(), 2);
        fs::write(&source, b"source changed after freezing").unwrap();
        let changed = fs::read(&source).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let first = registration_retry::retry_operation(&root, "one", &sha).unwrap();
        assert_eq!(fs::read(&source).unwrap(), changed);
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
        assert_eq!(first.version.as_ref().unwrap().sha256, sha);
        assert_eq!(Library::load_for_relink(&root).unwrap().items().len(), 1);
        assert_eq!(registration_retry::load(&root).unwrap().len(), 1);
        assert_eq!(
            registration_retry::retry_operation(&root, "one", &sha).unwrap(),
            first
        );
        assert_eq!(Library::load_for_relink(&root).unwrap().items().len(), 1);
        assert!(registration_retry::retry_operation(&root, "two", &"b".repeat(64)).is_err());
        assert_eq!(registration_retry::load(&root).unwrap().len(), 1);
    }
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

    mod verified_video {
        use super::Temp;
        use crate::personal_library::{AssetKind, OutputProvenance, with_library_write_lock};
        use crate::registration_retry::{self, VideoRegistrationCheckpoint as Checkpoint};
        use concat_core::{frame::Frame, time::FrameRate};
        use concat_host::export_verification::{
            OutputExpectation, VerificationLimits, VerifiedOutput, verify_output,
        };
        use concat_media::{EncodeOptions, Encoder, FrameSink, VideoCodec};
        use concat_project::model::VideoSettings;
        use std::{
            fs,
            path::Path,
            sync::atomic::{AtomicBool, Ordering},
        };

        fn movie(path: &Path, tint: u8) {
            let mut encoder = Encoder::create(
                path,
                64,
                64,
                FrameRate::THIRTY,
                &EncodeOptions {
                    codec: VideoCodec::H264,
                    hardware: false,
                    preset: "ultrafast".into(),
                    crf: 20,
                    ..Default::default()
                },
            )
            .expect("required real software H264 encoder must work; never skip");
            for n in 0..6 {
                let mut frame = Frame::black(64, 64);
                frame.fill([tint, 40 + n * 10, 160, 255]);
                encoder.write_frame(&frame).unwrap();
            }
            encoder.finish().unwrap();
        }

        fn proof(path: &Path) -> VerifiedOutput {
            verify_output(
                path,
                OutputExpectation {
                    video: VideoSettings {
                        width: 64,
                        height: 64,
                        rate_num: 30,
                        rate_den: 1,
                    },
                    frames: 6,
                    codec: VideoCodec::H264,
                    ten_bit: false,
                    audio_expected: false,
                },
                VerificationLimits::default(),
                &AtomicBool::new(false),
            )
            .expect("the actual file must pass complete independent decoding")
        }

        fn provenance() -> OutputProvenance {
            OutputProvenance {
                kind: "clip_export".into(),
                project_path: None,
                document_sha256: "test-frozen-document".into(),
                inputs: vec![],
                parameters: serde_json::json!({"width": 64, "height": 64, "frames": 6}),
            }
        }

        fn register(
            root: &Path,
            source: &Path,
            operation: &str,
            verified: &VerifiedOutput,
        ) -> Result<crate::personal_library::AssetReference, String> {
            registration_retry::register_verified_video_output(
                root,
                source,
                Some("verified video"),
                operation,
                verified,
                Some(provenance()),
                &AtomicBool::new(false),
            )
        }

        fn state(root: &Path) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
            (
                fs::read(root.join(crate::personal_library::MANIFEST_NAME)).ok(),
                fs::read(root.join(registration_retry::PENDING)).ok(),
            )
        }

        // Test fixtures are tiny. Production code never reads a whole MP4
        // into memory; this exact-size mutation isolates the SHA boundary.
        fn change_same_length(path: &Path) {
            let mut bytes = fs::read(path).unwrap();
            assert!(!bytes.is_empty());
            let last = bytes.len() - 1;
            bytes[last] ^= 0x80;
            fs::write(path, bytes).unwrap();
        }

        #[test]
        fn real_h264_registration_returns_fixed_independently_verified_managed_copy() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 30);
                let verified = proof(&source);
                assert_eq!(verified.summary().video_frames, 6);
                let reference = register(&root, &source, "original-export", &verified).unwrap();
                let version = reference.version.as_ref().unwrap();
                assert_eq!(version.registration_id, "operation:original-export");
                assert_eq!(version.sha256, verified.summary().sha256);
                let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 1);
                let asset = library.get(&reference.asset_id).unwrap();
                assert!(!asset.trashed);
                assert_eq!(asset.kind, AssetKind::Video);
                assert_eq!(asset.version, reference.version);
                assert_eq!(asset.provenance, Some(provenance()));
                let managed =
                    crate::personal_library::checked_managed_version_path(&root, asset).unwrap();
                assert_ne!(managed, source.canonicalize().unwrap());
                assert_eq!(proof(&managed).summary().sha256, verified.summary().sha256);
                assert!(registration_retry::load(&root).unwrap().is_empty());
                assert_eq!(
                    registration_retry::registered_operation(
                        &root,
                        "operation:original-export",
                        &verified.summary().sha256,
                    )
                    .unwrap(),
                    Some(reference),
                );
            });
        }

        #[test]
        fn same_operation_reuses_identity_and_does_not_restore_trashed_asset() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 40);
                let verified = proof(&source);
                let reference = register(&root, &source, "repeat", &verified).unwrap();
                assert_eq!(
                    register(&root, &source, "repeat", &verified).unwrap(),
                    reference
                );
                let mut library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 1);
                library.trash(&reference.asset_id).unwrap();
                let manifest = state(&root).0;
                let error = register(&root, &source, "repeat", &verified).unwrap_err();
                assert!(error.contains("本次未取得受管副本核验成功"), "{error}");
                let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 1);
                assert!(library.get(&reference.asset_id).unwrap().trashed);
                assert_eq!(state(&root).0, manifest);
            });
        }

        #[test]
        fn source_changed_after_full_decode_is_rejected_before_any_registration() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 50);
                let verified = proof(&source);
                change_same_length(&source);
                let error = register(&root, &source, "changed", &verified).unwrap_err();
                assert!(error.contains("output content differs"), "{error}");
                assert!(!root.exists());
                assert!(source.exists());
            });
        }

        #[test]
        fn copied_sha_is_checked_after_source_recheck_before_journal_or_commit() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 60);
                let verified = proof(&source);
                let existing = register(&root, &source, "existing", &verified).unwrap();
                let mut pending = crate::personal_library::prepare_generated(
                    &root,
                    &source,
                    None,
                    Some("keep-pending"),
                    None,
                )
                .unwrap();
                pending.provenance = Some(provenance());
                registration_retry::save(
                    &root,
                    &[registration_retry::Entry {
                        path: pending.source_path.clone(),
                        name: None,
                        prepared: Some(pending),
                    }],
                )
                .unwrap();
                let before = state(&root);
                let mut prepared_copy = None;
                let error = registration_retry::register_verified_video_with_checkpoint(
                    &root,
                    &source,
                    None,
                    "changed-after-recheck",
                    &verified,
                    Some(provenance()),
                    &AtomicBool::new(false),
                    &mut |point, path| match point {
                        Checkpoint::SourceRechecked => change_same_length(path),
                        Checkpoint::Prepared => prepared_copy = Some(path.to_path_buf()),
                        _ => panic!("unverified bytes must never reach registration"),
                    },
                )
                .unwrap_err();
                assert!(
                    error.contains("视频入库副本与完整解码核验结果不一致"),
                    "{error}"
                );
                let prepared_copy = prepared_copy.expect("must exercise the actual copy boundary");
                assert_eq!(
                    fs::metadata(&prepared_copy).unwrap().len(),
                    verified.summary().bytes
                );
                assert_ne!(
                    prepared_copy.file_stem().unwrap().to_str().unwrap(),
                    verified.summary().sha256,
                );
                assert_eq!(
                    state(&root),
                    before,
                    "both original durable records must be unchanged"
                );
                assert_eq!(
                    registration_retry::registered_operation(
                        &root,
                        "operation:existing",
                        &verified.summary().sha256,
                    )
                    .unwrap(),
                    Some(existing),
                );
                assert!(
                    prepared_copy.exists(),
                    "do not delete a potentially shared managed file"
                );
            });
        }

        #[test]
        fn changed_prepared_length_is_rejected_before_writing_pending_or_index() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 70);
                let verified = proof(&source);
                let mut saw_prepared = false;
                let error = registration_retry::register_verified_video_with_checkpoint(
                    &root,
                    &source,
                    None,
                    "prepared-length",
                    &verified,
                    None,
                    &AtomicBool::new(false),
                    &mut |point, path| {
                        if point == Checkpoint::Prepared {
                            saw_prepared = true;
                            fs::OpenOptions::new()
                                .write(true)
                                .open(path)
                                .unwrap()
                                .set_len(verified.summary().bytes + 1)
                                .unwrap();
                        }
                    },
                )
                .unwrap_err();
                assert!(saw_prepared);
                assert!(
                    error.contains("视频入库副本与完整解码核验结果不一致"),
                    "{error}"
                );
                assert_eq!(state(&root), (None, None));
            });
        }

        #[test]
        fn same_length_prepared_change_preserves_pending_index_and_shared_file() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 75);
                let verified = proof(&source);
                let existing = register(&root, &source, "existing-shared", &verified).unwrap();
                let mut pending = crate::personal_library::prepare_generated(
                    &root,
                    &source,
                    None,
                    Some("keep-pending"),
                    None,
                )
                .unwrap();
                pending.provenance = Some(provenance());
                registration_retry::save(
                    &root,
                    &[registration_retry::Entry {
                        path: pending.source_path.clone(),
                        name: None,
                        prepared: Some(pending),
                    }],
                )
                .unwrap();
                let before = state(&root);
                let mut changed = None;
                let error = registration_retry::register_verified_video_with_checkpoint(
                    &root,
                    &source,
                    None,
                    "changed-prepared",
                    &verified,
                    Some(provenance()),
                    &AtomicBool::new(false),
                    &mut |point, path| match point {
                        Checkpoint::SourceRechecked => {}
                        Checkpoint::Prepared => {
                            // This is the original verified SHA's shared file:
                            // the cached hash and the length both still match.
                            assert_eq!(
                                path.file_stem().unwrap().to_str().unwrap(),
                                verified.summary().sha256,
                            );
                            change_same_length(path);
                            assert_eq!(fs::metadata(path).unwrap().len(), verified.summary().bytes);
                            changed = Some(path.to_path_buf());
                        }
                        _ => panic!("changed managed bytes must never reach registration"),
                    },
                )
                .unwrap_err();
                assert!(error.contains("output content differs"), "{error}");
                let changed = changed.expect("must exercise a same-length prepared-file change");
                assert_eq!(state(&root), before);
                assert!(
                    changed.exists(),
                    "never delete the pre-existing shared managed file"
                );
                let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 1);
                let asset = library.get(&existing.asset_id).unwrap();
                assert_eq!(asset.path, changed);
                assert_eq!(asset.version, existing.version);
                assert!(!asset.trashed);
                // The fixture deliberately corrupts that shared file; the
                // unchanged source still matches the original decode proof.
                verified
                    .recheck_same_content(&source, &AtomicBool::new(false))
                    .unwrap();
            });
        }

        #[test]
        fn original_pending_identity_is_not_overwritten_by_different_verified_bytes() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 80);
                let original = proof(&source);
                let existing = register(&root, &source, "existing", &original).unwrap();
                crate::personal_library::fail_next_manifest_save(&root);
                assert!(register(&root, &source, "pending-operation", &original).is_err());
                let before = state(&root);
                let pending = registration_retry::load(&root).unwrap();
                assert_eq!(pending.len(), 1);
                assert_eq!(
                    pending[0].prepared.as_ref().unwrap().sha256,
                    original.summary().sha256
                );
                movie(&source, 130);
                let replacement = proof(&source);
                assert_ne!(replacement.summary().sha256, original.summary().sha256);
                let error =
                    register(&root, &source, "pending-operation", &replacement).unwrap_err();
                assert!(error.contains("同一待入库输出的内容已变化"), "{error}");
                assert_eq!(state(&root), before);
                let recovered = registration_retry::retry_operation(
                    &root,
                    "pending-operation",
                    &original.summary().sha256,
                )
                .unwrap();
                let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 2);
                assert!(library.get(&existing.asset_id).is_some());
                let managed = &library.get(&recovered.asset_id).unwrap().path;
                original
                    .recheck_same_content(managed, &AtomicBool::new(false))
                    .unwrap();
                assert_eq!(recovered.version.unwrap().sha256, original.summary().sha256);
            });
        }

        #[test]
        fn cancellation_before_registration_never_writes_pending_or_index() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let source = t.0.join("output.mp4");
                movie(&source, 90);
                let verified = proof(&source);
                for point in [
                    None,
                    Some(Checkpoint::SourceRechecked),
                    Some(Checkpoint::Prepared),
                ] {
                    let root = t.0.join(format!("cancel-{point:?}"));
                    let cancel = AtomicBool::new(point.is_none());
                    let mut saw_point = point.is_none();
                    let error = registration_retry::register_verified_video_with_checkpoint(
                        &root,
                        &source,
                        None,
                        "cancel-original",
                        &verified,
                        None,
                        &cancel,
                        &mut |current, _| {
                            if point == Some(current) {
                                saw_point = true;
                                cancel.store(true, Ordering::Release);
                            }
                        },
                    )
                    .unwrap_err();
                    assert!(saw_point);
                    assert!(error.contains("视频登记核验已取消"), "{error}");
                    assert_eq!(state(&root), (None, None));
                    assert!(source.exists());
                }
            });
        }

        #[test]
        fn cancellation_after_commit_preserves_original_receipt_and_retry_identity() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 100);
                let verified = proof(&source);
                let cancel = AtomicBool::new(false);
                let mut registered = false;
                let error = registration_retry::register_verified_video_with_checkpoint(
                    &root,
                    &source,
                    None,
                    "cancel-after-commit",
                    &verified,
                    Some(provenance()),
                    &cancel,
                    &mut |point, _| {
                        if point == Checkpoint::Registered {
                            registered = true;
                            cancel.store(true, Ordering::Release);
                        }
                    },
                )
                .unwrap_err();
                assert!(registered);
                assert!(error.contains("已执行原视频登记"), "{error}");
                assert!(error.contains("视频登记核验已取消"), "{error}");
                let reference = registration_retry::registered_operation(
                    &root,
                    "operation:cancel-after-commit",
                    &verified.summary().sha256,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    register(&root, &source, "cancel-after-commit", &verified).unwrap(),
                    reference
                );
                assert_eq!(
                    crate::personal_library::Library::load_for_relink(&root)
                        .unwrap()
                        .items()
                        .len(),
                    1
                );
            });
        }

        #[test]
        fn managed_change_after_path_check_fails_independent_proof_without_undoing_commit() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 110);
                let verified = proof(&source);
                let mut changed = None;
                let error = registration_retry::register_verified_video_with_checkpoint(
                    &root,
                    &source,
                    None,
                    "managed-change",
                    &verified,
                    Some(provenance()),
                    &AtomicBool::new(false),
                    &mut |point, path| {
                        if point == Checkpoint::ManagedPathChecked {
                            changed = Some(path.to_path_buf());
                            change_same_length(path);
                        }
                    },
                )
                .unwrap_err();
                assert!(error.contains("已执行原视频登记"), "{error}");
                assert!(error.contains("output content differs"), "{error}");
                let changed = changed.expect("must exercise the final independent proof check");
                assert!(changed.exists());
                let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 1);
                let asset = &library.items()[0];
                assert!(!asset.trashed);
                assert_eq!(asset.path, changed);
                let version = asset.version.as_ref().unwrap();
                assert_eq!(version.registration_id, "operation:managed-change");
                assert_eq!(version.sha256, verified.summary().sha256);
                assert!(registration_retry::load(&root).unwrap().is_empty());
                verified
                    .recheck_same_content(&source, &AtomicBool::new(false))
                    .unwrap();
            });
        }

        #[test]
        fn original_index_provenance_is_not_replaced_by_a_conflicting_replay() {
            with_library_write_lock(|| {
                let t = Temp::new();
                let root = t.0.join("library");
                let source = t.0.join("output.mp4");
                movie(&source, 120);
                let verified = proof(&source);
                let original = register(&root, &source, "original-provenance", &verified).unwrap();
                let before = state(&root).0;
                let mut changed = provenance();
                changed.document_sha256 = "another-document".into();
                let error = registration_retry::register_verified_video_output(
                    &root,
                    &source,
                    None,
                    "original-provenance",
                    &verified,
                    Some(changed),
                    &AtomicBool::new(false),
                )
                .unwrap_err();
                assert!(error.contains("同一输出的内容已变化"), "{error}");
                assert_eq!(state(&root).0, before);
                let library = crate::personal_library::Library::load_for_relink(&root).unwrap();
                assert_eq!(library.items().len(), 1);
                assert_eq!(
                    library.get(&original.asset_id).unwrap().provenance,
                    Some(provenance())
                );
            });
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
