// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stable cache plans for every successful generation output.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Output {
    pub(crate) key: String,
    pub(crate) task_id: String,
    pub(crate) sha256: Option<String>,
    pub(crate) legacy: Option<PathBuf>,
    pub(crate) url: String,
    pub(crate) path: PathBuf,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn existing_file(path: &Path, root: &Path) -> Option<PathBuf> {
    path.is_file()
        .then(|| path.canonicalize().ok())
        .flatten()
        .filter(|path| path.starts_with(root))
}

pub(crate) fn plan(task: &Value, folder: &Path, local: &HashMap<String, String>) -> Vec<Output> {
    let id = task["id"].as_str().unwrap_or_default();
    if task["status"] != "succeeded" || !id.starts_with("gen_") || !valid_id(id) {
        return Vec::new();
    }
    let root = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let mut seen = HashSet::new();
    task["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|output| {
            let output_id = output["id"].as_str().unwrap_or_default();
            let url = output["download_url"]
                .as_str()
                .filter(|url| !url.is_empty())?;
            if !valid_id(output_id) || !seen.insert(output_id.to_owned()) {
                return None;
            }
            let extension = match output["content_type"].as_str().unwrap_or_default() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                "video/mp4" => "mp4",
                "video/webm" => "webm",
                "" | "application/octet-stream" => match task["kind"].as_str()? {
                    "image" => "png",
                    "video" => "mp4",
                    _ => return None,
                },
                _ => return None,
            };
            // Every output, including the first, has the same immutable identity rule.
            let key = format!("{id}:{output_id}");
            let destination = root.join(format!("{id}-{output_id}.{extension}"));
            // Never register an existing destination that resolves outside the cache.
            if destination.exists() && existing_file(&destination, &root).is_none() {
                return None;
            }
            let path = local
                .get(&key)
                .and_then(|path| existing_file(Path::new(path), &root))
                .or_else(|| existing_file(&destination, &root))
                .unwrap_or(destination);
            Some(Output {
                key,
                task_id: id.to_owned(),
                sha256: output["sha256"]
                    .as_str()
                    .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(|hash| hash.to_ascii_lowercase()),
                legacy: local
                    .get(id)
                    .and_then(|path| existing_file(Path::new(path), &root)),
                url: url.to_owned(),
                path,
            })
        })
        .collect()
}

/// Scope new caches while retaining explicitly bound legacy files from this account's history.
pub(crate) fn scoped_plan(
    task: &Value,
    folder: &Path,
    scope: &str,
    local: &HashMap<String, String>,
) -> Vec<Output> {
    let legacy_root = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let mut outputs = plan(task, &folder.join("scopes").join(scope), local);
    for output in &mut outputs {
        output.legacy = local
            .get(&output.key)
            .or_else(|| local.get(&output.task_id))
            .and_then(|p| Path::new(p).canonicalize().ok())
            .filter(|p| p.is_file() && p.parent() == Some(legacy_root.as_path()));
    }
    outputs
}

pub(crate) fn registered(items: &[Value], path: &Path) -> bool {
    items.iter().any(|item| {
        item["path"]
            .as_str()
            .is_some_and(|value| Path::new(value) == path)
            || (item["original_path"]
                .as_str()
                .is_some_and(|value| Path::new(value) == path)
                && item["path"]
                    .as_str()
                    .is_some_and(|value| Path::new(value).is_file()))
    })
}

fn matches_digest(path: &Path, expected: &str) -> Result<bool, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()) == expected)
}

