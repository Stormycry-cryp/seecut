// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local purchase intentions only. Never stores credentials or cashier URLs.
use crate::personal_library::with_library_write_lock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use url::Url;

pub(crate) const UNAVAILABLE: &str = "购买记录暂不可用，请重新打开后重试";
const INVALID: &str = "购买记录无效，请保留记录后重试";
const VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: usize = 9 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub(crate) plan_id: String,
    pub(crate) key: String,
    pub(crate) order_id: Option<String>,
    pub(crate) expires_at: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    base: String,
    uid: String,
    #[serde(deserialize_with = "unique_intents")]
    intents: BTreeMap<String, Intent>,
}

fn unique_intents<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Intent>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, Intent>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("unique purchase plan entries")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = BTreeMap::new();
            while let Some((plan, intent)) = map.next_entry::<String, Intent>()? {
                if entries.insert(plan, intent).is_some() {
                    return Err(serde::de::Error::custom("duplicate purchase plan"));
                }
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(Unique)
}

pub(crate) struct Journal {
    path: PathBuf,
    fingerprint: Option<[u8; 32]>,
    document: Document,
    blocked: bool,
}

pub(crate) fn canonical_base(base: &str) -> Result<String, &'static str> {
    let url = Url::parse(base).map_err(|_| INVALID)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
    {
        return Err(INVALID);
    }
    Ok(url.origin().ascii_serialization())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

fn journal_directory(data: &Path) -> Result<PathBuf, &'static str> {
    if !data.is_absolute() {
        return Err(UNAVAILABLE);
    }
    let root = data.join("purchase-journals").join("purchase-intents");
    let mut path = PathBuf::new();
    for component in root.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(UNAVAILABLE);
        }
        path.push(component);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|_| UNAVAILABLE)?;
                std::fs::symlink_metadata(&path).map_err(|_| UNAVAILABLE)?
            }
            Err(_) => return Err(UNAVAILABLE),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(UNAVAILABLE);
        }
    }
    Ok(root)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(UNAVAILABLE),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(UNAVAILABLE);
    }
    if metadata.len() > MAX_JOURNAL_BYTES as u64 {
        return Err(INVALID);
    }
    let file = File::open(path).map_err(|_| UNAVAILABLE)?;
    let mut bytes = Vec::new();
    file.take((MAX_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| UNAVAILABLE)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(INVALID);
    }
    Ok(Some(bytes))
}

