// SPDX-License-Identifier: AGPL-3.0-or-later
//! Identity-checked cleanup of this process's bound Unix socket.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct BoundEndpoint {
    path: PathBuf,
    device: u64,
    inode: u64,
    changed: (i64, i64),
    removed: AtomicBool,
}

impl BoundEndpoint {
    /// Capture only after binding and setting the socket's private permissions.
    pub(super) fn capture(path: PathBuf) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket() {
            return Err(io::Error::other("editor MCP endpoint is not a socket"));
        }
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            removed: AtomicBool::new(false),
        })
    }

    /// Best effort, without locks or thread waits; safe to call from atexit.
    pub(super) fn remove(&self) {
        // Normal shutdown and atexit share one attempt, so the later callback
        // cannot remove a new socket installed after normal cleanup.
        if self.removed.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && (metadata.ctime(), metadata.ctime_nsec()) == self.changed
        {
            // The containing directory is private to the user. This check
            // preserves existing replacements; unlink is not conditional on
            // identity if that same user actively races a rename here.
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, symlink};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::AtomicUsize;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            loop {
                // Keep paths within macOS's sockaddr_un limit.
                let path = PathBuf::from("/tmp").join(format!(
                    "sc-mcp-cleanup-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("could not create test directory: {error}"),
                }
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn normal_cleanup_then_exit_cleanup_preserves_a_new_socket() {
        let dir = TestDirectory::new();
        let path = dir.path("instance");
        let listener = UnixListener::bind(&path).unwrap();
        let endpoint = BoundEndpoint::capture(path.clone()).unwrap();
        assert!(UnixStream::connect(&path).is_ok());
        drop(listener);
        endpoint.remove();
        assert!(!path.exists());

        let replacement = UnixListener::bind(&path).unwrap();
        endpoint.remove();
        assert!(UnixStream::connect(&path).is_ok());
        drop(replacement);
    }

    #[test]
    fn preserves_a_socket_replacing_the_original_before_cleanup() {
        let dir = TestDirectory::new();
        let path = dir.path("instance");
        let original = UnixListener::bind(&path).unwrap();
        let endpoint = BoundEndpoint::capture(path.clone()).unwrap();
        fs::rename(&path, dir.path("original")).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();

        endpoint.remove();
        assert!(UnixStream::connect(&path).is_ok());
        drop((original, replacement));
    }

    #[test]
    fn preserves_replacement_files_and_does_not_follow_symlinks() {
        let dir = TestDirectory::new();
        for name in ["file", "link"] {
            let path = dir.path(name);
            let original = UnixListener::bind(&path).unwrap();
            let endpoint = BoundEndpoint::capture(path.clone()).unwrap();
            fs::rename(&path, dir.path(&format!("original-{name}"))).unwrap();
            if name == "file" {
                fs::write(&path, b"replacement").unwrap();
            } else {
                symlink(dir.path("other"), &path).unwrap();
            }
            endpoint.remove();
            if name == "file" {
                assert_eq!(fs::read(&path).unwrap(), b"replacement");
            } else {
                assert!(
                    fs::symlink_metadata(&path)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
            }
            drop(original);
        }
    }

    #[test]
    fn cleanup_is_scoped_to_the_captured_instance() {
        let dir = TestDirectory::new();
        let first_path = dir.path("first");
        let second_path = dir.path("second");
        let first = UnixListener::bind(&first_path).unwrap();
        let second = UnixListener::bind(&second_path).unwrap();
        let endpoint = BoundEndpoint::capture(first_path.clone()).unwrap();

        endpoint.remove();
        assert!(!first_path.exists());
        assert!(UnixStream::connect(&second_path).is_ok());
        drop((first, second));
    }

    #[test]
    fn cleanup_does_not_recreate_a_missing_directory() {
        let dir = TestDirectory::new();
        let parent = dir.path("gone");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("instance");
        let listener = UnixListener::bind(&path).unwrap();
        let endpoint = BoundEndpoint::capture(path.clone()).unwrap();
        drop(listener);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&parent).unwrap();

        endpoint.remove();
        endpoint.remove();
        assert!(!parent.exists());
    }
}
