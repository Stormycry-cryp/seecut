// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded native chat history. Runtime journals keep authority over operations;
//! this archive contains display state and frozen public request metadata only.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub(crate) const ARCHIVE_VERSION: u32 = 1;
pub(crate) const MAX_SESSIONS: usize = 32;
pub(crate) const MAX_SESSION_BYTES: usize = 256 * 1024;
const MAX_ARCHIVE_BYTES: usize = 9 * 1024 * 1024;
const INVALID: &str = "会话记录无效，已停止写入以保留原文件";
const UNAVAILABLE: &str = "会话记录暂不可用";
const UNSAFE: &str = "会话记录的路径、所有者或权限无效";
const TOO_LARGE: &str = "会话记录超过保存上限";
const CONFLICT: &str = "会话记录已被更改，请重新打开后继续";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Message {
    pub(crate) role: String,
    pub(crate) text: String,
    pub(crate) state: String,
    pub(crate) progress: String,
}

/// Frozen display context. The document variant retains the original JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ContextReference {
    Document(DocumentReference),
    App(AppReference),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentReference {
    pub(crate) project_key: String,
    pub(crate) project_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppReference {
    pub(crate) app_scope: AppReferenceScope,
    pub(crate) page_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppReferenceScope {
    pub(crate) instance_id: String,
    pub(crate) account_scope: String,
    pub(crate) app_session_id: String,
}
/// A history domain, independent of short-lived runtime and App session IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HistoryScope {
    Document(String),
    App(String),
}
impl HistoryScope {
    fn valid(&self) -> bool {
        valid_key(match self {
            Self::Document(key) | Self::App(key) => key,
        })
    }
    fn accepts(&self, context: &ContextReference) -> bool {
        match (self, context) {
            (Self::Document(key), ContextReference::Document(doc)) => &doc.project_key == key,
            (Self::App(key), ContextReference::App(app)) => {
                &app.app_scope.account_scope == key
                    && valid_key(&app.app_scope.instance_id)
                    && valid_id(&app.app_scope.app_session_id)
                    && app.page_name.len() <= 512
            }
            _ => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reference {
    pub kind: String,
    pub label: String,
    pub target: serde_json::Value,
}
impl Reference {
    pub fn valid(&self) -> bool {
        matches!(self.kind.as_str(), "asset" | "flow_node")
            && !self.label.is_empty()
            && self.label.len() <= 512
            && self.target.is_object()
            && serde_json::to_vec(&self.target).is_ok_and(|v| v.len() <= 8192)
    }
}

/// The caller constructs `policy` from allowlisted public catalog metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) input_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app_view_binding: Option<concat_editor_mcp::app_read::AppViewBinding>,
    #[serde(default)]
    pub(crate) task: Option<crate::agent_tasks::TaskIntent>,
    pub(crate) session_id: String,
    pub(crate) model_id: String,
    pub(crate) effort_id: Option<String>,
    pub(crate) policy: serde_json::Value,
    pub(crate) context: ContextReference,
    #[serde(default)]
    pub(crate) references: Vec<Reference>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) title_edited: bool,
    pub(crate) draft: String,
    #[serde(default)]
    pub(crate) draft_references: Vec<Reference>,
    #[serde(default)]
    pub(crate) message_references: BTreeMap<usize, Vec<Reference>>,
    pub(crate) messages: Vec<Message>,
    pub(crate) selected_model_id: Option<String>,
    #[serde(default)]
    pub(crate) selected_effort_id: Option<String>,
    #[serde(default)]
    pub(crate) selected_policy_revision: Option<String>,
    #[serde(default)]
    pub(crate) selected_effort_policy: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) generation_model_ids: HashMap<String, String>,
    // A record written before effort support must remain an explicit legacy choice.
    #[serde(default = "selection_initialized")]
    pub(crate) selection_initialized: bool,
    pub(crate) runtime_created: bool,
    #[serde(default)]
    pub(crate) current_task_id: Option<String>,
    #[serde(default)]
    pub(crate) task_updates: Vec<crate::agent_tasks::TaskUpdateIntent>,
    pub(crate) request_snapshots: Vec<RequestSnapshot>,
    pub(crate) pending: String,
    pub(crate) updated_at_ms: u64,
}

impl Record {
    pub(crate) fn rename(&mut self, title: &str) {
        self.title = title.into();
        self.title_edited = true;
    }

    pub(crate) fn name_first_input(&mut self, text: &str) {
        if self.title_edited || self.request_snapshots.len() != 1 {
            return;
        }
        self.title = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(24)
            .collect();
        if self.title.is_empty() {
            self.title = "素材讨论".into();
        }
    }
}

fn selection_initialized() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Archive {
    pub(crate) version: u32,
    pub(crate) active_id: String,
    pub(crate) sessions: Vec<Record>,
}

/// One account/project pair. A failed read latches read-only for this instance.
pub(crate) struct Store {
    root: PathBuf,
    directory: PathBuf,
    path: PathBuf,
    scope: HistoryScope,
    root_identity: (u64, u64),
    directory_identity: (u64, u64),
    loaded: Cell<bool>,
    blocked: Cell<bool>,
    fingerprint: Cell<Option<[u8; 32]>>,
}

impl Store {
    #[cfg(test)]
    pub(crate) fn open(
        root: &Path,
        account_key: &str,
        project_key: &str,
    ) -> Result<Self, &'static str> {
        Self::open_scope(
            root,
            account_key,
            HistoryScope::Document(project_key.into()),
        )
    }
    pub(crate) fn open_scope(
        root: &Path,
        account_key: &str,
        scope: HistoryScope,
    ) -> Result<Self, &'static str> {
        if !valid_key(account_key) || !scope.valid() {
            return Err(INVALID);
        }
        validate_path(root)?;
        let root_identity = private_directory(root)?;
        let directory = root.join("native-history");
        match fs::symlink_metadata(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o700).create(&directory) {
                    Ok(()) => {}
                    // Another owned Store may have created the same directory.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(UNAVAILABLE),
                }
            }
            Err(_) => return Err(UNAVAILABLE),
        }
        let directory_identity = private_directory(&directory)?;
        // Length framing keeps ("ab", "c") separate from ("a", "bc").
        let mut hash = Sha256::new();
        let keys: Vec<&str> = match &scope {
            // Preserve the existing document hash and on-disk location exactly.
            HistoryScope::Document(project) => vec![account_key, project],
            HistoryScope::App(account) => {
                vec!["seecut-native-app-history-v1", account_key, account]
            }
        };
        for key in keys {
            hash.update((key.len() as u64).to_be_bytes());
            hash.update(key.as_bytes());
        }
        let path = directory.join(format!("{:x}.json", hash.finalize()));
        Ok(Self {
            root: root.into(),
            directory,
            path,
            scope,
            root_identity,
            directory_identity,
            loaded: Cell::new(false),
            blocked: Cell::new(false),
            fingerprint: Cell::new(None),
        })
    }

    pub(crate) fn load(&self) -> Result<Option<Archive>, &'static str> {
        if self.blocked.get() {
            return Err(INVALID);
        }
        match self.read() {
            Ok(Some((archive, fingerprint))) => {
                self.loaded.set(true);
                self.fingerprint.set(Some(fingerprint));
                Ok(Some(archive))
            }
            Ok(None) => {
                self.loaded.set(true);
                self.fingerprint.set(None);
                Ok(None)
            }
            Err(error) => {
                self.blocked.set(true);
                Err(error)
            }
        }
    }

    pub(crate) fn save(&self, archive: &Archive) -> Result<(), &'static str> {
        if self.blocked.get() || !self.loaded.get() {
            return Err(INVALID);
        }
        self.validate(archive)?;
        let bytes = serde_json::to_vec(archive).map_err(|_| INVALID)?;
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(TOO_LARGE);
        }
        // Revalidate the retained file before replacing it. Corruption or an
        // independent writer must never be silently repaired by stale UI state.
        let current = match self.read() {
            Ok(value) => value.map(|(_, fingerprint)| fingerprint),
            Err(error) => {
                self.blocked.set(true);
                return Err(error);
            }
        };
        if current != self.fingerprint.get() {
            self.blocked.set(true);
            return Err(CONFLICT);
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.directory).map_err(|_| UNAVAILABLE)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| UNAVAILABLE)?;
        temporary.write_all(&bytes).map_err(|_| UNAVAILABLE)?;
        temporary.as_file().sync_all().map_err(|_| UNAVAILABLE)?;
        self.validate_directories()?;
        temporary.persist(&self.path).map_err(|_| UNAVAILABLE)?;
        // File and rename durability matter when the app closes immediately
        // after a context switch. A post-commit I/O error stays conservative.
        if File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .is_err()
        {
            self.blocked.set(true);
            return Err(UNAVAILABLE);
        }
        self.fingerprint.set(Some(digest(&bytes)));
        Ok(())
    }

    fn validate_directories(&self) -> Result<(), &'static str> {
        validate_path(&self.root)?;
        if private_directory(&self.root)? != self.root_identity
            || private_directory(&self.directory)? != self.directory_identity
        {
            return Err(UNSAFE);
        }
        Ok(())
    }

    fn read(&self) -> Result<Option<(Archive, [u8; 32])>, &'static str> {
        self.validate_directories()?;
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(UNAVAILABLE),
        };
        validate_file(&metadata)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
            .map_err(|_| UNAVAILABLE)?;
        let opened = file.metadata().map_err(|_| UNAVAILABLE)?;
        validate_file(&opened)?;
        if identity(&metadata) != identity(&opened) {
            return Err(UNSAFE);
        }
        let mut bytes = Vec::new();
        file.take((MAX_ARCHIVE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| UNAVAILABLE)?;
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(TOO_LARGE);
        }
        self.validate_directories()?;
        let archive: Archive = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        self.validate(&archive)?;
        Ok(Some((archive, digest(&bytes))))
    }

    fn validate(&self, archive: &Archive) -> Result<(), &'static str> {
        if archive.version != ARCHIVE_VERSION
            || archive.sessions.is_empty()
            || archive.sessions.len() > MAX_SESSIONS
        {
            return Err(INVALID);
        }
        let mut ids = HashSet::new();
        for session in &archive.sessions {
            if session.draft_references.len() > 4
                || session.draft_references.iter().any(|r| !r.valid())
                || session.message_references.iter().any(|(i, refs)| {
                    *i >= session.messages.len()
                        || refs.len() > 4
                        || refs.iter().any(|r| !r.valid())
                })
                || session
                    .request_snapshots
                    .iter()
                    .any(|s| s.references.len() > 4 || s.references.iter().any(|r| !r.valid()))
            {
                return Err(INVALID);
            }
            if !valid_id(&session.id) || !ids.insert(session.id.as_str()) {
                return Err(INVALID);
            }
            if session
                .selected_model_id
                .as_deref()
                .is_some_and(|id| !valid_id(id))
            {
                return Err(INVALID);
            }
            if session.selected_effort_id.is_some() != session.selected_policy_revision.is_some()
                || session
                    .selected_effort_id
                    .as_deref()
                    .is_some_and(|id| !valid_id(id))
                || session
                    .selected_policy_revision
                    .as_deref()
                    .is_some_and(|id| !valid_id(id))
                || session.generation_model_ids.iter().any(|(kind, id)| {
                    !matches!(kind.as_str(), "image" | "video") || !valid_model(id)
                })
                || session
                    .selected_effort_policy
                    .as_ref()
                    .is_some_and(|policy| {
                        !policy.is_object()
                            || policy["id"].as_str() != session.selected_effort_id.as_deref()
                            || policy["policy_revision"].as_str()
                                != session.selected_policy_revision.as_deref()
                            || session.selected_effort_id.is_none()
                    })
            {
                return Err(INVALID);
            }
            if !matches!(session.pending.as_str(), "" | "running" | "unknown") {
                return Err(INVALID);
            }
            if session.task_updates.len() > 64
                || session.task_updates.iter().any(|v| !v.valid())
                || session
                    .task_updates
                    .iter()
                    .map(|v| &v.client_request_id)
                    .collect::<HashSet<_>>()
                    .len()
                    != session.task_updates.len()
            {
                return Err(INVALID);
            }
            if session
                .current_task_id
                .as_deref()
                .is_some_and(|id| !crate::agent_tasks::valid_task_id(id))
            {
                return Err(INVALID);
            }
            if matches!(self.scope, HistoryScope::App(_))
                && (session.current_task_id.is_some()
                    || !session.task_updates.is_empty()
                    || session
                        .request_snapshots
                        .iter()
                        .any(|request| request.task.is_some()))
            {
                return Err(INVALID);
            }
            for request in &session.request_snapshots {
                if request.task.as_ref().is_some_and(|task| !task.valid())
                    || request.session_id != session.id
                    || !valid_model(&request.model_id)
                    || request.effort_id.as_deref().is_some_and(|id| !valid_id(id))
                    || !self.scope.accepts(&request.context)
                    || request.input_id.as_deref().is_some_and(|id| !valid_id(id))
                    || match &request.context {
                        ContextReference::Document(_) => request.app_view_binding.is_some(),
                        ContextReference::App(_) => {
                            request.input_id.is_none()
                                || request
                                    .app_view_binding
                                    .as_ref()
                                    .is_none_or(|view| !view.valid())
                        }
                    }
                {
                    return Err(INVALID);
                }
            }
            // Count without allocating a second potentially oversized copy of
            // an invalid in-memory draft before enforcing the record limit.
            serde_json::to_writer(&mut SizeLimit(MAX_SESSION_BYTES), session)
                .map_err(|_| TOO_LARGE)?;
        }
        if !ids.contains(archive.active_id.as_str()) {
            return Err(INVALID);
        }
        Ok(())
    }
}

