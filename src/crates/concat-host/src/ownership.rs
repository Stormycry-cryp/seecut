// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Cooperative, nonblocking writer ownership. Normal release leaves sidecars
//! in place. Callers must reuse their existing guard for an already-open identity.
//! This is not a defense against a same-user process continually replacing
//! directories or sidecars: std has no directory-relative no-follow open API.

use std::fmt;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Whether native project sessions use the audited desktop ownership path.
/// Other native platforms keep their existing editing behavior without an OS
/// writer guard; this does not enable or grant any MCP writing capability.
pub const fn desktop_writer_ownership_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))
}

#[derive(Debug)]
/// A failed writer acquisition or filesystem identity resolution.
pub enum OwnershipError {
    /// An independent writer already holds the target's OS lock.
    Conflict {
        /// The canonical project or canvas target.
        target: PathBuf,
    },
    /// The requested document or parent has an invalid type or is missing.
    InvalidTarget {
        /// The entry that failed validation.
        target: PathBuf,
        /// The validation rule that failed.
        reason: &'static str,
    },
    /// An unsupported symlink, hard link, or special filesystem entry.
    UnsupportedAlias {
        /// The entry with the unsupported alias or type.
        target: PathBuf,
        /// The alias or type rule that failed.
        reason: &'static str,
    },
    /// The resolved directory or sidecar changed identity before acquisition.
    IdentityAmbiguous {
        /// The entry whose identity changed.
        target: PathBuf,
        /// The consistency check that failed.
        reason: &'static str,
    },
    /// The platform or backing filesystem cannot provide the required lock.
    UnsupportedLock {
        /// The requested lock entry.
        target: PathBuf,
        /// The unsupported-platform or OS-lock error.
        source: io::Error,
    },
    /// A filesystem operation failed for a reason other than writer conflict.
    Io {
        /// The entry involved in the failed operation.
        target: PathBuf,
        /// The underlying filesystem error.
        source: io::Error,
    },
}

impl fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { target } => write!(f, "another writer owns {}", target.display()),
            Self::InvalidTarget { target, reason } => {
                write!(f, "invalid target {}: {reason}", target.display())
            }
            Self::UnsupportedAlias { target, reason } => {
                write!(f, "unsupported alias {}: {reason}", target.display())
            }
            Self::IdentityAmbiguous { target, reason } => {
                write!(f, "ambiguous identity {}: {reason}", target.display())
            }
            Self::UnsupportedLock { target, source } => write!(
                f,
                "unsupported writer lock for {}: {source}",
                target.display()
            ),
            Self::Io { target, source } => {
                write!(f, "ownership I/O for {}: {source}", target.display())
            }
        }
    }
}

impl std::error::Error for OwnershipError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::UnsupportedLock { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Project,
    Canvas,
}

/// Equality uses OS identities, including the stable lock inode. Constructing
/// an identity may create its private lock directory and empty sidecar.
#[derive(Clone, Debug)]
pub struct ResourceIdentity {
    kind: Kind,
    target: PathBuf,
    root: PathBuf,
    root_id: FileIdentity,
    lock_directory: PathBuf,
    lock_directory_id: FileIdentity,
    lock_path: PathBuf,
    lock_id: FileIdentity,
}

impl PartialEq for ResourceIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.root_id == other.root_id && self.lock_id == other.lock_id
    }
}
impl Eq for ResourceIdentity {}

impl ResourceIdentity {
    /// Resolves an existing project directory and creates its stable sidecar
    /// if needed. Symbolic aliases to the directory resolve to the same inode.
    /// This does not acquire writer ownership.
    pub fn for_project(path: impl AsRef<Path>) -> Result<Self, OwnershipError> {
        let root = canonical_directory(path.as_ref())?;
        let root_id = os_identity(&metadata(&root)?, &root)?;
        let lock_path = root.join(".seecut-writer.lock");
        let file = open_sidecar(&lock_path)?;
        verify_directory(&root, root_id)?;
        let lock_id = checked_lock_metadata(&file, &lock_path)?;
        verify_lock_entry(&lock_path, lock_id)?;
        Ok(Self {
            kind: Kind::Project,
            target: root.clone(),
            root: root.clone(),
            root_id,
            lock_directory: root,
            lock_directory_id: root_id,
            lock_path,
            lock_id,
        })
    }

