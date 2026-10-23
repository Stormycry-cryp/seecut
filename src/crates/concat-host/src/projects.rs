// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Project folders on disk, and the list of recently opened ones.
//!
//! A project is a directory containing `concat.json`. That file is written the
//! moment the project is created, so a project is a real thing from the start
//! rather than a promise the app keeps only if you remember to save.
//!
//! The recents list lives in the app's config directory, not in any project.
//! It describes this machine's history, and copying a project folder to
//! another machine should not drag one person's recent files along with it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[path = "save_commit.rs"]
mod save_commit;
pub use save_commit::{
    RevokeSaveResult, SaveGateState, SaveInvalidation, SavePermitRefusal, SaveRevoker,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[cfg(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
use crate::ownership::ResourceIdentity;
use crate::ownership::{OwnershipError, WriterGuard};

/// A project failure with an explicit, branchable ownership error channel.
#[derive(Debug)]
pub enum OwnedProjectError {
    /// Writer conflict, invalid/stale identity, unsupported lock, or lock I/O.
    Ownership(OwnershipError),
    /// Project creation, parsing, or document writing failed.
    Project(String),
}

impl std::fmt::Display for OwnedProjectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ownership(error) => std::fmt::Display::fmt(error, formatter),
            Self::Project(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for OwnedProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Ownership(error) => Some(error),
            Self::Project(_) => None,
        }
    }
}

impl From<OwnershipError> for OwnedProjectError {
    fn from(error: OwnershipError) -> Self {
        Self::Ownership(error)
    }
}

impl From<String> for OwnedProjectError {
    fn from(error: String) -> Self {
        Self::Project(error)
    }
}

impl From<&str> for OwnedProjectError {
    fn from(error: &str) -> Self {
        Self::Project(error.to_owned())
    }
}

/// A newly written project and its uninterrupted creator ownership.
/// This sealed handoff is not Clone: adopting it moves the original owner into
/// one Session. Reading its info does not transfer or release ownership.
#[derive(Debug)]
pub struct CreatedProject {
    info: ProjectInfo,
    owner: Option<WriterGuard>,
}

impl CreatedProject {
    /// The completed artifact's metadata while creator ownership remains held.
    pub fn info(&self) -> &ProjectInfo {
        &self.info
    }

    /// Consumes this creation and moves its original owner into one Session.
    pub fn into_session(self) -> Result<crate::session::Session, OwnedProjectError> {
        crate::session::Session::from_created(self)
    }

    /// Releases creator ownership and returns completed artifact metadata.
    /// Interactive handoffs should use into_session instead to retain the lock.
    pub fn into_info(self) -> ProjectInfo {
        self.info
    }

    /// Writes a filled template or other initial document under the original
    /// creator owner, without dropping/reacquiring it before adoption.
    pub fn save(&self, document: &serde_json::Value) -> Result<(), OwnedProjectError> {
        save_owned(&self.info.path, document, self.owner.as_ref())
    }

    pub(crate) fn into_parts(self) -> (ProjectInfo, Option<WriterGuard>) {
        (self.info, self.owner)
    }
}

pub(crate) fn acquire_project_owner(path: &Path) -> Result<Option<WriterGuard>, OwnedProjectError> {
    #[cfg(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))]
    {
        Ok(Some(WriterGuard::acquire(ResourceIdentity::for_project(
            path,
        )?)?))
    }
    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    {
        let _ = path;
        Ok(None)
    }
}

pub(crate) fn validate_project_owner(
    path: &str,
    owner: Option<&WriterGuard>,
) -> Result<(), OwnedProjectError> {
    if let Some(owner) = owner {
        owner.validate()?;
        if owner.matches_project(path)? {
            return Ok(());
        }
        return Err(OwnershipError::InvalidTarget {
            target: PathBuf::from(path),
            reason: "writer guard belongs to another project",
        }
        .into());
    }
    if crate::ownership::desktop_writer_ownership_supported() {
        return Err(OwnershipError::InvalidTarget {
            target: PathBuf::from(path),
            reason: "desktop project save requires its existing writer owner",
        }
        .into());
    }
    // Explicit native compatibility path. No ownership protection is claimed
    // for mobile, Windows, or an unaudited architecture, and no MCP grant follows.
    Ok(())
}

const MANIFEST: &str = "concat.json";
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Orders saves issued by one open project session. A newer request makes an
/// older snapshot obsolete, even if a worker starts or finishes out of order.
#[derive(Default)]
pub struct SaveLane {
    order: Arc<save_commit::SaveOrder>,
    writer: Mutex<()>,
}

impl SaveLane {
    /// Reserves the order of a save before its worker is dispatched.
    pub fn next(&self) -> u64 {
        self.order.next()
    }

    /// Invalidates queued snapshots and waits for the previous writer to leave.
    /// Call on a worker after freezing the UI's source session, never on the UI thread.
    pub fn invalidate_and_wait(&self) {
        self.next();
        drop(
            self.writer
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
    }

    /// Whether a completion still belongs to the most recently requested save.
    pub fn is_latest(&self, ticket: u64) -> bool {
        self.order.is_latest(ticket)
    }

    /// Writes only the latest snapshot. A synchronous close uses this same
    /// lane and waits for an in-flight write before it returns.
    /// This standalone compatibility entry acquires an independent owner;
    /// workers of an open Session must call write_owned with their retained owner.
    pub fn write(
        &self,
        ticket: u64,
        path: &str,
        document: &serde_json::Value,
    ) -> Result<bool, String> {
        let owner = acquire_project_owner(Path::new(path)).map_err(|error| error.to_string())?;
        self.write_owned(ticket, path, document, owner.as_ref())
            .map_err(|error| error.to_string())
    }

    /// Writes the latest snapshot with its existing Session/creator owner.
    /// The worker must retain its guard clone until this call finishes or its
    /// snapshot is discarded. None is only native compatibility off desktop.
    pub fn write_owned(
        &self,
        ticket: u64,
        path: &str,
        document: &serde_json::Value,
        owner: Option<&WriterGuard>,
    ) -> Result<bool, OwnedProjectError> {
        self.write_with(ticket, path, document, owner, || {})
    }

    fn write_with(
        &self,
        ticket: u64,
        path: &str,
        document: &serde_json::Value,
        owner: Option<&WriterGuard>,
        entered: impl FnOnce(),
    ) -> Result<bool, OwnedProjectError> {
        let _writer = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entered();
        if !self.is_latest(ticket) {
            return Ok(false);
        }
        save_guarded(path, document, owner, || self.is_latest(ticket))
    }
}
/// Bound applies only to the opt-in Agent path; existing native saves keep
/// their original behavior. Both encoding and the independent reopen are bounded.
pub const MAX_AGENT_SAVE_BYTES: usize = 16 * 1024 * 1024;
/// Primary Project collections are bounded in addition to serialized bytes.
pub const MAX_AGENT_PROJECT_ITEMS: usize = 100_000;

/// Frozen by the original Session after native UI authorization. This is the
/// one-shot execution object: it cannot be cloned, deserialized, or supplied a
/// replacement path/document/owner at execution time. It carries no UI object.
pub struct AgentSaveSnapshot {
    permit: save_commit::SavePermit,
    path: String,
    manifest: PathBuf,
    document: serde_json::Value,
    project: concat_project::Project,
    owner: WriterGuard,
}

impl AgentSaveSnapshot {
    /// Returns the original lane ticket; the number alone grants no authority.
    pub fn ticket(&self) -> u64 {
        self.permit.ticket()
    }
}

/// Known reason no manifest rename was attempted.
#[derive(Debug)]
pub enum AgentSaveNotCommitted {
    /// The post-CAS deadline check failed before any rename attempt.
    ExpiredAfterClaim,
    /// Actual encoded bytes or their restored Project failed preflight.
    Engine(String),
    /// The original permit could not claim its lane.
    Permit(SavePermitRefusal),
    /// Owner validation or preparation failed before commit started.
    Preparation(OwnedProjectError),
    /// The selected manifest differs from the original fixed target.
    ManifestChanged,
    /// The encoded bytes or Project collections exceed the Agent bound.
    ResourceLimit,
}

/// Last execution stage observed after the commit-start claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentSaveStage {
    /// Internal boundary for a known expiry before any rename attempt.
    DeadlineAfterClaim,
    /// Commit was claimed but publication has not yet been attempted.
    AfterClaim,
    /// The manifest rename was attempted.
    Rename,
    /// The manifest rename succeeded; later verification is incomplete.
    AfterRename,
    /// Parent-directory synchronization failed after rename.
    DirectorySync,
    /// Fresh file, JSON, engine, or original-target verification failed.
    Readback,
}

/// Native evidence, constructed only after a real rename, parent-directory
/// sync, fresh bounded read, SHA match and JSON equality under the same lane.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedAgentSave {
    ticket: u64,
    manifest: PathBuf,
    bytes: u64,
    snapshot_sha256: String,
    manifest_sha256: String,
    engine_document_version: u64,
    document_version: u64,
    project_items: usize,
    project: concat_project::Project,
}

impl VerifiedAgentSave {
    /// Returns the original verified save ticket.
    pub fn ticket(&self) -> u64 {
        self.ticket
    }
    /// Returns the original manifest path that was independently reopened.
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }
    /// Returns the number of actual independently read manifest bytes.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// Returns SHA-256 of the frozen encoded bytes, not a Project state identifier.
    pub fn snapshot_sha256(&self) -> &str {
        &self.snapshot_sha256
    }
    /// Returns SHA-256 of actual independently read manifest bytes.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    /// Version of the actual engine reader that validated Project equivalence.
    /// This is not a documentStateId or a normalized Project hash.
    pub fn engine_document_version(&self) -> u64 {
        self.engine_document_version
    }
    /// Returns the bounded primary collection count of the reopened Project.
    pub fn project_items(&self) -> usize {
        self.project_items
    }
    /// Actual disk JSON version, distinct from the reader's engine version.
    pub fn document_version(&self) -> u64 {
        self.document_version
    }
    /// The real independently reopened/parsed Project, never the source clone.
    pub fn project(&self) -> &concat_project::Project {
        &self.project
    }
}

/// Native-only frozen read origin. Not Clone/Deserialize; no user path or JSON.
/// Retains the original owner but grants no read/account/scope authority itself.
pub struct PersistedClipSnapshot {
    order: Arc<save_commit::SaveOrder>,
    path: String,
    manifest: PathBuf,
    owner: WriterGuard,
}

/// Failure reading the exact original persisted Clip under its owner lane.
#[derive(Debug)]
pub enum PersistedClipReadError {
    /// The snapshot belongs to a different lane allocation.
    WrongLane,
    /// The original writer owner failed validation.
    Ownership(OwnedProjectError),
    /// The fixed manifest no longer matches the selected original target.
    TargetChanged,
    /// Fresh bounded regular-file reading or identity checks failed.
    Read(String),
    /// Actual disk bytes failed bounded default-recursion JSON decoding.
    Decode(String),
    /// Actual disk settings, version, engine parsing, or Project bounds failed.
    Engine(String),
}

/// Actual independently read disk result. No target path/JSON/owner getters.
/// Native must recheck its original read source before exposing a bounded digest.
#[derive(Debug)]
pub struct PersistedClipRead {
    manifest_name: &'static str,
    bytes: u64,
    manifest_sha256: String,
    document_version: u64,
    project_items: usize,
    project: concat_project::Project,
}

impl PersistedClipRead {
    /// Returns the actual fixed manifest filename without its directory.
    pub fn manifest_name(&self) -> &str {
        self.manifest_name
    }
    /// Returns the number of actual independently read disk bytes.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// Returns SHA-256 of the actual disk bytes, not a Project state identifier.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    /// Returns the actual disk JSON version before engine migration.
    pub fn document_version(&self) -> u64 {
        self.document_version
    }
    /// Returns the bounded primary collection count of the parsed disk Project.
    pub fn project_items(&self) -> usize {
        self.project_items
    }
    /// Borrows the actual disk Project parsed by the engine or disk settings fallback.
    pub fn project(&self) -> &concat_project::Project {
        &self.project
    }
}

impl SaveLane {
    /// Freezes only the original in-memory origin and owner; performs no IO.
    pub(crate) fn freeze_persisted_read(
        &self,
        path: String,
        manifest: PathBuf,
        owner: WriterGuard,
    ) -> PersistedClipSnapshot {
        PersistedClipSnapshot {
            order: Arc::clone(&self.order),
            path,
            manifest,
            owner,
        }
    }