struct SizeLimit(usize);

impl Write for SizeLimit {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other(TOO_LARGE))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 4096 && !key.contains('\0')
}

fn valid_model(model: &str) -> bool {
    !model.trim().is_empty() && model.len() <= 256 && !model.contains('\0')
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn validate_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute() {
        return Err(UNSAFE);
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(UNSAFE);
        }
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|_| UNAVAILABLE)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(UNSAFE);
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(u64, u64), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| UNAVAILABLE)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o7777 != 0o700
    {
        return Err(UNSAFE);
    }
    Ok(identity(&metadata))
}

fn validate_file(metadata: &Metadata) -> Result<(), &'static str> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(UNSAFE);
    }
    if metadata.len() > MAX_ARCHIVE_BYTES as u64 {
        return Err(TOO_LARGE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn document_reference_mut(context: &mut ContextReference) -> &mut DocumentReference {
        match context {
            ContextReference::Document(document) => document,
            ContextReference::App(_) => panic!("document fixture"),
        }
    }
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    #[test]
    fn app_history_is_separate_and_document_wire_and_path_are_unchanged() {
        let fixture = fixture();
        let doc = Store::open(fixture.path(), "account", "project").unwrap();
        let explicit = Store::open_scope(
            fixture.path(),
            "account",
            HistoryScope::Document("project".into()),
        )
        .unwrap();
        let app = Store::open_scope(
            fixture.path(),
            "account",
            HistoryScope::App("project".into()),
        )
        .unwrap();
        assert_eq!(doc.path, explicit.path);
        assert_ne!(doc.path, app.path);
        let context: ContextReference =
            serde_json::from_value(json!({"project_key":"project","project_name":"测试工程"}))
                .unwrap();
        assert_eq!(
            serde_json::to_value(context).unwrap(),
            json!({"project_key":"project","project_name":"测试工程"})
        );
        assert!(serde_json::from_value::<ContextReference>(json!({"project_key":"project","project_name":"test","app_scope":{"instance_id":"instance","account_scope":"account","app_session_id":"app-session"},"page_name":"素材"})).is_err());
    }

    #[test]
    fn app_history_requires_original_input_view_and_rejects_document_tasks() {
        let fixture = fixture();
        let store = Store::open_scope(
            fixture.path(),
            "account",
            HistoryScope::App("account-scope".into()),
        )
        .unwrap();
        store.load().unwrap();
        let mut saved = archive("project");
        let request = &mut saved.sessions[0].request_snapshots[0];
        request.context = ContextReference::App(AppReference {
            app_scope: AppReferenceScope {
                instance_id: "instance".into(),
                account_scope: "account-scope".into(),
                app_session_id: "app-session".into(),
            },
            page_name: "个人素材".into(),
        });
        assert!(store.save(&saved).is_err());
        let request = &mut saved.sessions[0].request_snapshots[0];
        request.input_id = Some("input-original".into());
        request.app_view_binding = Some(serde_json::from_value(json!({"pageId":"personalLibrary","pageEpoch":3,"pageStateId":format!("app-page-v1-{}", "a".repeat(64))})).unwrap());
        store.save(&saved).unwrap();
        assert_eq!(store.load().unwrap(), Some(saved.clone()));
        saved.sessions[0].current_task_id =
            Some(crate::agent_tasks::task_id_for("not-an-app-task"));
        assert!(store.save(&saved).is_err());
    }

    #[test]
    fn manual_title_survives_first_input_after_archive_round_trip() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        let mut saved = archive("project");
        saved.sessions[0].rename("新会话 自定义名称");
        store.save(&saved).unwrap();
        let mut reopened = store.load().unwrap().unwrap();
        reopened.sessions[0].name_first_input("这一轮消息不应该覆盖名称");
        assert_eq!(reopened, saved);
        let mut automatic = archive("project");
        automatic.sessions[0].name_first_input("  整理\n素材 ");
        assert_eq!(automatic.sessions[0].title, "整理 素材");
        automatic.sessions[0].name_first_input("");
        assert_eq!(automatic.sessions[0].title, "素材讨论");
    }

    #[test]
    fn task_original_request_and_update_survive_archive_without_live_authority() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        let mut saved = archive("project");
        let task_id = crate::agent_tasks::task_id_for("original-create");
        let record = &mut saved.sessions[0];
        record.current_task_id = Some(task_id.clone());
        record.request_snapshots[0].task = Some(crate::agent_tasks::TaskIntent {
            client_request_id: "original-create".into(),
            task_id: task_id.clone(),
            goal: "完整目标".into(),
            max_task_credits: 100,
        });
        record
            .task_updates
            .push(crate::agent_tasks::TaskUpdateIntent {
                client_request_id: "original-update".into(),
                task_id: task_id.clone(),
                message: "原补充内容".into(),
                expected_revision: 5,
                retries: 0,
                confirmed: false,
            });
        store.save(&saved).unwrap();
        assert_eq!(store.load().unwrap(), Some(saved.clone()));
        let mut old = serde_json::to_value(archive("project")).unwrap();
        old["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("current_task_id");
        old["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("task_updates");
        old["sessions"][0]["request_snapshots"][0]
            .as_object_mut()
            .unwrap()
            .remove("task");
        let old: Archive = serde_json::from_value(old).unwrap();
        assert!(old.sessions[0].current_task_id.is_none());
        assert!(old.sessions[0].task_updates.is_empty());
        let mut invalid = saved.clone();
        invalid.sessions[0].request_snapshots[0]
            .task
            .as_mut()
            .unwrap()
            .client_request_id = "different-id".into();
        assert!(store.save(&invalid).is_err());
        assert_eq!(store.load().unwrap(), Some(saved));
    }

    fn fixture() -> tempfile::TempDir {
        // macOS's ambient /var spelling includes a symlink outside our fixture.
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let directory = tempfile::tempdir_in(temporary).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn archive(project: &str) -> Archive {
        Archive {
            version: ARCHIVE_VERSION,
            active_id: "session-one".into(),
            sessions: vec![Record {
                id: "session-one".into(),
                title: "画布调整".into(),
                title_edited: false,
                draft: "接着处理第二张图".into(),
                draft_references: Vec::new(),
                message_references: BTreeMap::new(),
                messages: vec![Message {
                    role: "Agent".into(),
                    text: "上一条回复".into(),
                    state: "操作结果待核实".into(),
                    progress: "移动画布对象".into(),
                }],
                selected_model_id: Some("managed-model".into()),
                selected_effort_id: None,
                selected_policy_revision: None,
                selected_effort_policy: None,
                generation_model_ids: HashMap::new(),
                selection_initialized: true,
                runtime_created: true,
                current_task_id: None,
                task_updates: Vec::new(),
                request_snapshots: vec![RequestSnapshot {
                    input_id: None,
                    app_view_binding: None,
                    task: None,
                    references: Vec::new(),
                    session_id: "session-one".into(),
                    model_id: "managed-model".into(),
                    effort_id: None,
                    policy: json!({"protocol_version": 1, "policy_revision": "fixture-1"}),
                    context: ContextReference::Document(DocumentReference {
                        project_key: project.into(),
                        project_name: "测试工程".into(),
                    }),
                }],
                pending: "unknown".into(),
                updated_at_ms: 1234,
            }],
        }
    }

    fn write_owned(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn legacy_archive_does_not_adopt_a_new_effort_policy() {
        let mut value = serde_json::to_value(archive("project")).unwrap();
        let session = value["sessions"][0].as_object_mut().unwrap();
        for key in [
            "draft_references",
            "message_references",
            "selected_effort_id",
            "selected_policy_revision",
            "selected_effort_policy",
            "generation_model_ids",
            "selection_initialized",
        ] {
            session.remove(key);
        }
        let restored: Archive = serde_json::from_value(value).unwrap();
        assert!(restored.sessions[0].selection_initialized);
        assert!(restored.sessions[0].selected_effort_id.is_none());
        assert!(restored.sessions[0].draft_references.is_empty());
        assert!(restored.sessions[0].message_references.is_empty());
        assert!(restored.sessions[0].selected_effort_policy.is_none());
        assert!(restored.sessions[0].generation_model_ids.is_empty());
        assert_eq!(restored.sessions[0].pending, "unknown");
    }
    #[test]
    fn renamed_session_and_frozen_references_survive_without_rewriting_sent_context() {
        let dir = fixture();
        let store = Store::open(dir.path(), "account", "project").unwrap();
        assert!(store.load().unwrap().is_none());
        let mut data = archive("project");
        let reference = Reference {
            kind: "asset".into(),
            label: "原素材".into(),
            target: json!({"assetId":"asset-one","version":{"sha256":"a".repeat(64),"registration_id":"import-one"}}),
        };
        let session = &mut data.sessions[0];
        session.title = "分镜讨论".into();
        session.draft_references = vec![reference.clone()];
        session
            .message_references
            .insert(0, vec![reference.clone()]);
        session.request_snapshots[0].references = vec![reference.clone()];
        store.save(&data).unwrap();
        let restored = Store::open(dir.path(), "account", "project")
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(restored, data);
        data.sessions[0].draft_references[0].target["version"]["sha256"] = json!("b".repeat(64));
        assert_eq!(
            data.sessions[0].message_references[&0],
            vec![reference.clone()]
        );
        assert_eq!(
            data.sessions[0].request_snapshots[0].references,
            vec![reference]
        );
        data.sessions[0].message_references.insert(10, Vec::new());
        assert!(store.save(&data).is_err());
        assert_eq!(store.load().unwrap().unwrap(), restored);
    }

    #[test]
    fn effort_selection_roundtrip_preserves_old_requests_and_rejects_partial_pairs() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        let mut original = archive("project");
        let previous = original.sessions[0].request_snapshots.clone();
        let session = &mut original.sessions[0];
        session.selected_effort_id = Some("balanced".into());
        session.selected_policy_revision = Some("revision-2".into());
        session.selected_effort_policy = Some(
            json!({"id":"balanced","policy_revision":"revision-2","agent":{"model_id":"managed-model","max_steps":8}}),
        );
        session.generation_model_ids = HashMap::from([
            ("image".into(), "image/one".into()),
            ("video".into(), "video/two".into()),
        ]);
        store.save(&original).unwrap();
        assert_eq!(store.load().unwrap(), Some(original.clone()));
        assert_eq!(original.sessions[0].request_snapshots, previous);
        original.sessions[0].selected_policy_revision = None;
        assert!(store.save(&original).is_err());
        assert!(
            store.load().unwrap().unwrap().sessions[0]
                .selected_policy_revision
                .is_some()
        );
    }

    #[test]
    fn provider_model_names_are_opaque_bounded_values() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        assert_eq!(store.load().unwrap(), None);
        let mut original = archive("project");
        original.sessions[0].request_snapshots[0].model_id = "openai/model".into();
        store.save(&original).unwrap();
        assert_eq!(store.load().unwrap(), Some(original.clone()));
        original.sessions[0].request_snapshots[0].model_id = "m".repeat(256);
        store.save(&original).unwrap();
        assert_eq!(store.load().unwrap(), Some(original.clone()));
        original.sessions[0].request_snapshots[0].model_id.push('m');
        assert!(store.save(&original).is_err());
    }

    #[test]
    fn reopening_preserves_each_session_and_all_frozen_requests() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        assert_eq!(store.load().unwrap(), None);
        let mut original = archive("project");
        let mut second = original.sessions[0].clone();
        second.id = "session-two".into();
        second.draft = "另一条草稿".into();
        second.request_snapshots[0].session_id = second.id.clone();
        second.request_snapshots[0].effort_id = Some("balanced".into());
        second.pending = "running".into();
        original.active_id = second.id.clone();
        original.sessions.push(second);
        store.save(&original).unwrap();
        let frozen = original.sessions[0].request_snapshots[0].clone();
        let mut next = frozen.clone();
        next.model_id = "second-model".into();
        next.effort_id = Some("high".into());
        next.policy = json!({"protocol_version": 1, "policy_revision": "fixture-2"});
        document_reference_mut(&mut next.context).project_name = "重新命名的工程".into();
        original.sessions[0].request_snapshots.push(next);
        store.save(&original).unwrap();
        assert_eq!(original.sessions[0].request_snapshots[0], frozen);
        drop(store);
        let reopened = Store::open(fixture.path(), "account", "project").unwrap();
        assert_eq!(reopened.load().unwrap(), Some(original));
        assert_eq!(
            fs::metadata(&reopened.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&reopened.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn account_and_project_names_are_isolated_and_never_paths() {
        let fixture = fixture();
        let first = Store::open(fixture.path(), "ab", "c").unwrap();
        let second = Store::open(fixture.path(), "a", "bc").unwrap();
        let other_account = Store::open(fixture.path(), "other", "c").unwrap();
        let other_project = Store::open(fixture.path(), "ab", "other").unwrap();
        let unusual = Store::open(fixture.path(), "../../account", "/a/project").unwrap();
        first.load().unwrap();
        first.save(&archive("c")).unwrap();
        for store in [&second, &other_account, &other_project, &unusual] {
            assert_ne!(store.path, first.path);
            assert_eq!(store.load().unwrap(), None);
            assert_eq!(store.path.parent(), Some(first.directory.as_path()));
            assert_eq!(store.path.file_name().unwrap().to_str().unwrap().len(), 69);
        }
    }

    #[test]
    fn sessions_without_requests_accept_only_known_pending_states() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        let mut value = archive("project");
        value.sessions[0].request_snapshots.clear();
        for pending in ["", "running", "unknown"] {
            value.sessions[0].pending = pending.into();
            store.save(&value).unwrap();
            assert_eq!(store.load().unwrap(), Some(value.clone()));
        }
        let before = fs::read(&store.path).unwrap();
        for pending in ["outcome_unknown", "completed", "UNKNOWN"] {
            value.sessions[0].pending = pending.into();
            assert!(store.save(&value).is_err());
            assert_eq!(fs::read(&store.path).unwrap(), before);
        }
    }

    #[test]
    fn corrupt_or_oversized_files_latch_read_only_without_replacement() {
        for bytes in [b"{broken".to_vec(), vec![b' '; MAX_ARCHIVE_BYTES + 1]] {
            let fixture = fixture();
            let store = Store::open(fixture.path(), "account", "project").unwrap();
            write_owned(&store.path, &bytes);
            assert!(store.load().is_err());
            assert!(store.save(&archive("project")).is_err());
            assert_eq!(fs::read(&store.path).unwrap(), bytes);
        }
    }

    #[test]
    fn invalid_candidates_leave_prior_bytes_and_unknown_session_intact() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        assert!(store.save(&archive("project")).is_err());
        store.load().unwrap();
        let original = archive("project");
        store.save(&original).unwrap();
        let before = fs::read(&store.path).unwrap();
        let mut candidates = Vec::new();
        let mut candidate = original.clone();
        candidate.sessions.push(candidate.sessions[0].clone());
        candidates.push(candidate);
        let mut candidate = original.clone();
        candidate.sessions[0].id = "../../session".into();
        candidates.push(candidate);
        let mut candidate = original.clone();
        candidate.active_id = "missing".into();
        candidates.push(candidate);
        let mut candidate = original.clone();
        candidate.sessions[0].request_snapshots[0].session_id = "other".into();
        candidates.push(candidate);
        let mut candidate = original.clone();
        let mut wrong_context = candidate.sessions[0].request_snapshots[0].clone();
        document_reference_mut(&mut wrong_context.context).project_key = "other".into();
        candidate.sessions[0].request_snapshots.push(wrong_context);
        candidates.push(candidate);
        let mut candidate = original.clone();
        candidate.sessions[0].pending = "outcome_unknown".into();
        candidates.push(candidate);
        let mut candidate = original.clone();
        candidate.sessions[0].draft = "x".repeat(MAX_SESSION_BYTES);
        candidates.push(candidate);
        let mut candidate = original.clone();
        let mut large_request = candidate.sessions[0].request_snapshots[0].clone();
        document_reference_mut(&mut large_request.context).project_name = "x".repeat(32 * 1024);
        candidate.sessions[0].request_snapshots = vec![large_request; 8];
        candidates.push(candidate);
        let mut candidate = original.clone();
        candidate.sessions = (0..=MAX_SESSIONS)
            .map(|index| {
                let mut session = original.sessions[0].clone();
                session.id = format!("session-{index}");
                session.request_snapshots[0].session_id = session.id.clone();
                session
            })
            .collect();
        candidate.active_id = candidate.sessions[0].id.clone();
        candidates.push(candidate);
        for candidate in candidates {
            assert!(store.save(&candidate).is_err());
            assert_eq!(fs::read(&store.path).unwrap(), before);
        }
        assert_eq!(store.load().unwrap(), Some(original));
    }

    #[test]
    fn save_replaces_atomically_and_detects_independent_changes() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        let original = archive("project");
        store.save(&original).unwrap();
        let held_file = File::open(&store.path).unwrap();
        let before = fs::read(&store.path).unwrap();
        let mut changed = original.clone();
        changed.sessions[0].draft = "新的草稿".into();
        store.save(&changed).unwrap();
        let mut held_bytes = Vec::new();
        (&held_file).read_to_end(&mut held_bytes).unwrap();
        assert_eq!(
            held_bytes, before,
            "replacement must not mutate the old inode"
        );
        assert_ne!(
            identity(&held_file.metadata().unwrap()),
            identity(&fs::metadata(&store.path).unwrap())
        );
        assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
        let stale = Store::open(fixture.path(), "account", "project").unwrap();
        stale.load().unwrap();
        store.save(&original).unwrap();
        assert_eq!(stale.save(&changed), Err(CONFLICT));
        assert_eq!(store.load().unwrap(), Some(original));
    }

    #[test]
    fn corruption_after_loading_is_not_overwritten() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        store.save(&archive("project")).unwrap();
        write_owned(&store.path, b"interrupted old writer");
        assert!(store.save(&archive("project")).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), b"interrupted old writer");
    }

    #[test]
    fn symlinks_permissions_and_replaced_directories_are_rejected() {
        let fixture = fixture();
        let store = Store::open(fixture.path(), "account", "project").unwrap();
        store.load().unwrap();
        let target = fixture.path().join("untouched");
        write_owned(&target, b"untouched");
        symlink(&target, &store.path).unwrap();
        assert!(store.save(&archive("project")).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        fs::remove_file(&store.path).unwrap();
        let invalid_mode = Store::open(fixture.path(), "account", "project").unwrap();
        write_owned(
            &invalid_mode.path,
            &serde_json::to_vec(&archive("project")).unwrap(),
        );
        fs::set_permissions(&invalid_mode.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(invalid_mode.load().is_err());
        fs::remove_file(&invalid_mode.path).unwrap();
        let replaced = Store::open(fixture.path(), "account", "project").unwrap();
        replaced.load().unwrap();
        fs::rename(&replaced.directory, fixture.path().join("saved-history")).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&replaced.directory)
            .unwrap();
        assert!(replaced.save(&archive("project")).is_err());
        fs::remove_dir(&replaced.directory).unwrap();
        symlink(fixture.path().join("saved-history"), &replaced.directory).unwrap();
        assert!(Store::open(fixture.path(), "account", "project").is_err());
    }

    #[test]
    fn unknown_authorization_fields_are_not_deserialized_into_history() {
        let mut value = serde_json::to_value(archive("project")).unwrap();
        let request = &mut value["sessions"][0]["request_snapshots"][0];
        let keys = request
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 7);
        assert!(keys.iter().all(|key| matches!(
            key.as_str(),
            "session_id" | "model_id" | "effort_id" | "policy" | "context" | "references" | "task"
        )));
        request["read_token"] = json!("owned-fixture-token");
        assert!(serde_json::from_value::<Archive>(value).is_err());
    }
}