    /// Resolves an existing `.comp` directory package or an absent `.comp` entry
    /// below an existing parent. Existing symlinks resolve to their final target;
    /// dangling symlinks and regular-file targets are refused. This creates stable private
    /// sidecars if needed, but creates no document and acquires no writer lock.
    pub fn for_canvas(path: impl AsRef<Path>) -> Result<Self, OwnershipError> {
        let path = path.as_ref();
        let target = match fs::symlink_metadata(path) {
            Ok(_) => {
                let resolved = fs::canonicalize(path).map_err(|error| target_error(path, error))?;
                validate_canvas(&resolved)?;
                resolved
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = path
                    .file_name()
                    .ok_or_else(|| invalid(path, "missing filename"))?;
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                canonical_directory(parent)?.join(name)
            }
            Err(error) => return Err(io_error(path, error)),
        };
        if !target
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("comp"))
        {
            return Err(invalid(&target, "canvas filename must end in .comp"));
        }
        // The target could have appeared while resolving an absent entry.
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(unsupported(
                    &target,
                    "target changed to a symlink during resolution",
                ));
            }
            Ok(_) => validate_canvas(&target)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&target, error)),
        }
        let root = target
            .parent()
            .ok_or_else(|| invalid(&target, "missing parent"))?
            .to_owned();
        let root_id = os_identity(&metadata(&root)?, &root)?;
        let lock_directory = root.join(".seecut-canvas-locks");
        create_lock_directory(&lock_directory)?;
        verify_directory(&root, root_id)?;
        let lock_directory_id = os_identity(&metadata(&lock_directory)?, &lock_directory)?;
        let lock_path = lock_directory.join(
            target
                .file_name()
                .ok_or_else(|| invalid(&target, "missing filename"))?,
        );
        let file = open_sidecar(&lock_path)?;
        verify_directory(&lock_directory, lock_directory_id)?;
        verify_directory(&root, root_id)?;
        let lock_id = checked_lock_metadata(&file, &lock_path)?;
        verify_lock_entry(&lock_path, lock_id)?;
        Ok(Self {
            kind: Kind::Canvas,
            target,
            root,
            root_id,
            lock_directory,
            lock_directory_id,
            lock_path,
            lock_id,
        })
    }

    /// The canonical project root or resolved canvas target used for saving.
    pub fn target(&self) -> &Path {
        &self.target
    }

    fn verify(&self) -> Result<(), OwnershipError> {
        verify_directory(&self.root, self.root_id)?;
        verify_directory(&self.lock_directory, self.lock_directory_id)?;
        verify_lock_entry(&self.lock_path, self.lock_id)?;
        if self.kind == Kind::Canvas {
            match fs::symlink_metadata(&self.target) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(unsupported(&self.target, "target changed to a symlink"));
                }
                Ok(_) => validate_canvas(&self.target)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(&self.target, error)),
            }
        }
        Ok(())
    }
}

/// Clones share the *same* File in one Arc, including save worker clones.
/// No cloned file descriptors and no explicit unlock: last-owner drop releases.
/// If a process forks, inherited OS descriptors also retain the lock until
/// closed at exec or exit; this can briefly extend ownership beyond last drop.
#[derive(Clone, Debug)]
pub struct WriterGuard {
    inner: Arc<Owner>,
}

#[derive(Debug)]
struct Owner {
    identity: ResourceIdentity,
    _file: File,
}

