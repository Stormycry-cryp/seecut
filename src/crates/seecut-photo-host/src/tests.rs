//! Synthetic processes and real sockets only; none of these tests accepts the PhotoCraft GUI.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    root: PathBuf,
    options: LaunchOptions,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.toonflow/photocraft-native-host")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let executable = root.join("synthetic-editor.py");
        fs::write(&executable, SCRIPT.replace("FIXTURE_MODE", mode)).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let (size, hash) = provenance::sha_file(&executable).unwrap();
        let receipt = json!({
            "schemaVersion":1,"status":"built","attemptId":Uuid::new_v4().to_string(),
            "processOwnership":"isolated-posix-session",
            "features":["photocraft/heif","photocraft-cli/heif"],
            "source":{
                "repository":"https://github.com/storytold/photocraft.git",
                "commit":"ff4738810134d7f7e68b49f2e4f231b686282ff3","version":"0.14.0",
                "sourceTreeSha256":"4bcec483a7490740d0a52d27d46542eac212e3c8895c4b9033ec1b42e8c812ac",
                "cargoLockSha256":"9ce360951da8005e23e81fd5dd3e7b2dd2b95c7f5056991ca556ed4fa239593e",
                "fileCount":2019,
                "patches":[
                    {"path":"patches/seecut-theme.patch","sha256":"c8706c00aa0dd2cdf4eee31f89d578db9b882f4b288a8388513a5910b1aa96c2"},
                    {"path":"patches/seecut-theme-polish.patch","sha256":"f2e80dc4b8bcf7b669f3ce53a4ea0d637504009cf0d3635aa42c447a8eaa61ca"},
                    {"path":"patches/seecut-agent-control.patch","sha256":"ea9894916e80c239cd3c535638f3ef403a26102321cf30b35f98417d65ab6ce9"},
                    {"path":"patches/seecut-profile-guard.patch","sha256":"b97baea37a929cecb8959f7480ab328fc1eaedb942ab17654115aa065e724d6e"},
                    {"path":"patches/seecut-adopted-snapshot.patch","sha256":"91558f45fbe7ae508aeefd1cd1f738dfb7368039a4d5a2c424a43a0d35991236"}
                ]
            },
            "binaries":{
                "photocraft":{"path":executable,"bytes":size,"sha256":hash},
                "photocraft-cli":{"path":executable,"bytes":size,"sha256":hash}
            }
        });
        let provenance_path = root.join("provenance.json");
        fs::write(&provenance_path, receipt.to_string()).unwrap();
        let mut options = LaunchOptions::new(provenance_path, root.join("private-state"));
        options.request_timeout = Duration::from_millis(300);
        options.startup_timeout = Duration::from_secs(3);
        Self { root, options }
    }
    fn commands(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("commands.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

struct TestHost(NativeHost);
impl TestHost {
    fn new() -> Self {
        Self(NativeHost::new())
    }
}
impl Drop for TestHost {
    fn drop(&mut self) {
        if let Ok(Some(info)) = self.0.current()
            && info.status != InstanceStatus::Exited
        {
            // Only synthetic fixtures created in this test are explicitly disposable.
            self.0.terminate_confirmed(info.id).unwrap();
        }
    }
}

#[test]
fn provenance_rejects_source_patch_feature_and_binary_changes() {
    for pointer in [
        "/source/commit",
        "/source/sourceTreeSha256",
        "/source/fileCount",
        "/source/cargoLockSha256",
        "/source/patches/0/sha256",
        "/source/patches/1/sha256",
        "/source/patches/2/sha256",
        "/source/patches/3/sha256",
        "/source/patches/4/sha256",
        "/features/0",
        "/status",
    ] {
        let f = Fixture::new("clean");
        let mut receipt: Value =
            serde_json::from_slice(&fs::read(&f.options.provenance_path).unwrap()).unwrap();
        *receipt.pointer_mut(pointer).unwrap() = json!("wrong");
        fs::write(&f.options.provenance_path, receipt.to_string()).unwrap();
        assert!(matches!(
            VerifiedBuild::from_provenance(&f.options.provenance_path),
            Err(HostError::InvalidBuild(_))
        ));
    }
    for outdated in [true, false] {
        let f = Fixture::new("clean");
        let mut receipt: Value =
            serde_json::from_slice(&fs::read(&f.options.provenance_path).unwrap()).unwrap();
        if outdated {
            receipt["source"]["sourceTreeSha256"] =
                json!("398c2fca006b44f3c1a241c123d3fad882f472ae9f72a7c1c27ef28b6b39cdfa");
            receipt["source"]["fileCount"] = json!(2011);
            receipt["source"]["patches"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
        } else {
            receipt["source"]["patches"]
                .as_array_mut()
                .unwrap()
                .swap(2, 3);
        }
        fs::write(&f.options.provenance_path, receipt.to_string()).unwrap();
        assert!(matches!(
            VerifiedBuild::from_provenance(&f.options.provenance_path),
            Err(HostError::InvalidBuild(_))
        ));
    }
    let f = Fixture::new("clean");
    let verified = VerifiedBuild::from_provenance(&f.options.provenance_path).unwrap();
    fs::write(&verified.editor, "changed executable").unwrap();
    assert!(matches!(
        verified.recheck_editor(),
        Err(HostError::InvalidBuild(_))
    ));
    assert!(matches!(
        VerifiedBuild::from_provenance(&f.options.provenance_path),
        Err(HostError::InvalidBuild(_))
    ));
}

#[test]
fn token_is_private_and_not_in_arguments_or_results() {
    const CHILD_TEST_ENV: &str = "SEECUT_PHOTO_HOST_INHERITED_ENV_TEST";
    const INHERITED_VALUE: &str = "inherited-not-a-host-instance";
    let inherited_fixture = std::env::var_os(CHILD_TEST_ENV).is_some();
    if inherited_fixture {
        assert_eq!(
            std::env::var("SEECUT_PHOTO_HOST_INSTANCE_ID").unwrap(),
            INHERITED_VALUE
        );
    }
    let f = Fixture::new("clean");
    let mut h = TestHost::new();
    let info = h.0.start(f.options.clone()).unwrap();
    let child_instance =
        h.0.call_internal_after_gate(info.id, "test.instance_id", json!({}))
            .unwrap();
    assert_eq!(child_instance, json!(info.id.to_string()));
    assert_ne!(child_instance, json!(INHERITED_VALUE));
    let r = h.0.running.as_ref().unwrap();
    assert_eq!(
        fs::metadata(&r.token_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&r.runtime_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let token = r.token.clone();
    assert_eq!(token.len(), 64);
    let value =
        h.0.call_internal_after_gate(info.id, "test.echo_secret", json!({}))
            .unwrap();
    assert_eq!(value, "[redacted]");
    assert!(!format!("{info:?}").contains(&token));
    assert!(h.0.start(f.options.clone()).is_err());
    if !inherited_fixture {
        let (_, test_module) = module_path!().split_once("::").unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{test_module}::token_is_private_and_not_in_arguments_or_results"),
                "--test-threads=1",
            ])
            .env(CHILD_TEST_ENV, "1")
            .env("SEECUT_PHOTO_HOST_INSTANCE_ID", INHERITED_VALUE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "isolated inherited-marker test process must pass"
        );
    }
}

#[test]
fn existing_token_or_public_state_is_refused_without_overwriting() {
    let f = Fixture::new("clean");
    let existing = f.root.join("existing-token");
    fs::write(&existing, "do-not-replace").unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(create_token(&existing).is_err());
    assert_eq!(fs::read_to_string(&existing).unwrap(), "do-not-replace");
    let link = f.root.join("token-link");
    std::os::unix::fs::symlink(&existing, &link).unwrap();
    assert!(create_token(&link).is_err());
    assert_eq!(fs::read_to_string(&existing).unwrap(), "do-not-replace");
    fs::create_dir(&f.options.state_dir).unwrap();
    fs::set_permissions(&f.options.state_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let mut host = NativeHost::new();
    assert!(matches!(
        host.start(f.options.clone()),
        Err(HostError::InvalidOptions(_))
    ));
    assert!(
        host.current().unwrap().is_none(),
        "validation must precede spawn"
    );
}

#[test]
fn wrong_auth_or_readiness_never_produces_ready_instance() {
    for mode in [
        "wrong-auth-id",
        "wrong-auth-result",
        "reject-auth",
        "foreign-ready",
        "oversized-stderr",
        "no-ready",
    ] {
        let mut f = Fixture::new(mode);
        f.options.startup_timeout = Duration::from_millis(300);
        let mut h = TestHost::new();
        assert!(h.0.start(f.options.clone()).is_err(), "mode={mode}");
        let info = h.0.current().unwrap().unwrap();
        assert!(!info.ready);
        assert!(matches!(
            info.status,
            InstanceStatus::StartupFailed | InstanceStatus::Exited
        ));
        assert_eq!(
            f.commands().len(),
            0,
            "authentication failures must not dispatch ui.inspect"
        );
    }
}

#[test]
fn write_unknown_on_disconnect_deadline_bad_id_and_oversize_is_never_retried() {
    for op in [
        "disconnect",
        "timeout",
        "wrong-id",
        "oversize",
        "malformed",
        "bad-shape",
        "uncertain-error",
        "side-effect-then-error",
        "trickle",
    ] {
        let f = Fixture::new("clean");
        let mut h = TestHost::new();
        let info = h.0.start(f.options.clone()).unwrap();
        let started = Instant::now();
        let error =
            h.0.call_internal_after_gate(info.id, "engine.execute", json!({"op":op}))
                .unwrap_err();
        assert!(matches!(error, HostError::Unknown(_)), "{op}: {error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "absolute deadline must bound trickles"
        );
        assert_eq!(
            f.commands()
                .iter()
                .filter(|line| line.contains("engine.execute"))
                .count(),
            1,
            "zero automatic resend"
        );
        assert_eq!(
            h.0.current().unwrap().unwrap().status,
            InstanceStatus::Ready,
            "a call failure must not kill the editor"
        );
        if op == "side-effect-then-error" {
            assert_eq!(
                fs::read_to_string(f.root.join("side-effect.txt")).unwrap(),
                "applied-once"
            );
        }
    }
}

#[test]
fn read_failures_are_distinct_and_bad_response_shape_rejected() {
    for mode in [
        "inspect-bad-id",
        "inspect-bad-shape",
        "inspect-disconnect",
        "inspect-timeout",
    ] {
        let f = Fixture::new(mode);
        let mut h = TestHost::new();
        assert!(h.0.start(f.options.clone()).is_err());
        assert!(h.0.current().unwrap().is_some());
    }
}

#[test]
fn oversized_request_is_not_sent_and_screenshot_cannot_request_focus() {
    let f = Fixture::new("clean");
    let mut h = TestHost::new();
    let info = h.0.start(f.options.clone()).unwrap();
    assert!(matches!(
        h.0.call_internal_after_gate(
            info.id,
            "engine.execute",
            json!({"large":"x".repeat(transport::MAX_REQUEST)})
        ),
        Err(HostError::NotSent(_))
    ));
    assert!(
        !f.commands()
            .iter()
            .any(|line| line.contains("engine.execute"))
    );
    h.0.call_internal_after_gate(info.id, "ui.screenshot", json!({"focus":true}))
        .unwrap();
    assert!(
        f.commands()
            .iter()
            .any(|line| line.contains("\"focus\": false"))
    );
    assert!(matches!(
        h.0.call_internal_after_gate(info.id, "app.quit", json!({})),
        Err(HostError::ForbiddenPrimitive)
    ));
    assert!(matches!(
        h.0.call_internal_after_gate(info.id, "ui.focus", json!({})),
        Err(HostError::ForbiddenPrimitive)
    ));
}

#[test]
fn normal_close_waits_for_exit_dirty_close_keeps_documents_and_drop_does_not_kill() {
    let f = Fixture::new("clean");
    let mut h = TestHost::new();
    let first = h.0.start(f.options.clone()).unwrap();
    let first_instance =
        h.0.call_internal_after_gate(first.id, "test.instance_id", json!({}))
            .unwrap();
    assert_eq!(first_instance, json!(first.id.to_string()));
    assert!(matches!(
        h.0.close(first.id, Duration::from_secs(1)).unwrap(),
        CloseOutcome::Exited
    ));
    let second = h.0.start(f.options.clone()).unwrap();
    assert_ne!(first.id, second.id);
    let second_instance =
        h.0.call_internal_after_gate(second.id, "test.instance_id", json!({}))
            .unwrap();
    assert_eq!(second_instance, json!(second.id.to_string()));
    assert_ne!(first_instance, second_instance);
    assert!(matches!(
        h.0.inspect(first.id),
        Err(HostError::StaleInstance)
    ));
    h.0.close(second.id, Duration::from_secs(1)).unwrap();

    let f = Fixture::new("dirty");
    let mut h = NativeHost::new();
    let info = h.start(f.options.clone()).unwrap();
    let outcome = h.close(info.id, Duration::from_millis(50)).unwrap();
    match outcome {
        CloseOutcome::NeedsUser {
            inspection: Some(v),
        } => {
            assert_eq!(v["session"]["documents"][0]["dirty"], true);
            assert_eq!(v["dialogs"][0]["id"], "discard");
        }
        _ => panic!("dirty editor must remain available"),
    }
    assert!(
        !f.commands()
            .iter()
            .any(|line| line.contains("dontSave") || line.contains("app.quit"))
    );
    // Retain the actual Child only in this test so Drop can be checked and then reaped safely.
    let mut child = h.running.take().unwrap();
    let owned = std::mem::replace(
        &mut child.child,
        Command::new("/usr/bin/true").spawn().unwrap(),
    );
    child.child.wait().unwrap();
    drop(child);
    drop(h);
    let mut owned = owned;
    assert!(
        owned.try_wait().unwrap().is_none(),
        "dropping host ownership must not kill dirty fixture"
    );
    owned.kill().unwrap();
    owned.wait().unwrap();
}

#[test]
fn ready_parser_requires_exact_owned_log_loopback_and_nonzero_port() {
    assert!(parse_ready(b"photocraft: control server listening on 127.0.0.1:1234\n").is_some());
    for line in [
        "127.0.0.1:1234",
        "other: photocraft: control server listening on 127.0.0.1:1234",
        "photocraft: control server listening on 127.0.0.1:0",
        "photocraft: control server listening on 10.0.0.1:1234",
        "photocraft: control server listening on localhost:1234",
    ] {
        assert!(parse_ready(line.as_bytes()).is_none());
    }
}

const SCRIPT: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, socket, stat, sys, time, uuid
MODE = 'FIXTURE_MODE'
args = sys.argv[1:]
assert args[args.index('--control') + 1] == '0'
assert '--control-token' not in args
assert 'PHOTOCRAFT_CONTROL_TOKEN' not in os.environ
token_path = pathlib.Path(args[args.index('--control-token-file') + 1])
assert stat.S_IMODE(token_path.stat().st_mode) == 0o600
TOKEN = token_path.read_text().strip()
assert TOKEN not in args
assert pathlib.Path(os.environ['PHOTOCRAFT_CONFIG_DIR']).name == 'photocraft-profile'
HOST_INSTANCE = os.environ['SEECUT_PHOTO_HOST_INSTANCE_ID']
assert str(uuid.UUID(HOST_INSTANCE)) == HOST_INSTANCE
ROOT = pathlib.Path(__file__).parent
listener = socket.socket()
listener.bind(('127.0.0.1', 0))
listener.listen()
port = listener.getsockname()[1]
if MODE == 'no-ready':
    time.sleep(60)
elif MODE == 'foreign-ready':
    print('photocraft: control server listening on 10.0.0.1:1234', file=sys.stderr, flush=True)
elif MODE == 'oversized-stderr':
    print('x' * 9000, file=sys.stderr, flush=True)
else:
    print('photocraft: control server listening on 127.0.0.1:' + str(port), file=sys.stderr, flush=True)
prompt = False
def send(w, value):
    w.write((json.dumps(value) + '\n').encode()); w.flush()
while True:
    connection, _ = listener.accept()
    with connection:
        reader = connection.makefile('rb')
        writer = connection.makefile('wb')
        auth = json.loads(reader.readline())
        assert auth['method'] == 'auth' and auth['params']['token'] == TOKEN
        auth_id = auth['id'] + (1 if MODE == 'wrong-auth-id' else 0)
        if MODE == 'reject-auth':
            send(writer, {'id':auth_id,'ok':False,'error':'authentication required'}); continue
        send(writer, {'id':auth_id,'ok':True,'result':{'authenticated':MODE != 'wrong-auth-result'}})
        if MODE in ('wrong-auth-id', 'wrong-auth-result'): continue
        while True:
            raw = reader.readline()
            if not raw: break
            request = json.loads(raw)
            with open(ROOT / 'commands.jsonl', 'a') as log:
                log.write(json.dumps(request) + '\n')
            method, params, ident = request['method'], request['params'], request['id']
            if method == 'ui.inspect':
                if MODE == 'inspect-disconnect': connection.shutdown(socket.SHUT_RDWR); break
                if MODE == 'inspect-timeout': time.sleep(60)
                if MODE == 'inspect-bad-id': ident += 1
                result = {'window':{},'session':{'documents':[{'dirty':True}] if MODE == 'dirty' else []},'dialogs':[{'id':'discard'}] if prompt else []}
                if MODE == 'inspect-bad-shape': result = {'missing':'session'}
                send(writer, {'id':ident,'ok':True,'result':result})
            elif method == 'ui.menu.invoke':
                assert params['command'] == 'file.exit'
                prompt = MODE == 'dirty'
                send(writer, {'id':ident,'ok':True,'result':None})
                if not prompt: sys.exit(0)
            elif method == 'test.echo_secret': send(writer, {'id':ident,'ok':True,'result':TOKEN})
            elif method == 'test.instance_id': send(writer, {'id':ident,'ok':True,'result':HOST_INSTANCE})
            elif method == 'ui.screenshot':
                assert params['focus'] is False
                send(writer, {'id':ident,'ok':True,'result':None})
            else:
                op = params.get('op')
                if op == 'disconnect': connection.shutdown(socket.SHUT_RDWR); break
                if op == 'timeout': time.sleep(60)
                if op == 'wrong-id': send(writer, {'id':ident+1,'ok':True,'result':None})
                if op == 'oversize':
                    try: writer.write(b'x' * ((8 << 20) + 1)); writer.flush()
                    except (BrokenPipeError, ConnectionResetError): pass
                    break
                if op == 'malformed': writer.write(b'{broken\n'); writer.flush()
                if op == 'bad-shape': send(writer, {'id':ident,'ok':'true','result':None})
                if op == 'uncertain-error': send(writer, {'id':ident,'ok':False,'error':'timeout: operation may have completed'})
                if op == 'side-effect-then-error':
                    (ROOT / 'side-effect.txt').write_text('applied-once')
                    send(writer, {'id':ident,'ok':False,'error':'ordinary operation error'})
                if op == 'trickle':
                    for byte in b'{"slow":true}':
                        try: writer.write(bytes([byte])); writer.flush()
                        except (BrokenPipeError, ConnectionResetError): break
                        time.sleep(0.1)
"#;
