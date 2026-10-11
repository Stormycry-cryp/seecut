// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stable cache plans for every successful generation output.
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) key: String,
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
        .enumerate()
        .filter_map(|(index, output)| {
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
            // Preserve the legacy primary key/path used by preview and handoff.
            // Additional outputs use the server's immutable output ID, not an index.
            let key = if index == 0 {
                id.to_owned()
            } else {
                format!("{id}:{output_id}")
            };
            let destination = root.join(if index == 0 {
                format!("{id}.{extension}")
            } else {
                format!("{id}-{output_id}.{extension}")
            });
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
                url: url.to_owned(),
                path,
            })
        })
        .collect()
}

pub(crate) fn registered(items: &[Value], path: &Path) -> bool {
    items.iter().any(|item| {
        item["path"]
            .as_str()
            .is_some_and(|value| Path::new(value) == path)
    })
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
            ["gen_task", "gen_task:out_b", "gen_task:out_c"]
        );
        assert_eq!(outputs[1].path, Path::new("cache/gen_task-out_b.jpg"));
        assert_eq!(outputs[2].url, "/c");
        let mut reordered = task();
        reordered["outputs"].as_array_mut().unwrap().swap(1, 2);
        let again = plan(&reordered, Path::new("cache"), &HashMap::new());
        assert_eq!(again[2].key, outputs[1].key);
        assert_eq!(again[2].path, outputs[1].path);
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
}
