//! Synthetic owned child + actual authenticated loopback transport, not editor acceptance.
use super::*;
use crate::{InstanceInfo, InstanceStatus, Running};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const TOKEN: &str = "synthetic-edit-auth-only";
struct Fixture {
    root: PathBuf,
    host: NativeHost,
    id: Uuid,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.toonflow/photocraft-native-edit-tests");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let script = root.join("synthetic-edit-child.py");
        fs::write(&script, SCRIPT).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime_dir = root.join("runtime");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime_dir)
            .unwrap();
        let token_path = runtime_dir.join("control.token");
        fs::write(&token_path, TOKEN).unwrap();
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
        let id = Uuid::new_v4();
        let child = Command::new("python3")
            .arg(&script)
            .arg(&root)
            .arg(mode)
            .env("EDIT_TEST_HOST", id.to_string())
            .env("EDIT_TEST_AUTH", TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut fixture = Self {
            root,
            id,
            host: NativeHost {
                running: Some(Running {
                    child,
                    info: InstanceInfo {
                        id,
                        pid,
                        build_attempt: "synthetic-wire-only".into(),
                        ready: false,
                        status: InstanceStatus::Starting,
                    },
                    addr: None,
                    token: TOKEN.into(),
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
        // Actual authentication and ui.inspect exchange, without a fake Connection.
        let inspected = crate::call(running, "ui.inspect", json!({}), false).unwrap();
        crate::validate_inspection(&inspected).unwrap();
        running.info.ready = true;
        running.info.status = InstanceStatus::Ready;
        fixture
    }
    fn identity(&self) -> PhotoIdentity {
        PhotoIdentity {
            host_instance_id: self.id,
            doc_id: u64::MAX,
            document_generation: u64::MAX - 1,
            revision: u64::MAX - 2,
        }
    }
    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn count(&self, method: &str) -> usize {
        self.calls()
            .iter()
            .filter(|v| v["method"] == method)
            .count()
    }
    fn execute(&mut self, operation_id: &str, action: PhotoEditAction) -> Result<PhotoReceipt> {
        let expected = self.identity();
        self.host
            .photo_execute_after_gate(self.id, operation_id, &expected, action)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(running) = self.host.running.as_mut() {
            if running.child.try_wait().ok().flatten().is_none() {
                let _ = running.child.kill();
            }
            let _ = running.child.wait();
        }
        self.host.running = None;
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn state(host: Uuid) -> Value {
    json!({"identity":{"hostInstanceId":host.to_string(),"docId":u64::MAX,"documentGeneration":u64::MAX-1,"revision":u64::MAX-2},
        "activeLayer":u64::MAX-3,"target":{"id":u64::MAX-3,"name":"untrusted-target-title","opacity":0.5,"visible":true},
        "canUndo":true,"canRedo":false,"historyEntries":u64::MAX})
}
fn receipt(host: Uuid) -> Value {
    json!({"operationId":"original:fixed","status":"applied","errorCode":null,"before":state(host),"after":state(host),"safeToResend":false})
}
#[test]
fn seecut_edit_real_wire_is_exact_at_zero_one_and_undo_with_native_u64() {
    let mut f = Fixture::new("normal");
    let expected = f.identity();
    for (id, opacity) in [("parent-fixed:zero", 0.0), ("parent-fixed:one", 1.0)] {
        let result = f
            .execute(
                id,
                PhotoEditAction::LayerSetOpacity {
                    layer_id: u64::MAX - 3,
                    opacity,
                },
            )
            .unwrap();
        assert_eq!(result.operation_id(), id);
        assert_eq!(result.status, PhotoEditStatus::Applied);
        assert!(!result.safe_to_resend());
        let before = result.before.as_ref().unwrap();
        let after = result.after.as_ref().unwrap();
        assert_eq!(before.identity, expected);
        assert_eq!(
            after.identity, expected,
            "no-op receipt need not increment revision"
        );
        assert_eq!(before.history_entries, u64::MAX);
        assert_eq!(
            after.history_entries,
            u64::MAX,
            "no-op receipt need not add history"
        );
        assert_eq!(after.target.as_ref().unwrap().opacity, opacity);
        assert!(!format!("{result:?}").contains("untrusted-target-title"));
    }
    let result = f
        .execute("parent-fixed:undo", PhotoEditAction::Undo)
        .unwrap();
    assert!(result.before.unwrap().target.is_none());
    assert!(result.after.unwrap().target.is_none());
    let calls: Vec<Value> = f
        .calls()
        .into_iter()
        .filter(|v| v["method"] == "seecut.execute")
        .collect();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0]["params"],
        json!({"operationId":"parent-fixed:zero","expected":{
        "hostInstanceId":f.id.to_string(),"docId":u64::MAX,"documentGeneration":u64::MAX-1,"revision":u64::MAX-2},
        "action":{"kind":"layerSetOpacity","layerId":u64::MAX-3,"opacity":0.0}})
    );
    assert_eq!(
        calls[1]["params"]["action"],
        json!({"kind":"layerSetOpacity","layerId":u64::MAX-3,"opacity":1.0})
    );
    assert_eq!(calls[2]["params"]["action"], json!({"kind":"undo"}));
    assert_eq!(calls[2]["params"].as_object().unwrap().len(), 3);
}
#[test]
fn seecut_edit_invalid_local_bindings_and_opacity_never_send() {
    let mut f = Fixture::new("normal");
    for opacity in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01] {
        assert!(matches!(
            f.execute(
                "fixed",
                PhotoEditAction::LayerSetOpacity {
                    layer_id: 1,
                    opacity
                }
            ),
            Err(HostError::NotSent(_))
        ));
    }
    assert!(matches!(
        f.execute(
            "fixed",
            PhotoEditAction::LayerSetOpacity {
                layer_id: 0,
                opacity: 0.5
            }
        ),
        Err(HostError::NotSent(_))
    ));
    for id in [
        "".to_owned(),
        "x".repeat(129),
        "non ASCII 空".into(),
        "bad/slash".into(),
    ] {
        assert!(matches!(
            f.execute(&id, PhotoEditAction::Undo),
            Err(HostError::NotSent(_))
        ));
        assert!(matches!(
            f.host.photo_receipt_after_gate(f.id, &id),
            Err(HostError::NotSent(_))
        ));
    }
    for field in 0..4 {
        let mut expected = f.identity();
        match field {
            0 => expected.doc_id = 0,
            1 => expected.document_generation = 0,
            2 => expected.revision = 0,
            _ => expected.host_instance_id = Uuid::new_v4(),
        }
        assert!(matches!(
            f.host
                .photo_execute_after_gate(f.id, "fixed", &expected, PhotoEditAction::Undo),
            Err(HostError::NotSent(_))
        ));
    }
    assert_eq!(f.count("seecut.execute"), 0);
    assert_eq!(f.count("seecut.receipt"), 0);
    let longest = "x".repeat(128);
    f.execute(&longest, PhotoEditAction::Undo).unwrap();
    assert_eq!(f.count("seecut.execute"), 1);
}
#[test]
fn seecut_edit_after_dispatch_malformed_error_and_disconnect_are_unknown_without_retry() {
    for mode in [
        "error",
        "unknown",
        "wrong-operation",
        "wrong-host",
        "wrong-before",
        "wrong-after",
        "extra",
        "wrong-status",
        "safe-true",
        "oversized",
        "corrupt",
        "wrong-wire-id",
        "disconnect",
    ] {
        let mut f = Fixture::new(mode);
        assert!(
            matches!(
                f.execute("parent-original", PhotoEditAction::Undo),
                Err(HostError::Unknown(_))
            ),
            "{mode}"
        );
        assert_eq!(
            f.count("seecut.execute"),
            1,
            "{mode}: one actual write only"
        );
        assert_eq!(
            f.count("seecut.receipt"),
            0,
            "no implicit follow-up or recovery"
        );
    }
}
#[test]
fn seecut_edit_rejected_and_failed_preserve_actual_different_after_state() {
    for (mode, status) in [
        ("rejected", PhotoEditStatus::Rejected),
        ("failed", PhotoEditStatus::Failed),
    ] {
        let mut f = Fixture::new(mode);
        let result = f.execute("parent-original", PhotoEditAction::Undo).unwrap();
        assert_eq!(result.status, status);
        let before = result.before.as_ref().unwrap();
        let after = result.after.as_ref().unwrap();
        assert_ne!(before.identity.doc_id, after.identity.doc_id);
        assert_ne!(
            before.identity.document_generation,
            after.identity.document_generation
        );
        assert_ne!(before.history_entries, after.history_entries);
        assert!(!result.safe_to_resend());
        assert_eq!(f.count("seecut.execute"), 1);
    }
}
#[test]
fn seecut_edit_receipt_unknown_is_independent_closed_read_with_zero_resend() {
    let mut f = Fixture::new("unknown");
    for _ in 0..3 {
        let result = f
            .host
            .photo_receipt_after_gate(f.id, "parent-original")
            .unwrap();
        let PhotoReceiptLookup::Unknown(unknown) = result else {
            panic!("expected independent unknown receipt")
        };
        assert_eq!(unknown.operation_id(), "parent-original");
        assert!(!unknown.safe_to_resend() && !unknown.restart_recovery());
    }
    assert_eq!(f.count("seecut.execute"), 0);
    let calls: Vec<Value> = f
        .calls()
        .into_iter()
        .filter(|v| v["method"] == "seecut.receipt")
        .collect();
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|v| v["params"] == json!({"operationId":"parent-original"}))
    );
    // A restarted instance identity rejects before any lookup is sent.
    assert!(matches!(
        f.host
            .photo_receipt_after_gate(Uuid::new_v4(), "parent-original"),
        Err(HostError::StaleInstance)
    ));
    assert_eq!(f.count("seecut.receipt"), 3);
    let previous_child_id = f.id;
    drop(f);
    let mut restarted = Fixture::new("unknown");
    assert_ne!(restarted.id, previous_child_id);
    assert!(matches!(
        restarted
            .host
            .photo_receipt_after_gate(restarted.id, "parent-original")
            .unwrap(),
        PhotoReceiptLookup::Unknown(_)
    ));
    assert_eq!(restarted.count("seecut.execute"), 0);
    assert_eq!(restarted.count("seecut.receipt"), 1);
}
#[test]
fn seecut_edit_receipt_known_read_and_bad_read_are_never_writes() {
    let mut f = Fixture::new("normal");
    let PhotoReceiptLookup::Known(result) = f
        .host
        .photo_receipt_after_gate(f.id, "parent-original")
        .unwrap()
    else {
        panic!("known receipt")
    };
    assert_eq!(result.status, PhotoEditStatus::Applied);
    assert_eq!(result.before.unwrap().identity.doc_id, u64::MAX);
    assert_eq!(f.count("seecut.execute"), 0);
    assert_eq!(f.count("seecut.receipt"), 1);
    for mode in [
        "error",
        "wrong-operation",
        "wrong-host",
        "extra",
        "wrong-status",
        "safe-true",
        "oversized",
        "corrupt",
        "wrong-wire-id",
        "disconnect",
    ] {
        let mut f = Fixture::new(mode);
        assert!(
            f.host
                .photo_receipt_after_gate(f.id, "parent-original")
                .is_err(),
            "{mode}"
        );
        assert_eq!(f.count("seecut.execute"), 0);
        assert_eq!(f.count("seecut.receipt"), 1);
    }
}
#[test]
fn seecut_edit_parser_is_strict_at_all_receipt_levels_and_preserves_u64max() {
    let host = Uuid::new_v4();
    let base = receipt(host);
    let parsed = parse_lookup(&base, host, "original:fixed").unwrap();
    let PhotoReceiptLookup::Known(parsed) = parsed else {
        panic!("known")
    };
    assert_eq!(parsed.before.as_ref().unwrap().identity.doc_id, u64::MAX);
    assert_eq!(parsed.after.as_ref().unwrap().history_entries, u64::MAX);
    for (pointer, value) in [
        ("/status", json!("queued")),
        ("/safeToResend", json!(true)),
        ("/operationId", json!("different")),
        ("/before/identity/docId", json!(0)),
        ("/after/identity/documentGeneration", json!(0)),
        ("/before/identity/revision", json!(0)),
        ("/before/identity/revision", json!(9007199254740992_f64)),
        ("/before/activeLayer", json!(0)),
        ("/after/target/id", json!(0)),
        ("/after/target/opacity", Value::Null),
        ("/after/target/opacity", json!(1.1)),
        ("/after/target/visible", json!(1)),
        ("/after/target/name", json!("x".repeat(1025))),
        ("/before/historyEntries", json!(-1)),
        ("/errorCode", json!("engineError")),
        ("/before", Value::Null),
        ("/after", Value::Null),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            parse_lookup(&changed, host, "original:fixed").is_err(),
            "{pointer}"
        );
    }
    for pointer in ["", "/before", "/after", "/before/identity", "/after/target"] {
        let mut changed = base.clone();
        changed
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unrecognized".into(), json!(true));
        assert!(
            parse_lookup(&changed, host, "original:fixed").is_err(),
            "{pointer}"
        );
    }
    let mut enormous = base.clone();
    enormous["after"]["target"]["name"] = json!("x".repeat(MAX_RESULT));
    assert!(parse_lookup(&enormous, host, "original:fixed").is_err());
    let unknown = json!({"operationId":"original:fixed","status":"unknown","safeToResend":false,"cache":"memoryOnlyRestartDoesNotRecover"});
    assert!(matches!(
        parse_lookup(&unknown, host, "original:fixed"),
        Ok(PhotoReceiptLookup::Unknown(_))
    ));
    for (key, value) in [
        ("before", Value::Null),
        ("cache", json!("durable")),
        ("safeToResend", json!(true)),
        ("operationId", json!("other")),
    ] {
        let mut changed = unknown.clone();
        changed[key] = value;
        assert!(parse_lookup(&changed, host, "original:fixed").is_err());
    }
    // Real no-document rejected receipts may have neither boundary document.
    let mut rejected = base;
    rejected["status"] = json!("rejected");
    rejected["errorCode"] = json!("noDocument");
    rejected["before"] = Value::Null;
    rejected["after"] = Value::Null;
    assert!(parse_lookup(&rejected, host, "original:fixed").is_ok());
}

