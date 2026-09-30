// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Focused OS writer-lock tests, including real independent subprocesses.
//!
//! Unrelated cases are serialized to isolate OS descriptor inheritance during
//! subprocess startup. Each contention case retains its own concurrent owners.
//! These process tests run only on the audited desktop Unix flag/architecture
//! combinations; macOS currently has runtime evidence, Linux still needs a run.
#![cfg(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]

#[path = "../src/ownership.rs"]
mod ownership;

use ownership::{OwnershipError, ResourceIdentity, WriterGuard};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};

static NEXT: AtomicU64 = AtomicU64::new(0);
static TEST_SEQUENCE: Mutex<()> = Mutex::new(());
// These tiny packages exercise the OS target shape only, not the loader's full
// schema. preview.png is a valid 1x1 RGBA PNG (Python stdlib zlib/CRC generated).
const PACKAGE_MANIFEST: &[u8] = br#"{"width":1,"height":1,"layers":[]}"#;
const PREVIEW_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 96, 96, 96, 248, 15, 0, 1,
    4, 1, 0, 95, 229, 195, 75, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

fn serialized() -> MutexGuard<'static, ()> {
    // Avoid unrelated tests spawning a process during another test's last-FD
    // drop check. Cross-process contention within a test remains concurrent.
    TEST_SEQUENCE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        loop {
            let root = std::env::temp_dir().join(format!(
                "seecut-ownership-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => return Self(root),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create fixture {}: {error}", root.display()),
            }
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn project(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir(&path).unwrap();
        path
    }
    fn canvas(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("images")).unwrap();
        fs::write(path.join("manifest.json"), PACKAGE_MANIFEST).unwrap();
        fs::write(path.join("preview.png"), PREVIEW_PNG).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn identity(kind: &str, path: &Path) -> Result<ResourceIdentity, OwnershipError> {
    match kind {
        "project" => ResourceIdentity::for_project(path),
        "canvas" => ResourceIdentity::for_canvas(path),
        _ => panic!("unknown kind"),
    }
}

struct Probe {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    finished: bool,
}
impl Probe {
    fn ready(kind: &str, path: &Path, cwd: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "child_process", "--nocapture"])
            .env("OWNERSHIP_CHILD_KIND", kind)
            .env("OWNERSHIP_CHILD_PATH", path)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut probe = Self {
            child,
            stdin,
            stdout,
            finished: false,
        };
        assert_eq!(probe.read(), "READY");
        probe
    }
    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").unwrap();
        self.stdin.flush().unwrap();
    }
    fn read(&mut self) -> String {
        loop {
            let mut line = String::new();
            assert_ne!(
                self.stdout.read_line(&mut line).unwrap(),
                0,
                "child exited without a result"
            );
            if let Some((_, message)) = line.split_once("OWNERSHIP|") {
                return message.trim().to_owned();
            }
        }
    }
    fn finish(mut self, acquired: bool) {
        if acquired {
            self.send("release");
        }
        assert!(self.child.wait().unwrap().success());
        self.finished = true;
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn child_result(kind: &str, path: &Path, cwd: &Path, expected: &str) {
    let mut probe = Probe::ready(kind, path, cwd);
    probe.send("go");
    assert_eq!(probe.read(), expected);
    probe.finish(expected == "ACQUIRED");
}

#[test]
#[ignore = "subprocess entry point, invoked by the focused tests"]
fn child_process() {
    let Ok(kind) = std::env::var("OWNERSHIP_CHILD_KIND") else {
        return;
    };
    let path = PathBuf::from(std::env::var_os("OWNERSHIP_CHILD_PATH").unwrap());
    let mut input = BufReader::new(std::io::stdin());
    let mut line = String::new();
    println!("OWNERSHIP|READY");
    std::io::stdout().flush().unwrap();
    input.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "go");
    let guard = match identity(&kind, &path).and_then(WriterGuard::acquire) {
        Ok(guard) => {
            println!("OWNERSHIP|ACQUIRED");
            Some(guard)
        }
        Err(OwnershipError::Conflict { .. }) => {
            println!("OWNERSHIP|CONFLICT");
            None
        }
        Err(error) => {
            println!("OWNERSHIP|ERROR:{error:?}");
            None
        }
    };
    std::io::stdout().flush().unwrap();
    if guard.is_some() {
        line.clear();
        input.read_line(&mut line).unwrap();
        if line.trim() == "exit" {
            // Deliberately bypass every Rust destructor, including WriterGuard.
            std::process::exit(0);
        }
        assert_eq!(line.trim(), "release");
    }
    drop(guard);
}

