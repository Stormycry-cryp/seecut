//! Worker-only bounded per-COTO-session journal. Unix file leases, no UI disk I/O.
#[cfg(not(unix))]
compile_error!(
    "AppControl journal requires Unix flock and file identity; gate the AppControl modules and adapter on unsupported targets"
);
use crate::agent_app_control_core::*;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

// Only Journal can create/activate this state after acquiring the real flock.
struct LeaseState {
    generation: uuid::Uuid,
    writer_live: AtomicBool,
    execution_live: AtomicBool,
}
#[derive(Clone)]
pub(crate) struct LeaseOwner {
    state: Weak<LeaseState>,
}
impl LeaseOwner {
    pub(crate) fn check_writer(&self) -> Result<(), Error> {
        let state = self.state.upgrade().ok_or(Error::Busy)?;
        if !state.writer_live.load(Ordering::Acquire) {
            return Err(Error::Busy);
        }
        Ok(())
    }
    pub(crate) fn check_execution(&self) -> Result<(), Error> {
        self.check_writer()?;
        let state = self.state.upgrade().ok_or(Error::Busy)?;
        if !state.execution_live.load(Ordering::Acquire) {
            return Err(Error::Busy);
        }
        Ok(())
    }
    pub(crate) fn same_writer(&self, other: &Self) -> Result<(), Error> {
        self.check_writer()?;
        other.check_writer()?;
        let a = self.state.upgrade().ok_or(Error::Busy)?;
        let b = other.state.upgrade().ok_or(Error::Busy)?;
        if !Arc::ptr_eq(&a, &b) || a.generation != b.generation {
            return Err(Error::QueryOnly);
        }
        Ok(())
    }
    pub(crate) fn fence_execution(&self) {
        if let Some(state) = self.state.upgrade() {
            state.execution_live.store(false, Ordering::Release);
        }
    }
}
/// Revocation only: host can invalidate pending execution without any UI I/O.
/// No public constructor or reactivation; the original Journal retains the flock.
pub struct ExecutionFence {
    owner: LeaseOwner,
}
impl ExecutionFence {
    pub fn fence(&self) {
        self.owner.fence_execution();
    }
}

#[derive(Clone, Debug)]
pub struct QueryScope {
    pub account_scope: String,
    pub client_id: String,
    pub session_id: String,
}
/// May admit only newly created records during this actual lease lifetime.
pub struct Prepared {
    record: Record,
    fresh: bool,
    owner: LeaseOwner,
}
impl Prepared {
    pub(crate) fn into_fresh(self) -> Result<(Record, LeaseOwner), Error> {
        if self.fresh {
            self.owner.check_execution()?;
            Ok((self.record, self.owner))
        } else {
            Err(Error::QueryOnly)
        }
    }
}
pub struct Saved {
    record: Record,
    owner: LeaseOwner,
}
impl Saved {
    pub(crate) fn into_record(self) -> Result<(Record, LeaseOwner), Error> {
        self.owner.check_writer()?;
        Ok((self.record, self.owner))
    }
    /// Consume a real file+directory barrier receipt for query metadata only.
    /// This never grants execution and rejects receipts from a dropped writer.
    pub fn into_recovery(self) -> Result<RecoveryView, Error> {
        let (record, _owner) = self.into_record()?;
        Ok(RecoveryView::new(record, true))
    }
}
pub struct RecoveryView {
    durable: bool,
    recorded: Record,
    effective_status: Status,
    execution_recoverable: bool,
}
impl RecoveryView {
    fn new(recorded: Record, durable: bool) -> Self {
        let effective_status = if recorded.status == Status::Running
            || (!durable && recorded.status == Status::Completed)
        {
            Status::OutcomeUnknown
        } else {
            recorded.status
        };
        Self {
            recorded,
            effective_status,
            execution_recoverable: false,
            durable,
        }
    }
    /// The exact original record covered by this view's barrier metadata.
    pub fn recorded(&self) -> &Record {
        &self.recorded
    }
    pub fn effective_status(&self) -> Status {
        self.effective_status
    }
    pub fn execution_recoverable(&self) -> bool {
        self.execution_recoverable
    }
    /// True only after consuming a successful same-writer Saved capability.
    pub fn is_durable(&self) -> bool {
        self.durable
    }
}
pub struct Journal {
    path: PathBuf,
    scope: QueryScope,
    _lease: File,
    owner: Arc<LeaseState>,
    #[cfg(test)]
    fault: Option<Fault>,
}
#[cfg(test)]
#[derive(Clone, Copy)]
pub enum Fault {
    BeforePublish,
    AfterPublish,
    DirectorySync,
}