impl WriterGuard {
    /// Opens a new independent file descriptor and tries the OS lock once.
    /// Repeated acquisition, even using the same identity in one process,
    /// conflicts with an existing owner. Stale identities fail closed.
    pub fn acquire(identity: ResourceIdentity) -> Result<Self, OwnershipError> {
        identity.verify()?;
        let file = open_sidecar(&identity.lock_path)?;
        if checked_lock_metadata(&file, &identity.lock_path)? != identity.lock_id {
            return Err(ambiguous(
                &identity.lock_path,
                "sidecar changed since identity resolution",
            ));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(OwnershipError::Conflict {
                    target: identity.target.clone(),
                });
            }
            Err(TryLockError::Error(error)) if error.kind() == io::ErrorKind::Unsupported => {
                return Err(OwnershipError::UnsupportedLock {
                    target: identity.lock_path.clone(),
                    source: error,
                });
            }
            Err(TryLockError::Error(error)) => return Err(io_error(&identity.lock_path, error)),
        }
        identity.verify()?;
        Ok(Self {
            inner: Arc::new(Owner {
                identity,
                _file: file,
            }),
        })
    }

    /// The acquired resource identity, for caller-managed same-session reuse.
    pub fn identity(&self) -> &ResourceIdentity {
        &self.inner.identity
    }

    /// Checks the held file and its current directory entries without creating
    /// any filesystem entries or acquiring another lock. A stale owner fails.
    pub fn validate(&self) -> Result<(), OwnershipError> {
        let identity = self.identity();
        identity.verify()?;
        if checked_lock_metadata(&self.inner._file, &identity.lock_path)? != identity.lock_id {
            return Err(ambiguous(&identity.lock_path, "held lock identity changed"));
        }
        Ok(())
    }

    /// Matches an existing project root to this owner using OS identity only.
    /// Missing/unopened targets return false. No sidecar is created or opened.
    pub fn matches_project(&self, path: impl AsRef<Path>) -> Result<bool, OwnershipError> {
        let identity = self.identity();
        if identity.kind != Kind::Project {
            return Ok(false);
        }
        let path = path.as_ref();
        let Some(root) = existing_lookup_directory(path)? else {
            if std::path::absolute(path).map_err(|error| io_error(path, error))? == identity.root {
                self.validate()?;
            }
            return Ok(false);
        };
        let root_id = os_identity(&metadata(&root)?, &root)?;
        if root_id != identity.root_id && root != identity.root {
            return Ok(false);
        }
        // Validate only a relevant owner, so a stale unrelated Session cannot
        // hide a matching Session during a caller's read-only lookup loop.
        self.validate()?;
        verify_lock_entry(&root.join(".seecut-writer.lock"), identity.lock_id)?;
        Ok(true)
    }

    /// Matches a canvas entry, including an absent document whose stable
    /// sidecar already exists. Missing other targets return false, with no
    /// directory/file creation and no independent lock acquisition.
    pub fn matches_canvas(&self, path: impl AsRef<Path>) -> Result<bool, OwnershipError> {
        let identity = self.identity();
        if identity.kind != Kind::Canvas {
            return Ok(false);
        }
        let path = path.as_ref();
        let target = match fs::symlink_metadata(path) {
            Ok(_) => {
                let resolved = fs::canonicalize(path).map_err(|error| target_error(path, error))?;
                validate_canvas(&resolved)?;
                resolved
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = path
                    .file_name()
                    .ok_or_else(|| invalid(path, "missing filename"))?;
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let Some(parent) = existing_lookup_directory(parent)? else {
                    if std::path::absolute(path).map_err(|error| io_error(path, error))?
                        == identity.target
                    {
                        self.validate()?;
                    }
                    return Ok(false);
                };
                parent.join(name)
            }
            Err(error) => return Err(io_error(path, error)),
        };
        if !target
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("comp"))
        {
            return Err(invalid(&target, "canvas filename must end in .comp"));
        }
        let root = target
            .parent()
            .ok_or_else(|| invalid(&target, "missing parent"))?;
        let root_id = os_identity(&metadata(root)?, root)?;
        if root_id != identity.root_id && root != identity.root {
            return Ok(false);
        }
        let directory = root.join(".seecut-canvas-locks");
        let sidecar = directory.join(
            target
                .file_name()
                .ok_or_else(|| invalid(&target, "missing filename"))?,
        );
        let meta = match fs::symlink_metadata(&sidecar) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if target == identity.target {
                    self.validate()?;
                }
                return Ok(false);
            }
            Err(error) => return Err(io_error(&sidecar, error)),
        };
        validate_lock_meta(&meta, &sidecar)?;
        let sidecar_id = os_identity(&meta, &sidecar)?;
        let same_entry = sidecar_id == identity.lock_id
            || target == identity.target
            || fs::canonicalize(&sidecar).map_err(|error| io_error(&sidecar, error))?
                == identity.lock_path;
        if !same_entry {
            return Ok(false);
        }
        self.validate()?;
        verify_directory(&directory, identity.lock_directory_id)?;
        Ok(sidecar_id == identity.lock_id)
    }
}

