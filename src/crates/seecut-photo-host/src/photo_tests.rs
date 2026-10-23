//! Synthetic states and real private filesystem objects; no editor acceptance.
use super::*;
use std::fs::{self, OpenOptions};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.toonflow/photocraft-native-backing-tests");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn state(host: Uuid, path: &Path) -> Value {
    json!({"kind":"uiCallbackMetadata","atomicPixels":false,"frame":u64::MAX,
        "hostInstanceId":host.to_string(),"document":{
            "identity":{"hostInstanceId":host.to_string(),"docId":u64::MAX,"documentGeneration":u64::MAX-1,"revision":u64::MAX-2},
            "name":"Synthetic document","size":[8,8],"activeLayer":u64::MAX-3,"selectedLayers":[u64::MAX-3],
            "selectionListTruncated":false,"hasPixelSelection":false,"selectionBounds":null,"selectionBoundsMeasurement":"notSampled",
            "layers":[{"id":u64::MAX-3,"name":"Synthetic layer","opacity":1.0,"visible":true}],
            "layerListMayBeTruncated":false,"target":null,"dirty":false,"canUndo":false,"canRedo":false,"historyEntries":0,
            "backing":{"nativePath":path.to_str().unwrap(),"pathStatus":"recorded","sourceReadOnly":false,"savedRevision":1}},
        "view":null,"geometrySource":"lastLaidOutFrame","lastLaidOutCanvasRect":{"min":[null,null],"max":[null,null]},"busy":null,
        "receiptCache":{"storage":"memoryOnly","restartRecovery":false,"capacity":1024,"entries":0,"eviction":false}})
}
fn bind(value: &Value, host: Uuid) -> Result<PhotoBackingBinding> {
    verify_backing(|| parse_state(value, host))
}

#[test]
fn state_preserves_full_u64_and_redacts_backing_debug() {
    let host = Uuid::new_v4();
    let value = state(host, Path::new("/synthetic-sensitive-name.pcraft"));
    let parsed = parse_state(&value, host).unwrap();
    assert_eq!(parsed.frame, u64::MAX);
    let doc = parsed.document.as_ref().unwrap();
    assert_eq!(doc.identity.doc_id, u64::MAX);
    assert_eq!(doc.identity.document_generation, u64::MAX - 1);
    assert_eq!(doc.identity.revision, u64::MAX - 2);
    assert_eq!(doc.layers[0].id, u64::MAX - 3);
    assert!(!format!("{parsed:?}").contains("synthetic-sensitive-name"));
    let mut empty = value;
    empty["document"] = Value::Null;
    assert!(parse_state(&empty, host).unwrap().document.is_none());
}

#[test]
fn state_rejects_foreign_identity_extra_fields_wrong_types_and_limits() {
    let host = Uuid::new_v4();
    let base = state(host, Path::new("/synthetic.pcraft"));
    for (pointer, replacement) in [
        ("/hostInstanceId", json!(Uuid::new_v4().to_string())),
        (
            "/document/identity/hostInstanceId",
            json!(Uuid::new_v4().to_string()),
        ),
        ("/atomicPixels", json!(true)),
        ("/kind", json!("snapshot")),
        ("/document/identity/docId", json!(0)),
        ("/document/identity/revision", json!(9007199254740992_f64)),
        ("/document/backing/nativePath", json!("a\0b.pcraft")),
        ("/document/backing/nativePath", json!("x".repeat(4097))),
        ("/document/backing/pathStatus", json!("canonical")),
        ("/document/backing/pathStatus", json!("missing")),
        ("/document/backing/savedRevision", json!(-1)),
        ("/document/layers/0/opacity", json!(1.1)),
        ("/document/selectedLayers", json!(vec![1; 257])),
        ("/document/name", json!("x".repeat(1025))),
        ("/view", json!({"oversized":"x".repeat(MAX_STATE_BYTES)})),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(parse_state(&changed, host).is_err(), "{pointer}");
    }
    for pointer in ["", "/document", "/document/identity", "/document/backing"] {
        let mut changed = base.clone();
        changed
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        assert!(parse_state(&changed, host).is_err());
    }
    let maximum_path = format!("/{}.pcraft", "x".repeat(4088));
    assert_eq!(maximum_path.len(), 4096);
    assert!(parse_state(&state(host, Path::new(&maximum_path)), host).is_ok());
    // A valid wire length alone cannot create a filesystem binding.
    assert!(bind(&state(host, Path::new(&maximum_path)), host).is_err());
}

#[test]
fn real_regular_file_and_recorded_symlink_share_canonical_history_key() {
    let f = Fixture::new();
    let path = f.file("own.pcraft");
    let alias = f.0.join("alias.pcraft");
    symlink(&path, &alias).unwrap();
    let host = Uuid::new_v4();
    let direct = bind(&state(host, &path), host).unwrap();
    let linked = bind(&state(host, &alias), host).unwrap();
    assert_eq!(
        direct.history_key(),
        format!("photo:{}", path.to_str().unwrap())
    );
    assert_eq!(direct.history_key(), linked.history_key());
    assert_eq!(direct.file_identity, linked.file_identity);
    assert!(direct._file.metadata().unwrap().is_file());
    assert!(!format!("{direct:?}").contains("own.pcraft"));
    // The open helper itself refuses a last-component symlink, even though the
    // binding algorithm supports recorded aliases by canonicalizing first.
    assert!(open_regular(&alias).is_err());
}

