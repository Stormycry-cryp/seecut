// SPDX-License-Identifier: AGPL-3.0-or-later
//! Load only the complete, verified runtime shipped in the application bundle.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

const INVALID: &str = "Agent 运行组件校验失败，请重新安装应用";
const MISSING: &str = "此应用尚未包含 Agent 运行组件";

pub(crate) struct Components {
    pub runtime: PathBuf,
    pub entrypoint: PathBuf,
    pub mcp_client: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    version: u32,
    runtime: String,
    entrypoint: String,
    mcp_client: String,
    files: Vec<FileDigest>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    path: String,
    sha256: String,
}

/// This optional override is supplied by the launching host for isolated QA.
/// It is never populated from a model, document, UI field or saved preference.
pub(crate) fn manifest_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SEECUT_AGENT_COMPONENTS") {
        let path = PathBuf::from(path);
        return path.is_absolute().then_some(path);
    }
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    #[cfg(target_os = "macos")]
    let path = directory.parent()?.join("Resources/agent/components.json");
    #[cfg(not(target_os = "macos"))]
    let path = directory.join("agent/components.json");
    Some(path)
}

fn relative_file(root: &Path, value: &str) -> Result<PathBuf, &'static str> {
    if value.is_empty() || value.len() > 1024 || value.contains('\\') {
        return Err(INVALID);
    }
    let mut path = root.to_owned();
    let parts: Vec<_> = Path::new(value).components().collect();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(part) = part else {
            return Err(INVALID);
        };
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(|_| INVALID)?;
        if metadata.file_type().is_symlink()
            || (index + 1 == parts.len() && !metadata.is_file())
            || (index + 1 != parts.len() && !metadata.is_dir())
        {
            return Err(INVALID);
        }
    }
    Ok(path)
}

pub(crate) fn load(manifest_path: &Path) -> Result<Components, &'static str> {
    let metadata = fs::symlink_metadata(manifest_path).map_err(|_| MISSING)?;
    if !manifest_path.is_absolute()
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 2 * 1024 * 1024
    {
        return Err(INVALID);
    }
    let parent = manifest_path.parent().ok_or(INVALID)?;
    // The bundle root itself must not be a symlink. Its host-owned ancestors
    // may include platform paths such as macOS /var -> /private/var.
    if fs::symlink_metadata(parent)
        .map_err(|_| INVALID)?
        .file_type()
        .is_symlink()
    {
        return Err(INVALID);
    }
    let root = parent.canonicalize().map_err(|_| INVALID)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path).map_err(|_| INVALID)?)
        .map_err(|_| INVALID)?;
    if manifest.version != 1 || manifest.files.is_empty() || manifest.files.len() > 16_384 {
        return Err(INVALID);
    }
    let mut listed = BTreeSet::new();
    let mut total = 0_u64;
    for file in &manifest.files {
        if file.path == "components.json"
            || !listed.insert(PathBuf::from(&file.path))
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(INVALID);
        }
        let path = relative_file(&root, &file.path)?;
        let mut input = fs::File::open(path).map_err(|_| INVALID)?;
        let length = input.metadata().map_err(|_| INVALID)?.len();
        total = total.checked_add(length).ok_or(INVALID)?;
        if total > 512 * 1024 * 1024 {
            return Err(INVALID);
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut consumed = 0_u64;
        loop {
            let count = input.read(&mut buffer).map_err(|_| INVALID)?;
            if count == 0 {
                break;
            }
            consumed += count as u64;
            if consumed > length {
                return Err(INVALID);
            }
            hasher.update(&buffer[..count]);
        }
        if consumed != length || format!("{:x}", hasher.finalize()) != file.sha256.to_lowercase() {
            return Err(INVALID);
        }
    }
    // Reject additional imported code or symlinks not represented by the manifest.
    let mut directories = vec![root.clone()];
    let mut actual = BTreeSet::new();
    let mut entries = 0_usize;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|_| INVALID)? {
            let entry = entry.map_err(|_| INVALID)?;
            entries += 1;
            if entries > 32_768 {
                return Err(INVALID);
            }
            let kind = entry.file_type().map_err(|_| INVALID)?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path.strip_prefix(&root).map_err(|_| INVALID)?;
                if relative != Path::new("components.json") {
                    actual.insert(relative.to_owned());
                }
            } else {
                return Err(INVALID);
            }
        }
    }
    if actual != listed {
        return Err(INVALID);
    }
    for entry in [
        &manifest.runtime,
        &manifest.entrypoint,
        &manifest.mcp_client,
    ] {
        if !listed.contains(Path::new(entry)) {
            return Err(INVALID);
        }
    }
    let result = Components {
        runtime: relative_file(&root, &manifest.runtime)?,
        entrypoint: relative_file(&root, &manifest.entrypoint)?,
        mcp_client: relative_file(&root, &manifest.mcp_client)?,
    };
    for executable in [&result.runtime, &result.mcp_client] {
        if fs::metadata(executable)
            .map_err(|_| INVALID)?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err(INVALID);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for name in ["node", "cli.mjs", "mcp", "package.json"] {
            fs::write(root.path().join(name), name).unwrap();
            fs::set_permissions(root.path().join(name), fs::Permissions::from_mode(0o700)).unwrap();
            files.push(
                json!({"path":name,"sha256":format!("{:x}",Sha256::digest(name.as_bytes()))}),
            );
        }
        fs::write(root.path().join("components.json"), json!({"version":1,"runtime":"node","entrypoint":"cli.mjs","mcpClient":"mcp","files":files}).to_string()).unwrap();
        root
    }
    #[test]
    fn complete_bundle_is_accepted_and_tampering_is_rejected() {
        let root = fixture();
        let manifest = root.path().join("components.json");
        assert!(load(&manifest).is_ok());
        fs::write(root.path().join("cli.mjs"), "changed").unwrap();
        assert!(load(&manifest).is_err());
    }
    #[test]
    fn unlisted_code_and_symlinks_are_rejected() {
        let root = fixture();
        let manifest = root.path().join("components.json");
        fs::write(root.path().join("extra.mjs"), "extra").unwrap();
        assert!(load(&manifest).is_err());
        fs::remove_file(root.path().join("extra.mjs")).unwrap();
        fs::rename(root.path().join("cli.mjs"), root.path().join("extra.mjs")).unwrap();
        std::os::unix::fs::symlink("extra.mjs", root.path().join("cli.mjs")).unwrap();
        assert!(load(&manifest).is_err());
    }
    #[test]
    fn traversal_duplicate_missing_and_non_executable_entries_are_rejected() {
        for variant in ["traversal", "duplicate", "missing", "executable"] {
            let root = fixture();
            let manifest = root.path().join("components.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
            match variant {
                "traversal" => value["files"][0]["path"] = json!("../node"),
                "duplicate" => {
                    let item = value["files"][0].clone();
                    value["files"].as_array_mut().unwrap().push(item);
                }
                "missing" => value["entrypoint"] = json!("absent.mjs"),
                _ => {
                    fs::set_permissions(root.path().join("node"), fs::Permissions::from_mode(0o600))
                        .unwrap()
                }
            }
            fs::write(&manifest, value.to_string()).unwrap();
            assert!(load(&manifest).is_err(), "{variant}");
        }
    }
}