#[test]
fn project_and_canvas_second_process_conflict_and_distinct_targets_coexist() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let project = fixture.project("clip");
    let other_project = fixture.project("other");
    let canvas = fixture.canvas("image.comp");
    let other_canvas = fixture.canvas("other.comp");
    let _project_owner =
        WriterGuard::acquire(ResourceIdentity::for_project(&project).unwrap()).unwrap();
    let _canvas_owner =
        WriterGuard::acquire(ResourceIdentity::for_canvas(&canvas).unwrap()).unwrap();
    child_result("project", &project, &fixture.0, "CONFLICT");
    child_result("canvas", &canvas, &fixture.0, "CONFLICT");
    child_result("project", &other_project, &fixture.0, "ACQUIRED");
    child_result("canvas", &other_canvas, &fixture.0, "ACQUIRED");
}

#[test]
fn independent_file_descriptors_in_same_process_conflict() {
    let _serial = serialized();
    let fixture = Fixture::new();
    for (kind, path) in [
        ("project", fixture.project("clip")),
        ("canvas", fixture.canvas("image.comp")),
    ] {
        let id = identity(kind, &path).unwrap();
        let guard = WriterGuard::acquire(id.clone()).unwrap();
        assert_eq!(guard.identity(), &id);
        assert_eq!(guard.identity().target(), id.target());
        assert!(matches!(
            WriterGuard::acquire(id.clone()),
            Err(OwnershipError::Conflict { .. })
        ));
        assert!(matches!(
            WriterGuard::acquire(identity(kind, &path).unwrap()),
            Err(OwnershipError::Conflict { .. })
        ));
        // Closing the independently opened, rejected FD must not release guard's lock.
        child_result(kind, &path, &fixture.0, "CONFLICT");
        drop(guard);
        let reopened = WriterGuard::acquire(id);
        assert!(reopened.is_ok(), "{kind}: {reopened:?}");
    }
}

#[test]
fn aliases_resolve_to_the_same_owner() {
    let _serial = serialized();
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let project = fixture.project("clip");
    symlink(&project, fixture.path("linked-clip")).unwrap();
    let project_id = ResourceIdentity::for_project(&project).unwrap();
    assert_eq!(
        project_id,
        ResourceIdentity::for_project(fixture.path("linked-clip")).unwrap()
    );
    let _owner = WriterGuard::acquire(project_id).unwrap();
    child_result("project", Path::new("clip"), &fixture.0, "CONFLICT");
    child_result(
        "project",
        &fixture.path("linked-clip"),
        &fixture.0,
        "CONFLICT",
    );
    child_result(
        "project",
        &fixture.path("clip/../clip"),
        &fixture.0,
        "CONFLICT",
    );
    let canvas = fixture.canvas("target.comp");
    symlink(&canvas, fixture.path("alias.comp")).unwrap();
    let canvas_id = ResourceIdentity::for_canvas(&canvas).unwrap();
    assert_eq!(
        canvas_id,
        ResourceIdentity::for_canvas(fixture.path("alias.comp")).unwrap()
    );
    let _canvas_owner = WriterGuard::acquire(canvas_id).unwrap();
    child_result(
        "canvas",
        &fixture.path("alias.comp"),
        &fixture.0,
        "CONFLICT",
    );
    symlink(&fixture.0, fixture.path("parent-alias")).unwrap();
    let new = fixture.path("new.comp");
    let _new_owner = WriterGuard::acquire(ResourceIdentity::for_canvas(&new).unwrap()).unwrap();
    child_result(
        "canvas",
        &fixture.path("parent-alias/new.comp"),
        &fixture.0,
        "CONFLICT",
    );
}

fn case_or_unicode_alias(fixture: &Fixture, first: &str, second: &str) -> bool {
    let probe = fixture.canvas(first);
    let is_alias = fs::metadata(fixture.path(second)).is_ok();
    let first_id = ResourceIdentity::for_canvas(&probe).unwrap();
    let second_id = ResourceIdentity::for_canvas(fixture.path(second)).unwrap();
    println!(
        "Rust alias identity: first={:?}, alias={:?}, target_path_equal={}, resource_identity_equal={}",
        first_id.target(),
        second_id.target(),
        first_id.target() == second_id.target(),
        first_id == second_id,
    );
    assert_eq!(first_id == second_id, is_alias);
    let _owner = WriterGuard::acquire(first_id).unwrap();
    child_result(
        "canvas",
        &fixture.path(second),
        &fixture.0,
        if is_alias { "CONFLICT" } else { "ACQUIRED" },
    );
    is_alias
}

