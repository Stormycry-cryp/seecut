//! Synthetic states and real private filesystem objects; no editor acceptance.
use super::*;
use crate::{InstanceInfo, InstanceStatus, Running};
use std::fs::{self, OpenOptions};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
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

// Reuse the real-file state fixture above with a real disposable process and
// authenticated TCP transport. No fake Connection or authorization object.
const OWNED_AUTH: &str = "synthetic-owned-metadata-auth-only";
struct OwnedWireFixture {
    files: Fixture,
    host: NativeHost,
    id: Uuid,
    path: PathBuf,
}
impl OwnedWireFixture {
    fn new(mode: &str) -> Self {
        let files = Fixture::new();
        let path = files.file("owned.pcraft");
        let _ = files.file("replacement.pcraft");
        let id = Uuid::new_v4();
        let script = files.0.join("synthetic-owned-metadata-child.py");
        fs::write(&script, OWNED_SCRIPT).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let state_file = files.0.join("state.json");
        fs::write(&state_file, state(id, &path).to_string()).unwrap();
        fs::set_permissions(&state_file, fs::Permissions::from_mode(0o600)).unwrap();
        let runtime_dir = files.0.join("runtime");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime_dir)
            .unwrap();
        let token_path = runtime_dir.join("control.token");
        fs::write(&token_path, OWNED_AUTH).unwrap();
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
        let child = Command::new("python3")
            .arg(&script)
            .arg(&files.0)
            .arg(mode)
            .env("OWNED_METADATA_AUTH", OWNED_AUTH)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut fixture = Self {
            files,
            id,
            path,
            host: NativeHost {
                running: Some(Running {
                    child,
                    info: InstanceInfo {
                        id,
                        pid,
                        build_attempt: "synthetic-owned-metadata-wire".into(),
                        ready: false,
                        status: InstanceStatus::Starting,
                    },
                    addr: None,
                    token: OWNED_AUTH.into(),
                    token_path,
                    runtime_dir,
                    conn: None,
                    next_id: 1,
                    timeout: Duration::from_millis(400),
                }),
            },
        };
        let running = fixture.host.running.as_mut().unwrap();
        let ready = crate::watch_stderr(running.child.stderr.take().unwrap());
        running.addr = Some(ready.recv_timeout(Duration::from_secs(3)).unwrap().unwrap());
        let inspected = crate::call(running, "ui.inspect", json!({}), false).unwrap();
        crate::validate_inspection(&inspected).unwrap();
        running.info.ready = true;
        running.info.status = InstanceStatus::Ready;
        fixture
    }
    fn binding(&mut self) -> Result<PhotoBackingBinding> {
        self.host.owned_photo_workspace_binding(self.id)
    }
    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.files.0.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn assert_read_only_calls(&self, states: usize) {
        let calls = self.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|v| v["method"] == "seecut.state")
                .count(),
            states
        );
        for call in calls {
            assert!(matches!(
                call["method"].as_str(),
                Some("ui.inspect" | "seecut.state")
            ));
            assert_eq!(call["params"], json!({}));
        }
        assert_eq!(
            fs::metadata(&self.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
impl Drop for OwnedWireFixture {
    fn drop(&mut self) {
        // This exact disposable child was spawned above; no global PID/PG scan.
        if let Some(running) = self.host.running.as_mut() {
            if running.child.try_wait().ok().flatten().is_none() {
                let _ = running.child.kill();
            }
            let _ = running.child.wait();
        }
        self.host.running = None;
    }
}

#[test]
fn seecut_owned_metadata_real_wire_double_read_and_gated_entry_share_binding() {
    let mut f = OwnedWireFixture::new("normal");
    let bytes_before = fs::read(&f.path).unwrap();
    let owned = f.binding().unwrap();
    assert_eq!(owned.identity().host_instance_id, f.id);
    assert_eq!(owned.identity().doc_id, u64::MAX);
    assert_eq!(owned.identity().document_generation, u64::MAX - 1);
    assert_eq!(owned.identity().revision, u64::MAX - 2);
    assert_eq!(
        owned.history_key(),
        format!("photo:{}", f.path.to_str().unwrap())
    );
    assert!(owned._file.metadata().unwrap().is_file());
    assert!(!format!("{owned:?}").contains(f.path.to_str().unwrap()));
    f.assert_read_only_calls(2);
    let gated = f.host.photo_backing_after_gate(f.id).unwrap();
    assert_eq!(owned.identity(), gated.identity());
    assert_eq!(owned.history_key(), gated.history_key());
    assert_eq!(owned.file_identity(), gated.file_identity());
    f.assert_read_only_calls(4);
    assert_eq!(fs::read(&f.path).unwrap(), bytes_before);
    // No grant/Capture/execute endpoint exists in this fixture. Successful
    // metadata acquisition exchanged only the two bounded seecut.state reads.
}

#[test]
fn seecut_owned_metadata_wrong_instance_and_not_ready_never_read() {
    let mut empty = NativeHost::new();
    assert!(matches!(
        empty.owned_photo_workspace_binding(Uuid::new_v4()),
        Err(HostError::StaleInstance)
    ));
    let mut f = OwnedWireFixture::new("normal");
    assert!(matches!(
        f.host.owned_photo_workspace_binding(Uuid::nil()),
        Err(HostError::StaleInstance)
    ));
    f.host.running.as_mut().unwrap().info.ready = false;
    assert!(matches!(f.binding(), Err(HostError::NotReady)));
    f.assert_read_only_calls(0);
}

#[test]
fn seecut_owned_metadata_unsaved_recovery_read_only_and_no_document_are_refused() {
    for mode in ["unsaved", "recovery", "read-only", "no-document"] {
        let mut f = OwnedWireFixture::new(mode);
        assert!(
            matches!(f.binding(), Err(HostError::ReadFailed(_))),
            "{mode}"
        );
        f.assert_read_only_calls(1);
    }
}

#[test]
fn seecut_owned_metadata_read_failures_are_not_retried_or_upgraded_to_permissions() {
    for (mode, reads) in [
        ("read-error-first", 1),
        ("read-error-second", 2),
        ("malformed-first", 1),
    ] {
        let mut f = OwnedWireFixture::new(mode);
        assert!(f.binding().is_err(), "{mode}");
        f.assert_read_only_calls(reads);
    }
}

#[test]
fn seecut_owned_metadata_second_read_or_file_replacement_cannot_bind() {
    for mode in [
        "change-generation",
        "change-revision",
        "change-path",
        "replace-file",
    ] {
        let mut f = OwnedWireFixture::new(mode);
        assert!(
            matches!(f.binding(), Err(HostError::ReadFailed(_))),
            "{mode}"
        );
        f.assert_read_only_calls(2);
    }
}

const OWNED_SCRIPT: &str = r#"#!/usr/bin/env python3
import copy,json,os,pathlib,socket,sys
root=pathlib.Path(sys.argv[1]); mode=sys.argv[2]
base=json.loads((root/'state.json').read_text()); reads=0
server=socket.socket(); server.bind(('127.0.0.1',0)); server.listen(4)
print('photocraft: control server listening on 127.0.0.1:'+str(server.getsockname()[1]),file=sys.stderr,flush=True)
while True:
    conn,_=server.accept()
    with conn:
        reader=conn.makefile('rb')
        for line in reader:
            msg=json.loads(line); method=msg['method']; params=msg['params']
            if method=='auth':
                assert params=={'token':os.environ['OWNED_METADATA_AUTH']}
                result={'authenticated':True}
            else:
                with (root/'calls.jsonl').open('a') as log: log.write(json.dumps(msg)+'\n')
                assert params=={}
                if method=='ui.inspect':
                    result={'session':{'documents':[]},'dialogs':[],'window':{}}
                else:
                    assert method=='seecut.state'
                    reads+=1; result=copy.deepcopy(base)
                    if mode=='unsaved': result['document']['backing'].update(nativePath=None,pathStatus='missing',savedRevision=0)
                    elif mode=='recovery': result['document']['backing']['savedRevision']=0
                    elif mode=='read-only': result['document']['backing']['sourceReadOnly']=True
                    elif mode=='no-document': result['document']=None
                    if reads==2:
                        if mode=='change-generation': result['document']['identity']['documentGeneration']-=1
                        elif mode=='change-revision': result['document']['identity']['revision']-=1
                        elif mode=='change-path': result['document']['backing']['nativePath']=str(root/'replacement.pcraft')
                        elif mode=='replace-file': os.replace(root/'replacement.pcraft',root/'owned.pcraft')
                    if mode=='read-error-first' or (mode=='read-error-second' and reads==2):
                        conn.sendall((json.dumps({'id':msg['id'],'ok':False,'error':'synthetic read failure'})+'\n').encode()); continue
                    if mode=='malformed-first':
                        conn.sendall(b'not JSON\n'); continue
            conn.sendall((json.dumps({'id':msg['id'],'ok':True,'result':result})+'\n').encode())
        reader.close()
"#;