fn existing_lookup_directory(path: &Path) -> Result<Option<PathBuf>, OwnershipError> {
    match fs::canonicalize(path) {
        Ok(canonical) => {
            if !metadata(&canonical)?.is_dir() {
                return Err(invalid(path, "expected an existing directory"));
            }
            Ok(Some(canonical))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn metadata(path: &Path) -> Result<Metadata, OwnershipError> {
    fs::symlink_metadata(path).map_err(|error| io_error(path, error))
}

fn canonical_directory(path: &Path) -> Result<PathBuf, OwnershipError> {
    let canonical = fs::canonicalize(path).map_err(|error| target_error(path, error))?;
    if !metadata(&canonical)?.is_dir() {
        return Err(invalid(path, "expected an existing directory"));
    }
    Ok(canonical)
}

fn validate_canvas(path: &Path) -> Result<(), OwnershipError> {
    let meta = metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(invalid(path, "expected a canvas directory package"));
    }
    // Directory nlink naturally counts its children. Only the independent
    // regular lock sidecar needs the single-hardlink rule.
    Ok(())
}

fn verify_directory(path: &Path, expected: FileIdentity) -> Result<(), OwnershipError> {
    let meta = stable_entry_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() || os_identity(&meta, path)? != expected {
        return Err(ambiguous(
            path,
            "lock directory changed or is not a real directory",
        ));
    }
    Ok(())
}

fn create_lock_directory(path: &Path) -> Result<(), OwnershipError> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error(path, error)),
    }
    let meta = metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(unsupported(path, "lock directory must be a real directory"));
    }
    Ok(())
}

fn open_sidecar(path: &Path) -> Result<File, OwnershipError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => validate_lock_meta(&meta, path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(path, error)),
    }
    let mut create = sidecar_options(path)?;
    create.create_new(true);
    let file = match create.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // Revalidate after a concurrent creator before opening any existing object.
            validate_lock_meta(&metadata(path)?, path)?;
            sidecar_options(path)?
                .open(path)
                .map_err(|error| io_error(path, error))?
        }
        Err(error) => return Err(io_error(path, error)),
    };
    let id = checked_lock_metadata(&file, path)?;
    verify_lock_entry(path, id)?;
    Ok(file)
}