#[test]
fn recovery_read_only_unsaved_and_non_native_backing_cannot_bind() {
    let f = Fixture::new();
    let path = f.file("own.pcraft");
    let host = Uuid::new_v4();
    let base = state(host, &path);
    for (pointer, replacement) in [
        ("/document/backing/savedRevision", json!(0)),
        ("/document/backing/sourceReadOnly", json!(true)),
        ("/document/backing/nativePath", json!("relative.pcraft")),
        ("/document/backing/nativePath", json!(f.file("export.png"))),
        ("/document", Value::Null),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(bind(&changed, host).is_err());
    }
    for status in ["missing", "invalid", "tooLong"] {
        let mut changed = base.clone();
        changed["document"]["backing"]["nativePath"] = Value::Null;
        changed["document"]["backing"]["pathStatus"] = json!(status);
        assert!(parse_state(&changed, host).is_ok());
        assert!(bind(&changed, host).is_err());
    }
}

#[test]
fn real_missing_directory_fifo_and_non_utf8_canonical_target_are_refused() {
    let f = Fixture::new();
    let host = Uuid::new_v4();
    let directory = f.0.join("directory.pcraft");
    fs::create_dir(&directory).unwrap();
    let fifo = f.0.join("pipe.pcraft");
    assert!(
        Command::new("mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    for path in [f.0.join("missing.pcraft"), directory, fifo] {
        let error = bind(&state(host, &path), host).unwrap_err();
        assert!(matches!(error, HostError::ReadFailed(_)));
        assert!(!format!("{error:?}").contains(f.0.to_str().unwrap()));
    }
    let target = f.0.join(std::ffi::OsString::from_vec(
        b"non-utf8-\xff.pcraft".to_vec(),
    ));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&target)
    {
        Ok(file) => {
            drop(file);
            let alias = f.0.join("utf8-alias.pcraft");
            symlink(&target, &alias).unwrap();
            assert!(bind(&state(host, &alias), host).is_err());
        }
        Err(error) if cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EILSEQ) => {
            assert_eq!(error.raw_os_error(), Some(libc::EILSEQ));
            eprintln!(
                "nonUtf8FixtureUnavailable: this macOS filesystem rejects the filename with EILSEQ; the helper non-UTF8 canonical-path branch was not exercised"
            );
        }
        Err(error) => panic!("non-UTF8 fixture creation failed: {error}"),
    }
}

#[test]
fn double_read_rejects_save_as_restart_revision_generation_and_backing_changes() {
    let f = Fixture::new();
    let host = Uuid::new_v4();
    let base = state(host, &f.file("one.pcraft"));
    let other = f.file("two.pcraft");
    for (pointer, replacement) in [
        ("/document/backing/nativePath", json!(other)),
        ("/hostInstanceId", json!(Uuid::new_v4().to_string())),
        ("/document/identity/docId", json!(9)),
        ("/document/identity/documentGeneration", json!(10)),
        ("/document/identity/revision", json!(11)),
        ("/document/backing/savedRevision", json!(2)),
        ("/document/backing/sourceReadOnly", json!(true)),
    ] {
        let mut second = base.clone();
        *second.pointer_mut(pointer).unwrap() = replacement;
        let mut reads = 0;
        let result = verify_backing(|| {
            reads += 1;
            parse_state(if reads == 1 { &base } else { &second }, host)
        });
        assert!(result.is_err(), "{pointer}");
        assert_eq!(reads, 2);
    }
}

#[test]
fn replacement_during_second_state_read_is_rejected_even_with_held_file() {
    let f = Fixture::new();
    let path = f.file("own.pcraft");
    let replacement = f.file("replacement.pcraft");
    let host = Uuid::new_v4();
    let value = state(host, &path);
    let mut reads = 0;
    let result = verify_backing(|| {
        reads += 1;
        if reads == 2 {
            fs::rename(&replacement, &path).unwrap();
        }
        parse_state(&value, host)
    });
    assert!(result.is_err());
    assert_eq!(reads, 2);
}

#[test]
fn atomic_save_to_same_path_keeps_history_key_and_refreshes_file_identity() {
    let f = Fixture::new();
    let path = f.file("own.pcraft");
    let host = Uuid::new_v4();
    let value = state(host, &path);
    let first = bind(&value, host).unwrap();
    let replacement = f.file("replacement.pcraft");
    fs::rename(replacement, &path).unwrap();
    let second = bind(&value, host).unwrap();
    assert_eq!(first.history_key(), second.history_key());
    assert_ne!(first.file_identity.inode, second.file_identity.inode);
    assert_eq!(first.identity, second.identity);
}

#[test]
fn nonblocking_open_refuses_fifo_in_a_bounded_owned_child() {
    const CHILD_FIFO: &str = "SEECUT_TEST_ONLY_BACKING_FIFO";
    if let Some(path) = std::env::var_os(CHILD_FIFO) {
        assert!(open_regular(Path::new(&path)).is_err());
        return;
    }
    let f = Fixture::new();
    let fifo = f.0.join("race-replacement.pcraft");
    assert!(
        Command::new("mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "photo::tests::nonblocking_open_refuses_fifo_in_a_bounded_owned_child",
            "--test-threads=1",
        ])
        .env(CHILD_FIFO, &fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned synthetic child blocked while opening a FIFO");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