impl Journal {
    pub(crate) fn open(data: &Path, base: &str, uid: &str) -> Result<Self, &'static str> {
        let base = canonical_base(base)?;
        if !valid_id(uid) {
            return Err(INVALID);
        }
        with_library_write_lock(|| {
            let scope = serde_json::to_string(&(&base, uid)).map_err(|_| INVALID)?;
            let mut hash = Sha256::new();
            // Keep the previous length-framed account/document filename stable.
            for key in [scope.as_str(), "purchase-journal-v1"] {
                hash.update((key.len() as u64).to_be_bytes());
                hash.update(key.as_bytes());
            }
            let path = journal_directory(data)?.join(format!("{:x}.json", hash.finalize()));
            let bytes = read_bytes(&path)?;
            let fingerprint = bytes.as_deref().map(digest);
            let document = match bytes {
                Some(bytes) => match serde_json::from_slice::<Document>(&bytes) {
                    Ok(document) if Self::valid(&document, &base, uid) => document,
                    _ => return Err(INVALID),
                },
                None => Document {
                    version: VERSION,
                    base,
                    uid: uid.into(),
                    intents: BTreeMap::new(),
                },
            };
            Ok(Self {
                path,
                fingerprint,
                document,
                blocked: false,
            })
        })
    }
    fn valid(document: &Document, base: &str, uid: &str) -> bool {
        let mut keys = HashSet::new();
        let mut orders = HashSet::new();
        document.version == VERSION
            && document.base == base
            && document.uid == uid
            && document.intents.len() <= 128
            && document.intents.iter().all(|(plan, intent)| {
                valid_id(plan)
                    && plan == &intent.plan_id
                    && uuid::Uuid::parse_str(&intent.key).is_ok()
                    && keys.insert(&intent.key)
                    && intent.order_id.as_ref().is_none_or(|id| orders.insert(id))
                    && intent.order_id.as_deref().is_none_or(valid_id)
                    && intent.expires_at.is_none_or(|expiry| expiry > 0)
            })
    }
    pub(crate) fn matches(&self, base: &str, uid: &str) -> bool {
        canonical_base(base).is_ok_and(|base| base == self.document.base)
            && uid == self.document.uid
    }
    pub(crate) fn get(&self, plan: &str) -> Option<&Intent> {
        self.document.intents.get(plan)
    }
    pub(crate) fn plan_for_order(&self, id: &str) -> Option<String> {
        self.document
            .intents
            .values()
            .find(|intent| intent.order_id.as_deref() == Some(id))
            .map(|intent| intent.plan_id.clone())
    }
    pub(crate) fn blocked(&self) -> bool {
        self.blocked
    }
    fn commit(&mut self, next: Document) -> Result<(), &'static str> {
        if self.blocked {
            return Err(UNAVAILABLE);
        }
        if !Self::valid(&next, &self.document.base, &self.document.uid) {
            return Err(INVALID);
        }
        let bytes = serde_json::to_vec(&next).map_err(|_| INVALID)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(INVALID);
        }
        let result = with_library_write_lock(|| {
            // Reads and replacements share the existing in-process file lock.
            // A changed or deleted document cannot be replaced by stale state.
            let current = read_bytes(&self.path)?;
            if current.as_deref().map(digest) != self.fingerprint {
                return Err(UNAVAILABLE);
            }
            let directory = self.path.parent().ok_or(UNAVAILABLE)?;
            let metadata = fs::symlink_metadata(directory).map_err(|_| UNAVAILABLE)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(UNAVAILABLE);
            }
            let temporary = directory.join(format!(".purchase-{}.tmp", uuid::Uuid::new_v4()));
            // Match the established registration journal's portable save path.
            let saved = (|| {
                let mut file = File::create(&temporary).map_err(|_| UNAVAILABLE)?;
                file.write_all(&bytes).map_err(|_| UNAVAILABLE)?;
                file.sync_all().map_err(|_| UNAVAILABLE)?;
                fs::rename(&temporary, &self.path).map_err(|_| UNAVAILABLE)
            })();
            if saved.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            saved
        });
        if result.is_err() {
            // Preserve the retained key and prohibit a later automatic new one.
            self.blocked = true;
            return Err(UNAVAILABLE);
        }
        self.fingerprint = Some(digest(&bytes));
        self.document = next;
        Ok(())
    }
    pub(crate) fn prepare(
        &mut self,
        plan: &str,
        replace_expired: bool,
    ) -> Result<Intent, &'static str> {
        if self.blocked || !valid_id(plan) {
            return Err(UNAVAILABLE);
        }
        if !replace_expired && let Some(intent) = self.get(plan).cloned() {
            self.commit(self.document.clone())?;
            return Ok(intent);
        }
        let intent = Intent {
            plan_id: plan.into(),
            key: uuid::Uuid::new_v4().to_string(),
            order_id: None,
            expires_at: None,
        };
        let mut next = self.document.clone();
        next.intents.insert(plan.into(), intent.clone());
        self.commit(next)?;
        Ok(intent)
    }
    pub(crate) fn accept(
        &mut self,
        plan: &str,
        id: &str,
        status: &str,
        expiry: Option<i64>,
    ) -> Result<(), &'static str> {
        let Some(old) = self.get(plan) else {
            return Ok(());
        };
        if !valid_id(id) || old.order_id.as_deref().is_some_and(|old| old != id) {
            return Err(INVALID);
        }
        let mut next = self.document.clone();
        if matches!(status, "paid" | "closed") {
            next.intents.remove(plan);
        } else {
            let intent = next.intents.get_mut(plan).ok_or(INVALID)?;
            intent.order_id = Some(id.into());
            intent.expires_at = expiry.or(intent.expires_at);
        }
        if next.intents == self.document.intents {
            return Ok(());
        }
        self.commit(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("seecut-purchase-journal-")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap()
    }
    #[test]
    fn restart_retains_all_plans_and_scope_uses_canonical_base() {
        let root = root();
        let mut journal =
            Journal::open(root.path(), "https://EXAMPLE.invalid:443/", "user-a").unwrap();
        let a = journal.prepare("plan-a", false).unwrap();
        let b = journal.prepare("plan-b", false).unwrap();
        journal
            .accept("plan-a", "ord-a", "pending", Some(1900))
            .unwrap();
        drop(journal);
        let mut reopened = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        assert_eq!(reopened.prepare("plan-a", false).unwrap().key, a.key);
        assert_eq!(
            reopened.get("plan-a").unwrap().order_id.as_deref(),
            Some("ord-a")
        );
        assert_eq!(reopened.prepare("plan-b", false).unwrap().key, b.key);
        assert!(
            Journal::open(root.path(), "https://example.invalid", "user-b")
                .unwrap()
                .get("plan-a")
                .is_none()
        );
        assert!(
            Journal::open(root.path(), "https://other.invalid", "user-a")
                .unwrap()
                .get("plan-a")
                .is_none()
        );
    }
    #[test]
    fn terminal_order_retirement_survives_restart() {
        for terminal in ["paid", "closed"] {
            let root = root();
            let mut journal =
                Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
            let original = journal.prepare("plan-a", false).unwrap();
            journal
                .accept("plan-a", "ord-a", terminal, Some(1900))
                .unwrap();
            drop(journal);
            let mut reopened =
                Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
            assert!(reopened.get("plan-a").is_none());
            assert_ne!(reopened.prepare("plan-a", false).unwrap().key, original.key);
        }
    }
    #[test]
    fn corrupt_or_unknown_schema_is_preserved_and_blocks_loading() {
        for invalid in [b"broken".as_slice(), b"{\"version\":9,\"base\":\"https://example.invalid\",\"uid\":\"user-a\",\"intents\":{}}".as_slice()] {
            let root = root();
            let mut journal = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
            journal.prepare("plan-a", false).unwrap();
            drop(journal);
            let directory = root.path().join("purchase-journals/purchase-intents");
            let path = std::fs::read_dir(directory).unwrap().next().unwrap().unwrap().path();
            std::fs::write(&path, invalid).unwrap();
            assert!(Journal::open(root.path(), "https://example.invalid", "user-a").is_err());
            assert_eq!(std::fs::read(path).unwrap(), invalid);
        }
    }
    #[test]
    fn duplicate_plan_key_order_and_scope_mismatch_preserve_malformed_bytes() {
        let a = r#"{"plan_id":"plan-a","key":"00000000-0000-4000-8000-000000000001","order_id":"ord-a","expires_at":1900}"#;
        let b = r#"{"plan_id":"plan-b","key":"00000000-0000-4000-8000-000000000002","order_id":"ord-b","expires_at":1900}"#;
        let envelope = |intents: String| {
            format!(
                r#"{{"version":1,"base":"https://example.invalid","uid":"user-a","intents":{{{intents}}}}}"#
            )
        };
        let invalid = [
            envelope(format!(r#""plan-a":{a},"plan-a":{a}"#)),
            envelope(format!(
                r#""plan-a":{a},"plan-b":{}"#,
                b.replace("000000000002", "000000000001")
            )),
            envelope(format!(
                r#""plan-a":{a},"plan-b":{}"#,
                b.replace("ord-b", "ord-a")
            )),
            envelope(format!(r#""plan-a":{a}"#)).replace("user-a", "other-user"),
            envelope(format!(r#""plan-a":{a}"#))
                .replace("https://example.invalid", "https://other.invalid"),
        ];
        for bytes in invalid {
            let root = root();
            let mut journal =
                Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
            journal.prepare("plan-a", false).unwrap();
            drop(journal);
            let path = std::fs::read_dir(root.path().join("purchase-journals/purchase-intents"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            std::fs::write(&path, &bytes).unwrap();
            assert!(Journal::open(root.path(), "https://example.invalid", "user-a").is_err());
            assert_eq!(std::fs::read_to_string(path).unwrap(), bytes);
        }
    }

    #[test]
    fn write_conflict_preserves_key_and_fails_closed_without_credentials() {
        let root = root();
        let mut journal = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        let original = journal.prepare("plan-a", false).unwrap();
        let mut other = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        other.prepare("plan-b", false).unwrap();
        assert!(
            journal
                .accept("plan-a", "ord-a", "pending", Some(1900))
                .is_err()
        );
        assert!(journal.prepare("plan-a", true).is_err());
        assert_eq!(journal.get("plan-a").unwrap().key, original.key);
        let directory = root.path().join("purchase-journals/purchase-intents");
        let path = std::fs::read_dir(directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert_eq!(value["intents"]["plan-a"].as_object().unwrap().len(), 4);
        assert!(!value.to_string().contains("payment_action"));
        assert!(!value.to_string().contains("token"));
    }

    #[test]
    fn two_handles_opened_before_first_write_cannot_replace_the_first_key() {
        let root = root();
        let mut first = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        let mut second = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        let original = first.prepare("plan-a", false).unwrap();
        assert!(second.prepare("plan-a", false).is_err());
        assert!(second.blocked());
        assert!(second.get("plan-a").is_none());
        let reopened = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        assert_eq!(reopened.get("plan-a").unwrap().key, original.key);
    }

    #[test]
    fn deleted_document_blocks_recreation_and_retains_the_original_key() {
        let root = root();
        let mut journal = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        let original = journal.prepare("plan-a", false).unwrap();
        fs::remove_file(&journal.path).unwrap();
        assert!(journal.prepare("plan-a", false).is_err());
        assert!(journal.blocked());
        assert!(journal.prepare("plan-a", true).is_err());
        assert_eq!(journal.get("plan-a").unwrap().key, original.key);
        assert!(!journal.path.exists());
    }

    #[test]
    fn failed_first_persistence_latches_blocked_even_after_storage_returns() {
        let root = root();
        let mut journal = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        let directory = journal.path.parent().unwrap().to_path_buf();
        fs::remove_dir(&directory).unwrap();
        assert!(journal.prepare("plan-a", false).is_err());
        assert!(journal.blocked());
        assert!(journal.get("plan-a").is_none());
        fs::create_dir(&directory).unwrap();
        assert!(journal.prepare("plan-a", false).is_err());
        assert!(!journal.path.exists());
        assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
    }

    #[test]
    fn oversized_document_is_preserved_and_blocks_opening() {
        let root = root();
        let mut journal = Journal::open(root.path(), "https://example.invalid", "user-a").unwrap();
        journal.prepare("plan-a", false).unwrap();
        let bytes = vec![b' '; MAX_JOURNAL_BYTES + 1];
        fs::write(&journal.path, &bytes).unwrap();
        assert!(Journal::open(root.path(), "https://example.invalid", "user-a").is_err());
        assert_eq!(fs::read(&journal.path).unwrap(), bytes);
    }
}