/// Called on the existing download worker, so migration hashing never blocks the UI thread.
pub(crate) fn prepare(
    output: &Output,
    download: impl FnOnce(&Path) -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    if output.path.is_file() {
        if let Some(expected) = &output.sha256
            && !matches_digest(&output.path, expected)?
        {
            return Err("缓存内容与输出身份不一致，已保留原文件".into());
        }
        return Ok(output.path.clone());
    }
    if let (Some(legacy), Some(expected)) = (&output.legacy, &output.sha256)
        && legacy.is_file()
        && matches_digest(legacy, expected)?
    {
        return Ok(legacy.clone());
    }
    let path = download(&output.path)?;
    if let Some(expected) = &output.sha256
        && !matches_digest(&path, expected)?
    {
        // This path was just created for this immutable output ID, never the legacy primary.
        let _ = std::fs::remove_file(&path);
        return Err("下载内容校验失败，请重试".into());
    }
    Ok(path)
}

/// Persist the immutable output mapping in the existing account history. Keep the old task preview alias.
pub(crate) fn record(local: &mut HashMap<String, String>, key: &str, task: &str, path: &str) {
    local.insert(key.to_owned(), path.to_owned());
    if !task.is_empty() && !local.get(task).is_some_and(|p| Path::new(p).is_file()) {
        local.insert(task.to_owned(), path.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task() -> Value {
        json!({"id":"gen_task","status":"succeeded","kind":"image","outputs":[
            {"id":"out_a","content_type":"image/png","download_url":"/a"},
            {"id":"out_b","content_type":"image/jpeg","download_url":"/b"},
            {"id":"out_c","content_type":"image/webp","download_url":"/c"}]})
    }

    #[test]
    fn every_output_has_its_own_stable_cache_route() {
        let outputs = plan(&task(), Path::new("cache"), &HashMap::new());
        assert_eq!(outputs.len(), 3);
        assert_eq!(
            outputs.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            ["gen_task:out_a", "gen_task:out_b", "gen_task:out_c"]
        );
        assert_eq!(outputs[1].path, Path::new("cache/gen_task-out_b.jpg"));
        assert_eq!(outputs[2].url, "/c");
        let mut reordered = task();
        reordered["outputs"].as_array_mut().unwrap().swap(0, 1);
        let again = plan(&reordered, Path::new("cache"), &HashMap::new());
        assert_eq!(again[0].key, outputs[1].key);
        assert_eq!(again[0].path, outputs[1].path);
    }

    #[test]
    fn malformed_ids_cannot_escape_cache_or_repeat_an_output() {
        let mut value = task();
        value["outputs"][1]["id"] = json!("../../foreign");
        value["outputs"][2]["id"] = json!("out_a");
        assert_eq!(plan(&value, Path::new("cache"), &HashMap::new()).len(), 1);
        value["id"] = json!("gen_../../foreign");
        assert!(plan(&value, Path::new("cache"), &HashMap::new()).is_empty());
    }

    #[test]
    fn incomplete_tasks_and_unavailable_urls_do_not_create_cache_jobs() {
        let mut value = task();
        value["status"] = json!("running");
        assert!(plan(&value, Path::new("cache"), &HashMap::new()).is_empty());
        value["status"] = json!("succeeded");
        value["outputs"][0]["download_url"] = json!("");
        let outputs = plan(&value, Path::new("cache"), &HashMap::new());
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].key, "gen_task:out_b");
    }

    #[test]
    fn recycled_assets_are_already_registered_and_are_not_restored_by_polling() {
        let items = vec![json!({"path":"cache/gen_task.png","trashed":true,"favorite":true})];
        assert!(registered(&items, Path::new("cache/gen_task.png")));
        assert!(!registered(&items, Path::new("cache/gen_other.png")));
    }
    #[test]
    fn managed_registration_requires_available_snapshot() {
        let folder = std::env::temp_dir().join(format!("registered-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("managed.png");
        std::fs::write(&path, b"snapshot").unwrap();
        let items = vec![json!({"path":path,"original_path":"cache/output.png","trashed":true})];
        assert!(registered(&items, Path::new("cache/output.png")));
        std::fs::remove_file(&path).unwrap();
        assert!(!registered(&items, Path::new("cache/output.png")));
        std::fs::remove_dir_all(folder).unwrap();
    }
}
