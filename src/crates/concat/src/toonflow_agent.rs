//! Native authorization lives in editor_mcp. This module only transports current-UI calls.
use std::{cell::RefCell, collections::HashMap, sync::{Mutex, OnceLock, mpsc}, time::Duration};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use concat_editor_mcp::{FlowWorkspace, Response};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all="camelCase")]
pub(crate) struct Context {
    pub project_id: String, pub canvas_id: String, pub session_id: String,
    pub account_scope: String, pub revision: u64,
    #[serde(default)] pub selected_node_ids: Vec<String>,
    #[serde(default)] pub name: String,
    #[serde(default)] pub busy: bool,
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }
pub(crate) fn context() -> Option<Context> { CONTEXT.with(|v| v.borrow().clone()) }
pub(crate) fn update(value: Value) { CONTEXT.with(|v| *v.borrow_mut() = serde_json::from_value::<Context>(value).ok().filter(|c| c.project_id.len()<=128 && c.canvas_id.len()<=512 && c.session_id.len()<=128 && c.account_scope.len()<=512 && c.selected_node_ids.len()<=10000)); }
fn pending() -> &'static Mutex<HashMap<String,mpsc::SyncSender<Response>>> { static PENDING: OnceLock<Mutex<HashMap<String,mpsc::SyncSender<Response>>>>=OnceLock::new(); PENDING.get_or_init(||Mutex::new(HashMap::new())) }
pub(crate) fn complete(id: String, value: Value, error: Option<String>) {
    if let Some(sender)=pending().lock().unwrap().remove(&id) {
        let reply=if let Some(error)=error { Response {status:"flowError".into(), data:Some(json!({"message":error}))} } else { Response::ok(value) };
        let _=sender.send(reply);
    }
}
pub(crate) fn call(call: FlowWorkspace, client: String, authorize: impl FnOnce() -> Result<(),Response> + Send + 'static) -> Response {
    let id=uuid::Uuid::new_v4().to_string(); let request_id=id.clone();
    let (sender,receiver)=mpsc::sync_channel(1);
    { let mut pending=pending().lock().unwrap(); if pending.len()>=8 {return Response::error("busy")};pending.insert(id.clone(),sender); }
    let scheduled=slint::invoke_from_event_loop(move|| {
        if let Err(error)=authorize() { if let Some(sender)=pending().lock().unwrap().remove(&request_id) {let _=sender.send(error);} return; }
        let mut payload=serde_json::to_value(&call).unwrap(); payload["clientId"]=json!(client);
        let script=format!("(async()=>{{try{{if(!window.__SEECUT_FLOW_CALL__)throw Error('FLOW_UNAVAILABLE');const result=await window.__SEECUT_FLOW_CALL__({});window.ipc.postMessage(JSON.stringify({{type:'seecut:flow-result',id:{},result}}));}}catch(e){{window.ipc.postMessage(JSON.stringify({{type:'seecut:flow-result',id:{},result:null,error:String(e.message||e)}}));}}}})()",serde_json::to_string(&payload).unwrap(),json!(request_id),json!(request_id));
        if !crate::toonflow_webview::evaluate(&script) { complete(request_id,Value::Null,Some("FLOW_UNAVAILABLE".into())); }
    });
    let result=if scheduled.is_err() { Response::error("appUnavailable") } else {receiver.recv_timeout(Duration::from_secs(8)).unwrap_or_else(|_|Response::error("outcomeUnknown"))};
    pending().lock().unwrap().remove(&id);result
}

// Exact trusted approval; allocating an operation ID later cannot change its intent.
struct Permit { id:u64, call:FlowWorkspace, operation:Option<String>, expires:std::time::Instant }
thread_local! { static PERMITS: RefCell<Vec<Permit>>=const {RefCell::new(Vec::new())}; static NEXT: CellCounter=const {CellCounter(std::cell::Cell::new(1))}; }
struct CellCounter(std::cell::Cell<u64>);
pub(crate) fn approval(tool:&str, args:Value) -> Option<FlowWorkspace> {
    let mut call:FlowWorkspace=serde_json::from_value(args).ok()?;
    call.name=tool.into(); call.operation_id=Some("approval".into());
    if call.is_read() || !call.valid() {return None} call.operation_id=None;Some(call)
}
pub(crate) fn grant(call:FlowWorkspace) -> u64 {
    let id=NEXT.with(|n|{let id=n.0.get();n.0.set(id+1);id | (1<<63)});
    PERMITS.with(|p|{let mut p=p.borrow_mut();p.retain(|p|p.expires>std::time::Instant::now());p.push(Permit{id,call,operation:None,expires:std::time::Instant::now()+Duration::from_secs(300)});});id
}
pub(crate) fn revoke(id:u64) {PERMITS.with(|p|p.borrow_mut().retain(|p|p.id!=id));}
pub(crate) fn claim(call:&FlowWorkspace)->bool {
    let mut intent=call.clone();intent.operation_id=None;
    PERMITS.with(|p|p.borrow_mut().iter_mut().any(|p| {
        if p.expires<=std::time::Instant::now() || p.call!=intent {return false}
        if p.operation.is_none(){p.operation=call.operation_id.clone();}
        p.operation.is_some() && p.operation==call.operation_id
    }))
}