#[test]
fn case_and_unicode_aliases_follow_the_actual_filesystem() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let case_alias = case_or_unicode_alias(&fixture, "case.comp", "CASE.COMP");
    let unicode_alias = case_or_unicode_alias(&fixture, "café.comp", "cafe\u{301}.comp");
    println!(
        "filesystem observations: case_alias={case_alias}, unicode_normalization_alias={unicode_alias}"
    );
    let project = fixture.project("ProjectCase");
    let project_alias = fixture.path("projectcase");
    if !case_alias {
        fs::create_dir(&project_alias).unwrap();
    }
    let project_id = ResourceIdentity::for_project(&project).unwrap();
    let project_alias_id = ResourceIdentity::for_project(&project_alias).unwrap();
    println!(
        "Rust existing project alias: first={:?}, alias={:?}, target_path_equal={}, resource_identity_equal={}",
        project_id.target(),
        project_alias_id.target(),
        project_id.target() == project_alias_id.target(),
        project_id == project_alias_id,
    );
    assert_eq!(project_id == project_alias_id, case_alias);
    for (first, second, aliases) in [
        ("missing.comp", "MISSING.COMP", case_alias),
        ("néw.comp", "ne\u{301}w.comp", unicode_alias),
    ] {
        assert!(!fixture.path(first).exists());
        let first_id = ResourceIdentity::for_canvas(fixture.path(first)).unwrap();
        let second_id = ResourceIdentity::for_canvas(fixture.path(second)).unwrap();
        println!(
            "Rust absent canvas alias: first={:?}, alias={:?}, target_path_equal={}, resource_identity_equal={}",
            first_id.target(),
            second_id.target(),
            first_id.target() == second_id.target(),
            first_id == second_id,
        );
        assert_eq!(first_id == second_id, aliases);
        let _owner = WriterGuard::acquire(first_id).unwrap();
        child_result(
            "canvas",
            &fixture.path(second),
            &fixture.0,
            if aliases { "CONFLICT" } else { "ACQUIRED" },
        );
    }
}