    /// Worker-only fresh read using this same original IO lane. Does not reserve
    /// a save ticket, touch an editor, invalidate saves or acquire a new owner.
    pub fn read_persisted_clip(
        &self,
        snapshot: PersistedClipSnapshot,
    ) -> Result<PersistedClipRead, PersistedClipReadError> {
        use sha2::{Digest, Sha256};
        let PersistedClipSnapshot {
            order,
            path,
            manifest,
            owner,
        } = snapshot;
        if !Arc::ptr_eq(&self.order, &order) {
            return Err(PersistedClipReadError::WrongLane);
        }
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        validate_project_owner(&path, Some(&owner)).map_err(PersistedClipReadError::Ownership)?;
        let root = Path::new(&path);
        if manifest_path(root) != manifest {
            return Err(PersistedClipReadError::TargetChanged);
        }
        let manifest_name = match manifest.file_name().and_then(|name| name.to_str()) {
            Some("concat.json") => "concat.json",
            Some("wolfcut.json") => "wolfcut.json",
            _ => return Err(PersistedClipReadError::TargetChanged),
        };
        let bytes = read_agent_manifest(&manifest).map_err(PersistedClipReadError::Read)?;
        let document = decode_agent_document(&bytes).map_err(PersistedClipReadError::Decode)?;
        let document_version = concat_project::document_version(&document);
        let (project, project_items) =
            parse_agent_project(&document).map_err(PersistedClipReadError::Engine)?;
        validate_project_owner(&path, Some(&owner)).map_err(PersistedClipReadError::Ownership)?;
        if manifest_path(root) != manifest {
            return Err(PersistedClipReadError::TargetChanged);
        }
        Ok(PersistedClipRead {
            manifest_name,
            bytes: bytes.len() as u64,
            manifest_sha256: format!("{:x}", Sha256::digest(&bytes)),
            document_version,
            project_items,
            project,
        })
    }
}

/// Original save outcome with explicit claim, publication, and verification facts.
#[derive(Debug)]
pub enum AgentSaveResult {
    /// No manifest rename was attempted. A post-CAS deadline rejection
    /// preserves commit_claimed=true rather than claiming the CAS never happened.
    NotCommitted {
        /// Original ticket bound to the consumed snapshot.
        ticket: u64,
        /// Whether the commit-start CAS succeeded, even on known post-claim expiry.
        commit_claimed: bool,
        /// Whether the original operation attempted manifest rename.
        rename_attempted: bool,
        /// Known refusal before any manifest rename attempt.
        reason: AgentSaveNotCommitted,
    },
    /// Rename, directory sync, and actual disk/engine equivalence were verified.
    Verified(VerifiedAgentSave),
    /// Commit started at the successful CAS. Never relabel as cancelled or
    /// not-dispatched after a rename attempt or later verification failure.
    /// Receipt loss/native restart remains UNKNOWN; this is not replay authority.
    StartedUnverified {
        /// Original ticket bound to the consumed snapshot.
        ticket: u64,
        /// Original fixed manifest whose publication needs verification.
        manifest: PathBuf,
        /// Stage at which the already-started operation failed.
        stage: AgentSaveStage,
        /// Whether the original operation attempted manifest rename.
        rename_attempted: bool,
        /// Whether the original manifest rename returned success.
        rename_succeeded: bool,
        /// Native failure detail; does not authorize replay.
        error: String,
    },
}

impl AgentSaveResult {
    /// Reports whether this original operation won its commit-start CAS.
    pub fn commit_claimed(&self) -> bool {
        match self {
            Self::NotCommitted { commit_claimed, .. } => *commit_claimed,
            Self::Verified(_) | Self::StartedUnverified { .. } => true,
        }
    }
    /// Reports whether this operation called the manifest rename.
    pub fn rename_attempted(&self) -> bool {
        match self {
            Self::NotCommitted {
                rename_attempted, ..
            }
            | Self::StartedUnverified {
                rename_attempted, ..
            } => *rename_attempted,
            Self::Verified(_) => true,
        }
    }
    /// Reports whether rename returned success, independently of verification.
    pub fn rename_succeeded(&self) -> bool {
        match self {
            Self::NotCommitted { .. } => false,
            Self::StartedUnverified {
                rename_succeeded, ..
            } => *rename_succeeded,
            Self::Verified(_) => true,
        }
    }
}

/// These hooks are private test machinery, never model/native API parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentSaveBoundary {
    BeforeClaim,
    #[cfg(test)]
    ImmediatelyAfterClaim,
    AfterClaim,
    AfterRename,
    BeforeReadback,
}

impl SaveLane {
    /// Called only by Session::prepare_agent_save, after the trusted UI has
    /// checked its exact named permit, lease and original account/document.
    /// Issuance does not inspect files or wait for the lane's IO writer.
    pub(crate) fn freeze_agent_save(
        &self,
        path: String,
        manifest: PathBuf,
        document: serde_json::Value,
        project: concat_project::Project,
        owner: WriterGuard,
        deadline: std::time::Instant,
    ) -> (AgentSaveSnapshot, SaveRevoker) {
        let (permit, revoker) = self.order.issue(deadline);
        (
            AgentSaveSnapshot {
                permit,
                path,
                manifest,
                document,
                project,
                owner,
            },
            revoker,
        )
    }

    /// Worker-only. Consumes the exact Session snapshot once and never reads UI.
    /// UI lifecycle handlers must invalidate the original SaveRevoker BEFORE
    /// changing its grant/token/account/conversation/document/revision/binding.
    /// A later next() automatically invalidates any still-pending Agent save.
    pub fn write_agent_save(&self, snapshot: AgentSaveSnapshot) -> AgentSaveResult {
        self.write_agent_save_with(snapshot, |_| Ok(()))
    }