#[test]
fn seecut_edit_applied_target_binding_mismatch_is_unknown_without_resend() {
    for mode in [
        "wrong-target-before",
        "wrong-target-after",
        "missing-target-before",
        "missing-target-after",
    ] {
        let mut f = Fixture::new(mode);
        assert!(
            matches!(
                f.execute(
                    "parent-original",
                    PhotoEditAction::LayerSetOpacity {
                        layer_id: u64::MAX - 3,
                        opacity: 0.5
                    }
                ),
                Err(HostError::Unknown(_))
            ),
            "{mode}"
        );
        assert_eq!(f.count("seecut.execute"), 1);
        assert_eq!(f.count("seecut.receipt"), 0);
    }
    for mode in ["undo-target-before", "undo-target-after"] {
        let mut f = Fixture::new(mode);
        assert!(
            matches!(
                f.execute("parent-original", PhotoEditAction::Undo),
                Err(HostError::Unknown(_))
            ),
            "{mode}"
        );
        assert_eq!(f.count("seecut.execute"), 1);
        assert_eq!(f.count("seecut.receipt"), 0);
    }
    // A receipt read has no original action argument: return the actual typed
    // facts for the original parent payload to verify, without inventing an action.
    let mut f = Fixture::new("wrong-target-before");
    let PhotoReceiptLookup::Known(receipt) = f
        .host
        .photo_receipt_after_gate(f.id, "parent-original")
        .unwrap()
    else {
        panic!("known actual facts")
    };
    assert_eq!(receipt.before.unwrap().target.unwrap().id, u64::MAX - 4);
    assert_eq!(f.count("seecut.execute"), 0);
    assert_eq!(f.count("seecut.receipt"), 1);
}