impl Journal {
    #[cfg(test)]
    pub(crate) fn test_file(&self, id: &str) -> PathBuf {
        self.file(id)
    }
    /// Root is a trusted host path. Account/client/original sid isolate capacity.
    /// Holding real flock on the open FD, not PID files, owns this journal writer.
    pub fn open(root: &Path, scope: QueryScope) -> Result<Self, Error> {
        if scope.account_scope.len() != 64
            || !scope
                .account_scope
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || operation_id(&scope.client_id, &scope.session_id, 1).is_err()
        {
            return Err(Error::InvalidInput);
        }
        directory(root)?;
        let mut path = root.canonicalize().map_err(|_| Error::Io)?;
        for part in [
            &scope.account_scope,
            &digest(scope.client_id.as_bytes()),
            &digest(scope.session_id.as_bytes()),
        ] {
            path.push(part);
            directory(&path)?;
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path.join("writer.lock"))
            .map_err(|_| Error::Io)?;
        // SAFETY: lease FD remains open until this Journal drops; no pointer access.
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::Busy);
        }
        Ok(Self {
            path,
            scope,
            _lease: lease,
            owner: Arc::new(LeaseState {
                generation: uuid::Uuid::new_v4(),
                writer_live: AtomicBool::new(true),
                execution_live: AtomicBool::new(true),
            }),
            #[cfg(test)]
            fault: None,
        })
    }
    fn owner_capability(&self) -> LeaseOwner {
        LeaseOwner {
            state: Arc::downgrade(&self.owner),
        }
    }
    /// UI retains this at open, including while prepare is still in flight.
    pub fn execution_fence(&self) -> ExecutionFence {
        ExecutionFence {
            owner: self.owner_capability(),
        }
    }
    fn scoped(&self, record: &Record) -> Result<(), Error> {
        record.validate()?;
        if record.intent.authority.account_scope != self.scope.account_scope
            || record.intent.authority.client_id != self.scope.client_id
            || record.intent.session_id != self.scope.session_id
        {
            return Err(Error::NotAuthorized);
        }
        Ok(())
    }
    fn file(&self, id: &str) -> PathBuf {
        self.path.join(format!("{}.json", digest(id.as_bytes())))
    }
    pub fn read(&self, id: &str) -> Result<Record, Error> {
        self.assert_lease()?;
        if id.len() > 256 {
            return Err(Error::InvalidInput);
        }
        let path = self.file(id);
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| Error::Io)?;
        if !file.metadata().map_err(|_| Error::Io)?.is_file() {
            return Err(Error::Corrupt);
        }
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|_| Error::Io)?;
        if data.len() > MAX_RECORD_BYTES {
            return Err(Error::ResourceLimit);
        }
        let record: Record = serde_json::from_slice(&data).map_err(|_| Error::Corrupt)?;
        self.scoped(&record)?;
        if record.intent.operation_id != id {
            return Err(Error::Corrupt);
        }
        Ok(record)
    }
    /// Query never checks current page, instance, AppSession or control token.
    /// Access adapter still requires trusted current same-account/client read grant.
    pub fn query(&self, id: &str) -> Result<RecoveryView, Error> {
        Ok(RecoveryView::new(self.read(id)?, false))
    }
    /// Worker-only recovery barrier for the original record, with no dispatch.
    /// Even readable/equal JSON must pass save's file+directory synchronization.
    pub fn confirm_query(&mut self, id: &str) -> Result<Saved, Error> {
        let original = self.query(id)?.recorded;
        self.save(original)
    }
    pub fn prepare(&mut self, intent: Intent) -> Result<Prepared, Error> {
        let record = Record::queued(intent)?;
        self.scoped(&record)?;
        let path = self.file(&record.intent.operation_id);
        if path.try_exists().map_err(|_| Error::Io)? {
            let old = self.read(&record.intent.operation_id)?;
            if old.intent_sha256 != record.intent_sha256 {
                return Err(Error::Conflict);
            }
            return Ok(Prepared {
                record: old,
                fresh: false,
                owner: self.owner_capability(),
            });
        }
        self.publish(&record, true)?;
        Ok(Prepared {
            record,
            fresh: true,
            owner: self.owner_capability(),
        })
    }
    /// Consume the unique unadmitted fresh capability and durably settle queued v1.
    /// An execution fence still permits this writer-only cleanup; no execution is granted.
    pub fn cancel_prepared(&mut self, prepared: Prepared) -> Result<Saved, Error> {
        self.assert_lease()?;
        self.owner_capability().same_writer(&prepared.owner)?;
        if !prepared.fresh {
            return Err(Error::QueryOnly);
        }
        let mut record = prepared.record;
        self.scoped(&record)?;
        if record.status != Status::Queued || record.revision != 1 {
            return Err(Error::Conflict);
        }
        if self.read(&record.intent.operation_id)? != record {
            return Err(Error::Conflict);
        }
        // The capability itself proves no Registry accepted or dispatched this request.
        // Use the existing save path for validation, file and parent-directory barriers.
        record.status = Status::Cancelled;
        record.revision = 2;
        self.save(record)
    }
    pub fn persist_running(&mut self, write: RunningWrite) -> Result<RunningProof, Error> {
        self.assert_lease()?;
        self.owner_capability().same_writer(&write.owner)?;
        if write.record.status != Status::Running || write.record.revision != 2 {
            return Err(Error::InvalidInput);
        }
        self.save(write.record.clone())?;
        Ok(RunningProof {
            record: write.record,
            ticket: write.ticket,
            owner: write.owner,
        })
    }
    pub fn save(&mut self, record: Record) -> Result<Saved, Error> {
        self.scoped(&record)?;
        let old = self.read(&record.intent.operation_id)?;
        if old.intent_sha256 != record.intent_sha256 {
            return Err(Error::Conflict);
        }
        if old == record {
            self.confirm_durable(&record.intent.operation_id)?;
            return Ok(Saved {
                record,
                owner: self.owner_capability(),
            });
        }
        // The worker serial queue writes stage-running before any later cancel.
        // No revision is reused, including cancellation in the dispatch window.
        if record.revision != old.revision + 1
            || !matches!(
                (old.status, record.status),
                (Status::Queued, Status::Running | Status::Cancelled)
                    | (
                        Status::Running,
                        Status::Running
                            | Status::Completed
                            | Status::Cancelled
                            | Status::OutcomeUnknown
                    )
                    | (Status::OutcomeUnknown, Status::Completed)
            )
        {
            return Err(Error::Conflict);
        }
        self.publish(&record, false)?;
        Ok(Saved {
            record,
            owner: self.owner_capability(),
        })
    }
    fn publish(&mut self, record: &Record, new: bool) -> Result<(), Error> {
        self.assert_lease()?;
        let data = serde_json::to_vec(record).map_err(|_| Error::Corrupt)?;
        if data.len() > MAX_RECORD_BYTES {
            return Err(Error::ResourceLimit);
        }
        let target = self.file(&record.intent.operation_id);
        let mut count = 0usize;
        let mut total = 0usize;
        for entry in fs::read_dir(&self.path).map_err(|_| Error::Io)? {
            let entry = entry.map_err(|_| Error::Io)?;
            let meta = entry.file_type().map_err(|_| Error::Io)?;
            if !meta.is_file() || meta.is_symlink() {
                return Err(Error::Corrupt);
            }
            if entry.file_name() == "writer.lock" {
                continue;
            }
            count = count.checked_add(1).ok_or(Error::ResourceLimit)?;
            let len = entry.metadata().map_err(|_| Error::Io)?.len();
            total = total
                .checked_add(usize::try_from(len).map_err(|_| Error::ResourceLimit)?)
                .ok_or(Error::ResourceLimit)?;
            if count > MAX_OPERATIONS || total > MAX_SESSION_BYTES {
                return Err(Error::ResourceLimit);
            }
        }
        if new && count >= MAX_OPERATIONS {
            return Err(Error::ResourceLimit);
        }
        // Reserve the temporary's full bytes before replacing an existing file.
        if total
            .checked_add(data.len())
            .is_none_or(|n| n > MAX_SESSION_BYTES)
        {
            return Err(Error::ResourceLimit);
        }
        let temporary = self.path.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(|_| Error::Io)?;
            file.write_all(&data).map_err(|_| Error::Io)?;
            file.sync_all().map_err(|_| Error::Io)?;
            #[cfg(test)]
            if matches!(self.fault, Some(Fault::BeforePublish)) {
                self.fault = None;
                return Err(Error::Io);
            }
            if new {
                fs::hard_link(&temporary, &target).map_err(|_| Error::Io)?;
            } else {
                fs::rename(&temporary, &target).map_err(|_| Error::Io)?;
            }
            #[cfg(test)]
            if matches!(self.fault, Some(Fault::AfterPublish)) {
                self.fault = None;
                return Err(Error::Unknown);
            }
            self.sync_directory()?;
            self.assert_lease()?;
            Ok(())
        })();
        // Only our unpublished temporary is removed; original records never evict.
        let _ = fs::remove_file(temporary);
        result
    }
    #[cfg(test)]
    pub fn inject_fault(&mut self, fault: Fault) {
        self.fault = Some(fault);
    }
    fn assert_lease(&self) -> Result<(), Error> {
        self.owner_capability().check_writer()?;
        let valid = (|| {
            let path =
                fs::symlink_metadata(self.path.join("writer.lock")).map_err(|_| Error::Busy)?;
            let fd = self._lease.metadata().map_err(|_| Error::Busy)?;
            if !path.is_file()
                || path.file_type().is_symlink()
                || fd.dev() != path.dev()
                || fd.ino() != path.ino()
            {
                return Err(Error::Busy);
            }
            Ok(())
        })();
        if valid.is_err() {
            self.invalidate_writer();
        }
        valid
    }
    fn invalidate_writer(&self) {
        self.owner.execution_live.store(false, Ordering::Release);
        self.owner.writer_live.store(false, Ordering::Release);
    }
    fn sync_directory(&mut self) -> Result<(), Error> {
        #[cfg(test)]
        if matches!(self.fault, Some(Fault::DirectorySync)) {
            self.fault = None;
            return Err(Error::Unknown);
        }
        File::open(&self.path)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| Error::Unknown)
    }
    fn confirm_durable(&mut self, id: &str) -> Result<(), Error> {
        self.assert_lease()?;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.file(id))
            .and_then(|file| file.sync_all())
            .map_err(|_| Error::Unknown)?;
        self.sync_directory()?;
        self.assert_lease()
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        self.invalidate_writer();
    }
}
fn directory(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.permissions().mode() & 0o077 == 0 =>
        {
            Ok(())
        }
        Ok(_) => Err(Error::Corrupt),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| Error::Io),
        Err(_) => Err(Error::Io),
    }
}