    fn write_agent_save_with(
        &self,
        snapshot: AgentSaveSnapshot,
        mut boundary: impl FnMut(AgentSaveBoundary) -> Result<(), String>,
    ) -> AgentSaveResult {
        use sha2::{Digest, Sha256};
        let AgentSaveSnapshot {
            permit,
            path,
            manifest,
            document,
            project,
            owner,
        } = snapshot;
        let ticket = permit.ticket();
        // This is the original IO lane. next()/revoke never take this mutex.
        // There is no worker -> UI roundtrip while the mutex is held.
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let refused = |reason| AgentSaveResult::NotCommitted {
            ticket,
            commit_claimed: false,
            rename_attempted: false,
            reason,
        };
        // Reject a permit bound to another lane before creating any temp file.
        if !self.order.belongs(&permit) {
            return refused(AgentSaveNotCommitted::Permit(SavePermitRefusal::WrongLane));
        }
        if !self.is_latest(ticket) {
            return refused(AgentSaveNotCommitted::Permit(
                SavePermitRefusal::Invalidated(SaveInvalidation::Superseded),
            ));
        }
        let root = PathBuf::from(&path);
        let mut temporary = None;
        // Preparation errors occur before the commit CAS, so are known not-started.
        let prepared = (|| -> Result<_, AgentSaveNotCommitted> {
            validate_project_owner(&path, Some(&owner))
                .map_err(AgentSaveNotCommitted::Preparation)?;
            if manifest_path(&root) != manifest {
                return Err(AgentSaveNotCommitted::ManifestChanged);
            }
            inspect_agent_manifest(&manifest)
                .map_err(|e| AgentSaveNotCommitted::Preparation(e.into()))?;
            let encoded = encode_agent_document(&document)?;
            checked_agent_project_items(&project)
                .map_err(|_| AgentSaveNotCommitted::ResourceLimit)?;
            // In-memory Value conversion does not exercise serde_json's byte
            // parser recursion limit. Validate these exact to-be-written bytes
            // with the SAME parser as the fresh readback before any publication.
            let decoded = decode_agent_document(&encoded).map_err(AgentSaveNotCommitted::Engine)?;
            if decoded != document {
                return Err(AgentSaveNotCommitted::Engine(
                    "encoded manifest differs from the original snapshot".into(),
                ));
            }
            let (round_trip, _) =
                parse_agent_project(&decoded).map_err(AgentSaveNotCommitted::Engine)?;
            if round_trip != project {
                return Err(AgentSaveNotCommitted::Engine(
                    "frozen engine document does not round-trip to the original Session Project"
                        .into(),
                ));
            }
            let digest = format!("{:x}", Sha256::digest(&encoded));
            let candidate = root.join(format!(
                "{}.saving-{}-{}",
                manifest
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(MANIFEST),
                std::process::id(),
                TEMPORARY_ID.fetch_add(1, Ordering::Relaxed),
            ));
            // Cleanup is scoped to the exact file successfully created here.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
                .map_err(|e| {
                    AgentSaveNotCommitted::Preparation(
                        format!("could not create {}: {e}", candidate.display()).into(),
                    )
                })?;
            temporary = Some(candidate.clone());
            std::io::Write::write_all(&mut file, &encoded)
                .and_then(|()| file.sync_all())
                .map_err(|e| {
                    AgentSaveNotCommitted::Preparation(
                        format!("could not write/sync {}: {e}", candidate.display()).into(),
                    )
                })?;
            drop(file);
            boundary(AgentSaveBoundary::BeforeClaim)
                .map_err(|e| AgentSaveNotCommitted::Preparation(e.into()))?;
            validate_project_owner(&path, Some(&owner))
                .map_err(AgentSaveNotCommitted::Preparation)?;
            if manifest_path(&root) != manifest {
                return Err(AgentSaveNotCommitted::ManifestChanged);
            }
            inspect_agent_manifest(&manifest)
                .map_err(|e| AgentSaveNotCommitted::Preparation(e.into()))?;
            Ok((candidate, manifest.clone(), encoded, digest))
        })();
        let result = match prepared {
            Err(reason) => refused(reason),
            Ok((candidate, manifest, encoded, digest)) => {
                // begin() rechecks latest and claims the gate while holding the
                // SAME short order mutex as next(). Neither holds it across IO.
                #[cfg(not(test))]
                let claim = self.order.begin(permit);
                #[cfg(test)]
                let claim = self.order.begin_after_cas(permit, || {
                    // Only the real test boundary may pause before the post-CAS clock read.
                    boundary(AgentSaveBoundary::ImmediatelyAfterClaim).expect("test boundary");
                });
                match claim {
                    Err(error) => refused(AgentSaveNotCommitted::Permit(error)),
                    Ok(started) => {
                        let mut rename_attempted = false;
                        let mut rename_succeeded = false;
                        let completed =
                            (|| -> Result<VerifiedAgentSave, (AgentSaveStage, String)> {
                                if !started.deadline_valid() {
                                    return Err((
                                        AgentSaveStage::DeadlineAfterClaim,
                                        "original save deadline elapsed after claim".into(),
                                    ));
                                }
                                boundary(AgentSaveBoundary::AfterClaim)
                                    .map_err(|e| (AgentSaveStage::AfterClaim, e))?;
                                rename_attempted = true;
                                std::fs::rename(&candidate, &manifest).map_err(|e| {
                                    (
                                        AgentSaveStage::Rename,
                                        format!("could not replace {}: {e}", manifest.display()),
                                    )
                                })?;
                                rename_succeeded = true;
                                temporary = None; // rename consumed our owned temporary entry.
                                boundary(AgentSaveBoundary::AfterRename)
                                    .map_err(|e| (AgentSaveStage::AfterRename, e))?;
                                // Required for this stronger Agent result; existing manual
                                // save paths retain their original durability behavior.
                                std::fs::File::open(&root)
                                    .and_then(|dir| dir.sync_all())
                                    .map_err(|e| {
                                        (
                                            AgentSaveStage::DirectorySync,
                                            format!("could not sync {}: {e}", root.display()),
                                        )
                                    })?;
                                boundary(AgentSaveBoundary::BeforeReadback)
                                    .map_err(|e| (AgentSaveStage::Readback, e))?;
                                validate_project_owner(&path, Some(&owner))
                                    .map_err(|e| (AgentSaveStage::Readback, e.to_string()))?;
                                if manifest_path(&root) != manifest {
                                    return Err((
                                        AgentSaveStage::Readback,
                                        "original manifest selection changed after rename".into(),
                                    ));
                                }
                                let actual = read_agent_manifest(&manifest)
                                    .map_err(|e| (AgentSaveStage::Readback, e))?;
                                let actual_digest = format!("{:x}", Sha256::digest(&actual));
                                let parsed = decode_agent_document(&actual).map_err(|e| {
                                    (
                                        AgentSaveStage::Readback,
                                        format!("{}: {e}", manifest.display()),
                                    )
                                })?;
                                if actual != encoded
                                    || actual_digest != digest
                                    || parsed != document
                                {
                                    return Err((
                                        AgentSaveStage::Readback,
                                        "saved manifest does not match the original snapshot"
                                            .into(),
                                    ));
                                }
                                // This consumes the independently reopened bytes' JSON,
                                // never an in-memory parse of the original save input.
                                // from_document performs the real migration/settle path.
                                let (reopened_project, project_items) =
                                    parse_agent_project(&parsed)
                                        .map_err(|e| (AgentSaveStage::Readback, e))?;
                                if reopened_project != project {
                                    return Err((AgentSaveStage::Readback, "engine reopened Project differs from the original frozen Session Project".into()));
                                }
                                validate_project_owner(&path, Some(&owner))
                                    .map_err(|e| (AgentSaveStage::Readback, e.to_string()))?;
                                if manifest_path(&root) != manifest {
                                    return Err((
                                        AgentSaveStage::Readback,
                                        "original manifest selection changed during readback"
                                            .into(),
                                    ));
                                }
                                Ok(VerifiedAgentSave {
                                    ticket,
                                    manifest: manifest.clone(),
                                    bytes: actual.len() as u64,
                                    snapshot_sha256: digest,
                                    manifest_sha256: actual_digest,
                                    engine_document_version: concat_project::DOCUMENT_VERSION,
                                    document_version: concat_project::document_version(&parsed),
                                    project_items,
                                    project: reopened_project,
                                })
                            })();
                        // The result exists before the gate advertises Finished.
                        let result = match completed {
                            Ok(proof) => AgentSaveResult::Verified(proof),
                            Err((AgentSaveStage::DeadlineAfterClaim, _)) => {
                                AgentSaveResult::NotCommitted {
                                    ticket,
                                    commit_claimed: true,
                                    rename_attempted: false,
                                    reason: AgentSaveNotCommitted::ExpiredAfterClaim,
                                }
                            }
                            Err((stage, error)) => AgentSaveResult::StartedUnverified {
                                ticket,
                                manifest,
                                stage,
                                rename_attempted,
                                rename_succeeded,
                                error,
                            },
                        };
                        started.finish();
                        result
                    }
                }
            }
        };
        // This function is worker-only. No RAII object crosses into UI with a
        // filesystem cleanup in Drop, and only our own temporary entry is removed.
        if let Some(temporary) = temporary {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

/// Bounds the primary Project collections. Nested extra data is separately
/// bounded by the 16 MiB encoded/read JSON and serde_json's recursion limit.
fn checked_agent_project_items(project: &concat_project::Project) -> Result<usize, String> {
    let mut items = project
        .media
        .len()
        .checked_add(project.fonts.len())
        .ok_or_else(|| "engine Project item count overflow".to_owned())?;
    for timeline in &project.timelines {
        items = items
            .checked_add(1)
            .and_then(|n| n.checked_add(timeline.tracks.len()))
            .and_then(|n| n.checked_add(timeline.clips.len()))
            .ok_or_else(|| "engine Project item count overflow".to_owned())?;
        if items > MAX_AGENT_PROJECT_ITEMS {
            return Err("engine Project exceeds primary item limit".into());
        }
    }
    if items > MAX_AGENT_PROJECT_ITEMS {
        return Err("engine Project exceeds primary item limit".into());
    }
    Ok(items)
}

fn parse_agent_project(
    document: &serde_json::Value,
) -> Result<(concat_project::Project, usize), String> {
    if concat_project::document_version(document) > concat_project::DOCUMENT_VERSION {
        return Err("manifest requires a newer engine document version".into());
    }
    // Native project opening also requires the actual manifest settings. Never
    // use a current in-memory video's dimensions to fill a settings-only file.
    let settings = parse_agent_disk_settings(document)?;
    let project = match concat_project::from_document(document) {
        Some(project) => project,
        None if is_settings_only(document) => concat_project::Project::with_video(settings.video()),
        None => return Err("manifest is not readable by the current engine".into()),
    };
    let items = checked_agent_project_items(&project)?;
    Ok((project, items))
}

fn parse_agent_disk_settings(
    document: &serde_json::Value,
) -> Result<concat_project::DocumentSettings, String> {
    let manifest: Manifest = serde_json::from_value(document.clone())
        .map_err(|error| format!("manifest settings are invalid: {error}"))?;
    // Matches projects::open's actual settings precondition. Full documents'
    // settled timelines remain authoritative, just as Session loading does.
    if manifest.video.rate_den == 0 {
        return Err("manifest has an invalid frame rate".into());
    }
    Ok(concat_project::DocumentSettings {
        name: manifest.name,
        width: manifest.video.width,
        height: manifest.video.height,
        rate_num: manifest.video.rate_num,
        rate_den: manifest.video.rate_den,
    })
}

/// Exactly the same byte parser is used before CAS and after real readback.
/// Keep serde_json's default recursion limit; a Value-only conversion does not
/// establish that an encoded/migrated document can actually be reopened.
fn decode_agent_document(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if bytes.len() > MAX_AGENT_SAVE_BYTES {
        return Err("manifest JSON exceeds byte limit".into());
    }
    serde_json::from_slice(bytes).map_err(|error| format!("manifest JSON is not readable: {error}"))
}

fn inspect_agent_manifest(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a regular manifest", path.display()));
    }
    Ok(())
}

fn encode_agent_document(document: &serde_json::Value) -> Result<Vec<u8>, AgentSaveNotCommitted> {
    struct BoundedBuffer(Vec<u8>);
    impl std::io::Write for BoundedBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_AGENT_SAVE_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("Agent manifest exceeds byte limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedBuffer(Vec::new());
    serde_json::to_writer_pretty(&mut output, document)
        .map_err(|_| AgentSaveNotCommitted::ResourceLimit)?;
    Ok(output.0)
}

fn read_agent_manifest(manifest: &Path) -> Result<Vec<u8>, String> {
    read_agent_manifest_with(manifest, || {})
}

// Private real-file test boundary; production never waits or calls UI here.
fn read_agent_manifest_with(
    manifest: &Path,
    after_metadata: impl FnOnce(),
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    inspect_agent_manifest(manifest)?;
    let mut file = std::fs::File::open(manifest)
        .map_err(|e| format!("could not reopen {}: {e}", manifest.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_AGENT_SAVE_BYTES as u64 {
        return Err("saved manifest exceeds byte limit or is not a regular file".into());
    }
    after_metadata();
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_AGENT_SAVE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_AGENT_SAVE_BYTES {
        return Err("saved manifest grew beyond byte limit".into());
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    let entry = std::fs::symlink_metadata(manifest).map_err(|e| e.to_string())?;
    if !entry.is_file()
        || entry.file_type().is_symlink()
        || metadata.len() != bytes.len() as u64
        || after.len() != metadata.len()
        || entry.len() != metadata.len()
        || metadata.modified().ok() != after.modified().ok()
        || after.modified().ok() != entry.modified().ok()
    {
        return Err("saved manifest changed or was short-read during reopening".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != entry.dev() || metadata.ino() != entry.ino() {
            return Err("saved manifest entry changed during reopening".into());
        }
    }
    Ok(bytes)
}

/// The manifest's earlier names, newest first. Projects created before a
/// rename keep theirs forever: the manifest is read and written under the
/// name it has, and only brand-new projects get the current one. A rename
/// must never orphan an edit.
const LEGACY_MANIFESTS: &[&str] = &["wolfcut.json"];

/// The manifest file this project actually uses: the current name, a legacy
/// name if that is what exists, the current name for a fresh folder.
pub fn manifest_path(root: &Path) -> PathBuf {
    let current = root.join(MANIFEST);
    if current.is_file() {
        return current;
    }
    for legacy in LEGACY_MANIFESTS {
        let candidate = root.join(legacy);
        if candidate.is_file() {
            return candidate;
        }
    }
    current
}

/// Whether `root` holds a project: any manifest, current or legacy.
pub fn is_project(root: &Path) -> bool {
    manifest_path(root).is_file()
}

const RECENTS: &str = "recents.json";
/// Long enough to be useful, short enough that the list stays scannable.
const MAX_RECENTS: usize = 12;

/// Everything needed to reopen a project.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    /// The project folder.
    pub path: String,
    /// The name shown in the title bar and the recents list.
    pub name: String,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Frame rate as an exact fraction; the decimal is never stored.
    pub rate_num: i64,
    /// Denominator of the frame rate.
    pub rate_den: i64,
    /// Milliseconds since the epoch.
    pub opened_at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    /// The version of the app that wrote the file, under whichever name the
    /// app had at the time. Informational: nothing reads it back.
    #[serde(alias = "wolfcut")]
    concat: String,
    name: String,
    video: Video,
    /// Everything else in the file is ignored when reading settings for the
    /// recents list. `flatten` keeps it from being dropped on a rewrite.
    #[serde(flatten)]
    #[allow(dead_code)]
    rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Video {
    width: u32,
    height: u32,
    rate_num: i64,
    rate_den: i64,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Creates the project folder and writes its manifest.
///
/// Refuses to touch a folder that already holds a manifest. Overwriting
/// someone's edit because they reused a name is not a recoverable mistake.
/// This compatibility wrapper returns a completed artifact and releases its
/// creator owner. Interactive creation should hand create_owned into Session.
pub fn create(
    location: &str,
    name: &str,
    width: u32,
    height: u32,
    rate_num: i64,
    rate_den: i64,
) -> Result<ProjectInfo, String> {
    create_owned(location, name, width, height, rate_num, rate_den)
        .map(CreatedProject::into_info)
        .map_err(|error| error.to_string())
}

/// Creates a fresh project while retaining creator ownership for template
/// filling and Session adoption. Rechecks every manifest after taking the lock.
pub fn create_owned(
    location: &str,
    name: &str,
    width: u32,
    height: u32,
    rate_num: i64,
    rate_den: i64,
) -> Result<CreatedProject, OwnedProjectError> {
    let root = Path::new(location).join(folder_name(name));
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;
    let owner = acquire_project_owner(&root)?;
    let root = owner
        .as_ref()
        .map(|owner| owner.identity().target().to_owned())
        .unwrap_or(root);
    finish_creation(root, name, width, height, rate_num, rate_den, owner)
}

/// Creates a full document at an exact, brand-new directory path. Unlike normal
/// project creation, an existing empty directory is also rejected. This is the
/// no-clobber boundary for repair previews and explicitly saved repair copies.
/// Ownership stays held from creation through the caller's Session adoption.
pub fn create_document_owned(
    path: &Path,
    settings: &concat_project::DocumentSettings,
    document: &serde_json::Value,
) -> Result<CreatedProject, OwnedProjectError> {
    std::fs::create_dir(path)
        .map_err(|error| format!("请选择尚不存在的新工程文件夹：{}：{error}", path.display()))?;
    let owner = acquire_project_owner(path)?;
    let root = owner
        .as_ref()
        .map(|owner| owner.identity().target().to_owned())
        .unwrap_or_else(|| path.to_owned());
    let created = finish_creation(
        root,
        &settings.name,
        settings.width,
        settings.height,
        settings.rate_num,
        settings.rate_den,
        owner,
    )?;
    created.save(document)?;
    Ok(created)
}

fn finish_creation(
    root: PathBuf,
    name: &str,
    width: u32,
    height: u32,
    rate_num: i64,
    rate_den: i64,
    owner: Option<WriterGuard>,
) -> Result<CreatedProject, OwnedProjectError> {
    validate_project_owner(&root.to_string_lossy(), owner.as_ref())?;
    // A contender may have observed an empty directory before another creator
    // wrote it. This check is deliberately *after* ownership acquisition.
    for filename in std::iter::once(MANIFEST).chain(LEGACY_MANIFESTS.iter().copied()) {
        match std::fs::symlink_metadata(root.join(filename)) {
            Ok(_) => {
                return Err(
                    format!("a Concat project already exists at {}", root.display()).into(),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect {}: {error}",
                    root.join(filename).display()
                )
                .into());
            }
        }
    }
    let manifest = root.join(MANIFEST);

    // Settings only: a fresh project has no edit yet. The full document -
    // timelines included - is written by the session's save from the first
    // change.
    let document = Manifest {
        concat: env!("CARGO_PKG_VERSION").to_owned(),
        name: name.to_owned(),
        video: Video {
            width,
            height,
            rate_num,
            rate_den,
        },
        rest: serde_json::Map::new(),
    };

    let encoded = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("could not encode the manifest: {error}"))?;
    // create_new never truncates an existing entry, including a dangling link
    // or unexpected file that arrived after the post-owner manifest check.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest)
        .map_err(|error| format!("could not create {}: {error}", manifest.display()))?;
    let written = std::io::Write::write_all(&mut file, &encoded).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        // Only the manifest this invocation successfully created is cleanup.
        let _ = std::fs::remove_file(&manifest);
        return Err(format!("could not write {}: {error}", manifest.display()).into());
    }

    Ok(CreatedProject {
        info: ProjectInfo {
            path: root.to_string_lossy().into_owned(),
            name: name.to_owned(),
            width,
            height,
            rate_num,
            rate_den,
            opened_at: now_millis(),
        },
        owner,
    })
}

/// Writes the whole project document to the project's manifest.
///
/// The document is passed through as opaque JSON rather than being mirrored
/// into Rust types: the engine (`concat-project`) owns the canonical model and
/// produced this document, and the host has no decisions to make about its
/// contents beyond writing it safely.
///
/// Written to a temporary file and renamed into place, because a save
/// interrupted halfway is worse than no save at all - a truncated manifest
/// loses the project, while a failed rename leaves the previous one intact.
/// This standalone entry acquires a writer; a live Session uses save_owned.
pub fn save(path: &str, document: &serde_json::Value) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| format!("could not create {path}: {error}"))?;
    let owner = acquire_project_owner(Path::new(path)).map_err(|error| error.to_string())?;
    save_owned(path, document, owner.as_ref()).map_err(|error| error.to_string())
}

/// Saves with the caller's already-held owner. Never reacquires a second writer
/// under an open Session. None is accepted only on native compatibility targets.
pub fn save_owned(
    path: &str,
    document: &serde_json::Value,
    owner: Option<&WriterGuard>,
) -> Result<(), OwnedProjectError> {
    save_guarded(path, document, owner, || true).map(|_| ())
}

fn save_guarded(
    path: &str,
    document: &serde_json::Value,
    owner: Option<&WriterGuard>,
    is_current: impl Fn() -> bool,
) -> Result<bool, OwnedProjectError> {
    validate_project_owner(path, owner)?;
    let root = PathBuf::from(path);
    let manifest = manifest_path(&root);
    let temporary = root.join(format!(
        "{}.saving-{}-{}",
        manifest
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(MANIFEST),
        std::process::id(),
        TEMPORARY_ID.fetch_add(1, Ordering::Relaxed),
    ));

    std::fs::create_dir_all(&root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;

    let encoded = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("could not encode the project: {error}"))?;

    // Written, then flushed to the disk, then renamed: a rename is only
    // atomic over bytes that have reached the platter. Without the sync a
    // power cut after the rename can leave a zero-length manifest.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    let written: Result<(), OwnedProjectError> = (|| {
        std::io::Write::write_all(&mut file, &encoded)
            .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("could not flush {}: {error}", temporary.display()))
            .map_err(OwnedProjectError::Project)
    })();
    drop(file);
    let result = written.and_then(|_| {
        if !is_current() {
            return Ok(false);
        }
        validate_project_owner(path, owner)?;
        std::fs::rename(&temporary, &manifest)
            .map_err(|error| format!("could not replace {}: {error}", manifest.display()))?;
        Ok(true)
    });
    if !matches!(&result, Ok(true)) {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// True when a document carries no edit state at all - the settings-only
/// manifest `create` writes, in a project closed before its first save.
/// Such a document is a legitimately empty project. A document that claims
/// edit state the build then cannot load is a different thing: corrupt.
pub fn is_settings_only(document: &serde_json::Value) -> bool {
    document.is_object()
        && ["timelines", "tracks", "clips", "media"]
            .iter()
            .all(|key| document.get(key).is_none())
}

/// Reads the whole project document back.
pub fn read_document(path: &str) -> Result<serde_json::Value, String> {
    read_document_at(&manifest_path(Path::new(path)))
}

/// Read the exact entry selected by Session::load_owned. Resolving the manifest
/// name again could load a different file than the Session records for saving.
pub(crate) fn read_document_at(manifest: &Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(manifest)
        .map_err(|error| format!("could not read {}: {error}", manifest.display()))?;

    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not a Concat project: {error}", manifest.display()))
}

/// Reads an existing project's settings.
pub fn open(path: &str) -> Result<ProjectInfo, String> {
    let root = PathBuf::from(path);
    let manifest = manifest_path(&root);

    let bytes = std::fs::read(&manifest)
        .map_err(|error| format!("could not read {}: {error}", manifest.display()))?;
    let document: Manifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not a Concat project: {error}", manifest.display()))?;

    if document.video.rate_den == 0 {
        return Err(format!("{} has an invalid frame rate", manifest.display()));
    }

    Ok(ProjectInfo {
        path: root.to_string_lossy().into_owned(),
        name: document.name,
        width: document.video.width,
        height: document.video.height,
        rate_num: document.video.rate_num,
        rate_den: document.video.rate_den,
        opened_at: now_millis(),
    })
}

/// Moves a project to the front of the recents list.
pub fn remember(config: &Path, project: &ProjectInfo) -> Result<(), String> {
    let mut entries = read_recents(config);
    entries.retain(|entry| !same_path(&entry.path, &project.path));
    entries.insert(0, project.clone());
    entries.truncate(MAX_RECENTS);
    write_recents(config, &entries)
}

/// The recents list, most recent first, with vanished folders dropped.
///
/// Filtering on read rather than pruning on write means a project on a drive
/// that happens to be unplugged comes back when the drive does.
pub fn list(config: &Path) -> Vec<ProjectInfo> {
    read_recents(config)
        .into_iter()
        .filter(|entry| is_project(Path::new(&entry.path)))
        .collect()
}

/// Removes one project from the list. The folder itself is left alone.
pub fn forget(config: &Path, path: &str) -> Result<(), String> {
    let mut entries = read_recents(config);
    entries.retain(|entry| !same_path(&entry.path, path));
    write_recents(config, &entries)
}

fn read_recents(config: &Path) -> Vec<ProjectInfo> {
    // A missing or corrupt list is not an error worth showing anyone; it just
    // means there is no history yet.
    std::fs::read(config.join(RECENTS))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_recents(config: &Path, entries: &[ProjectInfo]) -> Result<(), String> {
    std::fs::create_dir_all(config)
        .map_err(|error| format!("could not create {}: {error}", config.display()))?;

    let encoded = serde_json::to_vec_pretty(entries)
        .map_err(|error| format!("could not encode the recents list: {error}"))?;
    std::fs::write(config.join(RECENTS), encoded)
        .map_err(|error| format!("could not write the recents list: {error}"))
}

/// Compares paths case-insensitively, because Windows does.
fn same_path(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Turns a project name into something a filesystem will accept. Also used
/// for template bundle folders and the files inside them.
pub fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
                '-'
            } else {
                character
            }
        })
        .collect();