fn sidecar_options(path: &Path) -> Result<OpenOptions, OwnershipError> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let mut options = OpenOptions::new();
        options.write(true);
        use std::os::unix::fs::OpenOptionsExt;
        // Apple macOS/iPhoneOS SDK usr/include/sys/fcntl.h, lines 113 and 123.
        const O_NOFOLLOW: i32 = 0x100;
        const O_NONBLOCK: i32 = 0x4;
        options.mode(0o600).custom_flags(O_NOFOLLOW | O_NONBLOCK);
        let _ = path;
        Ok(options)
    }
    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    {
        let mut options = OpenOptions::new();
        options.write(true);
        use std::os::unix::fs::OpenOptionsExt;
        // Linux uapi flags: x86 uses asm-generic/fcntl.h; arm64 overrides
        // O_NOFOLLOW in arch/arm64/include/uapi/asm/fcntl.h for AArch32 compat.
        // https://github.com/torvalds/linux/blob/master/include/uapi/asm-generic/fcntl.h
        #[cfg(target_arch = "x86_64")]
        const O_NOFOLLOW: i32 = 1 << 17;
        #[cfg(target_arch = "aarch64")]
        const O_NOFOLLOW: i32 = 1 << 15;
        const O_NONBLOCK: i32 = 1 << 11;
        options.mode(0o600).custom_flags(O_NOFOLLOW | O_NONBLOCK);
        let _ = path;
        Ok(options)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        all(
            any(target_os = "linux", target_os = "android"),
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    {
        Err(OwnershipError::UnsupportedLock {
            target: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "no audited std-only no-follow open for this platform",
            ),
        })
    }
}

fn checked_lock_metadata(file: &File, path: &Path) -> Result<FileIdentity, OwnershipError> {
    let meta = file.metadata().map_err(|error| io_error(path, error))?;
    validate_lock_meta(&meta, path)?;
    os_identity(&meta, path)
}

fn verify_lock_entry(path: &Path, expected: FileIdentity) -> Result<(), OwnershipError> {
    let meta = stable_entry_metadata(path)?;
    validate_lock_meta(&meta, path)?;
    if os_identity(&meta, path)? != expected {
        return Err(ambiguous(
            path,
            "sidecar entry no longer names the opened file",
        ));
    }
    Ok(())
}

fn stable_entry_metadata(path: &Path) -> Result<Metadata, OwnershipError> {
    fs::symlink_metadata(path).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        ) {
            ambiguous(path, "owned directory or sidecar disappeared")
        } else {
            io_error(path, error)
        }
    })
}

fn validate_lock_meta(meta: &Metadata, path: &Path) -> Result<(), OwnershipError> {
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(unsupported(
            path,
            "sidecar must be a regular file, never a symlink",
        ));
    }
    reject_hardlinks(meta, path)
}

#[cfg(unix)]
fn reject_hardlinks(meta: &Metadata, path: &Path) -> Result<(), OwnershipError> {
    use std::os::unix::fs::MetadataExt;
    if meta.nlink() != 1 {
        return Err(unsupported(path, "multiple hard links are not supported"));
    }
    Ok(())
}
#[cfg(not(unix))]
fn reject_hardlinks(_: &Metadata, path: &Path) -> Result<(), OwnershipError> {
    Err(unsupported(
        path,
        "filesystem identity is not supported on this platform",
    ))
}

#[cfg(unix)]
fn os_identity(meta: &Metadata, _: &Path) -> Result<FileIdentity, OwnershipError> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}
#[cfg(not(unix))]
fn os_identity(_: &Metadata, path: &Path) -> Result<FileIdentity, OwnershipError> {
    Err(unsupported(
        path,
        "filesystem identity is not supported on this platform",
    ))
}

fn invalid(path: &Path, reason: &'static str) -> OwnershipError {
    OwnershipError::InvalidTarget {
        target: path.to_owned(),
        reason,
    }
}
fn unsupported(path: &Path, reason: &'static str) -> OwnershipError {
    OwnershipError::UnsupportedAlias {
        target: path.to_owned(),
        reason,
    }
}
fn ambiguous(path: &Path, reason: &'static str) -> OwnershipError {
    OwnershipError::IdentityAmbiguous {
        target: path.to_owned(),
        reason,
    }
}
fn io_error(path: &Path, source: io::Error) -> OwnershipError {
    OwnershipError::Io {
        target: path.to_owned(),
        source,
    }
}
fn target_error(path: &Path, error: io::Error) -> OwnershipError {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    ) {
        invalid(
            path,
            "target or parent does not exist, or a symlink is dangling",
        )
    } else {
        io_error(path, error)
    }
}