const SCRIPT: &str = r#"#!/usr/bin/env python3
import copy,json,os,pathlib,socket,sys
root=pathlib.Path(sys.argv[1]); mode=sys.argv[2]
host=os.environ['EDIT_TEST_HOST']; token=os.environ['EDIT_TEST_AUTH']
server=socket.socket(); server.bind(('127.0.0.1',0)); server.listen(4)
print('photocraft: control server listening on 127.0.0.1:'+str(server.getsockname()[1]),file=sys.stderr,flush=True)
def boundary(identity,action):
    layer=action.get('layerId',18446744073709551612)
    target=None if action.get('kind')=='undo' else {'id':layer,'name':'untrusted-target-title','opacity':action.get('opacity',0.5),'visible':True}
    return {'identity':identity,'activeLayer':18446744073709551612,'target':target,'canUndo':True,'canRedo':False,'historyEntries':18446744073709551615}
while True:
    conn,_=server.accept()
    with conn:
        reader=conn.makefile('rb')
        for line in reader:
            msg=json.loads(line); method=msg['method']; params=msg['params']
            if method=='auth':
                assert params=={'token':token}
                result={'authenticated':True}
            elif method=='ui.inspect':
                result={'session':{'documents':[]},'dialogs':[],'window':{}}
            else:
                assert method in ('seecut.execute','seecut.receipt')
                with (root/'calls.jsonl').open('a') as log: log.write(json.dumps(msg)+'\n')
                identity=params.get('expected',{'hostInstanceId':host,'docId':18446744073709551615,'documentGeneration':18446744073709551614,'revision':18446744073709551613})
                before=boundary(copy.deepcopy(identity),params.get('action',{})); after=copy.deepcopy(before)
                result={'operationId':params['operationId'],'status':'applied','errorCode':None,'before':before,'after':after,'safeToResend':False}
                if mode=='unknown': result={'operationId':params['operationId'],'status':'unknown','safeToResend':False,'cache':'memoryOnlyRestartDoesNotRecover'}
                elif mode=='wrong-operation': result['operationId']='wrong-original-id'
                elif mode=='wrong-host': result['before']['identity']['hostInstanceId']='b7b10cf7-a908-4fbb-80f4-56a2dfe169aa'
                elif mode=='wrong-before': result['before']['identity']['revision']-=1
                elif mode=='wrong-after': result['after']['identity']['docId']-=1
                elif mode=='wrong-target-before': result['before']['target']['id']-=1
                elif mode=='wrong-target-after': result['after']['target']['id']-=1
                elif mode=='missing-target-before': result['before']['target']=None
                elif mode=='missing-target-after': result['after']['target']=None
                elif mode=='undo-target-before': result['before']['target']={'id':18446744073709551612,'name':'untrusted-target-title','opacity':0.5,'visible':True}
                elif mode=='undo-target-after': result['after']['target']={'id':18446744073709551612,'name':'untrusted-target-title','opacity':0.5,'visible':True}
                elif mode=='extra': result['extra']=True
                elif mode=='wrong-status': result['status']='accepted'
                elif mode=='safe-true': result['safeToResend']=True
                elif mode=='oversized': result['unexpected']='x'*(512*1024)
                elif mode in ('rejected','failed'):
                    result['status']=mode;result['errorCode']='engineError' if mode=='failed' else 'staleRevision'
                    result['after']['identity']['docId']-=8;result['after']['identity']['documentGeneration']-=8;result['after']['historyEntries']-=8
                if mode=='disconnect': break
                if mode=='corrupt': conn.sendall(b'not JSON\n');continue
                if mode=='error': conn.sendall((json.dumps({'id':msg['id'],'ok':False,'error':'synthetic rejected after dispatch'})+'\n').encode());continue
            reply={'id':msg['id']+1 if mode=='wrong-wire-id' and method not in ('auth','ui.inspect') else msg['id'],'ok':True,'result':result}
            conn.sendall((json.dumps(reply)+'\n').encode())
        reader.close()
"#;
