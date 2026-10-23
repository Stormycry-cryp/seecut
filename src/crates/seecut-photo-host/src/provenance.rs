use crate::{HostError, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const REPOSITORY: &str = "https://github.com/storytold/photocraft.git";
const COMMIT: &str = "ff4738810134d7f7e68b49f2e4f231b686282ff3";
const TREE_SHA: &str = "d93a309a979058e1a9449b97ee666a32988b79f1cee38631646989397a7f10bd";
const LOCK_SHA: &str = "36a1de83fea16781dc7186d33d8064cb5dfa878a2a0847228737640b1e64e2d0";

/// Verified complete-build artifact. The provenance is a trusted local build receipt,
/// not a cryptographic signature from a remote publisher.
#[derive(Clone, Debug)]
pub struct VerifiedBuild {
    /// Absolute path of the verified desktop executable.
    pub(crate) editor: PathBuf,
    /// Desktop digest recorded in the receipt and checked again before spawn.
    pub(crate) editor_sha: String,
    /// Expected desktop executable length.
    pub(crate) editor_bytes: u64,
    /// Build attempt identity kept with the editor instance.
    pub(crate) attempt: String,
}

/// Stream a regular non-symlink file into its length and SHA-256 digest.
pub(crate) fn sha_file(path: &Path) -> Result<(u64, String)> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(HostError::InvalidBuild(
            "binary must be a regular non-symlink file",
        ));
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buf = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        count += n as u64;
    }
    Ok((count, format!("{:x}", hash.finalize())))
}

impl VerifiedBuild {
    /// Validate fixed source, patch, complete feature set and both output binaries.
    pub fn from_provenance(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(HostError::InvalidBuild("provenance exceeds limit"));
        }
        let p: Value = serde_json::from_slice(&bytes)
            .map_err(|_| HostError::InvalidBuild("invalid provenance JSON"))?;
        let source = &p["source"];
        if p["schemaVersion"] != 1
            || p["status"] != "built"
            || source["repository"] != REPOSITORY
            || source["commit"] != COMMIT
            || source["version"] != "0.14.0"
            || source["sourceTreeSha256"] != TREE_SHA
            || source["cargoLockSha256"] != LOCK_SHA
            || source["fileCount"] != 2014
            || source["patches"]
                != json!([
                    {"path":"patches/seecut-theme.patch", "sha256":"c8706c00aa0dd2cdf4eee31f89d578db9b882f4b288a8388513a5910b1aa96c2"},
                    {"path":"patches/seecut-theme-polish.patch", "sha256":"f2e80dc4b8bcf7b669f3ce53a4ea0d637504009cf0d3635aa42c447a8eaa61ca"},
                    {"path":"patches/seecut-agent-control.patch", "sha256":"ea9894916e80c239cd3c535638f3ef403a26102321cf30b35f98417d65ab6ce9"},
                    {"path":"patches/seecut-profile-guard.patch", "sha256":"b97baea37a929cecb8959f7480ab328fc1eaedb942ab17654115aa065e724d6e"}
                ])
            || p["features"] != json!(["photocraft/heif", "photocraft-cli/heif"])
            || p["processOwnership"] != "isolated-posix-session"
        {
            return Err(HostError::InvalidBuild(
                "source, patches or complete-build identity mismatch",
            ));
        }
        let attempt = p["attemptId"]
            .as_str()
            .filter(|s| uuid::Uuid::parse_str(s).is_ok())
            .ok_or(HostError::InvalidBuild("missing build attempt identity"))?
            .to_owned();
        for name in ["photocraft", "photocraft-cli"] {
            let artifact = &p["binaries"][name];
            let binary = Path::new(
                artifact["path"]
                    .as_str()
                    .ok_or(HostError::InvalidBuild("missing binary path"))?,
            );
            if !binary.is_absolute() {
                return Err(HostError::InvalidBuild("binary path must be absolute"));
            }
            let (size, hash) = sha_file(binary)?;
            if size == 0
                || artifact["bytes"].as_u64() != Some(size)
                || artifact["sha256"].as_str() != Some(hash.as_str())
            {
                return Err(HostError::InvalidBuild("binary bytes or SHA-256 mismatch"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(binary)?.permissions().mode() & 0o111 == 0 {
                    return Err(HostError::InvalidBuild("binary is not executable"));
                }
            }
        }
        Ok(Self {
            editor: PathBuf::from(p["binaries"]["photocraft"]["path"].as_str().unwrap()),
            editor_sha: p["binaries"]["photocraft"]["sha256"]
                .as_str()
                .unwrap()
                .to_owned(),
            editor_bytes: p["binaries"]["photocraft"]["bytes"].as_u64().unwrap(),
            attempt,
        })
    }

    /// Detect desktop changes between initial receipt validation and launch.
    pub(crate) fn recheck_editor(&self) -> Result<()> {
        let (size, hash) = sha_file(&self.editor)?;
        if size != self.editor_bytes || hash != self.editor_sha {
            return Err(HostError::InvalidBuild(
                "editor changed after provenance validation",
            ));
        }
        Ok(())
    }
}