#[test]
fn regular_file_targets_and_hardlinked_sidecars_are_rejected() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let canvas = fixture.path("image.comp");
    fs::write(&canvas, b"unsupported regular-file canvas").unwrap();
    fs::hard_link(&canvas, fixture.path("hard.comp")).unwrap();
    assert!(matches!(
        ResourceIdentity::for_canvas(&canvas),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    assert!(matches!(
        ResourceIdentity::for_canvas(fixture.path("hard.comp")),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    let project = fixture.project("clip");
    ResourceIdentity::for_project(&project).unwrap();
    fs::hard_link(
        project.join(".seecut-writer.lock"),
        fixture.path("lock-hardlink"),
    )
    .unwrap();
    assert!(matches!(
        ResourceIdentity::for_project(&project),
        Err(OwnershipError::UnsupportedAlias { .. })
    ));
    let canvas = fixture.canvas("safe.comp");
    ResourceIdentity::for_canvas(&canvas).unwrap();
    fs::hard_link(
        fixture.path(".seecut-canvas-locks/safe.comp"),
        fixture.path("canvas-lock-hardlink"),
    )
    .unwrap();
    assert!(matches!(
        ResourceIdentity::for_canvas(&canvas),
        Err(OwnershipError::UnsupportedAlias { .. })
    ));
}

#[test]
fn invalid_targets_and_malicious_sidecar_paths_are_rejected() {
    let _serial = serialized();
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let file = fixture.canvas("image.comp");
    let regular_file = fixture.path("file.comp");
    fs::write(&regular_file, b"regular file").unwrap();
    assert!(matches!(
        ResourceIdentity::for_project(&regular_file),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    assert!(matches!(
        ResourceIdentity::for_project(fixture.path("missing")),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    assert!(matches!(
        ResourceIdentity::for_canvas(fixture.path("missing-parent/a.comp")),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    assert!(matches!(
        ResourceIdentity::for_canvas(&regular_file),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    symlink(fixture.path("missing.comp"), fixture.path("dangling.comp")).unwrap();
    assert!(matches!(
        ResourceIdentity::for_canvas(fixture.path("dangling.comp")),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    assert!(matches!(
        ResourceIdentity::for_canvas(fixture.path("bad.txt")),
        Err(OwnershipError::InvalidTarget { .. })
    ));
    let project = fixture.project("clip");
    symlink(&file, project.join(".seecut-writer.lock")).unwrap();
    assert!(matches!(
        ResourceIdentity::for_project(&project),
        Err(OwnershipError::UnsupportedAlias { .. })
    ));
    assert_eq!(
        fs::read(file.join("manifest.json")).unwrap(),
        PACKAGE_MANIFEST
    );
    let lock_directory_target = fixture.project("actual-locks");
    symlink(&lock_directory_target, fixture.path(".seecut-canvas-locks")).unwrap();
    assert!(matches!(
        ResourceIdentity::for_canvas(&file),
        Err(OwnershipError::UnsupportedAlias { .. })
    ));
    assert_eq!(fs::read_dir(&lock_directory_target).unwrap().count(), 0);
    let fixture2 = Fixture::new();
    fs::create_dir(fixture2.path(".seecut-canvas-locks")).unwrap();
    symlink(&file, fixture2.path(".seecut-canvas-locks/image.comp")).unwrap();
    assert!(matches!(
        ResourceIdentity::for_canvas(fixture2.path("image.comp")),
        Err(OwnershipError::UnsupportedAlias { .. })
    ));
    let project2 = fixture2.project("clip");
    fs::create_dir(project2.join(".seecut-writer.lock")).unwrap();
    assert!(matches!(
        ResourceIdentity::for_project(&project2),
        Err(OwnershipError::UnsupportedAlias { .. })
    ));
}

#[test]
fn atomic_document_replacement_does_not_replace_ownership() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let project = fixture.project("clip");
    fs::write(project.join("concat.json"), b"old").unwrap();
    let project_id = ResourceIdentity::for_project(&project).unwrap();
    let _project_owner = WriterGuard::acquire(project_id.clone()).unwrap();
    let canvas = fixture.canvas("image.comp");
    let canvas_id = ResourceIdentity::for_canvas(&canvas).unwrap();
    let _canvas_owner = WriterGuard::acquire(canvas_id.clone()).unwrap();
    child_result("project", &project, &fixture.0, "CONFLICT");
    child_result("canvas", &canvas, &fixture.0, "CONFLICT");
    fs::write(project.join("saving"), b"new").unwrap();
    fs::rename(project.join("saving"), project.join("concat.json")).unwrap();
    let candidate = fixture.canvas("saving.comp");
    fs::write(
        candidate.join("manifest.json"),
        br#"{"width":1,"height":1,"layers":[],"label":"replacement"}"#,
    )
    .unwrap();
    fs::rename(&canvas, fixture.path("backup.comp")).unwrap();
    // The package replacement's brief absent-target interval still names the
    // original stable sidecar; no second writer can exploit that interval.
    child_result("canvas", &canvas, &fixture.0, "CONFLICT");
    fs::rename(&candidate, &canvas).unwrap();
    assert_eq!(project_id, ResourceIdentity::for_project(&project).unwrap());
    assert_eq!(canvas_id, ResourceIdentity::for_canvas(&canvas).unwrap());
    child_result("project", &project, &fixture.0, "CONFLICT");
    child_result("canvas", &canvas, &fixture.0, "CONFLICT");
}

#[test]
fn worker_clone_retains_owner_until_its_last_drop() {
    let _serial = serialized();
    let fixture = Fixture::new();
    for (kind, path) in [
        ("project", fixture.project("clip")),
        ("canvas", fixture.canvas("image.comp")),
    ] {
        let owner = WriterGuard::acquire(identity(kind, &path).unwrap()).unwrap();
        let worker_owner = owner.clone();
        let (release, wait) = mpsc::channel();
        let (started, running) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            drop(worker_owner);
        });
        running.recv().unwrap();
        drop(owner);
        child_result(kind, &path, &fixture.0, "CONFLICT");
        release.send(()).unwrap();
        worker.join().unwrap();
        child_result(kind, &path, &fixture.0, "ACQUIRED");
    }
}

#[test]
fn normal_process_exit_releases_and_preserves_sidecar() {
    let _serial = serialized();
    let fixture = Fixture::new();
    for (kind, path, sidecar) in [
        (
            "project",
            fixture.project("clip"),
            fixture.path("clip/.seecut-writer.lock"),
        ),
        (
            "canvas",
            fixture.canvas("image.comp"),
            fixture.path(".seecut-canvas-locks/image.comp"),
        ),
    ] {
        let mut owner_process = Probe::ready(kind, &path, &fixture.0);
        owner_process.send("go");
        assert_eq!(owner_process.read(), "ACQUIRED");
        assert!(matches!(
            WriterGuard::acquire(identity(kind, &path).unwrap()),
            Err(OwnershipError::Conflict { .. })
        ));
        let before = fs::metadata(&sidecar).unwrap();
        owner_process.finish(true);
        assert!(sidecar.is_file());
        use std::os::unix::fs::MetadataExt;
        assert_eq!(before.ino(), fs::metadata(&sidecar).unwrap().ino());
        assert!(WriterGuard::acquire(identity(kind, &path).unwrap()).is_ok());
    }
}

#[test]
fn os_process_exit_without_rust_drop_releases_and_preserves_sidecar() {
    let _serial = serialized();
    let fixture = Fixture::new();
    for (kind, path, sidecar) in [
        (
            "project",
            fixture.project("clip"),
            fixture.path("clip/.seecut-writer.lock"),
        ),
        (
            "canvas",
            fixture.canvas("image.comp"),
            fixture.path(".seecut-canvas-locks/image.comp"),
        ),
    ] {
        let mut owner_process = Probe::ready(kind, &path, &fixture.0);
        owner_process.send("go");
        assert_eq!(owner_process.read(), "ACQUIRED");
        let before = fs::metadata(&sidecar).unwrap();
        assert!(matches!(
            WriterGuard::acquire(identity(kind, &path).unwrap()),
            Err(OwnershipError::Conflict { .. })
        ));
        owner_process.send("exit");
        owner_process.finish(false);
        use std::os::unix::fs::MetadataExt;
        let after = fs::metadata(&sidecar).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert!(WriterGuard::acquire(identity(kind, &path).unwrap()).is_ok());
    }
}

#[test]
fn two_processes_racing_on_new_target_have_one_winner() {
    let _serial = serialized();
    let fixture = Fixture::new();
    // Both constructors, including sidecar directory/file creation, race.
    let path = fixture.path("fresh.comp");
    race("canvas", &path, &path, &fixture.0, true);
    assert!(!path.exists(), "ownership must not create the document");
    let project = fixture.project("clip");
    race("project", &project, &project, &fixture.0, true);
}

fn race(kind: &str, first_path: &Path, second_path: &Path, cwd: &Path, same_identity: bool) {
    let mut first = Probe::ready(kind, first_path, cwd);
    let mut second = Probe::ready(kind, second_path, cwd);
    first.send("go");
    second.send("go");
    let a = first.read();
    let b = second.read();
    assert!(
        if same_identity {
            matches!(
                (a.as_str(), b.as_str()),
                ("ACQUIRED", "CONFLICT") | ("CONFLICT", "ACQUIRED")
            )
        } else {
            a == "ACQUIRED" && b == "ACQUIRED"
        },
        "results: {a}, {b}"
    );
    first.finish(a == "ACQUIRED");
    second.finish(b == "ACQUIRED");
}

#[test]
fn fresh_case_and_unicode_aliases_race_by_os_entry_identity() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let case_alias = case_or_unicode_alias(&fixture, "case-probe.comp", "CASE-PROBE.COMP");
    let unicode_alias = case_or_unicode_alias(&fixture, "é-probe.comp", "e\u{301}-probe.comp");
    for (first, second, aliases) in [
        ("newcase.comp", "NEWCASE.COMP", case_alias),
        ("é-new.comp", "e\u{301}-new.comp", unicode_alias),
    ] {
        let first = fixture.path(first);
        let second = fixture.path(second);
        assert!(!first.exists() && !second.exists());
        race("canvas", &first, &second, &fixture.0, aliases);
        assert!(!first.exists() && !second.exists());
    }
}

#[test]
fn permissions_long_names_and_existing_sidecar_content_are_preserved() {
    let _serial = serialized();
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let name = format!("{}.comp", "x".repeat(250));
    let id = ResourceIdentity::for_canvas(fixture.path(&name)).unwrap();
    let directory = fixture.path(".seecut-canvas-locks");
    let sidecar = directory.join(&name);
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&sidecar).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&sidecar, b"preexisting sidecar payload").unwrap();
    let guard = WriterGuard::acquire(id).unwrap();
    assert_eq!(fs::read(&sidecar).unwrap(), b"preexisting sidecar payload");
    let project = fixture.project("clip");
    ResourceIdentity::for_project(&project).unwrap();
    assert_eq!(
        fs::metadata(project.join(".seecut-writer.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(guard);
    assert_eq!(fs::read(&sidecar).unwrap(), b"preexisting sidecar payload");
}

#[test]
fn stale_identity_refuses_a_replaced_sidecar_and_releases_failed_candidate() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let canvas = fixture.canvas("image.comp");
    let id = ResourceIdentity::for_canvas(&canvas).unwrap();
    let sidecar = fixture.path(".seecut-canvas-locks/image.comp");
    // Deliberate hostile replacement in a fixture, never a normal release.
    fs::rename(&sidecar, fixture.path("old-sidecar")).unwrap();
    fs::write(&sidecar, b"replacement").unwrap();
    assert!(matches!(
        WriterGuard::acquire(id),
        Err(OwnershipError::IdentityAmbiguous { .. })
    ));
    let fresh = ResourceIdentity::for_canvas(&canvas).unwrap();
    assert!(WriterGuard::acquire(fresh).is_ok());
}

#[test]
fn readonly_matching_creates_no_sidecars_or_unopened_targets() {
    let _serial = serialized();
    assert!(ownership::desktop_writer_ownership_supported());
    let fixture = Fixture::new();
    let project = fixture.project("held");
    let owner = WriterGuard::acquire(ResourceIdentity::for_project(&project).unwrap()).unwrap();
    let other = fixture.project("not-open");
    assert!(!owner.matches_project(&other).unwrap());
    assert!(!other.join(".seecut-writer.lock").exists());
    assert!(!owner.matches_project(fixture.path("missing-root")).unwrap());
    assert!(!fixture.path("missing-root").exists());
    assert!(owner.matches_project(&project).unwrap());
    owner.validate().unwrap();
    let package = fixture.canvas("held.comp");
    let canvas_owner =
        WriterGuard::acquire(ResourceIdentity::for_canvas(&package).unwrap()).unwrap();
    assert!(canvas_owner.matches_canvas(&package).unwrap());
    assert!(
        !canvas_owner
            .matches_canvas(fixture.path("unopened.comp"))
            .unwrap()
    );
    assert!(!fixture.path(".seecut-canvas-locks/unopened.comp").exists());
    let absent = fixture.path("not-saved.comp");
    let absent_owner =
        WriterGuard::acquire(ResourceIdentity::for_canvas(&absent).unwrap()).unwrap();
    assert!(absent_owner.matches_canvas(&absent).unwrap());
    let case_alias = fs::metadata(fixture.path("HELD.COMP")).is_ok();
    assert_eq!(
        absent_owner
            .matches_canvas(fixture.path("NOT-SAVED.COMP"))
            .unwrap(),
        case_alias
    );
    assert!(!absent.exists());
}

#[test]
fn stale_owner_matches_fail_only_for_the_related_target() {
    let _serial = serialized();
    let fixture = Fixture::new();
    let project = fixture.project("held");
    let owner = WriterGuard::acquire(ResourceIdentity::for_project(&project).unwrap()).unwrap();
    let sidecar = project.join(".seecut-writer.lock");
    fs::rename(&sidecar, fixture.path("project-old-lock")).unwrap();
    fs::write(&sidecar, b"replacement").unwrap();
    let other = fixture.project("unrelated");
    assert!(!owner.matches_project(&other).unwrap());
    assert!(!other.join(".seecut-writer.lock").exists());
    assert!(matches!(
        owner.matches_project(&project),
        Err(OwnershipError::IdentityAmbiguous { .. })
    ));
    let package = fixture.canvas("held.comp");
    let canvas_owner =
        WriterGuard::acquire(ResourceIdentity::for_canvas(&package).unwrap()).unwrap();
    let sidecar = fixture.path(".seecut-canvas-locks/held.comp");
    fs::rename(&sidecar, fixture.path("canvas-old-lock")).unwrap();
    fs::write(&sidecar, b"replacement").unwrap();
    assert!(
        !canvas_owner
            .matches_canvas(fixture.path("unrelated.comp"))
            .unwrap()
    );
    assert!(!fixture.path(".seecut-canvas-locks/unrelated.comp").exists());
    assert!(matches!(
        canvas_owner.matches_canvas(&package),
        Err(OwnershipError::IdentityAmbiguous { .. })
    ));
}