    // Windows refuses names ending in a dot or a space.
    let trimmed = cleaned.trim_matches(['.', ' ']);
    if trimmed.is_empty() {
        "Untitled project".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Removes all cached artwork and waveforms from a project folder.
///
/// The cache is entirely regenerable, so the whole folder goes, subfolders
/// (`art/`, peaks and all) included. `remove_file` alone would skip them:
/// it refuses directories.
pub fn clear_cache(path: &str) -> Result<usize, String> {
    let cache_dir = Path::new(path).join("cache");
    if !cache_dir.exists() {
        return Ok(0);
    }
    let count = count_files(&cache_dir);
    std::fs::remove_dir_all(&cache_dir)
        .map_err(|error| format!("cannot remove {}: {error}", cache_dir.display()))?;
    Ok(count)
}

/// Recursively counts the files under a directory, for the clear-cache toast.
fn count_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { count_files(&path) } else { 1 }
        })
        .sum()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn save_scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "concat-save-lane-{}-{}",
            std::process::id(),
            TEMPORARY_ID.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).expect("unique scratch directory");
        root
    }

    #[test]
    fn project_relink_create_document_preserves_full_copy_and_source_manifest() {
        let scratch = save_scratch();
        let source =
            create_owned(&scratch.to_string_lossy(), "Source", 1280, 720, 30000, 1001).unwrap();
        let source_document = serde_json::json!({
            "concat": "0.1.0", "version": 1, "name": "Source",
            "video": {"width": 1280, "height": 720, "rateNum": 30000, "rateDen": 1001},
            "media": [{"id": "m7", "path": "/missing/original.png", "kind": "image", "futureMedia": 7}],
            "timelines": [{"id": "TL1", "name": "Edited timeline", "tracks": [{"id":"T1"}],
                "clips": [{"id":"c9","mediaId":"m7","trackId":"T1","kind":"image","duration":5}]}],
            "futureProject": {"keep": true}
        });
        source.save(&source_document).unwrap();
        let source_manifest = manifest_path(Path::new(&source.info().path));
        let source_bytes = std::fs::read(&source_manifest).unwrap();
        let settings = concat_project::DocumentSettings {
            name: "Explicit repair copy".into(),
            width: 1280,
            height: 720,
            rate_num: 30000,
            rate_den: 1001,
        };
        let mut copy_document = source_document.clone();
        copy_document["name"] = serde_json::json!(settings.name);
        copy_document["media"][0]["path"] = serde_json::json!("/managed/fixed.png");
        copy_document["media"][0]["seecutLibraryBinding"] = serde_json::json!({
            "version":1,"asset_id":"asset-1","registration_id":"operation:original",
            "sha256":"a".repeat(64),"previous_path":"/missing/original.png","verified_original":false
        });
        let exact_target = scratch.join("chosen-copy-directory");
        let created = create_document_owned(&exact_target, &settings, &copy_document).unwrap();
        assert_eq!(
            Path::new(&created.info().path),
            exact_target.canonicalize().unwrap()
        );
        assert_eq!(created.info().name, settings.name);
        assert_eq!(
            (
                created.info().width,
                created.info().height,
                created.info().rate_num,
                created.info().rate_den
            ),
            (1280, 720, 30000, 1001)
        );
        assert_eq!(read_document(&created.info().path).unwrap(), copy_document);
        assert_eq!(std::fs::read(&source_manifest).unwrap(), source_bytes);
        assert!(
            create_document_owned(Path::new(&source.info().path), &settings, &copy_document)
                .is_err()
        );
        assert_eq!(std::fs::read(&source_manifest).unwrap(), source_bytes);
        drop(created);
        drop(source);
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn project_relink_create_document_rejects_existing_empty_directory_and_file() {
        let scratch = save_scratch();
        let settings = concat_project::DocumentSettings {
            name: "Copy".into(),
            width: 16,
            height: 16,
            rate_num: 30,
            rate_den: 1,
        };
        let document = serde_json::json!({"name":"Copy","futureField":true});
        let empty = scratch.join("existing-empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(create_document_owned(&empty, &settings, &document).is_err());
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);
        let file = scratch.join("existing-file");
        std::fs::write(&file, b"untouched existing file").unwrap();
        assert!(create_document_owned(&file, &settings, &document).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"untouched existing file");
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_relink_create_document_rejects_existing_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;
        let scratch = save_scratch();
        let settings = concat_project::DocumentSettings {
            name: "Copy".into(),
            width: 16,
            height: 16,
            rate_num: 30,
            rate_den: 1,
        };
        let document = serde_json::json!({"name":"Copy"});
        let actual = scratch.join("actual-directory");
        std::fs::create_dir(&actual).unwrap();
        let sentinel = actual.join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        let link = scratch.join("target-link");
        symlink(&actual, &link).unwrap();
        assert!(create_document_owned(&link, &settings, &document).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert!(!actual.join(MANIFEST).exists());
        let absent = scratch.join("absent-directory");
        let dangling = scratch.join("dangling-link");
        symlink(&absent, &dangling).unwrap();
        assert!(create_document_owned(&dangling, &settings, &document).is_err());
        assert!(!absent.exists());
        assert!(
            std::fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn project_relink_invalidate_waits_for_dispatched_writer_and_discards_its_snapshot() {
        let scratch = save_scratch();
        let path = scratch.to_string_lossy().into_owned();
        let owner = acquire_project_owner(&scratch).unwrap();
        let original = serde_json::json!({"source":"unchanged"});
        save_owned(&path, &original, owner.as_ref()).unwrap();
        let before = std::fs::read(scratch.join(MANIFEST)).unwrap();
        let lane = std::sync::Arc::new(SaveLane::default());
        let ticket = lane.next();
        let (entered, inside) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let writer_lane = lane.clone();
        let writer_path = path.clone();
        let writer_owner = owner.clone();
        let writer = std::thread::spawn(move || {
            writer_lane.write_with(
                ticket,
                &writer_path,
                &serde_json::json!({"stale":"dispatched"}),
                writer_owner.as_ref(),
                || {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                },
            )
        });
        inside.recv().unwrap();
        let invalidating_lane = lane.clone();
        let (finished, done) = std::sync::mpsc::channel();
        let invalidation = std::thread::spawn(move || {
            invalidating_lane.invalidate_and_wait();
            finished.send(()).unwrap();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while lane.is_latest(ticket) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let was_invalidated = !lane.is_latest(ticket);
        let waited_for_writer =
            matches!(done.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        release.send(()).unwrap();
        let saved = writer.join().unwrap().unwrap();
        invalidation.join().unwrap();
        assert!(
            was_invalidated,
            "invalidation must reserve a newer ticket before waiting"
        );
        assert!(
            waited_for_writer,
            "invalidation must not return with an active writer"
        );
        assert!(!saved, "the invalidated dispatched snapshot must not write");
        assert_eq!(std::fs::read(scratch.join(MANIFEST)).unwrap(), before);
        assert_eq!(read_document(&path).unwrap(), original);
        drop(owner);
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn project_relink_invalidate_discards_queued_snapshot_and_accepts_later_save() {
        let scratch = save_scratch();
        let path = scratch.to_string_lossy().into_owned();
        let owner = acquire_project_owner(&scratch).unwrap();
        save_owned(
            &path,
            &serde_json::json!({"source":"unchanged"}),
            owner.as_ref(),
        )
        .unwrap();
        let before = std::fs::read(scratch.join(MANIFEST)).unwrap();
        let lane = std::sync::Arc::new(SaveLane::default());
        let ticket = lane.next();
        let held_writer = lane.writer.lock().unwrap();
        let queued_lane = lane.clone();
        let queued_path = path.clone();
        let queued_owner = owner.clone();
        let (dispatched, started) = std::sync::mpsc::channel();
        let queued = std::thread::spawn(move || {
            dispatched.send(()).unwrap();
            queued_lane.write_owned(
                ticket,
                &queued_path,
                &serde_json::json!({"stale":"queued"}),
                queued_owner.as_ref(),
            )
        });
        started.recv().unwrap();
        let invalidating_lane = lane.clone();
        let invalidation = std::thread::spawn(move || invalidating_lane.invalidate_and_wait());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while lane.is_latest(ticket) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let was_invalidated = !lane.is_latest(ticket);
        drop(held_writer);
        let saved = queued.join().unwrap().unwrap();
        invalidation.join().unwrap();
        assert!(was_invalidated);
        assert!(
            !saved,
            "queued snapshots reserved before invalidation remain obsolete"
        );
        assert_eq!(std::fs::read(scratch.join(MANIFEST)).unwrap(), before);
        let fresh = lane.next();
        assert!(
            lane.write_owned(
                fresh,
                &path,
                &serde_json::json!({"fresh":true}),
                owner.as_ref()
            )
            .unwrap()
        );
        assert_eq!(read_document(&path).unwrap()["fresh"], true);
        drop(owner);
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn delayed_old_save_cannot_replace_newer_document() {
        let root = save_scratch();
        let path = root.to_string_lossy().into_owned();
        let lane = std::sync::Arc::new(SaveLane::default());
        let owner = acquire_project_owner(&root).unwrap();
        let old_owner = owner.clone();
        let old_ticket = lane.next();
        let (release, wait) = std::sync::mpsc::channel();
        let old_lane = lane.clone();
        let old_path = path.clone();
        let old = std::thread::spawn(move || {
            wait.recv().expect("release old save");
            old_lane.write_owned(
                old_ticket,
                &old_path,
                &serde_json::json!({"version": "old"}),
                old_owner.as_ref(),
            )
        });

        let new_ticket = lane.next();
        assert!(
            lane.write_owned(
                new_ticket,
                &path,
                &serde_json::json!({"version": "new"}),
                owner.as_ref()
            )
            .unwrap()
        );
        release.send(()).unwrap();
        assert!(!old.join().unwrap().unwrap());
        assert_eq!(read_document(&path).unwrap()["version"], "new");
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn close_style_save_waits_for_in_flight_writer() {
        let root = save_scratch();
        let path = root.to_string_lossy().into_owned();
        let lane = std::sync::Arc::new(SaveLane::default());
        let owner = acquire_project_owner(&root).unwrap();
        let old_owner = owner.clone();
        let old_ticket = lane.next();
        let (entered, inside) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let old_lane = lane.clone();
        let old_path = path.clone();
        let old = std::thread::spawn(move || {
            old_lane.write_with(
                old_ticket,
                &old_path,
                &serde_json::json!({"version": "old"}),
                old_owner.as_ref(),
                || {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                },
            )
        });
        inside.recv().unwrap();

        let close_ticket = lane.next();
        let close_lane = lane.clone();
        let close_path = path.clone();
        let close_owner = owner.clone();
        let (started, starting) = std::sync::mpsc::channel();
        let (finished, result) = std::sync::mpsc::channel();
        let close = std::thread::spawn(move || {
            started.send(()).unwrap();
            let saved = close_lane.write_owned(
                close_ticket,
                &close_path,
                &serde_json::json!({"version": "closed"}),
                close_owner.as_ref(),
            );
            finished.send(saved).unwrap();
        });
        starting.recv().unwrap();
        assert!(
            result.try_recv().is_err(),
            "close must wait for the older writer"
        );
        release.send(()).unwrap();
        assert!(!old.join().unwrap().unwrap());
        close.join().unwrap();
        assert!(result.recv().unwrap().unwrap());
        assert_eq!(read_document(&path).unwrap()["version"], "closed");
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_late_creator_rechecks_current_and_legacy_after_acquiring_owner() {
        for legacy in [false, true] {
            let scratch = save_scratch();
            let root = scratch.join("Shared");
            std::fs::create_dir(&root).unwrap();
            assert!(
                !is_project(&root),
                "the late creator's original observation"
            );
            let first =
                create_owned(&scratch.to_string_lossy(), "Shared", 1920, 1080, 30, 1).unwrap();
            if legacy {
                std::fs::rename(root.join(MANIFEST), root.join(LEGACY_MANIFESTS[0])).unwrap();
            }
            let manifest = manifest_path(&root);
            let before = std::fs::read(&manifest).unwrap();
            let _artifact = first.into_info();
            // The contender now gets ownership, but the earlier empty-folder
            // observation must not authorize truncating the winner's manifest.
            let late_owner = acquire_project_owner(&root).unwrap();
            let late = finish_creation(root, "Replacement", 1, 1, 1, 1, late_owner);
            assert!(matches!(late, Err(OwnedProjectError::Project(_))));
            assert_eq!(std::fs::read(&manifest).unwrap(), before);
            std::fs::remove_dir_all(scratch).unwrap();
        }
    }

    #[test]
    fn initial_creation_never_follows_or_truncates_an_existing_manifest_entry() {
        let scratch = save_scratch();
        let root = scratch.join("Existing");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join(MANIFEST)).unwrap();
        assert!(create_owned(&scratch.to_string_lossy(), "Existing", 1, 1, 1, 1).is_err());
        assert!(root.join(MANIFEST).is_dir());
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn sanitises_names_windows_would_reject() {
        assert_eq!(folder_name("My: Project?"), "My- Project-");
        assert_eq!(folder_name("  trailing.  "), "trailing");
        assert_eq!(folder_name("..."), "Untitled project");
        assert_eq!(folder_name(""), "Untitled project");
    }

    #[test]
    fn a_legacy_manifest_keeps_its_name() {
        for legacy in LEGACY_MANIFESTS {
            let scratch = std::env::temp_dir().join(format!("concat-legacy-{legacy}-test"));
            let _ = std::fs::remove_dir_all(&scratch);
            std::fs::create_dir_all(&scratch).expect("scratch dir");
            // A fresh folder resolves to the current name...
            assert!(manifest_path(&scratch).ends_with(MANIFEST));
            assert!(!is_project(&scratch));
            // ...a pre-rename project keeps the manifest it has, forever...
            std::fs::write(scratch.join(legacy), b"{}").expect("writes");
            assert!(manifest_path(&scratch).ends_with(legacy));
            assert!(is_project(&scratch));
            // ...and the current name wins only where it actually exists.
            std::fs::write(scratch.join(MANIFEST), b"{}").expect("writes");
            assert!(manifest_path(&scratch).ends_with(MANIFEST));
            let _ = std::fs::remove_dir_all(&scratch);
        }
    }

    #[test]
    fn a_manifest_under_any_of_its_names_opens() {
        let scratch = std::env::temp_dir().join("concat-manifest-key-test");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        for key in ["concat", "wolfcut"] {
            let document = serde_json::json!({
                key: "0.2.0",
                "name": "Old",
                "video": { "width": 1920, "height": 1080, "rateNum": 30, "rateDen": 1 },
            });
            std::fs::write(
                scratch.join(MANIFEST),
                serde_json::to_vec(&document).unwrap(),
            )
            .expect("writes");
            let info = open(&scratch.to_string_lossy()).expect("opens");
            assert_eq!((info.width, info.height, info.rate_num), (1920, 1080, 30));
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_fresh_manifest_is_settings_only_and_a_saved_edit_is_not() {
        // Exactly what `create` writes: settings, no edit state.
        let fresh = serde_json::json!({
            "concat": "0.2.0",
            "name": "Untitled project",
            "video": { "width": 1080, "height": 1920, "rateNum": 30, "rateDen": 1 },
        });
        assert!(is_settings_only(&fresh));
        // Any edit state present means the document must load or fail loudly.
        assert!(!is_settings_only(&serde_json::json!({ "tracks": [] })));
        assert!(!is_settings_only(
            &serde_json::json!({ "timelines": "garbage" })
        ));
        assert!(!is_settings_only(&serde_json::json!("nonsense")));
    }

    #[test]
    fn paths_compare_case_insensitively() {
        assert!(same_path("D:\\Work\\Film", "d:\\work\\film"));
        assert!(!same_path("D:\\Work\\Film", "D:\\Work\\Other"));
    }
}

#[cfg(all(
    test,
    any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
mod agent_save_tests {
    use super::*;
    use crate::session::Session;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct Fixture {
        scratch: PathBuf,
        session: Option<Session>,
        lane: Arc<SaveLane>,
    }

    impl Fixture {
        fn new() -> Self {
            let mut fixture = Self::settings_only();
            fixture.session.as_mut().unwrap().save(None).unwrap();
            fixture
        }
        fn settings_only() -> Self {
            let scratch = std::env::temp_dir().join(format!(
                "concat-agent-save-{}-{}",
                std::process::id(),
                TEMPORARY_ID.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir(&scratch).unwrap();
            let created =
                create_owned(&scratch.to_string_lossy(), "Original", 1280, 720, 30, 1).unwrap();
            let session = created.into_session().unwrap();
            Self {
                scratch,
                session: Some(session),
                lane: Arc::new(SaveLane::default()),
            }
        }
        fn path(&self) -> PathBuf {
            PathBuf::from(self.session.as_ref().unwrap().path())
        }
        fn freeze(&mut self) -> (AgentSaveSnapshot, SaveRevoker) {
            self.session
                .as_mut()
                .unwrap()
                .prepare_agent_save(&self.lane, Instant::now() + Duration::from_secs(60))
                .unwrap()
        }
        fn edit(&mut self) {
            self.session
                .as_mut()
                .unwrap()
                .apply(concat_project::Command::AddTrack)
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.session = None;
            let _ = std::fs::remove_dir_all(&self.scratch);
        }
    }

    fn verified(result: AgentSaveResult) -> VerifiedAgentSave {
        match result {
            AgentSaveResult::Verified(proof) => proof,
            result => panic!("expected actual verified save, got {result:?}"),
        }
    }

    #[test]
    fn agent_real_session_save_retains_original_owner_then_reopens() {
        let mut f = Fixture::new();
        f.edit();
        let path = f.path();
        let expected = f.session.as_ref().unwrap().document();
        let expected_project = f.session.as_ref().unwrap().project().clone();
        let (snapshot, revoke) = f.freeze();
        let ticket = snapshot.ticket();
        let lane = Arc::clone(&f.lane);
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            lane.write_agent_save_with(snapshot, |phase| {
                if phase == AgentSaveBoundary::AfterClaim {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                Ok(())
            })
        });
        inside.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(f.session.take());
        assert_eq!(revoke.state(), SaveGateState::Committing);
        let info = open(&path.to_string_lossy()).unwrap();
        // The Session has dropped; only the worker's exact original clone owns it.
        assert!(Session::open_info_owned(&info).is_err());
        release.send(()).unwrap();
        let proof = verified(worker.join().unwrap());
        assert_eq!(proof.ticket(), ticket);
        assert_eq!(proof.manifest(), path.join(MANIFEST));
        assert_eq!(proof.snapshot_sha256(), proof.manifest_sha256());
        assert_eq!(proof.manifest_sha256().len(), 64);
        assert_eq!(
            proof.bytes(),
            std::fs::metadata(path.join(MANIFEST)).unwrap().len()
        );
        assert_eq!(proof.project(), &expected_project);
        assert_eq!(proof.document_version(), concat_project::DOCUMENT_VERSION);
        assert_eq!(revoke.state(), SaveGateState::Finished);
        let reopened = Session::open_info_owned(&info).unwrap();
        assert_eq!(reopened.document(), expected);
        assert_eq!(reopened.project(), &expected_project);
        assert_eq!(
            proof.engine_document_version(),
            concat_project::DOCUMENT_VERSION
        );
        assert!(proof.project_items() <= MAX_AGENT_PROJECT_ITEMS);
        drop(reopened);
    }

    #[test]
    fn agent_settings_only_first_save_reopens_the_same_empty_engine_project() {
        let mut f = Fixture::settings_only();
        let path = f.path();
        assert!(is_settings_only(
            &read_document(&path.to_string_lossy()).unwrap()
        ));
        let original = f.session.as_ref().unwrap().project().clone();
        assert!(original.active().clips.is_empty());
        let (snapshot, _) = f.freeze();
        let proof = verified(f.lane.write_agent_save(snapshot));
        assert_eq!(
            proof.engine_document_version(),
            concat_project::DOCUMENT_VERSION
        );
        drop(f.session.take());
        let reopened = Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap();
        assert_eq!(reopened.project(), &original);
        drop(reopened);
    }

    #[test]
    fn agent_actual_file_engine_reader_migrates_flat_and_rejects_future_or_corrupt() {
        let mut f = Fixture::new();
        f.edit();
        let session = f.session.as_ref().unwrap();
        let expected = session.project().clone();
        let mut flat = session.document();
        flat.as_object_mut().unwrap().remove("timelines");
        flat.as_object_mut().unwrap().remove("activeTimelineId");
        let path = f.path();
        let owner = session.writer_guard();
        // The real engine must lift the flat tracks/clips/video into a timeline.
        save_owned(&path.to_string_lossy(), &flat, owner.as_ref()).unwrap();
        let bytes = read_agent_manifest(&path.join(MANIFEST)).unwrap();
        let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(actual.get("timelines").is_none());
        assert_eq!(parse_agent_project(&actual).unwrap().0, expected);
        let mut future = session.document();
        future["version"] = serde_json::json!(concat_project::DOCUMENT_VERSION + 1);
        let bad = serde_json::json!({"version": concat_project::DOCUMENT_VERSION, "timelines": []});
        let future_settings_only = serde_json::json!({"version": concat_project::DOCUMENT_VERSION + 1, "name":"future empty"});
        for document in [future, bad, future_settings_only] {
            save_owned(&path.to_string_lossy(), &document, owner.as_ref()).unwrap();
            let bytes = read_agent_manifest(&path.join(MANIFEST)).unwrap();
            let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(parse_agent_project(&actual).is_err());
        }
        drop(owner);
    }

    #[test]
    fn agent_encoded_json_depth_is_checked_before_publish_and_preserves_old_legacy() {
        for depth in [120, 123] {
            let mut f = Fixture::settings_only();
            let path = f.path();
            let manifest = path.join(MANIFEST);
            drop(f.session.take());
            let mut extra = serde_json::json!(0);
            for _ in 0..depth {
                extra = serde_json::json!({"child": extra});
            }
            let legacy = serde_json::json!({
                "concat": "0.1.0", "version": 1, "name": "Legacy deep project",
                "video": {"width":1280,"height":720,"rateNum":30,"rateDen":1},
                "tracks": [{"id":"T1","deep":extra}], "clips": [], "media": []
            });
            let original_bytes = serde_json::to_vec_pretty(&legacy).unwrap();
            // Actual default byte parser and actual Session/Editor open succeed.
            assert!(serde_json::from_slice::<serde_json::Value>(&original_bytes).is_ok());
            std::fs::write(&manifest, &original_bytes).unwrap();
            f.session =
                Some(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap());
            let original_project = f.session.as_ref().unwrap().project().clone();
            let (snapshot, revoke) = f.freeze();
            // The old Value-only preflight would accept BOTH depths. The
            // migrated timeline wrapper makes encoded depth123 unreadable.
            assert_eq!(
                concat_project::from_document(&snapshot.document).unwrap(),
                original_project
            );
            let encoded = encode_agent_document(&snapshot.document).unwrap();
            assert!(encoded.len() < MAX_AGENT_SAVE_BYTES);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&encoded).is_ok(),
                depth == 120
            );
            // A valid legacy file remains readable without rewriting it, even
            // when the current writer would exceed the default nesting limit.
            let read = f
                .session
                .as_ref()
                .unwrap()
                .prepare_persisted_read(&f.lane)
                .unwrap();
            assert_eq!(
                f.lane.read_persisted_clip(read).unwrap().project(),
                &original_project
            );
            assert_eq!(revoke.state(), SaveGateState::Pending);
            let result = f.lane.write_agent_save(snapshot);
            if depth == 123 {
                assert!(!result.commit_claimed());
                assert!(!result.rename_attempted());
                assert!(matches!(result, AgentSaveResult::NotCommitted {
                    commit_claimed: false, rename_attempted: false,
                    reason: AgentSaveNotCommitted::Engine(ref error), ..
                } if error.contains("recursion limit exceeded")));
                assert_eq!(std::fs::read(&manifest).unwrap(), original_bytes);
                assert_ne!(revoke.state(), SaveGateState::Committing);
                assert!(!std::fs::read_dir(&path).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains(".saving-")
                }));
            } else {
                verified(result);
            }
            drop(f.session.take());
            let reopened =
                Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap();
            assert_eq!(reopened.project(), &original_project);
            drop(reopened);
        }
    }

    #[test]
    fn agent_engine_settle_content_loss_cannot_be_reported_as_saved() {
        let mut f = Fixture::new();
        let before = std::fs::read(f.path().join(MANIFEST)).unwrap();
        f.edit();
        let (mut snapshot, _) = f.freeze();
        // Private fixture fault: simulate a writer/read compatibility bug.
        // JSON still parses, but the real engine drops this invalid track.
        snapshot.document["timelines"][0]["tracks"][0]["id"] = serde_json::json!("");
        let read_project = concat_project::from_document(&snapshot.document).unwrap();
        assert_ne!(read_project, snapshot.project);
        assert!(matches!(
            f.lane.write_agent_save(snapshot),
            AgentSaveResult::NotCommitted {
                commit_claimed: false,
                reason: AgentSaveNotCommitted::Engine(_),
                ..
            }
        ));
        assert_eq!(std::fs::read(f.path().join(MANIFEST)).unwrap(), before);
    }

    #[test]
    fn agent_frozen_engine_primary_item_count_is_bounded() {
        let mut f = Fixture::new();
        let before = std::fs::read(f.path().join(MANIFEST)).unwrap();
        let (mut snapshot, _) = f.freeze();
        snapshot.project.active_mut().tracks.resize(
            MAX_AGENT_PROJECT_ITEMS + 1,
            concat_project::model::Track::default(),
        );
        assert!(matches!(
            f.lane.write_agent_save(snapshot),
            AgentSaveResult::NotCommitted {
                commit_claimed: false,
                reason: AgentSaveNotCommitted::ResourceLimit,
                ..
            }
        ));
        assert_eq!(std::fs::read(f.path().join(MANIFEST)).unwrap(), before);
    }

    #[test]
    fn agent_revocation_at_real_pre_cas_boundary_prevents_rename() {
        for reason in [
            SaveInvalidation::Revoked,
            SaveInvalidation::AccountChanged,
            SaveInvalidation::ConversationChanged,
            SaveInvalidation::DocumentChanged,
            SaveInvalidation::RevisionChanged,
            SaveInvalidation::BindingUnstable,
            SaveInvalidation::Stopped,
        ] {
            let mut f = Fixture::new();
            let manifest = f.path().join(MANIFEST);
            let before = std::fs::read(&manifest).unwrap();
            f.edit();
            let (snapshot, revoke) = f.freeze();
            let lane = Arc::clone(&f.lane);
            let (entered, inside) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                lane.write_agent_save_with(snapshot, |phase| {
                    if phase == AgentSaveBoundary::BeforeClaim {
                        entered.send(()).unwrap();
                        wait.recv().unwrap();
                    }
                    Ok(())
                })
            });
            inside.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(revoke.invalidate(reason), RevokeSaveResult::Invalidated);
            // An actual newer Session edit occurs only after source invalidation.
            if reason == SaveInvalidation::RevisionChanged {
                f.edit();
            }
            release.send(()).unwrap();
            assert!(
                matches!(worker.join().unwrap(), AgentSaveResult::NotCommitted {
                reason: AgentSaveNotCommitted::Permit(SavePermitRefusal::Invalidated(actual)), ..
            } if actual == reason)
            );
            assert_eq!(std::fs::read(&manifest).unwrap(), before);
            assert!(!std::fs::read_dir(f.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".saving-")
            }));
        }
    }

    #[test]
    fn agent_new_ticket_at_real_pre_cas_boundary_wins_even_after_latest_precheck() {
        let mut f = Fixture::new();
        let path = f.path();
        let before = std::fs::read(path.join(MANIFEST)).unwrap();
        f.edit();
        let (snapshot, revoke) = f.freeze();
        let lane = Arc::clone(&f.lane);
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            lane.write_agent_save_with(snapshot, |phase| {
                if phase == AgentSaveBoundary::BeforeClaim {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                Ok(())
            })
        });
        inside.recv_timeout(Duration::from_secs(5)).unwrap();
        let newest = f.lane.next(); // Must return while worker still owns IO mutex.
        assert!(f.lane.is_latest(newest));
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Superseded)
        );
        release.send(()).unwrap();
        assert!(matches!(
            worker.join().unwrap(),
            AgentSaveResult::NotCommitted {
                reason: AgentSaveNotCommitted::Permit(SavePermitRefusal::Invalidated(
                    SaveInvalidation::Superseded
                )),
                ..
            }
        ));
        assert_eq!(std::fs::read(path.join(MANIFEST)).unwrap(), before);
    }

    #[test]
    fn agent_after_cas_revocation_does_not_cancel_and_new_manual_save_finishes_later() {
        let mut f = Fixture::new();
        f.edit();
        let (snapshot, revoke) = f.freeze();
        let original_ticket = snapshot.ticket();
        let lane = Arc::clone(&f.lane);
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            lane.write_agent_save_with(snapshot, |phase| {
                if phase == AgentSaveBoundary::AfterClaim {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                Ok(())
            })
        });
        inside.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            revoke.invalidate(SaveInvalidation::Revoked),
            RevokeSaveResult::CommitAlreadyStarted
        );
        f.edit();
        let session = f.session.as_mut().unwrap();
        let (path, new_document) = session.prepare_save(None);
        let expected = new_document.clone();
        let owner = session.writer_guard();
        let newest = f.lane.next();
        let close_lane = Arc::clone(&f.lane);
        let (finished, done) = mpsc::channel();
        let closing = std::thread::spawn(move || {
            let result = close_lane.write_owned(newest, &path, &new_document, owner.as_ref());
            finished.send(result).unwrap();
        });
        assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.send(()).unwrap();
        let proof = verified(worker.join().unwrap());
        assert_eq!(proof.ticket(), original_ticket);
        closing.join().unwrap();
        assert!(done.recv().unwrap().unwrap());
        assert_eq!(
            read_document(&f.path().to_string_lossy()).unwrap(),
            expected
        );
        assert_eq!(revoke.state(), SaveGateState::Finished);
        assert!(!f.lane.is_latest(original_ticket));
    }

    #[test]
    fn agent_post_cas_deadline_is_known_not_committed_but_preserves_claim_fact() {
        let mut f = Fixture::new();
        let manifest = f.path().join(MANIFEST);
        let before = std::fs::read(&manifest).unwrap();
        f.edit();
        let expires = Instant::now() + Duration::from_secs(2);
        let (snapshot, revoke) = f
            .session
            .as_mut()
            .unwrap()
            .prepare_agent_save(&f.lane, expires)
            .unwrap();
        let result = f.lane.write_agent_save_with(snapshot, |phase| {
            if phase == AgentSaveBoundary::ImmediatelyAfterClaim {
                assert_eq!(revoke.state(), SaveGateState::Committing);
                std::thread::sleep(expires.saturating_duration_since(Instant::now()));
            }
            Ok(())
        });
        assert!(result.commit_claimed());
        assert!(!result.rename_attempted());
        assert!(!result.rename_succeeded());
        assert!(matches!(
            result,
            AgentSaveResult::NotCommitted {
                commit_claimed: true,
                rename_attempted: false,
                reason: AgentSaveNotCommitted::ExpiredAfterClaim,
                ..
            }
        ));
        assert_eq!(revoke.state(), SaveGateState::Finished);
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
    }

    #[test]
    fn agent_legacy_manifest_is_fixed_and_no_current_manifest_is_created() {
        let mut f = Fixture::new();
        let path = f.path();
        drop(f.session.take());
        std::fs::rename(path.join(MANIFEST), path.join("wolfcut.json")).unwrap();
        f.session =
            Some(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap());
        f.edit();
        let expected = f.session.as_ref().unwrap().document();
        let (snapshot, _) = f.freeze();
        let proof = verified(f.lane.write_agent_save(snapshot));
        assert_eq!(proof.manifest(), path.join("wolfcut.json"));
        assert!(!path.join(MANIFEST).exists());
        assert_eq!(read_document(&path.to_string_lossy()).unwrap(), expected);
    }

    #[test]
    fn agent_original_session_manifest_is_fixed_even_before_worker_starts() {
        let mut f = Fixture::new();
        let path = f.path();
        drop(f.session.take());
        std::fs::rename(path.join(MANIFEST), path.join("wolfcut.json")).unwrap();
        f.session =
            Some(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap());
        let old = std::fs::read(path.join("wolfcut.json")).unwrap();
        f.edit();
        let (snapshot, _) = f.freeze();
        std::fs::write(
            path.join(MANIFEST),
            b"new name appeared while worker queued",
        )
        .unwrap();
        assert!(matches!(
            f.lane.write_agent_save(snapshot),
            AgentSaveResult::NotCommitted {
                commit_claimed: false,
                reason: AgentSaveNotCommitted::ManifestChanged,
                ..
            }
        ));
        assert_eq!(std::fs::read(path.join("wolfcut.json")).unwrap(), old);
        assert_eq!(
            std::fs::read(path.join(MANIFEST)).unwrap(),
            b"new name appeared while worker queued"
        );
    }

    #[test]
    fn agent_session_fixed_file_reader_does_not_select_a_second_manifest() {
        let mut f = Fixture::new();
        let path = f.path();
        drop(f.session.take());
        std::fs::rename(path.join(MANIFEST), path.join("wolfcut.json")).unwrap();
        let selected = manifest_path(&path);
        let original = read_document_at(&selected).unwrap();
        std::fs::write(
            path.join(MANIFEST),
            b"a later candidate must not replace the selected read",
        )
        .unwrap();
        assert_eq!(selected, path.join("wolfcut.json"));
        assert_eq!(read_document_at(&selected).unwrap(), original);
        assert!(read_document(&path.to_string_lossy()).is_err());
    }

    #[test]
    fn agent_manifest_selection_change_is_not_silently_retargeted() {
        let mut f = Fixture::new();
        let path = f.path();
        drop(f.session.take());
        std::fs::rename(path.join(MANIFEST), path.join("wolfcut.json")).unwrap();
        f.session =
            Some(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap());
        let before = std::fs::read(path.join("wolfcut.json")).unwrap();
        f.edit();
        let (snapshot, _) = f.freeze();
        let result = f.lane.write_agent_save_with(snapshot, |phase| {
            if phase == AgentSaveBoundary::BeforeClaim {
                std::fs::write(path.join(MANIFEST), b"new external manifest").unwrap();
            }
            Ok(())
        });
        assert!(matches!(
            result,
            AgentSaveResult::NotCommitted {
                reason: AgentSaveNotCommitted::ManifestChanged,
                ..
            }
        ));
        assert_eq!(std::fs::read(path.join("wolfcut.json")).unwrap(), before);
        assert_eq!(
            std::fs::read(path.join(MANIFEST)).unwrap(),
            b"new external manifest"
        );
    }

    #[test]
    fn agent_stale_original_owner_is_rejected_before_claim() {
        let mut f = Fixture::new();
        let path = f.path();
        let before = std::fs::read(path.join(MANIFEST)).unwrap();
        f.edit();
        let (snapshot, _) = f.freeze();
        let result = f.lane.write_agent_save_with(snapshot, |phase| {
            if phase == AgentSaveBoundary::BeforeClaim {
                let lock = path.join(".seecut-writer.lock");
                std::fs::remove_file(&lock).unwrap();
                std::fs::write(lock, b"replaced sidecar").unwrap();
            }
            Ok(())
        });
        assert!(matches!(
            result,
            AgentSaveResult::NotCommitted {
                reason: AgentSaveNotCommitted::Preparation(OwnedProjectError::Ownership(_)),
                ..
            }
        ));
        assert_eq!(std::fs::read(path.join(MANIFEST)).unwrap(), before);
    }

    #[test]
    fn agent_real_rename_failure_stays_started_unverified() {
        let mut f = Fixture::new();
        let manifest = f.path().join(MANIFEST);
        f.edit();
        let (snapshot, revoke) = f.freeze();
        let result = f.lane.write_agent_save_with(snapshot, |phase| {
            if phase == AgentSaveBoundary::AfterClaim {
                std::fs::remove_file(&manifest).unwrap();
                std::fs::create_dir(&manifest).unwrap();
            }
            Ok(())
        });
        assert!(matches!(
            result,
            AgentSaveResult::StartedUnverified {
                stage: AgentSaveStage::Rename,
                rename_attempted: true,
                rename_succeeded: false,
                ..
            }
        ));
        assert_eq!(revoke.state(), SaveGateState::Finished);
    }

    #[test]
    fn agent_parent_directory_sync_failure_stays_started_unverified() {
        let mut f = Fixture::new();
        let path = f.path();
        let moved = f.scratch.join("moved-after-rename");
        f.edit();
        let (snapshot, _) = f.freeze();
        let result = f.lane.write_agent_save_with(snapshot, |phase| {
            if phase == AgentSaveBoundary::AfterRename {
                std::fs::rename(&path, &moved).unwrap();
            }
            Ok(())
        });
        assert!(matches!(
            result,
            AgentSaveResult::StartedUnverified {
                stage: AgentSaveStage::DirectorySync,
                rename_attempted: true,
                rename_succeeded: true,
                ..
            }
        ));
        assert!(moved.join(MANIFEST).is_file());
    }

    #[test]
    fn agent_readback_reopens_actual_file_and_rejects_corruption_or_growth() {
        for oversize in [false, true] {
            let mut f = Fixture::new();
            let manifest = f.path().join(MANIFEST);
            f.edit();
            let (snapshot, _) = f.freeze();
            let result = f.lane.write_agent_save_with(snapshot, |phase| {
                if phase == AgentSaveBoundary::BeforeReadback {
                    if oversize {
                        std::fs::OpenOptions::new()
                            .write(true)
                            .open(&manifest)
                            .unwrap()
                            .set_len(MAX_AGENT_SAVE_BYTES as u64 + 1)
                            .unwrap();
                    } else {
                        std::fs::write(&manifest, b"not json from actual filesystem").unwrap();
                    }
                }
                Ok(())
            });
            assert!(matches!(
                result,
                AgentSaveResult::StartedUnverified {
                    stage: AgentSaveStage::Readback,
                    rename_succeeded: true,
                    ..
                }
            ));
        }
    }

    #[test]
    fn agent_wrong_lane_and_expired_permit_never_rename() {
        let mut f = Fixture::new();
        let before = std::fs::read(f.path().join(MANIFEST)).unwrap();
        f.edit();
        let (snapshot, _) = f.freeze();
        assert!(matches!(
            SaveLane::default().write_agent_save(snapshot),
            AgentSaveResult::NotCommitted {
                reason: AgentSaveNotCommitted::Permit(SavePermitRefusal::WrongLane),
                ..
            }
        ));
        let (expired, revoke) = f
            .session
            .as_mut()
            .unwrap()
            .prepare_agent_save(&f.lane, Instant::now())
            .unwrap();
        assert!(matches!(
            f.lane.write_agent_save(expired),
            AgentSaveResult::NotCommitted {
                reason: AgentSaveNotCommitted::Permit(SavePermitRefusal::Invalidated(
                    SaveInvalidation::Expired
                )),
                ..
            }
        ));
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Expired)
        );
        assert_eq!(std::fs::read(f.path().join(MANIFEST)).unwrap(), before);
    }

    #[test]
    fn agent_session_mutation_entries_invalidate_before_the_real_commit() {
        for action in [
            "apply",
            "outcome",
            "within",
            "video",
            "gesture",
            "undo",
            "redo",
            "historyUndo",
            "historyRedo",
            "name",
        ] {
            let mut f = Fixture::new();
            f.edit();
            if matches!(action, "redo" | "historyRedo") {
                f.session.as_mut().unwrap().undo();
            }
            let path = f.path();
            let before = std::fs::read(path.join(MANIFEST)).unwrap();
            let (snapshot, revoke) = f.freeze();
            let lane = Arc::clone(&f.lane);
            let (entered, inside) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                lane.write_agent_save_with(snapshot, |phase| {
                    if phase == AgentSaveBoundary::BeforeClaim {
                        entered.send(()).unwrap();
                        wait.recv().unwrap();
                    }
                    Ok(())
                })
            });
            inside.recv_timeout(Duration::from_secs(5)).unwrap();
            let session = f.session.as_mut().unwrap();
            match action {
                "apply" => {
                    session.apply(concat_project::Command::AddTrack).unwrap();
                }
                "outcome" => {
                    session
                        .apply_outcome(concat_project::Command::AddTrack)
                        .unwrap();
                }
                "within" => {
                    session
                        .apply_within(Some("test"), concat_project::Command::AddTrack)
                        .unwrap();
                }
                "video" => {
                    session
                        .set_video(concat_project::model::VideoSettings {
                            width: 640,
                            height: 480,
                            rate_num: 25,
                            rate_den: 1,
                        })
                        .unwrap();
                }
                "gesture" => session.end_gesture(),
                "undo" => {
                    session.undo();
                }
                "redo" => {
                    session.redo();
                }
                "historyUndo" => {
                    session.history_outcome(false);
                }
                "historyRedo" => {
                    session.history_outcome(true);
                }
                "name" => {
                    session.prepare_save(Some("Changed"));
                }
                _ => unreachable!(),
            }
            let expected = if action == "gesture" {
                SaveInvalidation::BindingUnstable
            } else {
                SaveInvalidation::RevisionChanged
            };
            assert_eq!(
                revoke.state(),
                SaveGateState::Invalidated(expected),
                "{action}"
            );
            release.send(()).unwrap();
            assert!(
                matches!(worker.join().unwrap(), AgentSaveResult::NotCommitted { commit_claimed:false, reason:AgentSaveNotCommitted::Permit(SavePermitRefusal::Invalidated(actual)), .. } if actual == expected)
            );
            assert_eq!(std::fs::read(path.join(MANIFEST)).unwrap(), before);
        }
    }

    #[test]
    fn agent_session_drop_before_claim_invalidates_without_waiting_for_io() {
        let mut f = Fixture::new();
        f.edit();
        let path = f.path();
        let before = std::fs::read(path.join(MANIFEST)).unwrap();
        let (snapshot, revoke) = f.freeze();
        let lane = Arc::clone(&f.lane);
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            lane.write_agent_save_with(snapshot, |phase| {
                if phase == AgentSaveBoundary::BeforeClaim {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                Ok(())
            })
        });
        inside.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(f.session.take()); // Worker still holds the IO lock; Drop cannot wait on it.
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::DocumentChanged)
        );
        assert!(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).is_err());
        release.send(()).unwrap();
        assert!(matches!(
            worker.join().unwrap(),
            AgentSaveResult::NotCommitted {
                commit_claimed: false,
                ..
            }
        ));
        assert_eq!(std::fs::read(path.join(MANIFEST)).unwrap(), before);
        let reopened = Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap();
        drop(reopened);
    }

    #[test]
    fn agent_new_session_save_and_manual_save_do_not_revive_old_handles() {
        let mut f = Fixture::new();
        f.edit();
        let (old, old_revoke) = f.freeze();
        let (new, new_revoke) = f.freeze();
        assert_eq!(
            old_revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Superseded)
        );
        assert!(matches!(
            f.lane.write_agent_save(old),
            AgentSaveResult::NotCommitted { .. }
        ));
        assert_eq!(new_revoke.state(), SaveGateState::Pending);
        verified(f.lane.write_agent_save(new));
        let (pending, revoke) = f.freeze();
        f.session.as_mut().unwrap().save_owned(None).unwrap();
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::Superseded)
        );
        assert!(matches!(
            f.lane.write_agent_save(pending),
            AgentSaveResult::NotCommitted { .. }
        ));
    }

    #[test]
    fn agent_reads_and_fresh_persisted_reads_do_not_revoke_or_allocate_tickets() {
        let mut f = Fixture::new();
        let disk =
            concat_project::from_document(&read_document(&f.path().to_string_lossy()).unwrap())
                .unwrap();
        f.edit();
        let memory = f.session.as_ref().unwrap().project().clone();
        let (save, revoke) = f.freeze();
        let ticket = save.ticket();
        let session = f.session.as_mut().unwrap();
        let same_name = session.settings().name;
        let _ = session.project();
        let _ = session.settings();
        let _ = session.video();
        let _ = session.view();
        let _ = session.document();
        let _ = session.peek_undo();
        let _ = session.peek_redo();
        let _ = session.can_undo();
        let _ = session.can_redo();
        let _ = session.flattened_clips();
        let _ = session.writer_guard();
        session.prepare_save(None);
        session.prepare_save(Some("   "));
        session.prepare_save(Some(&same_name));
        let read = session.prepare_persisted_read(&f.lane).unwrap();
        assert_eq!(revoke.state(), SaveGateState::Pending);
        assert!(f.lane.is_latest(ticket));
        let actual = f.lane.read_persisted_clip(read).unwrap();
        assert_eq!(actual.project(), &disk);
        assert_ne!(actual.project(), &memory);
        assert_eq!(revoke.state(), SaveGateState::Pending);
        assert!(f.lane.is_latest(ticket));
        let saved = verified(f.lane.write_agent_save(save));
        assert_eq!(saved.project(), &memory);
        let fresh = f
            .session
            .as_ref()
            .unwrap()
            .prepare_persisted_read(&f.lane)
            .unwrap();
        assert_eq!(
            f.lane.read_persisted_clip(fresh).unwrap().project(),
            saved.project()
        );
    }

    #[test]
    fn agent_persisted_settings_only_uses_actual_disk_settings_not_memory_video() {
        let mut f = Fixture::settings_only();
        let path = f.path();
        f.session
            .as_mut()
            .unwrap()
            .set_video(concat_project::model::VideoSettings {
                width: 1920,
                height: 1080,
                rate_num: 60,
                rate_den: 1,
            })
            .unwrap();
        let disk = serde_json::json!({"wolfcut":"0.1.0", "version":0, "name":"Disk only", "video":{"width":640,"height":480,"rateNum":25,"rateDen":1}});
        let bytes = serde_json::to_vec_pretty(&disk).unwrap();
        std::fs::write(path.join(MANIFEST), &bytes).unwrap();
        let read = f
            .session
            .as_ref()
            .unwrap()
            .prepare_persisted_read(&f.lane)
            .unwrap();
        let actual = f.lane.read_persisted_clip(read).unwrap();
        assert_eq!(
            actual.project().active().video,
            concat_project::model::VideoSettings {
                width: 640,
                height: 480,
                rate_num: 25,
                rate_den: 1
            }
        );
        assert_eq!(f.session.as_ref().unwrap().video().width, 1920);
        assert_eq!(actual.document_version(), 0);
        assert_eq!(actual.manifest_name(), MANIFEST);
        assert_eq!(actual.bytes(), bytes.len() as u64);
    }

    #[test]
    fn agent_persisted_legacy_version_and_project_come_from_actual_changed_disk() {
        use sha2::{Digest, Sha256};
        let mut f = Fixture::new();
        let path = f.path();
        drop(f.session.take());
        std::fs::rename(path.join(MANIFEST), path.join("wolfcut.json")).unwrap();
        f.session =
            Some(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap());
        f.edit();
        let memory = f.session.as_ref().unwrap().project().clone();
        let disk = serde_json::json!({"wolfcut":"0.1.0", "version":0, "name":"Disk flat", "video":{"width":800,"height":600,"rateNum":24,"rateDen":1}, "tracks":[{"id":"disk-track"}], "clips":[], "media":[]});
        let bytes = serde_json::to_vec_pretty(&disk).unwrap();
        std::fs::write(path.join("wolfcut.json"), &bytes).unwrap();
        let expected = concat_project::from_document(&disk).unwrap();
        let read = f
            .session
            .as_ref()
            .unwrap()
            .prepare_persisted_read(&f.lane)
            .unwrap();
        let actual = f.lane.read_persisted_clip(read).unwrap();
        assert_eq!(actual.project(), &expected);
        assert_ne!(actual.project(), &memory);
        assert_eq!(actual.document_version(), 0);
        assert_eq!(actual.manifest_name(), "wolfcut.json");
        assert_eq!(
            actual.manifest_sha256(),
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(actual.bytes(), bytes.len() as u64);
    }

    #[test]
    fn agent_persisted_read_waits_same_lane_and_observes_real_published_save() {
        let mut f = Fixture::new();
        f.edit();
        let (save, revoke) = f.freeze();
        let ticket = save.ticket();
        let lane = Arc::clone(&f.lane);
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            lane.write_agent_save_with(save, |phase| {
                if phase == AgentSaveBoundary::AfterClaim {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                Ok(())
            })
        });
        inside.recv_timeout(Duration::from_secs(5)).unwrap();
        let read = f
            .session
            .as_ref()
            .unwrap()
            .prepare_persisted_read(&f.lane)
            .unwrap();
        assert_eq!(revoke.state(), SaveGateState::Committing);
        assert!(f.lane.is_latest(ticket));
        let lane = Arc::clone(&f.lane);
        let (finished, done) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let result = lane.read_persisted_clip(read);
            finished.send(result).unwrap();
        });
        assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.send(()).unwrap();
        let proof = verified(writer.join().unwrap());
        reader.join().unwrap();
        let actual = done.recv().unwrap().unwrap();
        assert_eq!(actual.project(), proof.project());
        assert_eq!(actual.manifest_sha256(), proof.manifest_sha256());
        assert_eq!(actual.document_version(), proof.document_version());
        assert!(f.lane.is_latest(ticket));
    }

    #[test]
    fn agent_persisted_read_rejects_wrong_lane_changed_owner_target_and_bad_files() {
        for failure in [
            "lane", "owner", "target", "missing", "json", "future", "corrupt", "settings", "rate",
            "depth", "oversize", "symlink", "items",
        ] {
            let mut f = Fixture::new();
            let path = f.path();
            if failure == "target" {
                drop(f.session.take());
                std::fs::rename(path.join(MANIFEST), path.join("wolfcut.json")).unwrap();
                f.session = Some(
                    Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap(),
                );
            }
            let read = f
                .session
                .as_ref()
                .unwrap()
                .prepare_persisted_read(&f.lane)
                .unwrap();
            let valid = serde_json::json!({"concat":"0.1.0", "name":"Disk", "video":{"width":640,"height":480,"rateNum":25,"rateDen":1}});
            match failure {
                "owner" => {
                    let lock = path.join(".seecut-writer.lock");
                    std::fs::remove_file(&lock).unwrap();
                    std::fs::write(lock, b"changed").unwrap();
                }
                "target" => {
                    std::fs::write(path.join(MANIFEST), b"new candidate").unwrap();
                }
                "missing" => {
                    std::fs::remove_file(path.join(MANIFEST)).unwrap();
                }
                "json" => {
                    std::fs::write(path.join(MANIFEST), b"not json").unwrap();
                }
                "future" => {
                    let mut value = valid.clone();
                    value["version"] = serde_json::json!(concat_project::DOCUMENT_VERSION + 1);
                    std::fs::write(path.join(MANIFEST), serde_json::to_vec(&value).unwrap())
                        .unwrap();
                }
                "corrupt" => {
                    let mut value = valid.clone();
                    value["timelines"] = serde_json::json!([]);
                    std::fs::write(path.join(MANIFEST), serde_json::to_vec(&value).unwrap())
                        .unwrap();
                }
                "settings" => {
                    std::fs::write(
                        path.join(MANIFEST),
                        br#"{"concat":"0.1.0","name":"missing video"}"#,
                    )
                    .unwrap();
                }
                "rate" => {
                    let mut value = valid.clone();
                    value["video"]["rateDen"] = serde_json::json!(0);
                    std::fs::write(path.join(MANIFEST), serde_json::to_vec(&value).unwrap())
                        .unwrap();
                }
                "depth" => {
                    let mut value = valid.clone();
                    let mut extra = serde_json::json!(0);
                    for _ in 0..130 {
                        extra = serde_json::json!({"child":extra});
                    }
                    value["extra"] = extra;
                    std::fs::write(path.join(MANIFEST), serde_json::to_vec(&value).unwrap())
                        .unwrap();
                }
                "oversize" => {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(path.join(MANIFEST))
                        .unwrap()
                        .set_len(MAX_AGENT_SAVE_BYTES as u64 + 1)
                        .unwrap();
                }
                "symlink" => {
                    let other = f.scratch.join("other.json");
                    std::fs::write(&other, serde_json::to_vec(&valid).unwrap()).unwrap();
                    std::fs::remove_file(path.join(MANIFEST)).unwrap();
                    std::os::unix::fs::symlink(other, path.join(MANIFEST)).unwrap();
                }
                "items" => {
                    let mut value = valid.clone();
                    value["tracks"] = serde_json::json!(
                        (0..MAX_AGENT_PROJECT_ITEMS)
                            .map(|id| serde_json::json!({"id":format!("T{id}")}))
                            .collect::<Vec<_>>()
                    );
                    std::fs::write(path.join(MANIFEST), serde_json::to_vec(&value).unwrap())
                        .unwrap();
                }
                "lane" => {}
                _ => unreachable!(),
            }
            let result = if failure == "lane" {
                SaveLane::default().read_persisted_clip(read)
            } else {
                f.lane.read_persisted_clip(read)
            };
            assert!(result.is_err(), "{failure}: {result:?}");
        }
    }

    #[test]
    fn agent_read_snapshot_retains_original_owner_without_reviving_dropped_session_save() {
        let mut f = Fixture::new();
        let path = f.path();
        f.edit();
        let (save, revoke) = f.freeze();
        let read = f
            .session
            .as_ref()
            .unwrap()
            .prepare_persisted_read(&f.lane)
            .unwrap();
        drop(f.session.take());
        assert_eq!(
            revoke.state(),
            SaveGateState::Invalidated(SaveInvalidation::DocumentChanged)
        );
        assert!(Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).is_err());
        let actual = f.lane.read_persisted_clip(read).unwrap();
        assert_eq!(actual.manifest_name(), MANIFEST);
        assert!(matches!(
            f.lane.write_agent_save(save),
            AgentSaveResult::NotCommitted { .. }
        ));
        let reopened = Session::open_info_owned(&open(&path.to_string_lossy()).unwrap()).unwrap();
        drop(reopened);
    }

    #[test]
    fn agent_actual_reader_rejects_short_grown_and_replaced_manifest_entries() {
        for change in ["short", "grown", "replaced"] {
            let f = Fixture::new();
            let manifest = f.path().join(MANIFEST);
            let result = read_agent_manifest_with(&manifest, || match change {
                "short" => std::fs::OpenOptions::new()
                    .write(true)
                    .open(&manifest)
                    .unwrap()
                    .set_len(0)
                    .unwrap(),
                "grown" => {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&manifest)
                        .unwrap()
                        .write_all(b" ")
                        .unwrap();
                }
                "replaced" => {
                    let replacement = f.path().join("replacement.json");
                    std::fs::write(&replacement, std::fs::read(&manifest).unwrap()).unwrap();
                    std::fs::rename(replacement, &manifest).unwrap();
                }
                _ => unreachable!(),
            });
            assert!(result.is_err(), "{change}: {result:?}");
        }
    }

    #[test]
    fn agent_encoding_bound_does_not_limit_existing_manual_saves() {
        assert!(matches!(
            encode_agent_document(&serde_json::json!({"large": "x".repeat(MAX_AGENT_SAVE_BYTES)})),
            Err(AgentSaveNotCommitted::ResourceLimit)
        ));
        // The opt-in path's limit is not added to save_owned/write_owned.
        let mut f = Fixture::new();
        let owner = f.session.as_ref().unwrap().writer_guard();
        let path = f.path();
        let document = serde_json::json!({"large": "x".repeat(MAX_AGENT_SAVE_BYTES)});
        let ticket = f.lane.next();
        assert!(
            f.lane
                .write_owned(ticket, &path.to_string_lossy(), &document, owner.as_ref())
                .unwrap()
        );
        assert_eq!(read_document(&path.to_string_lossy()).unwrap(), document);
        drop(owner);
        drop(f.session.take());
    }
}
