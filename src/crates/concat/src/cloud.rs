// SPDX-License-Identifier: AGPL-3.0-or-later
//! SeeCut's non-blocking desktop bridge. Provider credentials never enter this process.

use crate::format::project_timestamp;
use crate::ui::{
    AccountEntry, App, CloudItem, CreditPlan, GenerationBatch, GenerationTemplate as TemplateRow,
    PersonalAssetGroup, SeeCut,
};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    net::IpAddr,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
use url::Url;

mod preview_validation;

/// The opt-in software-rendered fixture exercises local state transitions with
/// a signed-out synthetic Cloud. Normal startup never invokes this entry.
pub(crate) fn validate_preview_state(app: &App, directory: &std::path::Path) -> Result<(), String> {
    preview_validation::run(app, directory)
}

const DEFAULT_API_URL: &str = "https://seecut.stormycry.cloud";

struct ClientError {
    status: u16,
    code: String,
    message: String,
    retry_after: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TaskInputSnapshot {
    mode: i32,
    prompt: String,
    model_id: String,
    parameters: Value,
    #[serde(default)]
    references: Vec<crate::generation_templates::TemplateReference>,
}

#[derive(Clone)]
struct PendingSubmission {
    body: Value,
    key: String,
    snapshot: TaskInputSnapshot,
}

impl From<String> for ClientError {
    fn from(message: String) -> Self {
        Self {
            status: 0,
            code: String::new(),
            message,
            retry_after: 0,
        }
    }
}

impl From<&str> for ClientError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

#[derive(Clone, Default)]
struct Cloud {
    base: String,
    token: String,
    teams: Vec<Value>,
    models: Vec<Value>,
    /// The catalog can be refreshed and reordered while the form is open. Keep
    /// the user's choice by stable model id and retain the raw values per model
    /// instead of treating the UI index as persisted state.
    selected_model_ids: HashMap<String, String>,
    parameter_drafts: HashMap<String, Value>,
    generation_mode: Option<i32>,
    updating_model_options: bool,
    unavailable_model_id: Option<String>,
    unavailable_model_kind: Option<String>,
    tasks: Vec<Value>,
    selected_tasks: HashSet<String>,
    pending_result_batch: Option<PendingResultBatch>,
    pending_task_delete_ids: Vec<String>,
    quote_id: String,
    quote_credits: Option<i64>,
    quote_body: Value,
    pending: VecDeque<(String, Request)>,
    assets: Vec<Value>,
    assets_context: String,
    personal: Vec<Value>,
    personal_folders: Vec<Value>,
    selected_personal: HashSet<String>,
    pending_handoff: Option<PendingHandoff>,
    pending_delete_ids: Vec<String>,
    /// Ordered, temporary selection used only by the asset picker. It is
    /// scoped to the current source/purpose/team context and never overlaps
    /// the personal-library management selection above.
    picker_selected_ids: Vec<String>,
    picker_context: String,
    picker_batch: Option<(String, Vec<PickerSelection>)>,
    pending_personal_references: VecDeque<(String, String)>,
    pending_template_id: Option<String>,
    pending_task_refill_id: Option<String>,
    pending_team_picker: bool,
    auth_return_page: Option<i32>,
    pending_imports: Vec<PathBuf>,
    pending_import_display_names: HashMap<PathBuf, String>,
    references: Vec<Value>,
    local: HashMap<String, String>,
    folder: PathBuf,
    history_name: String,
    task_snapshot_name: String,
    task_snapshots: HashMap<String, TaskInputSnapshot>,
    invite_id: String,
    orders: Vec<Value>,
    submission: Option<PendingSubmission>,
    pending_task_references: HashSet<String>,
    epoch: u64,
    active_name: String,
    download_attempts: std::collections::HashSet<String>,
    media_preview_token: u64,
    export_copy: Option<ExportCopyJob>,
}

#[derive(Clone)]
struct ExportCopyItem {
    id: String,
    source: PathBuf,
    name: String,
}

#[derive(Clone)]
struct ExportCopyJob {
    items: Vec<ExportCopyItem>,
    folder: PathBuf,
    completed: HashSet<String>,
    running: bool,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    bytes_done: std::sync::Arc<std::sync::atomic::AtomicU64>,
    bytes_total: u64,
}

#[derive(Clone)]
struct CanvasProjectRow {
    item: CloudItem,
    updated: u64,
}

// Slint images are UI-thread values and cannot enter the Cloud snapshot sent
// to workers. Keep decoded gallery previews beside the UI for search/sort.
thread_local! {
    static CANVAS_PROJECT_CACHE: RefCell<Vec<CanvasProjectRow>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
struct PendingHandoff {
    kind: String,
    imports: Vec<crate::panes::canvas::CanvasImport>,
    awaiting_new_project: bool,
}

#[derive(Clone)]
struct PendingResultBatch {
    intent: String,
    ids: Vec<String>,
}
#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    body: Value,
    idem: Option<String>,
}
fn request(method: &str, path: impl Into<String>, body: Value) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        body,
        idem: None,
    }
}
fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn set_recovery_warning(ui: &SeeCut, warning: impl Into<SharedString>) {
    let warning = warning.into();
    ui.set_recovery_warning(warning.clone());
    ui.set_error(warning);
}

fn clear_recovery_warning(ui: &SeeCut) {
    ui.set_recovery_warning("".into());
}

fn sync_reference_recovery_warning(ui: &SeeCut, state: &Rc<RefCell<Cloud>>) {
    let unresolved = state
        .borrow()
        .references
        .iter()
        .filter(|reference| text(reference, "status") != "ready")
        .map(|reference| {
            let name = text(reference, "display_name");
            if name.is_empty() {
                "未命名素材".to_owned()
            } else {
                name
            }
        })
        .collect::<Vec<_>>();
    let previous = ui.get_recovery_warning().to_string();
    if unresolved.is_empty() {
        if previous.contains("参考素材") {
            clear_recovery_warning(ui);
            if ui.get_error().as_str() == previous {
                ui.set_error("".into());
            }
        }
        return;
    }
    let message = format!(
        "{}项参考素材仍需处理：{}",
        unresolved.len(),
        unresolved.join("、")
    );
    set_recovery_warning(ui, message);
}
fn items(v: &Value) -> Vec<Value> {
    v.get("items")
        .or_else(|| v.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
fn strings(values: Vec<String>) -> ModelRc<SharedString> {
    Rc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
    .into()
}
fn rows<T: Clone + 'static>(values: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(values)).into()
}

fn base_url(raw: &str) -> Result<String, String> {
    let u = Url::parse(raw.trim()).map_err(|_| "服务地址格式无效")?;
    let loopback = u.scheme() == "http"
        && u.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
    if u.username() != ""
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || !matches!(u.path(), "" | "/")
    {
        return Err("服务地址格式无效".into());
    }
    if u.scheme() != "https" && !loopback {
        return Err("服务地址必须使用 HTTPS".into());
    }
    Ok(raw.trim().trim_end_matches('/').to_owned())
}

fn call(c: &Cloud, req: &Request) -> Result<Value, ClientError> {
    if req.path == "local:export-copy" {
        return copy_export_call(c).map_err(Into::into);
    }
    if req.path.starts_with("local:library-") {
        return library_call(c, req).map_err(Into::into);
    }
    if req.path == "local:upload" {
        let path = PathBuf::from(text(&req.body, "path"));
        let team = text(&req.body, "team");
        let purpose = text(&req.body, "purpose");
        let kind = reference_kind(&path);
        let mut preview = Value::Null;
        let mut duration_ms = None;
        if purpose == "generation_input" {
            // Probe and thumbnail off the UI thread before uploading the media.
            let info = concat_media::probe(&path)
                .map_err(|_| ClientError::from("无法读取参考素材，请检查文件是否完整"))?;
            if (kind == "audio" && info.audio.is_none())
                || (kind != "audio" && info.video.is_none())
            {
                return Err("参考素材内容与文件格式不一致".into());
            }
            duration_ms = info
                .duration
                .map(|time| time.as_f64())
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| (seconds * 1000.0).round() as u64);
            if let Ok(root) = library_root() {
                use crate::personal_library::{Asset, AssetKind, AssetSource, thumbnail};
                let asset = Asset {
                    id: String::new(),
                    name: String::new(),
                    path: path.clone(),
                    kind: match kind {
                        "video" => AssetKind::Video,
                        "audio" => AssetKind::Audio,
                        _ => AssetKind::Image,
                    },
                    source: AssetSource::Imported,
                    original_path: None,
                    created_at: 0,
                    trashed: false,
                    favorite: false,
                    folder_id: None,
                };
                preview = json!(thumbnail(&asset, &root));
            }
        }
        let upload = crate::cloud_files::upload(
            &c.base,
            &c.token,
            &path,
            &purpose,
            if team.is_empty() {
                None
            } else {
                Some(team.as_str())
            },
        )?;
        let endpoint = if purpose == "team_asset" {
            format!("/api/teams/{team}/assets")
        } else {
            "/api/generation/assets".into()
        };
        let mut result = call(
            c,
            &request("POST", endpoint, json!({"upload_id":upload["upload_id"]})),
        )?;
        result["local_path"] = json!(path);
        result["client_id"] = req.body["client_id"].clone();
        result["status"] = json!("ready");
        result["kind"] = json!(kind);
        result["preview_path"] = preview;
        if let Some(duration_ms) = duration_ms {
            result["media_duration_ms"] = json!(duration_ms);
        }
        result["intent_team"] = json!(team);
        return Ok(result);
    }
    if req.path == "local:download" {
        let mut url = text(&req.body, "url");
        if let Some(endpoint) = req.body.get("endpoint").and_then(Value::as_str) {
            let value = call(c, &request("POST", endpoint, Value::Null))?;
            url = text(&value, "download_url");
        }
        let path = crate::cloud_files::download(
            &c.base,
            &c.token,
            &url,
            &PathBuf::from(text(&req.body, "path")),
        )?;
        return Ok(json!({
            "id": req.body["id"],
            "path": path,
            "intent": req.body["intent"],
            "intent_team": req.body["intent_team"],
            "preview_token": req.body["preview_token"],
        }));
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut r = agent.request(&req.method, &format!("{}{}", c.base, req.path));
    if !c.token.is_empty() {
        r = r.set("Authorization", &format!("Bearer {}", c.token));
    }
    if let Some(key) = &req.idem {
        r = r.set("Idempotency-Key", key);
    }
    let result = if req.method == "GET" || req.method == "DELETE" {
        r.call()
    } else {
        r.send_json(req.body.clone())
    };
    match result {
        Ok(response) if response.status() == 204 => Ok(Value::Null),
        Ok(response) => response
            .into_json()
            .map_err(|_| "服务器返回了无效数据".into()),
        Err(ureq::Error::Status(code, response)) => {
            let body: Value = response.into_json().unwrap_or_default();
            let error_code = text(&body["error"], "code");
            let message = if error_code.starts_with("GENERATION_ASSET_") {
                "参考素材已过期或不可用，请重新上传".into()
            } else {
                body.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("请求失败 ({code})"))
            };
            Err(ClientError {
                status: code,
                code: error_code,
                message,
                retry_after: body
                    .pointer("/error/retry_after")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .clamp(0, 3600) as i32,
            })
        }
        Err(_) => Err("暂时无法连接 Seecut，请检查网络后重试".into()),
    }
}

fn is_auth_job(name: &str) -> bool {
    matches!(
        name,
        "login"
            | "register"
            | "verify-email"
            | "verification-resend"
            | "password-reset-request"
            | "password-reset-confirm"
    )
}

fn is_background_job(name: &str) -> bool {
    name.starts_with("local-cache:")
        || matches!(
            name,
            "quote"
                | "tasks"
                | "wallet"
                | "capabilities"
                | "models"
                | "teams"
                | "assets"
                | "members"
                | "plans"
                | "orders"
                | "personal-cache"
        )
}

fn session_expired(error: &ClientError) -> bool {
    error.status == 401
        || matches!(
            error.code.as_str(),
            "SESSION_EXPIRED"
                | "INVALID_SESSION"
                | "INVALID_ACCESS_TOKEN"
                | "AUTHENTICATION_REQUIRED"
                | "AUTH_REQUIRED"
                | "UNAUTHORIZED"
        )
}

fn request_requires_auth(req: &Request) -> bool {
    match req.path.as_str() {
        "local:export-copy" => false,
        "/api/capabilities" => false,
        path if path.starts_with("/api/auth/") && path != "/api/auth/logout" => false,
        path if path.starts_with("local:library-") => false,
        _ => true,
    }
}

fn can_start_request(signed_in: bool, req: &Request) -> bool {
    signed_in || !request_requires_auth(req)
}

fn should_surface_job_error(
    signed_in: bool,
    auth_open: bool,
    current_error_empty: bool,
    name: &str,
    authentication_error: bool,
) -> bool {
    if !signed_in && !is_auth_job(name) && authentication_error {
        return false;
    }
    is_auth_job(name) || !is_background_job(name) || (!auth_open && current_error_empty)
}

fn is_cloud_identity_error(message: &str) -> bool {
    matches!(
        message,
        "请先登录" | "登录状态已失效，请重新登录" | "请求失败 (401)"
    )
}

fn should_clear_identity_error_for_target(target: i32, message: &str) -> bool {
    matches!(target, 0 | 5) && is_cloud_identity_error(message)
}

fn sync_operation_state(app: &App, state: &Rc<RefCell<Cloud>>) {
    let cloud = state.borrow();
    let names = std::iter::once(cloud.active_name.as_str())
        .filter(|name| !name.is_empty())
        .chain(cloud.pending.iter().map(|(name, _)| name.as_str()));
    let (mut auth_busy, mut working) = (false, false);
    for name in names {
        auth_busy |= is_auth_job(name);
        working |= !is_auth_job(name) && !is_background_job(name);
    }
    let ui = app.global::<SeeCut>();
    ui.set_auth_busy(auth_busy);
    ui.set_working(working);
}

fn job(app: &App, state: &Rc<RefCell<Cloud>>, name: String, req: Request) {
    let ui = app.global::<SeeCut>();
    if !can_start_request(ui.get_signed_in(), &req) {
        if !is_background_job(&name) && name != "logout" {
            show_auth(app, state, None);
        }
        return;
    }
    if ui.get_busy() {
        let mut c = state.borrow_mut();
        if matches!(name.as_str(), "quote" | "tasks" | "wallet") {
            c.pending.retain(|(n, _)| n != &name);
        }
        c.pending.push_back((name, req));
        drop(c);
        sync_operation_state(app, state);
        return;
    }
    let snapshot = state.borrow().clone();
    state.borrow_mut().active_name = name.clone();
    let epoch = snapshot.epoch;
    let mut request_body = req.body.clone();
    if name == "quote" {
        request_body.as_object_mut().map(|body| body.remove("kind"));
    }
    let weak = app.as_weak();
    let state = state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let ui = app.global::<SeeCut>();
    ui.set_busy(true);
    if !is_background_job(&name) {
        ui.set_error("".into());
    }
    sync_operation_state(app, &state);
    std::thread::spawn(move || {
        let result = call(&snapshot, &req).map(|mut value| {
            if req.path == "/api/generation/quote" {
                value["client_request_body"] = request_body;
            }
            if req.path.starts_with("/api/teams/") && req.path.contains("/assets?trash=") {
                value = json!({"items": items(&value), "client_assets_context": req.path});
            }
            value
        });
        let _ = tx.send(result);
    });
    poll(weak, state, name, epoch, rx);
}

fn poll(
    weak: slint::Weak<App>,
    state: Rc<RefCell<Cloud>>,
    name: String,
    epoch: u64,
    rx: std::sync::mpsc::Receiver<Result<Value, ClientError>>,
) {
    slint::Timer::single_shot(Duration::from_millis(80), move || {
        let Some(app) = weak.upgrade() else { return };
        if state.borrow().epoch != epoch {
            return;
        }
        let result = match rx.try_recv() {
            Ok(value) => value,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                poll(weak, state, name, epoch, rx);
                return;
            }
            Err(_) => Err("请求未完成，请重试".into()),
        };
        app.global::<SeeCut>().set_busy(false);
        state.borrow_mut().active_name.clear();
        match result {
            Ok(value) => publish(&app, &state, &name, value),
            Err(error) => {
                let ui = app.global::<SeeCut>();
                if name == "export-copy" {
                    if let Some(batch) = state.borrow_mut().export_copy.as_mut() {
                        batch.running = false;
                    }
                    ui.set_export_copy_running(false);
                    ui.set_export_copy_error(format!("导出中断：{}", error.message).into());
                }
                if name.starts_with("personal-dialog-") {
                    ui.set_personal_dialog_busy(false);
                    ui.set_personal_dialog_error(error.message.into());
                    sync_operation_state(&app, &state);
                    let next = state.borrow_mut().pending.pop_front();
                    if let Some((name, req)) = next {
                        job(&app, &state, name, req);
                    }
                    return;
                }
                if ui.get_signed_in()
                    && name != "logout"
                    && !is_auth_job(&name)
                    && session_expired(&error)
                {
                    clear_session(&app, &state, true);
                    switch_auth(&ui, 0);
                    ui.set_auth_open(true);
                    ui.set_error("登录状态已失效，请重新登录".into());
                    sync_operation_state(&app, &state);
                    return;
                }
                if name == "logout" {
                    clear_session(&app, &state, false);
                    switch_auth(&ui, 0);
                    ui.set_auth_open(true);
                    ui.set_notice("已在本机退出登录".into());
                    sync_operation_state(&app, &state);
                    return;
                }
                if name == "login" {
                    ui.set_auth_password("".into());
                    if error.code == "EMAIL_NOT_VERIFIED" {
                        switch_auth(&ui, 3);
                    }
                }
                if name == "register" && error.code == "EMAIL_ALREADY_REGISTERED" {
                    switch_auth(&ui, 0);
                }
                if name == "register" && error.code == "EMAIL_DELIVERY_UNAVAILABLE" {
                    switch_auth(&ui, 3);
                }
                if matches!(
                    name.as_str(),
                    "verification-resend" | "password-reset-request"
                ) && error.retry_after > 0
                {
                    ui.set_auth_resend_seconds(error.retry_after);
                }
                if matches!(name.as_str(), "quote" | "generate")
                    && error.code.starts_with("GENERATION_ASSET_")
                {
                    for item in &mut state.borrow_mut().references {
                        if text(item, "status") == "ready" {
                            item["status"] = json!("expired");
                        }
                    }
                    render_references(&app, &state);
                    sync_reference_recovery_warning(&app.global::<SeeCut>(), &state);
                    refresh_quote(&app, &state);
                }
                if let Some(id) = name.strip_prefix("reference:") {
                    if let Some(item) = state
                        .borrow_mut()
                        .references
                        .iter_mut()
                        .find(|v| text(v, "client_id") == id)
                    {
                        item["status"] = json!("failed");
                        item["error"] = json!(error.message);
                    }
                    render_references(&app, &state);
                    sync_reference_recovery_warning(&app.global::<SeeCut>(), &state);
                    refresh_quote(&app, &state);
                }
                if let Some(id) = name.strip_prefix("local-reference:") {
                    state.borrow_mut().pending_task_references.remove(id);
                }
                if let Some(id) = name.strip_prefix("local-cache:") {
                    state.borrow_mut().download_attempts.remove(id);
                }
                if let Some(rest) = name.strip_prefix("media-preview:")
                    && let Some((id, token)) = rest.rsplit_once(':')
                    && let Ok(token) = token.parse()
                {
                    fail_media_preview(&app, &state, id, token, &error.message);
                }
                if name == "picker-batch" {
                    state.borrow_mut().picker_batch = None;
                }
                if name == "generate" {
                    refresh_quote(&app, &state);
                }
                if should_surface_job_error(
                    ui.get_signed_in(),
                    ui.get_auth_open(),
                    ui.get_error().is_empty(),
                    &name,
                    session_expired(&error),
                ) {
                    ui.set_error(error.message.into());
                }
            }
        }
        let next = state.borrow_mut().pending.pop_front();
        if let Some((name, req)) = next {
            job(&app, &state, name, req)
        } else {
            sync_operation_state(&app, &state);
        }
    });
}

fn selected_model(ui: &SeeCut, state: &Cloud) -> String {
    let kind = model_kind(ui.get_mode());
    if let Some(id) = state.selected_model_ids.get(kind)
        && state
            .models
            .iter()
            .any(|model| text(model, "id") == id.as_str() && text(model, "kind") == kind)
    {
        return id.clone();
    }
    model_id_at_index(&state.models, ui.get_mode(), ui.get_model_index()).unwrap_or_default()
}

fn model_kind(mode: i32) -> &'static str {
    if mode == 0 { "image" } else { "video" }
}

fn model_id_at_index(models: &[Value], mode: i32, index: i32) -> Option<String> {
    let index = usize::try_from(index).ok()?;
    models
        .iter()
        .filter(|model| text(model, "kind") == model_kind(mode))
        .nth(index)
        .map(|model| text(model, "id"))
        .filter(|id| !id.is_empty())
}

fn stable_model_index(models: &[Value], mode: i32, model_id: &str) -> Option<i32> {
    models
        .iter()
        .filter(|model| text(model, "kind") == model_kind(mode))
        .position(|model| text(model, "id") == model_id)
        .and_then(|index| i32::try_from(index).ok())
}

fn model_by_id<'a>(models: &'a [Value], kind: &str, id: &str) -> Option<&'a Value> {
    models
        .iter()
        .find(|model| text(model, "kind") == kind && text(model, "id") == id)
}

fn parameter_draft_key(mode: i32, model_id: &str) -> String {
    format!("{}:{model_id}", model_kind(mode))
}

fn parameter_draft_from_ui(ui: &SeeCut, model: &Value, mode: i32) -> Value {
    let mut values = serde_json::Map::new();
    let names: &[(&str, i32)] = if mode == 0 {
        &[
            ("size", ui.get_resolution_index()),
            ("quality", ui.get_quality_index()),
        ]
    } else {
        &[
            ("resolution", ui.get_resolution_index()),
            ("duration", ui.get_duration_index()),
            ("aspect_ratio", ui.get_ratio_index()),
        ]
    };
    for (name, index) in names {
        let value = parameter_value(model, name, *index);
        if !value.is_null() {
            values.insert((*name).into(), value);
        }
    }
    if model["parameters"].get("generate_audio").is_some() {
        values.insert("generate_audio".into(), json!(ui.get_generate_audio()));
    }
    if model["parameters"].get("quantity").is_some() {
        let value = parameter_value(model, "quantity", ui.get_quantity_index());
        if !value.is_null() {
            values.insert("quantity".into(), value);
        }
    }
    Value::Object(values)
}

fn remember_model_draft(ui: &SeeCut, state: &mut Cloud, mode: i32, model_id: &str) {
    if model_id.is_empty() {
        return;
    }
    if let Some(model) = model_by_id(&state.models, model_kind(mode), model_id) {
        state.parameter_drafts.insert(
            parameter_draft_key(mode, model_id),
            parameter_draft_from_ui(ui, model, mode),
        );
    }
    state
        .selected_model_ids
        .insert(model_kind(mode).to_owned(), model_id.to_owned());
}

fn remember_current_model_draft(ui: &SeeCut, state: &mut Cloud) {
    // The response may arrive after the user has switched modes. The live UI
    // mode is the only safe source for the form indices being captured here.
    let mode = ui.get_mode();
    let kind = model_kind(mode);
    let model_id = state
        .selected_model_ids
        .get(kind)
        .cloned()
        .or_else(|| model_id_at_index(&state.models, mode, ui.get_model_index()));
    if let Some(model_id) = model_id {
        remember_model_draft(ui, state, mode, &model_id);
    }
}

// Rewrite complete mention tokens in one pass so renumbering cannot cascade.
fn rewrite_mentions(prompt: &str, mut replacement: impl FnMut(usize) -> String) -> String {
    rewrite_media_mentions(prompt, |kind, number| {
        if kind == "图片" {
            replacement(number)
        } else {
            format!("@[{kind}{number}]")
        }
    })
}

fn rewrite_media_mentions(
    prompt: &str,
    mut replacement: impl FnMut(&str, usize) -> String,
) -> String {
    let mut result = String::new();
    let mut rest = prompt;
    while let Some(start) = rest.find("@[") {
        result.push_str(&rest[..start]);
        let token = &rest[start..];
        let kind = ["图片", "视频", "音频"]
            .into_iter()
            .find(|kind| token[2..].starts_with(kind));
        if let Some(kind) = kind
            && let Some(end) = token.find(']')
            && let Ok(number) = token[2 + kind.len()..end].parse::<usize>()
        {
            result.push_str(&replacement(kind, number));
            rest = &token[end + 1..];
        } else {
            result.push('@');
            rest = &token[1..];
        }
    }
    result.push_str(rest);
    result
}

fn reference_kind(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "webp" => "image",
        "mp4" | "mov" => "video",
        "mp3" | "wav" => "audio",
        _ => "unsupported",
    }
}

fn reference_label(kind: &str) -> &'static str {
    match kind {
        "video" => "视频",
        "audio" => "音频",
        _ => "图片",
    }
}

fn reference_number(references: &[Value], index: usize) -> usize {
    let kind = reference_label(&text(&references[index], "kind"));
    references[..=index]
        .iter()
        .filter(|v| reference_label(&text(v, "kind")) == kind)
        .count()
}

fn accepts_reference(model: &Value, kind: &str) -> bool {
    model["parameters"]["reference_asset_ids"]["accepted_media"]
        .as_array()
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str() == Some(&format!("{kind}/*")))
        })
}

fn validate_configuration_references(
    model: &Value,
    references: &[crate::generation_templates::TemplateReference],
    source: &str,
) -> Result<(), String> {
    let reference_limit = model["parameters"]["reference_asset_ids"]["max_items"]
        .as_u64()
        .unwrap_or(0) as usize;
    if references.len() > reference_limit {
        return Err(format!(
            "{source}包含 {} 项参考素材，当前模型最多支持 {reference_limit} 项",
            references.len()
        ));
    }
    let mut paths = HashSet::new();
    for reference in references {
        if !matches!(reference.kind.as_str(), "image" | "video" | "audio") {
            return Err(format!("{source}包含不支持的参考素材类型"));
        }
        if !paths.insert(reference.path.clone()) {
            return Err(format!("{source}包含重复的参考素材：{}", reference.name));
        }
        if !accepts_reference(model, &reference.kind) {
            return Err(format!(
                "当前模型不再支持{source}中的{}素材",
                reference_label(&reference.kind)
            ));
        }
        let kind_limit = model["parameters"]["reference_asset_ids"]["max_per_kind"][&reference.kind]
            .as_u64()
            .unwrap_or(reference_limit as u64) as usize;
        let kind_count = references
            .iter()
            .filter(|item| item.kind == reference.kind)
            .count();
        if kind_count > kind_limit {
            return Err(format!(
                "{source}中的{}参考最多 {kind_limit} 项",
                reference_label(&reference.kind)
            ));
        }
    }
    if !references.is_empty() && references.iter().all(|reference| reference.kind == "audio") {
        return Err(format!(
            "{source}中的参考音频需要搭配至少一张图片或一段视频"
        ));
    }
    Ok(())
}

fn reference_validation(ui: &SeeCut, state: &Cloud) -> String {
    if selected_model(ui, state).is_empty() {
        return "请选择可用的生成模型后再继续".into();
    }
    let model = state
        .models
        .iter()
        .find(|v| text(v, "id") == selected_model(ui, state))
        .cloned()
        .unwrap_or_default();
    if state
        .references
        .iter()
        .any(|reference| text(reference, "status") == "missing")
    {
        return "部分历史参考素材缺少本机文件，请替换后再生成".into();
    }
    if state
        .references
        .iter()
        .any(|reference| text(reference, "kind") == "unknown")
    {
        return "历史参考素材类型未知，请替换后再生成".into();
    }
    for kind in ["image", "video", "audio"] {
        let count = state
            .references
            .iter()
            .filter(|v| reference_label(&text(v, "kind")) == reference_label(kind))
            .count();
        if count > 0 && !accepts_reference(&model, kind) {
            return format!(
                "当前模型不支持{}参考，请移除对应素材",
                reference_label(kind)
            );
        }
        let limit = model["parameters"]["reference_asset_ids"]["max_per_kind"][kind]
            .as_u64()
            .unwrap_or(ui.get_reference_max().max(0) as u64) as usize;
        if count > limit {
            return format!("{}参考最多 {limit} 项", reference_label(kind));
        }
    }
    if !state.references.is_empty() && state.references.iter().all(|v| text(v, "kind") == "audio") {
        return "参考音频需要搭配至少一张图片或一段视频".into();
    }
    if state
        .references
        .iter()
        .any(|v| text(v, "status") == "expired")
    {
        return "参考素材已过期，请重新上传或移除".into();
    }
    if state.references.len() > ui.get_reference_max().max(0) as usize {
        return format!(
            "当前模型最多支持 {} 项参考素材，请移除多余素材",
            ui.get_reference_max()
        );
    }
    if state
        .references
        .iter()
        .any(|v| text(v, "status") == "failed")
    {
        return "参考素材上传失败，请重试或移除".into();
    }
    if state
        .references
        .iter()
        .any(|v| text(v, "status") != "ready")
    {
        return "参考素材上传中".into();
    }
    let mut invalid = ui.get_prompt().contains("[已移除参考");
    rewrite_media_mentions(&ui.get_prompt(), |kind, number| {
        let count = state
            .references
            .iter()
            .filter(|v| reference_label(&text(v, "kind")) == kind)
            .count();
        invalid |= number == 0 || number > count;
        String::new()
    });
    if invalid {
        "提示词包含已移除或无效的素材引用，请修改后生成".into()
    } else {
        String::new()
    }
}

#[cfg(test)]
fn invalid_reference_prompt(prompt: &str, count: usize) -> bool {
    let mut invalid = prompt.contains("[已移除参考图片]");
    rewrite_mentions(prompt, |n| {
        invalid |= n == 0 || n > count;
        String::new()
    });
    invalid
}

fn mention_start(prompt: &str, cursor: usize) -> Option<usize> {
    let before = prompt.get(..cursor)?;
    let start = before.rfind('@')?;
    let query = &before[start + 1..];
    (!query.chars().any(|c| c.is_whitespace() || c == ']')).then_some(start)
}

fn update_mention(app: &App) {
    let ui = app.global::<SeeCut>();
    ui.set_mention_open(
        mention_start(&ui.get_prompt(), ui.get_prompt_cursor().max(0) as usize).is_some(),
    );
}

fn remove_reference(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let index = state
        .borrow()
        .references
        .iter()
        .position(|v| text(v, "client_id") == id);
    let Some(index) = index else { return };
    let kind = reference_label(&text(&state.borrow().references[index], "kind"));
    let number = reference_number(&state.borrow().references, index);
    state.borrow_mut().references.remove(index);
    state
        .borrow_mut()
        .pending
        .retain(|(name, _)| name != &format!("reference:{id}"));
    let ui = app.global::<SeeCut>();
    let prompt = rewrite_media_mentions(&ui.get_prompt(), |label, n| {
        if label != kind {
            return format!("@[{label}{n}]");
        }
        if n == number {
            format!("[已移除参考{kind}]")
        } else {
            format!("@[{kind}{}]", if n > number { n - 1 } else { n })
        }
    });
    ui.set_prompt(prompt.into());
    ui.set_mention_open(false);
    render_references(app, state);
    refresh_quote(app, state);
}

fn insert_mention(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let index = state
        .borrow()
        .references
        .iter()
        .position(|v| text(v, "client_id") == id && text(v, "status") == "ready");
    let Some(index) = index else { return };
    let ui = app.global::<SeeCut>();
    let mut prompt = ui.get_prompt().to_string();
    let cursor = ui.get_prompt_cursor().max(0) as usize;
    let Some(start) = mention_start(&prompt, cursor) else {
        return;
    };
    let kind = reference_label(&text(&state.borrow().references[index], "kind"));
    let token = format!(
        "@[{kind}{}] ",
        reference_number(&state.borrow().references, index)
    );
    prompt.replace_range(start..cursor, &token);
    ui.set_prompt(prompt.into());
    ui.set_prompt_cursor((start + token.len()) as i32);
    ui.set_mention_open(false);
    refresh_quote(app, state);
}

fn open_reference_mention(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    open_reference_mention_ui(&ui);
    state.borrow_mut().quote_id.clear();
    state.borrow_mut().quote_credits = None;
}

pub(crate) fn open_reference_mention_ui(ui: &SeeCut) {
    let mut prompt = ui.get_prompt().to_string();
    let mut cursor = (ui.get_prompt_cursor().max(0) as usize).min(prompt.len());
    while !prompt.is_char_boundary(cursor) {
        cursor -= 1;
    }
    prompt.insert(cursor, '@');
    ui.set_prompt(prompt.into());
    ui.set_prompt_cursor((cursor + 1) as i32);
    ui.set_mention_open(true);
    ui.set_can_generate(false);
    ui.set_quote("".into());
}

fn generation_body(ui: &SeeCut, state: &Cloud) -> Value {
    let video = ui.get_mode() != 0;
    let prompt = if video {
        rewrite_media_mentions(ui.get_prompt().trim(), |kind, number| {
            format!("{kind}{number}")
        })
    } else {
        rewrite_mentions(ui.get_prompt().trim(), |number| {
            format!("第{number}张参考图片")
        })
    };
    let mut body =
        json!({"model": selected_model(ui, state), "operation":"generate", "prompt":prompt});
    let model_id = selected_model(ui, state);
    let model = state
        .models
        .iter()
        .find(|model| {
            text(model, "id") == model_id && text(model, "kind") == model_kind(ui.get_mode())
        })
        .cloned()
        .unwrap_or_default();
    if video {
        body["resolution"] = parameter_value(&model, "resolution", ui.get_resolution_index());
        body["duration"] = parameter_value(&model, "duration", ui.get_duration_index());
        body["aspect_ratio"] = parameter_value(&model, "aspect_ratio", ui.get_ratio_index());
        if model["parameters"].get("generate_audio").is_some() {
            body["generate_audio"] = json!(ui.get_generate_audio());
        }
    } else {
        body["size"] = parameter_value(&model, "size", ui.get_resolution_index());
        body["quality"] = parameter_value(&model, "quality", ui.get_quality_index());
    }
    if model["parameters"].get("quantity").is_some() {
        body["quantity"] = parameter_value(&model, "quantity", ui.get_quantity_index());
    }
    let references: Vec<_> = state
        .references
        .iter()
        .filter(|reference| text(reference, "status") == "ready")
        .filter_map(|reference| {
            let id = reference.get("id").filter(|value| !value.is_null())?;
            id.as_str()
                .filter(|value| !value.is_empty())
                .map(|value| json!(value))
        })
        .collect();
    if !references.is_empty() {
        body["reference_asset_ids"] = json!(references);
        if !video {
            body["operation"] = json!("edit");
        }
    }
    body
}

fn parameter_value(model: &Value, name: &str, index: i32) -> Value {
    usize::try_from(index)
        .ok()
        .and_then(|index| model["parameters"][name]["values"].get(index))
        .cloned()
        .unwrap_or(Value::Null)
}

fn parameter_label(name: &str, value: &Value) -> String {
    let raw = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    match (name, raw.as_str()) {
        ("size", "auto") => "自动".into(),
        ("size", "1024x1024") => "1:1 · 1024 × 1024".into(),
        ("size", "1536x1024") => "3:2 · 1536 × 1024".into(),
        ("size", "1024x1536") => "2:3 · 1024 × 1536".into(),
        ("quality", "high") => "高清".into(),
        ("duration", _) => format!("{raw} 秒"),
        _ => raw,
    }
}

fn parameter_default_index(model: &Value, name: &str) -> i32 {
    let parameter = &model["parameters"][name];
    parameter["values"]
        .as_array()
        .and_then(|values| {
            values
                .iter()
                .position(|value| value == &parameter["default"])
        })
        .unwrap_or(0) as i32
}
fn refresh_models(app: &App, state: &Rc<RefCell<Cloud>>) {
    if !app.global::<SeeCut>().get_signed_in() {
        return;
    }
    job(
        app,
        state,
        "models".into(),
        request("GET", "/api/generation/models", Value::Null),
    );
}
fn team_id(ui: &SeeCut, state: &Cloud) -> String {
    state
        .teams
        .get(ui.get_team_index().max(0) as usize)
        .map(|v| text(v, "id"))
        .unwrap_or_default()
}

fn picker_context(ui: &SeeCut, state: &Cloud) -> String {
    let team = if ui.get_asset_picker_source() == 1 {
        team_id(ui, state)
    } else {
        String::new()
    };
    format!(
        "{}:{}:{}",
        ui.get_asset_picker_source(),
        ui.get_asset_picker_purpose(),
        team
    )
}

fn clear_picker_selection(ui: &SeeCut, state: &Rc<RefCell<Cloud>>) {
    state.borrow_mut().picker_selected_ids.clear();
    ui.set_asset_picker_selected_count(0);
}

/// Scope temporary selection to source/purpose/team. Keep hidden or stale ids
/// until confirmation so filtering or refresh cannot silently shrink a batch.
fn sync_picker_selection(ui: &SeeCut, state: &Rc<RefCell<Cloud>>) {
    if !ui.get_asset_picker_open() {
        ui.set_asset_picker_selected_count(0);
        return;
    }
    let (context, purpose) = {
        let cloud = state.borrow();
        (
            picker_context(ui, &cloud),
            ui.get_asset_picker_purpose().to_string(),
        )
    };
    let mut cloud = state.borrow_mut();
    if cloud.picker_context != context {
        cloud.picker_context = context;
        cloud.picker_selected_ids.clear();
    }
    // Preserve stale selections so confirmation rejects the entire batch.
    let count = if purpose == "canvas" {
        0
    } else {
        cloud.picker_selected_ids.len() as i32
    };
    drop(cloud);
    ui.set_asset_picker_selected_count(count);
}

fn begin_asset_picker(app: &App, state: &Rc<RefCell<Cloud>>, purpose: &str, source: i32) {
    let ui = app.global::<SeeCut>();
    ui.set_asset_picker_open(true);
    ui.set_asset_picker_purpose(purpose.into());
    ui.set_asset_picker_source(source);
    ui.set_personal_reference_search("".into());
    let image_only = purpose == "canvas" || (purpose == "reference" && ui.get_mode() == 0);
    ui.set_asset_picker_personal_filter(if image_only { 1 } else { 0 });
    ui.set_asset_picker_team_search("".into());
    ui.set_asset_picker_team_filter(if image_only { 1 } else { 0 });
    ui.set_trash_open(false);
    clear_picker_selection(&ui, state);
    let context = picker_context(&ui, &state.borrow());
    state.borrow_mut().picker_context = context;
    ui.set_error("".into());
    render_personal(app, state);
    render_assets(app, state);
    if source == 0 {
        personal_job(app, state, "list", Value::Null);
    } else if ui.get_signed_in() {
        job(
            app,
            state,
            "teams".into(),
            request("GET", "/api/teams", Value::Null),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavigationDecision {
    Open(i32),
    Authenticate { stay: i32, target: i32 },
}

fn navigation_decision(current: i32, target: i32, signed_in: bool) -> NavigationDecision {
    let target = target.clamp(0, 7);
    if !signed_in && matches!(target, 2..=4) {
        NavigationDecision::Authenticate {
            stay: current,
            target,
        }
    } else {
        NavigationDecision::Open(target)
    }
}

fn auth_return_target(pending: Option<i32>, current: i32) -> i32 {
    pending.unwrap_or(current).clamp(0, 7)
}

fn show_auth(app: &App, state: &Rc<RefCell<Cloud>>, target: Option<i32>) {
    let ui = app.global::<SeeCut>();
    if let Some(target) = target {
        state.borrow_mut().auth_return_page = Some(target.clamp(0, 7));
    }
    if !ui.get_signed_in() {
        switch_auth(&ui, 0);
    }
    ui.set_auth_open(true);
}

fn load_page(app: &App, state: &Rc<RefCell<Cloud>>, page: i32) {
    let ui = app.global::<SeeCut>();
    ui.set_page(page);
    match page {
        5 => personal_job(app, state, "list", Value::Null),
        7 => {
            render_templates(app, state);
            if ui.get_signed_in() {
                refresh_models(app, state);
            }
        }
        1 if ui.get_signed_in() => {
            refresh_models(app, state);
            job(
                app,
                state,
                "tasks".into(),
                request("GET", "/api/generation/tasks", Value::Null),
            );
        }
        2 => job(
            app,
            state,
            "teams".into(),
            request("GET", "/api/teams", Value::Null),
        ),
        3 => {
            job(
                app,
                state,
                "plans".into(),
                request("GET", "/api/credit-plans", Value::Null),
            );
            job(
                app,
                state,
                "wallet".into(),
                request("GET", "/api/wallet", Value::Null),
            );
            job(
                app,
                state,
                "orders".into(),
                request("GET", "/api/orders", Value::Null),
            );
        }
        _ => {}
    }
}

fn template_root() -> Result<PathBuf, String> {
    concat_host::AppDirs::locate()
        .map(|dirs| dirs.data.join("generation-templates"))
        .map_err(|error| format!("无法打开生成模板：{error}"))
}

fn render_templates(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let library = match template_root().and_then(crate::generation_templates::Library::load) {
        Ok(library) => library,
        Err(error) => {
            ui.set_error(error.into());
            return;
        }
    };
    let cloud = state.borrow();
    let personal_root = library_root().ok();
    let mut templates = library.items().iter().collect::<Vec<_>>();
    templates.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    ui.set_templates(rows(
        templates
            .into_iter()
            .map(|item| {
                let model_name = cloud
                    .models
                    .iter()
                    .find(|model| text(model, "id") == item.model_id)
                    .map(|model| {
                        let label = text(model, "display_name");
                        if label.is_empty() {
                            item.model_id.clone()
                        } else {
                            label
                        }
                    })
                    .unwrap_or_else(|| item.model_id.clone());
                let reference_rows = item
                    .references
                    .iter()
                    .map(|reference| {
                        use crate::personal_library::{
                            Asset, AssetKind, AssetSource, cached_thumbnail,
                        };
                        let kind = match reference.kind.as_str() {
                            "video" => AssetKind::Video,
                            "audio" => AssetKind::Audio,
                            _ => AssetKind::Image,
                        };
                        let asset = Asset {
                            id: String::new(),
                            name: reference.name.clone(),
                            path: reference.path.clone(),
                            kind,
                            source: AssetSource::Generated,
                            original_path: None,
                            created_at: 0,
                            trashed: false,
                            favorite: false,
                            folder_id: None,
                        };
                        CloudItem {
                            id: reference.path.to_string_lossy().into_owned().into(),
                            name: reference.name.clone().into(),
                            kind: reference.kind.clone().into(),
                            ready: reference.path.is_file(),
                            local: true,
                            preview: personal_root
                                .as_deref()
                                .and_then(|root| cached_thumbnail(&asset, root))
                                .and_then(|path| slint::Image::load_from_path(&path).ok())
                                .unwrap_or_default(),
                            ..Default::default()
                        }
                    })
                    .collect::<Vec<_>>();
                TemplateRow {
                    id: item.id.clone().into(),
                    name: item.name.clone().into(),
                    mode_label: if item.mode == 0 { "图片" } else { "视频" }.into(),
                    model_name: model_name.into(),
                    parameter_summary: template_parameter_summary(&item.parameters).into(),
                    reference_summary: format!("{} 项参考素材", item.references.len()).into(),
                    updated_label: local_date_label(Some(item.updated_at)).into(),
                    prompt_preview: item.prompt.clone().into(),
                    references: rows(reference_rows),
                }
            })
            .collect(),
    ));
}

fn template_parameter_summary(parameters: &Value) -> String {
    [
        "size",
        "resolution",
        "aspect_ratio",
        "duration",
        "quality",
        "generate_audio",
        "quantity",
    ]
    .into_iter()
    .filter_map(|key| {
        let value = parameters.get(key)?;
        if value.is_null() {
            return None;
        }
        let label = match key {
            "size" | "resolution" => "尺寸",
            "aspect_ratio" => "比例",
            "duration" => "时长",
            "quality" => "质量",
            "generate_audio" => "音频",
            "quantity" => "数量",
            _ => key,
        };
        let value = if key == "generate_audio" {
            if value.as_bool().unwrap_or(false) {
                "开启".into()
            } else {
                "关闭".into()
            }
        } else {
            parameter_label(key, value)
        };
        Some(format!("{label} {value}"))
    })
    .collect::<Vec<_>>()
    .join(" · ")
}

fn current_template_parameters(ui: &SeeCut, state: &Cloud) -> Value {
    let model_id = selected_model(ui, state);
    let model = state
        .models
        .iter()
        .find(|model| {
            text(model, "id") == model_id && text(model, "kind") == model_kind(ui.get_mode())
        })
        .cloned()
        .unwrap_or_default();
    let mut parameters = if ui.get_mode() == 0 {
        json!({
            "size": parameter_value(&model, "size", ui.get_resolution_index()),
            "quality": parameter_value(&model, "quality", ui.get_quality_index()),
        })
    } else {
        json!({
            "resolution": parameter_value(&model, "resolution", ui.get_resolution_index()),
            "duration": parameter_value(&model, "duration", ui.get_duration_index()),
            "aspect_ratio": parameter_value(&model, "aspect_ratio", ui.get_ratio_index()),
        })
    };
    if model["parameters"].get("generate_audio").is_some() {
        parameters["generate_audio"] = json!(ui.get_generate_audio());
    }
    if model["parameters"].get("quantity").is_some() {
        parameters["quantity"] = parameter_value(&model, "quantity", ui.get_quantity_index());
    }
    parameters
}

fn template_references(state: &Cloud) -> Vec<crate::generation_templates::TemplateReference> {
    state
        .references
        .iter()
        .map(|reference| {
            let path = PathBuf::from(text(reference, "local_path"));
            crate::generation_templates::TemplateReference {
                path,
                name: text(reference, "display_name"),
                kind: text(reference, "kind"),
                client_id: text(reference, "client_id"),
                source_id: text(reference, "id"),
            }
        })
        .collect()
}

fn current_task_snapshot(ui: &SeeCut, state: &Cloud) -> TaskInputSnapshot {
    TaskInputSnapshot {
        mode: ui.get_mode(),
        prompt: ui.get_prompt().to_string(),
        model_id: selected_model(ui, state),
        parameters: current_template_parameters(ui, state),
        references: template_references(state),
    }
}

fn save_task_snapshots(state: &Cloud) -> Result<(), String> {
    if state.task_snapshot_name.is_empty() {
        return Err("尚未登录，无法保存历史任务配置".into());
    }
    std::fs::create_dir_all(&state.folder)
        .map_err(|error| format!("无法创建任务配置目录：{error}"))?;
    let destination = state.folder.join(&state.task_snapshot_name);
    let temporary = state.folder.join(format!(
        ".{}.{}.tmp",
        state.task_snapshot_name,
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(&state.task_snapshots)
        .map_err(|error| format!("无法生成历史任务配置：{error}"))?;
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| format!("无法写入历史任务配置：{error}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("无法写入历史任务配置：{error}"))?;
        std::fs::rename(&temporary, destination)
            .map_err(|error| format!("无法更新历史任务配置：{error}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn historical_references(
    task: &Value,
    request: &serde_json::Map<String, Value>,
) -> Vec<crate::generation_templates::TemplateReference> {
    let raw = request
        .get("references")
        .or_else(|| request.get("reference_assets"))
        .or_else(|| request.get("reference_asset_ids"))
        .or_else(|| task.get("references"))
        .or_else(|| task.get("reference_assets"))
        .or_else(|| task.get("reference_asset_ids"))
        .and_then(Value::as_array);
    let Some(raw) = raw else { return Vec::new() };
    raw.iter()
        .enumerate()
        .map(|(index, value)| {
            let (source_id, path, name, kind) = if let Some(source_id) = value.as_str() {
                (
                    source_id.to_owned(),
                    PathBuf::new(),
                    format!("缺失参考素材 {}（类型未知）", index + 1),
                    "unknown".to_owned(),
                )
            } else {
                let source_id = value
                    .get("id")
                    .or_else(|| value.get("asset_id"))
                    .or_else(|| value.get("reference_asset_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let path = value
                    .get("local_path")
                    .or_else(|| value.get("path"))
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_default();
                let kind = value
                    .get("kind")
                    .or_else(|| value.get("media_type"))
                    .and_then(Value::as_str)
                    .map(|kind| {
                        if kind.starts_with("image/") {
                            "image"
                        } else if kind.starts_with("video/") {
                            "video"
                        } else if kind.starts_with("audio/") {
                            "audio"
                        } else {
                            kind
                        }
                    })
                    .unwrap_or("unknown")
                    .to_owned();
                let name = value
                    .get("name")
                    .or_else(|| value.get("filename"))
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("缺失参考素材 {}", index + 1));
                (source_id, path, name, kind)
            };
            let client_id = if source_id.is_empty() {
                format!("historical-reference-{index}")
            } else {
                format!("historical-reference-{source_id}")
            };
            crate::generation_templates::TemplateReference {
                path,
                name,
                kind,
                client_id,
                source_id,
            }
        })
        .collect()
}

fn server_task_snapshot(task: &Value) -> Result<TaskInputSnapshot, String> {
    let request = task
        .get("request")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let model_id = request
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| {
            task.get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
        })
        .to_owned();
    let prompt = request
        .get("prompt")
        .or_else(|| task.get("prompt"))
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| "该历史任务没有可恢复的提示词".to_owned())?
        .to_owned();
    let mode = if text(task, "kind") == "video" { 1 } else { 0 };
    let mut parameters = serde_json::Map::new();
    for key in [
        "size",
        "quality",
        "resolution",
        "duration",
        "aspect_ratio",
        "generate_audio",
        "quantity",
    ] {
        if let Some(value) = request.get(key) {
            parameters.insert(key.into(), value.clone());
        }
    }
    Ok(TaskInputSnapshot {
        mode,
        prompt,
        model_id,
        parameters: Value::Object(parameters),
        references: historical_references(task, &request),
    })
}

fn configuration_parameter_index(
    model: &Value,
    name: &str,
    expected: &Value,
    source: &str,
) -> Result<i32, String> {
    if expected.is_null() {
        return Ok(parameter_default_index(model, name));
    }
    model["parameters"][name]["values"]
        .as_array()
        .and_then(|values| values.iter().position(|value| value == expected))
        .map(|index| index as i32)
        .ok_or_else(|| {
            format!(
                "{source}参数 {}={} 不再受当前模型支持，请调整后重试",
                name,
                parameter_label(name, expected)
            )
        })
}

fn template_action(app: &App, state: &Rc<RefCell<Cloud>>, action: &str, id: &str) {
    let ui = app.global::<SeeCut>();
    ui.set_error("".into());
    let root = match template_root() {
        Ok(root) => root,
        Err(error) => {
            ui.set_error(error.into());
            return;
        }
    };
    let mut library = match crate::generation_templates::Library::load(root) {
        Ok(library) => library,
        Err(error) => {
            ui.set_error(error.into());
            return;
        }
    };
    let result = match action {
        "template-new" => {
            ui.set_template_id("".into());
            ui.set_template_name("".into());
            ui.set_template_prompt(ui.get_prompt());
            ui.set_template_mode(ui.get_mode());
            ui.set_template_model_id(selected_model(&ui, &state.borrow()).into());
            Ok(())
        }
        "template-edit" => match library.get(id).cloned() {
            Some(item) => {
                ui.set_template_id(item.id.into());
                ui.set_template_name(item.name.into());
                ui.set_template_prompt(item.prompt.into());
                ui.set_template_mode(item.mode);
                ui.set_template_model_id(item.model_id.into());
                ui.set_template_parameters_json(item.parameters.to_string().into());
                ui.set_template_reference_paths(
                    serde_json::to_string(
                        &item.references.iter().map(|r| &r.path).collect::<Vec<_>>(),
                    )
                    .unwrap_or_default()
                    .into(),
                );
                Ok(())
            }
            None => Err("未找到该生成模板，请刷新后重试".into()),
        },
        "template-save" => library.update_text(
            id,
            ui.get_template_name().as_str(),
            ui.get_template_prompt().to_string(),
        ),
        "template-save-current" => {
            let cloud = state.borrow();
            let model_id = selected_model(&ui, &cloud);
            let parameters = current_template_parameters(&ui, &cloud);
            let references = template_references(&cloud);
            drop(cloud);
            library
                .save_current(
                    (!id.is_empty()).then_some(id),
                    ui.get_template_name().as_str(),
                    ui.get_template_prompt().to_string(),
                    ui.get_mode(),
                    model_id,
                    parameters,
                    references,
                )
                .map(|saved_id| {
                    ui.set_template_id(saved_id.into());
                    ui.set_template_editor_open(false);
                    ui.set_notice("模板已保存".into());
                })
        }
        "template-delete" => library.delete(id).map(|_| {
            if ui.get_template_id().as_str() == id {
                ui.set_template_id("".into());
            }
            ui.set_notice("模板已删除".into());
        }),
        "template-apply" if state.borrow().models.is_empty() => {
            state.borrow_mut().pending_template_id = Some(id.to_owned());
            if ui.get_signed_in() {
                ui.set_notice("正在加载模型目录，加载完成后将继续应用模板".into());
                refresh_models(app, state);
            } else {
                ui.set_notice("登录并加载模型目录后将继续应用模板".into());
                show_auth(app, state, Some(7));
            }
            Ok(())
        }
        "template-apply" => apply_template(app, state, library.get(id).cloned()),
        _ => Ok(()),
    };
    if let Err(error) = result {
        ui.set_error(error.into());
    } else if matches!(
        action,
        "template-save" | "template-save-current" | "template-delete"
    ) {
        render_templates(app, state);
    }
}

fn apply_template(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    template: Option<crate::generation_templates::GenerationTemplate>,
) -> Result<(), String> {
    let Some(template) = template else {
        return Err("未找到该生成模板，请刷新后重试".into());
    };
    apply_generation_configuration(app, state, &template, "模板", true)?;
    let ui = app.global::<SeeCut>();
    if ui.get_recovery_warning().is_empty() {
        ui.set_notice("模板已应用，可继续调整后生成".into());
    }
    Ok(())
}

fn apply_task_snapshot(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    snapshot: TaskInputSnapshot,
) -> Result<(), String> {
    let configuration = crate::generation_templates::GenerationTemplate {
        id: String::new(),
        name: String::new(),
        prompt: snapshot.prompt,
        mode: snapshot.mode,
        model_id: snapshot.model_id,
        parameters: snapshot.parameters,
        references: snapshot.references,
        created_at: 0,
        updated_at: 0,
    };
    apply_generation_configuration(app, state, &configuration, "历史任务", false)?;
    let ui = app.global::<SeeCut>();
    ui.set_generation_step(0);
    ui.set_result_preview_open(false);
    if ui.get_recovery_warning().is_empty() {
        ui.set_notice("历史任务输入已回填，可调整后重新报价".into());
    }
    Ok(())
}

fn refill_task(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) -> Result<(), String> {
    let snapshot = {
        let cloud = state.borrow();
        let task = cloud
            .tasks
            .iter()
            .find(|task| text(task, "id") == id)
            .ok_or_else(|| "未找到该历史任务，请刷新后重试".to_owned())?;
        cloud
            .task_snapshots
            .get(id)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| server_task_snapshot(task))?
    };
    apply_task_snapshot(app, state, snapshot)
}

fn apply_generation_configuration(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    configuration: &crate::generation_templates::GenerationTemplate,
    source: &str,
    update_template_metadata: bool,
) -> Result<(), String> {
    let ui = app.global::<SeeCut>();
    let mode = if configuration.mode == 0 { 0 } else { 1 };
    let kind = model_kind(mode);
    let model_id = configuration.model_id.clone();
    let model = state
        .borrow()
        .models
        .iter()
        .find(|model| text(model, "id") == model_id && text(model, "kind") == kind)
        .cloned();
    let mut warnings = Vec::new();
    if model.is_none() {
        warnings.push(if model_id.is_empty() {
            format!("{source}缺少模型，请选择一个可用模型")
        } else {
            format!("{source}使用的模型 {model_id} 当前不可用，请选择其他模型")
        });
    }
    for reference in &configuration.references {
        if reference.path.as_os_str().is_empty() || !reference.path.is_file() {
            warnings.push(format!(
                "{}缺少参考素材“{}”，请替换后再生成",
                source,
                if reference.name.is_empty() {
                    "未命名素材"
                } else {
                    reference.name.as_str()
                }
            ));
        }
    }
    let required_parameters: &[&str] = if mode == 0 {
        &["size", "quality"]
    } else {
        &["resolution", "duration", "aspect_ratio", "generate_audio"]
    };
    for name in required_parameters {
        let parameter_expected = *name != "generate_audio"
            || model
                .as_ref()
                .map(|model| model["parameters"].get("generate_audio").is_some())
                .unwrap_or(true);
        if parameter_expected && configuration.parameters.get(*name).is_none() {
            warnings.push(format!("{source}缺少参数 {name}，已暂用当前模型默认值"));
        }
    }
    if let Some(model) = model.as_ref() {
        // Keep recoverable inputs even when their reference configuration is
        // no longer compatible. Surface the validation result as a warning.
        if let Err(error) =
            validate_configuration_references(model, &configuration.references, source)
        {
            warnings.push(error);
        }
        for name in [
            "size",
            "quality",
            "resolution",
            "duration",
            "aspect_ratio",
            "quantity",
        ] {
            if let Some(value) = configuration.parameters.get(name)
                && !value.is_null()
                && configuration_parameter_index(model, name, value, source).is_err()
            {
                warnings.push(format!(
                    "{source}参数 {}={} 已不受当前模型支持，已改用可用值",
                    name,
                    parameter_label(name, value)
                ));
            }
        }
        if let Some(value) = configuration.parameters.get("generate_audio") {
            if model["parameters"].get("generate_audio").is_none() {
                warnings.push(format!(
                    "当前模型不再支持{source}中的生成音频参数，已按模型默认处理"
                ));
            } else if value.as_bool().is_none() {
                warnings.push(format!("{source}的音频参数无效，已按模型默认处理"));
            }
        }
    }
    ui.set_prompt(configuration.prompt.clone().into());
    ui.set_prompt_cursor(configuration.prompt.len() as i32);
    update_model_options_for_configuration(app, state, mode, &model_id, &configuration.parameters);
    if update_template_metadata {
        ui.set_template_id(configuration.id.clone().into());
        ui.set_template_name(configuration.name.clone().into());
        ui.set_template_prompt(configuration.prompt.clone().into());
        ui.set_template_mode(mode);
        ui.set_template_model_id(model_id.clone().into());
        ui.set_template_parameters_json(configuration.parameters.to_string().into());
    } else {
        // A task refill is independent of whichever template was last edited.
        // Keeping the old id would make a later save overwrite an unrelated
        // template after the history task has been recovered.
        ui.set_template_id("".into());
        ui.set_template_name("".into());
        ui.set_template_prompt("".into());
        ui.set_template_model_id("".into());
        ui.set_template_parameters_json("".into());
        ui.set_template_reference_paths("".into());
    }
    let references = configuration.references.clone();
    let mut cloud = state.borrow_mut();
    cloud.references.clear();
    cloud.quote_id.clear();
    cloud.quote_credits = None;
    cloud.quote_body = Value::Null;
    cloud.submission = None;
    drop(cloud);
    ui.set_quote("".into());
    ui.set_can_generate(false);
    ui.set_insufficient_credits(false);
    ui.set_page(1);
    render_references(app, state);
    ui.set_reference_error("".into());
    let had_references = !references.is_empty();
    for reference in references {
        queue_configuration_reference(app, state, reference);
    }
    render_references(app, state);
    if !had_references {
        refresh_quote(app, state);
    }
    if !warnings.is_empty() {
        set_recovery_warning(&ui, warnings.join("；"));
    } else {
        clear_recovery_warning(&ui);
    }
    Ok(())
}

fn navigate(app: &App, state: &Rc<RefCell<Cloud>>, target: i32) {
    let ui = app.global::<SeeCut>();
    if target == 6 {
        ui.set_canvas_gallery_open(true);
        refresh_canvas_projects(app, state);
    }
    if should_clear_identity_error_for_target(target, ui.get_error().as_str()) {
        ui.set_error("".into());
    }
    match navigation_decision(ui.get_page(), target, ui.get_signed_in()) {
        NavigationDecision::Open(page) => load_page(app, state, page),
        NavigationDecision::Authenticate { target, .. } => show_auth(app, state, Some(target)),
    }
}

fn refresh_canvas_projects(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let paths = match crate::panes::canvas::canvas_recent_paths() {
        Ok(paths) => paths,
        Err(error) => {
            ui.set_error(error.into());
            return;
        }
    };
    let projects = paths
        .into_iter()
        .filter_map(|path| {
            let manifest_path = path.join("manifest.json");
            let bytes = std::fs::read(&manifest_path).ok()?;
            let manifest: Value = serde_json::from_slice(&bytes).ok()?;
            let name = manifest["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "画布项目".to_owned());
            let width = manifest["width"].as_u64().unwrap_or(0);
            let height = manifest["height"].as_u64().unwrap_or(0);
            let updated = manifest_path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            Some(CanvasProjectRow {
                item: CloudItem {
                    id: path.to_string_lossy().into_owned().into(),
                    name: name.into(),
                    detail: format!("{width} × {height}").into(),
                    date_label: project_timestamp(updated).into(),
                    ready: true,
                    preview: slint::Image::load_from_path(&path.join("preview.png"))
                        .unwrap_or_default(),
                    ..Default::default()
                },
                updated,
            })
        })
        .collect();
    CANVAS_PROJECT_CACHE.with(|cache| *cache.borrow_mut() = projects);
    render_canvas_projects(app, state);
}

fn render_canvas_projects(app: &App, _state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let query = ui.get_canvas_project_search().trim().to_lowercase();
    let sort = ui.get_canvas_project_sort();
    let mut projects = CANVAS_PROJECT_CACHE.with(|cache| cache.borrow().clone());
    projects.retain(|project| {
        query.is_empty()
            || project
                .item
                .name
                .to_string()
                .to_lowercase()
                .contains(&query)
    });
    if sort == 1 {
        projects.sort_by_cached_key(|project| project.item.name.to_string().to_lowercase());
    } else {
        projects.sort_by_key(|project| std::cmp::Reverse(project.updated));
    }
    ui.set_canvas_projects(rows(
        projects.into_iter().map(|project| project.item).collect(),
    ));
}

fn begin_handoff(app: &App, state: &Rc<RefCell<Cloud>>, kind: &str, paths: Vec<PathBuf>) {
    let imports = paths
        .into_iter()
        .map(crate::panes::canvas::CanvasImport::from_path)
        .collect();
    begin_handoff_with_imports(app, state, kind, imports);
}

fn begin_handoff_with_imports(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    kind: &str,
    imports: Vec<crate::panes::canvas::CanvasImport>,
) {
    let ui = app.global::<SeeCut>();
    if imports.is_empty() || imports.iter().any(|item| !item.path.is_file()) {
        ui.set_error("所选素材文件不可用，请刷新后重试".into());
        return;
    }
    if kind == "canvas"
        && imports.iter().any(|item| {
            !matches!(
                item.path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("png" | "jpg" | "jpeg" | "webp" | "bmp")
            )
        })
    {
        ui.set_error("画布只支持图片素材".into());
        return;
    }
    let mut targets = vec![CloudItem {
        id: "new".into(),
        name: if kind == "canvas" {
            "新建画布"
        } else {
            "新建剪辑项目"
        }
        .into(),
        detail: "创建新工程并加入素材".into(),
        ready: true,
        ..Default::default()
    }];
    if kind == "canvas" {
        if app.global::<crate::ui::Editor>().get_canvas_has_document() {
            targets.push(CloudItem {
                id: "current".into(),
                name: "当前画布".into(),
                detail: app.global::<crate::ui::Editor>().get_canvas_name(),
                ready: true,
                ..Default::default()
            });
        }
        if let Ok(paths) = crate::panes::canvas::canvas_recent_paths() {
            for path in paths.into_iter().take(8) {
                let Ok(bytes) = std::fs::read(path.join("manifest.json")) else {
                    continue;
                };
                let Ok(manifest) = serde_json::from_slice::<Value>(&bytes) else {
                    continue;
                };
                let name = manifest["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .or_else(|| {
                        path.file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| "画布项目".to_owned());
                targets.push(CloudItem {
                    id: format!("recent:{}", path.display()).into(),
                    name: name.into(),
                    detail: format!(
                        "{} × {}",
                        manifest["width"].as_u64().unwrap_or(0),
                        manifest["height"].as_u64().unwrap_or(0)
                    )
                    .into(),
                    preview: slint::Image::load_from_path(&path.join("preview.png"))
                        .unwrap_or_default(),
                    ready: true,
                    ..Default::default()
                });
            }
        }
    } else {
        if ui.get_project_open() {
            targets.push(CloudItem {
                id: "current".into(),
                name: "当前剪辑项目".into(),
                detail: app.get_project_name(),
                ready: true,
                ..Default::default()
            });
        }
        for project in app.get_recents().iter().take(8) {
            targets.push(CloudItem {
                id: format!("recent:{}", project.path).into(),
                name: project.name,
                detail: project.detail,
                preview: project.poster,
                ready: true,
                ..Default::default()
            });
        }
    }
    ui.set_handoff_source_page(ui.get_page());
    ui.set_handoff_kind(kind.into());
    ui.set_handoff_count(imports.len() as i32);
    ui.set_handoff_targets(rows(targets));
    ui.set_handoff_selected_id("".into());
    state.borrow_mut().pending_handoff = Some(PendingHandoff {
        kind: kind.to_owned(),
        imports,
        awaiting_new_project: false,
    });
    ui.set_handoff_open(true);
}

fn clip_imports(
    imports: &[crate::panes::canvas::CanvasImport],
) -> Vec<crate::panes::media_bin::MediaImport> {
    imports
        .iter()
        .map(|item| crate::panes::media_bin::MediaImport {
            path: item.path.clone(),
            display_name: item.display_name.clone(),
        })
        .collect()
}

fn select_handoff(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let ui = app.global::<SeeCut>();
    let Some(pending) = state.borrow().pending_handoff.clone() else {
        ui.set_handoff_open(false);
        return;
    };
    if !ui
        .get_handoff_targets()
        .iter()
        .any(|target| target.id.as_str() == id)
    {
        ui.set_error("目标工程不存在，请重新选择".into());
        return;
    }
    if pending.kind == "canvas" {
        let Ok(payload) = serde_json::to_string(&pending.imports) else {
            ui.set_error("无法准备画布素材".into());
            return;
        };
        ui.set_handoff_open(false);
        ui.set_canvas_gallery_open(false);
        ui.set_page(6);
        if id == "new" {
            app.invoke_canvas_new_with_paths(payload.into());
        } else if id == "current" {
            app.invoke_canvas_handoff_current(payload.into());
        } else if let Some(path) = id.strip_prefix("recent:") {
            app.invoke_canvas_open_with_paths(path.into(), payload.into());
        }
        return;
    } else {
        if id != "current" && !app.get_on_start() {
            app.invoke_close_clip_project();
            if !app.get_on_start() {
                ui.set_error("当前剪辑项目未能保存，请重试".into());
                return;
            }
        }
        if id == "new" {
            if let Some(handoff) = state.borrow_mut().pending_handoff.as_mut() {
                handoff.awaiting_new_project = true;
            }
            ui.set_pending_import_count(
                (state.borrow().pending_imports.len() + pending.imports.len()) as i32,
            );
            ui.set_page(0);
            app.set_clip_create_open(true);
            ui.set_handoff_open(false);
            return;
        } else if id == "current" {
            import_named_paths(app, clip_imports(&pending.imports));
        } else if let Some(path) = id.strip_prefix("recent:") {
            app.invoke_start_open_recent(path.into());
            if app.get_on_start() {
                ui.set_error("剪辑项目未能打开，请重新选择".into());
                ui.set_page(ui.get_handoff_source_page());
                return;
            }
            import_named_paths(app, clip_imports(&pending.imports));
        }
    }
    state.borrow_mut().pending_handoff = None;
    ui.set_handoff_open(false);
}

fn publish(app: &App, state: &Rc<RefCell<Cloud>>, name: &str, value: Value) {
    let ui = app.global::<SeeCut>();
    match name {
        "export-copy" => {
            let mut cloud = state.borrow_mut();
            let Some(batch) = cloud.export_copy.as_mut() else {
                return;
            };
            for id in value["completed"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                batch.completed.insert(id.to_owned());
            }
            batch.running = false;
            let done = batch.completed.len();
            let total = batch.items.len();
            let bytes_done: u64 = batch
                .items
                .iter()
                .filter(|item| batch.completed.contains(&item.id))
                .filter_map(|item| std::fs::metadata(&item.source).ok())
                .map(|metadata| metadata.len())
                .sum();
            batch
                .bytes_done
                .store(bytes_done, std::sync::atomic::Ordering::Relaxed);
            let message = if value["cancelled"] == true {
                format!("已取消，完成 {done}/{total} 项")
            } else if let Some(error) = value["error"].as_str() {
                format!(
                    "已导出 {done}/{total} 项；{} 失败：{error}",
                    text(&value, "failed")
                )
            } else {
                format!("已导出 {done} 项")
            };
            ui.set_export_copy_done(done as i32);
            ui.set_export_copy_progress(if done == total {
                100
            } else if batch.bytes_total == 0 {
                0
            } else {
                ((bytes_done.min(batch.bytes_total) * 100) / batch.bytes_total) as i32
            });
            ui.set_export_copy_error(message.into());
            ui.set_export_copy_running(false);
        }
        "personal"
        | "personal-cache"
        | "personal-bulk"
        | "personal-dialog-rename"
        | "personal-dialog-create-folder"
        | "personal-dialog-move"
        | "personal-drag-move" => {
            state.borrow_mut().personal = items(&value);
            state.borrow_mut().personal_folders =
                value["folders"].as_array().cloned().unwrap_or_default();
            if matches!(
                name,
                "personal-bulk" | "personal-dialog-move" | "personal-drag-move"
            ) {
                state.borrow_mut().selected_personal.clear();
            }
            render_personal(app, state);
            if name.starts_with("personal-dialog-") {
                ui.set_personal_dialog_busy(false);
                ui.set_personal_dialog_error("".into());
                ui.set_personal_dialog(0);
            }
            if name == "personal-cache" {
                render_tasks(app, state);
            }
            if !text(&value, "notice").is_empty() {
                ui.set_notice(text(&value, "notice").into());
            }
        }
        "login" => {
            let token = text(&value, "access_token");
            if token.is_empty() {
                ui.set_error("登录响应缺少会话凭据".into());
                return;
            }
            clear_session(app, state, true);
            state.borrow_mut().token = token;
            ui.set_signed_in(true);
            ui.set_email(ui.get_auth_email());
            ui.set_account_label(
                ui.get_auth_email()
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string()
                    .into(),
            );
            ui.set_auth_password("".into());
            ui.set_auth_open(false);
            let uid = text(&value["user"], "id");
            let history_name = format!("{uid}-history.json");
            let task_snapshot_name = format!("{uid}-task-inputs.json");
            let history = state.borrow().folder.join(&history_name);
            let task_snapshots = state.borrow().folder.join(&task_snapshot_name);
            state.borrow_mut().history_name = history_name;
            state.borrow_mut().task_snapshot_name = task_snapshot_name;
            state.borrow_mut().local = std::fs::read(history)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            state.borrow_mut().task_snapshots = std::fs::read(task_snapshots)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            job(
                app,
                state,
                "capabilities".into(),
                request("GET", "/api/capabilities", Value::Null),
            );
            job(
                app,
                state,
                "wallet".into(),
                request("GET", "/api/wallet", Value::Null),
            );
            let target =
                auth_return_target(state.borrow_mut().auth_return_page.take(), ui.get_page());
            load_page(app, state, target);
            let pending_team_picker = std::mem::take(&mut state.borrow_mut().pending_team_picker);
            if pending_team_picker {
                job(
                    app,
                    state,
                    "teams".into(),
                    request("GET", "/api/teams", Value::Null),
                );
            }
            if !state.borrow().pending_personal_references.is_empty() && target != 1 {
                refresh_models(app, state);
            }
        }
        "logout" => {
            clear_session(app, state, false);
            switch_auth(&ui, 0);
            ui.set_auth_open(true);
        }
        "register" => {
            switch_auth(&ui, 3);
            email_notice(app, &value);
            if value["email_delivery"] == "sent" {
                ui.set_auth_resend_seconds(60);
            }
        }
        "verify-email" | "password-reset-confirm" => {
            if name == "password-reset-confirm" {
                clear_session(app, state, false);
            }
            switch_auth(&ui, 0);
            ui.set_notice(
                if name == "verify-email" {
                    "邮箱验证成功，请登录"
                } else {
                    "密码已更新，请使用新密码登录"
                }
                .into(),
            );
        }
        "verification-resend" | "password-reset-request" => {
            let mode = if name == "verification-resend" { 3 } else { 4 };
            if ui.get_auth_mode() != mode {
                switch_auth(&ui, mode);
            }
            email_notice(app, &value);
            ui.set_auth_resend_seconds(60);
        }
        "wallet" => {
            ui.set_ledger(rows(
                value["ledger"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| AccountEntry {
                        id: text(v, "id").into(),
                        title: match text(v, "kind").as_str() {
                            "hold" => "生成预扣",
                            "capture" => "生成消费",
                            "release" => "退回积分",
                            "recharge" => "充值到账",
                            _ => "积分变动",
                        }
                        .into(),
                        detail: text(v, "reference_id").into(),
                        value: v["delta_available"].to_string().into(),
                    })
                    .collect(),
            ));
            ui.set_balance(
                value["available_credits"]
                    .as_i64()
                    .unwrap_or(0)
                    .to_string()
                    .into(),
            );
            ui.set_frozen(
                value["held_credits"]
                    .as_i64()
                    .unwrap_or(0)
                    .to_string()
                    .into(),
            );
            if let Some(credits) = state.borrow().quote_credits {
                ui.set_insufficient_credits(ui.get_balance().parse::<i64>().unwrap_or(0) < credits);
            }
        }
        "models" => {
            // Capture the live form before replacing the catalog. A provider
            // refresh may reorder models, but it must not turn the current
            // numeric index into a different model or reset raw parameter
            // values while the user is editing.
            {
                let mut cloud = state.borrow_mut();
                remember_current_model_draft(&ui, &mut cloud);
            }
            state.borrow_mut().models = value
                .get("models")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| items(&value));
            update_model_options_preserving_catalog(app, state);
            refresh_quote(app, state);
            if ui.get_page() == 7 {
                render_templates(app, state);
            }
            let pending_template_id = state.borrow_mut().pending_template_id.take();
            if let Some(template_id) = pending_template_id {
                if state.borrow().models.is_empty() {
                    state.borrow_mut().pending_template_id = Some(template_id);
                    ui.set_error("模型目录暂未加载，请重试应用模板".into());
                } else {
                    let result = template_root()
                        .and_then(crate::generation_templates::Library::load)
                        .and_then(|library| {
                            apply_template(app, state, library.get(&template_id).cloned())
                        });
                    if let Err(error) = result {
                        ui.set_error(error.into());
                    }
                }
            }
            let pending_task_refill_id = state.borrow_mut().pending_task_refill_id.take();
            if let Some(task_id) = pending_task_refill_id {
                if state.borrow().models.is_empty() {
                    state.borrow_mut().pending_task_refill_id = Some(task_id);
                    ui.set_error("模型目录暂未加载，请重试回填历史任务".into());
                } else if let Err(error) = refill_task(app, state, &task_id) {
                    ui.set_error(error.into());
                }
            }
            if ui.get_signed_in() && state.borrow().picker_batch.is_some() {
                continue_picker_batch(app, state);
            }
            if ui.get_signed_in() {
                let pending = state.borrow().pending_personal_references.clone();
                let candidates = pending
                    .iter()
                    .map(|(path, name)| PickerSelection {
                        id: path.clone(),
                        name: name.clone(),
                        kind: reference_kind(Path::new(path)).to_owned(),
                        local_path: path.clone(),
                        download_endpoint: String::new(),
                        download_path: PathBuf::new(),
                    })
                    .collect::<Vec<_>>();
                if !candidates.is_empty() {
                    if let Err(error) =
                        validate_picker_reference_selection(&ui, &state.borrow(), &candidates)
                    {
                        ui.set_error(error.into());
                        return;
                    }
                    state.borrow_mut().pending_personal_references.clear();
                }
                if !pending.is_empty() {
                    ui.set_asset_picker_open(false);
                    ui.set_page(1);
                }
                for (path, name) in pending {
                    upload_personal_reference(app, state, path, name);
                }
            }
        }
        "quote" => {
            let current = generation_body(&ui, &state.borrow());
            if value["client_request_body"] != current {
                return;
            }
            if !state
                .borrow()
                .pending
                .iter()
                .all(|(name, _)| name != "quote")
            {
                return;
            }
            let credits = value["credits"].as_i64().unwrap_or(0);
            state.borrow_mut().quote_id = text(&value, "quote_id");
            state.borrow_mut().quote_credits = Some(credits);
            state.borrow_mut().quote_body = current;
            ui.set_quote(format!("预计消耗 {credits} 积分").into());
            ui.set_insufficient_credits(ui.get_balance().parse::<i64>().unwrap_or(0) < credits);
            ui.set_can_generate(
                !ui.get_prompt().trim().is_empty()
                    && reference_validation(&ui, &state.borrow()).is_empty(),
            );
        }
        "generate" => {
            let submission = state.borrow_mut().submission.take();
            if let Some(submission) = submission {
                let task_ids = value["tasks"]
                    .as_array()
                    .map(|tasks| {
                        tasks
                            .iter()
                            .map(|task| text(task, "id"))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| vec![text(&value, "id")]);
                if task_ids.iter().any(|id| !id.is_empty()) {
                    for task_id in task_ids.into_iter().filter(|id| !id.is_empty()) {
                        state
                            .borrow_mut()
                            .task_snapshots
                            .insert(task_id, submission.snapshot.clone());
                    }
                    if let Err(error) = save_task_snapshots(&state.borrow()) {
                        ui.set_error(error.into());
                    }
                }
            }
            state.borrow_mut().quote_id.clear();
            ui.set_can_generate(false);
            ui.set_notice("任务已提交".into());
            ui.set_selected_task(0);
            job(
                app,
                state,
                "tasks".into(),
                request("GET", "/api/generation/tasks", Value::Null),
            );
        }
        "tasks" => {
            let tasks = items(&value);
            let changed = state.borrow().tasks != tasks;
            let selected_id = selected_task_id(&state.borrow().tasks, ui.get_selected_task());
            {
                let mut cloud = state.borrow_mut();
                cloud
                    .selected_tasks
                    .retain(|id| tasks.iter().any(|task| text(task, "id") == *id));
                cloud.tasks = tasks.clone();
            }
            render_tasks(app, state);
            match selected_id
                .as_deref()
                .and_then(|id| selected_task_index(&tasks, id))
            {
                Some(index) => ui.set_selected_task(index),
                None if ui.get_result_preview_open() => {
                    ui.set_selected_task(-1);
                    ui.set_result_preview_open(false);
                }
                None => ui.set_selected_task(-1),
            }
            if changed {
                job(
                    app,
                    state,
                    "wallet".into(),
                    request("GET", "/api/wallet", Value::Null),
                );
                refresh_quote(app, state);
            }
            for task in tasks.iter().filter(|v| text(v, "status") == "succeeded") {
                let id = text(task, "id");
                let needs_download = !state
                    .borrow()
                    .local
                    .get(&id)
                    .is_some_and(|p| std::path::Path::new(p).is_file());
                if needs_download && state.borrow_mut().download_attempts.insert(id.clone()) {
                    download_task(app, state, &id, "cache", "");
                }
            }
        }
        "tasks-trash" => {
            state.borrow_mut().selected_tasks.clear();
            ui.set_task_selection_mode(false);
            ui.set_task_selected_count(0);
            ui.set_notice("已从结果列表移除".into());
            job(
                app,
                state,
                "tasks".into(),
                request("GET", "/api/generation/tasks", Value::Null),
            );
        }
        "teams" => {
            let teams = items(&value);
            state.borrow_mut().teams = teams.clone();
            ui.set_team_names(strings(teams.iter().map(|v| text(v, "name")).collect()));
            if !teams.is_empty() {
                ui.set_team_index(0);
                ui.set_team_owner(text(&teams[0], "role") == "owner");
                refresh_assets(app, state);
            }
        }
        "plans" => ui.set_plans(rows(
            items(&value)
                .iter()
                .map(|v| CreditPlan {
                    id: text(v, "id").into(),
                    name: text(v, "name").into(),
                    price: format!("¥{:.2}", v["price_fen"].as_f64().unwrap_or(0.) / 100.).into(),
                    credits: format!("{} 积分", v["credits"]).into(),
                })
                .collect(),
        )),
        "team-create" | "team-join" => job(
            app,
            state,
            "teams".into(),
            request("GET", "/api/teams", Value::Null),
        ),
        "invite" => {
            state.borrow_mut().invite_id = text(&value, "id");
            ui.set_invite_url(text(&value, "invite_url").into());
            ui.set_invite_detail("24 小时内有效，仅可使用一次".into());
        }
        "invite-revoke" => {
            ui.set_invite_url("".into());
            state.borrow_mut().invite_id.clear();
        }
        "members" => ui.set_members(rows(
            items(&value)
                .iter()
                .map(|v| AccountEntry {
                    id: text(v, "id").into(),
                    title: text(v, "email").into(),
                    detail: "".into(),
                    value: if text(v, "role") == "owner" {
                        "创建者"
                    } else {
                        "成员"
                    }
                    .into(),
                })
                .collect(),
        )),
        "member-remove" => {
            let team = team_id(&ui, &state.borrow());
            job(
                app,
                state,
                "members".into(),
                request("GET", format!("/api/teams/{team}/members"), Value::Null),
            );
        }
        "assets" => {
            let expected = format!(
                "/api/teams/{}/assets?trash={}",
                team_id(&ui, &state.borrow()),
                ui.get_trash_open()
            );
            if !accept_asset_list(&mut state.borrow_mut(), &value, &expected) {
                return;
            }
            render_assets(app, state);
        }
        "asset-change" => {
            let path = text(&value, "local_path");
            if !path.is_empty() {
                state.borrow_mut().local.insert(text(&value, "id"), path);
                ui.set_notice("已上传到团队资产库".into());
            }
            let uploaded_team = text(&value, "intent_team");
            if uploaded_team.is_empty() || uploaded_team == team_id(&ui, &state.borrow()) {
                refresh_assets(app, state);
            }
        }
        name if name.starts_with("reference:") => {
            let id = name.trim_start_matches("reference:");
            if let Some(item) = state
                .borrow_mut()
                .references
                .iter_mut()
                .find(|v| text(v, "client_id") == id)
            {
                let display_name = item["display_name"].clone();
                let client_id = item["client_id"].clone();
                let local_path = item["local_path"].clone();
                let kind = item["kind"].clone();
                let source_id = item["source_id"].clone();
                *item = value;
                item["display_name"] = display_name;
                // Upload responses normally echo these fields, but the
                // client identity and slot order must survive even when a
                // provider response only contains its remote asset id.
                if text(item, "client_id").is_empty() {
                    item["client_id"] = client_id;
                }
                if text(item, "local_path").is_empty() {
                    item["local_path"] = local_path;
                }
                if text(item, "kind").is_empty() {
                    item["kind"] = kind;
                }
                if text(item, "source_id").is_empty() {
                    item["source_id"] = source_id;
                }
            }
            render_references(app, state);
            sync_reference_recovery_warning(&ui, state);
            refresh_quote(app, state);
        }
        name if name == "local"
            || name.starts_with("local-cache:")
            || name.starts_with("local-reference:")
            || name.starts_with("media-preview:")
            || name == "picker-batch" =>
        {
            let path = text(&value, "path");
            let id = text(&value, "id");
            state.borrow_mut().pending_task_references.remove(&id);
            let generated = id.starts_with("gen_");
            state.borrow_mut().local.insert(id.clone(), path.clone());
            let folder = state.borrow().folder.clone();
            let _ = std::fs::create_dir_all(&folder);
            if let Ok(bytes) = serde_json::to_vec(&state.borrow().local) {
                let _ = std::fs::write(folder.join(&state.borrow().history_name), bytes);
            }
            render_tasks(app, state);
            render_assets(app, state);
            let intent = text(&value, "intent");
            match intent.as_str() {
                "media-preview" => {
                    finish_media_preview(app, state, &id, &path, value["preview_token"].as_u64())
                }
                "batch-output" | "cache" => continue_result_batch(app, state),
                "picker-batch" => {
                    set_picker_batch_path(&mut state.borrow_mut(), &id, &path);
                    continue_picker_batch(app, state);
                }
                "preview" => open_file(&path),
                "import" => begin_handoff(app, state, "clip", vec![PathBuf::from(&path)]),
                "reference" => upload_file(app, state, path.clone(), "generation_input"),
                "canvas" => {
                    ui.set_asset_picker_open(false);
                    begin_handoff(app, state, "canvas", vec![PathBuf::from(&path)]);
                }
                "download" => ui.set_notice("结果已保存到本机".into()),
                "save-team" => upload_file_for_team(
                    app,
                    state,
                    path.clone(),
                    "team_asset",
                    text(&value, "intent_team"),
                ),
                _ => {}
            }
            if generated {
                if intent == "cache" {
                    job(
                        app,
                        state,
                        "personal-cache".into(),
                        request("POST", "local:library-register", json!({"path":path})),
                    );
                } else {
                    personal_job(app, state, "register", json!({"path":path}));
                }
            }
        }
        "purchase" | "order" | "orders" => {
            if name == "orders" {
                state.borrow_mut().orders = items(&value);
            } else {
                let id = text(&value, "id");
                state.borrow_mut().orders.retain(|v| text(v, "id") != id);
                state.borrow_mut().orders.insert(0, value.clone());
            }
            ui.set_orders(rows(
                state
                    .borrow()
                    .orders
                    .iter()
                    .map(|v| AccountEntry {
                        id: text(v, "id").into(),
                        title: "支付宝充值".into(),
                        detail: match text(v, "status").as_str() {
                            "paid" => "已到账",
                            "closed" => "已关闭",
                            _ => "等待支付",
                        }
                        .into(),
                        value: format!("¥{:.2}", v["amount_fen"].as_f64().unwrap_or(0.) / 100.)
                            .into(),
                    })
                    .collect(),
            ));
            if name == "purchase"
                && let Some(raw) = value.pointer("/payment_action/url").and_then(Value::as_str)
                && Url::parse(raw).is_ok_and(|u| {
                    u.scheme() == "https"
                        && u.host_str().is_some_and(|h| {
                            h == "openapi.alipay.com" || h == "openapi-sandbox.dl.alipaydev.com"
                        })
                })
            {
                open_file(raw);
            }
            job(
                app,
                state,
                "wallet".into(),
                request("GET", "/api/wallet", Value::Null),
            );
        }
        "connect" | "capabilities" => {
            ui.set_payment_enabled(
                value
                    .pointer("/payment/live_initiation_enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            if name == "connect" {
                ui.set_notice("已连接服务器".into());
                refresh_models(app, state);
            }
        }
        _ => {}
    }
}
fn email_notice(app: &App, value: &Value) {
    let ui = app.global::<SeeCut>();
    if value["email_delivery"] == "not_configured" {
        ui.set_error("邮件服务尚未配置，请联系管理员".into());
    } else if value["email_delivery"] == "failed" {
        ui.set_error("邮件发送失败，请稍后重新发送".into());
    } else {
        ui.set_notice(
            if value["email_delivery"] == "sent" {
                "验证码已发送，30 分钟内有效。重新发送后请使用最新验证码。"
            } else if ui.get_auth_mode() == 4 {
                "若该邮箱已注册，你将收到重置验证码，请检查收件箱和垃圾邮件。"
            } else {
                "若该邮箱尚未验证，你将收到验证码，请检查收件箱和垃圾邮件。"
            }
            .into(),
        );
    }
}
fn render_tasks(app: &App, state: &Rc<RefCell<Cloud>>) {
    let c = state.borrow();
    let task_rows: Vec<CloudItem> = c
        .tasks
        .iter()
        .map(|v| {
            let mut item = task_item(v);
            item.selected = c.selected_tasks.contains(&text(v, "id"));
            let snapshot = c
                .task_snapshots
                .get(&text(v, "id"))
                .cloned()
                .or_else(|| server_task_snapshot(v).ok());
            item.refillable = snapshot.is_some();
            if item.kind.as_str() == "video"
                && let Some(snapshot) = snapshot.as_ref()
                && let Some(duration) = snapshot.parameters.get("duration")
            {
                item.detail = format!(
                    "{} · {}",
                    parameter_label("duration", duration),
                    item.detail
                )
                .into();
            }
            if let Some(path) = c.local.get(&text(v, "id")) {
                item.local = std::path::Path::new(path).is_file();
                if item.local {
                    item.ready = true;
                    if text(v, "status") == "expired" {
                        item.status = "已保存到本机".into();
                    }
                }
                if text(v, "kind") == "image" {
                    item.preview = slint::Image::load_from_path(std::path::Path::new(path))
                        .unwrap_or_default();
                } else if text(v, "kind") == "video"
                    && let Ok(root) = library_root()
                {
                    use crate::personal_library::{
                        Asset, AssetKind, AssetSource, cached_thumbnail,
                    };
                    let asset = Asset {
                        id: String::new(),
                        name: item.name.to_string(),
                        path: PathBuf::from(path),
                        kind: AssetKind::Video,
                        source: AssetSource::Generated,
                        original_path: None,
                        created_at: 0,
                        trashed: false,
                        favorite: false,
                        folder_id: None,
                    };
                    item.preview = cached_thumbnail(&asset, &root)
                        .and_then(|thumbnail| slint::Image::load_from_path(&thumbnail).ok())
                        .unwrap_or_default();
                }
            }
            item
        })
        .collect();
    let ui = app.global::<SeeCut>();
    ui.set_task_selected_count(c.selected_tasks.len() as i32);
    ui.set_tasks(rows(task_rows.clone()));
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, task) in c.tasks.iter().enumerate() {
        let id = text(task, "batch_id");
        let id = if id.is_empty() { text(task, "id") } else { id };
        if let Some((_, indices)) = groups.iter_mut().find(|(group_id, _)| *group_id == id) {
            indices.push(index);
        } else {
            groups.push((id, vec![index]));
        }
    }
    ui.set_batches(rows(
        groups
            .into_iter()
            .map(|(id, mut indices)| {
                indices.sort_by_key(|index| c.tasks[*index]["batch_index"].as_i64().unwrap_or(0));
                let first = &c.tasks[indices[0]];
                let created = first["created_at"]
                    .as_i64()
                    .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
                    .map(|time| {
                        let local = time.with_timezone(&Local);
                        if local.date_naive() == Local::now().date_naive() {
                            format!("今天 {}", local.format("%H:%M"))
                        } else {
                            local.format("%m-%d %H:%M").to_string()
                        }
                    })
                    .unwrap_or_else(|| "最近".to_owned());
                let success = indices
                    .iter()
                    .filter(|index| {
                        text(&c.tasks[**index], "status") == "succeeded" || task_rows[**index].ready
                    })
                    .count();
                let failed = indices
                    .iter()
                    .filter(|index| text(&c.tasks[**index], "status") == "failed")
                    .count();
                let count = indices.len();
                let unit = if text(first, "kind") == "video" {
                    "条"
                } else {
                    "张"
                };
                let summary = if failed == 0 && success == count {
                    let mut fields = vec![format!("{count} {unit}")];
                    let ratio = text(&first["request"], "aspect_ratio");
                    let size = text(&first["request"], "size");
                    if !ratio.is_empty() {
                        fields.push(ratio);
                    }
                    if !size.is_empty() {
                        fields.push(size.replace('x', " × "));
                    }
                    fields.join(" · ")
                } else if success + failed == count {
                    format!("完成 {success}/{count} · {failed} {unit}失败")
                } else {
                    format!("{count} {unit} · 生成中")
                };
                GenerationBatch {
                    id: id.into(),
                    label: created.into(),
                    summary: summary.into(),
                    items: rows(
                        indices
                            .into_iter()
                            .map(|index| task_rows[index].clone())
                            .collect(),
                    ),
                }
            })
            .collect(),
    ));
}

fn start_result_batch(app: &App, state: &Rc<RefCell<Cloud>>, intent: &str) {
    let ui = app.global::<SeeCut>();
    let (ids, tasks) = {
        let cloud = state.borrow();
        let tasks = cloud
            .tasks
            .iter()
            .filter(|task| cloud.selected_tasks.contains(&text(task, "id")))
            .cloned()
            .collect::<Vec<_>>();
        let ids = tasks
            .iter()
            .map(|task| text(task, "id"))
            .collect::<Vec<_>>();
        (ids, tasks)
    };
    if ids.is_empty() {
        ui.set_error("请先选择生成结果".into());
        return;
    }
    if intent == "canvas" && tasks.iter().any(|task| text(task, "kind") != "image") {
        ui.set_error("画布只支持图片结果".into());
        return;
    }
    if intent == "reference"
        && ui.get_mode() == 0
        && tasks.iter().any(|task| text(task, "kind") != "image")
    {
        ui.set_error("当前图片模型只支持图片参考".into());
        return;
    }
    if intent == "reference" {
        let cloud = state.borrow();
        let selected = tasks
            .iter()
            .map(|task| PickerSelection {
                id: text(task, "id"),
                name: text(task, "name"),
                kind: text(task, "kind"),
                local_path: cloud
                    .local
                    .get(&text(task, "id"))
                    .cloned()
                    .unwrap_or_default(),
                download_endpoint: String::new(),
                download_path: PathBuf::new(),
            })
            .collect::<Vec<_>>();
        if let Err(error) = validate_picker_reference_selection(&ui, &cloud, &selected) {
            ui.set_error(error.into());
            return;
        }
    }
    if tasks.iter().any(|task| {
        let id = text(task, "id");
        let cached = state
            .borrow()
            .local
            .get(&id)
            .is_some_and(|path| Path::new(path).is_file());
        !cached && (text(task, "status") != "succeeded" || !task_has_downloadable_output(task))
    }) {
        ui.set_error("所选结果包含尚未完成或文件不可用的项目".into());
        return;
    }
    state.borrow_mut().pending_result_batch = Some(PendingResultBatch {
        intent: intent.to_owned(),
        ids,
    });
    continue_result_batch(app, state);
}

fn continue_result_batch(app: &App, state: &Rc<RefCell<Cloud>>) {
    let Some(batch) = state.borrow().pending_result_batch.clone() else {
        return;
    };
    let missing = {
        let cloud = state.borrow();
        batch
            .ids
            .iter()
            .find(|id| {
                !cloud
                    .local
                    .get(*id)
                    .is_some_and(|path| Path::new(path).is_file())
            })
            .cloned()
    };
    if let Some(id) = missing {
        if state.borrow_mut().download_attempts.insert(id.clone()) {
            download_task(app, state, &id, "batch-output", "");
        }
        return;
    }
    let paths = {
        let cloud = state.borrow();
        batch
            .ids
            .iter()
            .filter_map(|id| cloud.local.get(id).map(PathBuf::from))
            .collect::<Vec<_>>()
    };
    if paths.len() != batch.ids.len() {
        return;
    }
    state.borrow_mut().pending_result_batch = None;
    let ui = app.global::<SeeCut>();
    match batch.intent.as_str() {
        "reference" => {
            let selected = {
                let cloud = state.borrow();
                batch
                    .ids
                    .iter()
                    .zip(paths.iter())
                    .map(|(id, path)| {
                        let task = cloud.tasks.iter().find(|task| text(task, "id") == *id);
                        PickerSelection {
                            id: id.clone(),
                            name: task
                                .map(|task| text(task, "name"))
                                .unwrap_or_else(|| id.clone()),
                            kind: task.map(|task| text(task, "kind")).unwrap_or_default(),
                            local_path: path.to_string_lossy().into_owned(),
                            download_endpoint: String::new(),
                            download_path: PathBuf::new(),
                        }
                    })
                    .collect::<Vec<_>>()
            };
            if let Err(error) = validate_picker_reference_selection(&ui, &state.borrow(), &selected)
            {
                ui.set_error(error.into());
                return;
            }
            ui.set_page(1);
            ui.set_generation_step(0);
            for path in paths {
                upload_file(
                    app,
                    state,
                    path.to_string_lossy().into_owned(),
                    "generation_input",
                );
            }
        }
        "canvas" | "clip" => begin_handoff(app, state, &batch.intent, paths),
        "export" => {
            begin_copy_export(
                app,
                state,
                batch
                    .ids
                    .iter()
                    .zip(paths)
                    .map(|(id, source)| ExportCopyItem {
                        id: id.clone(),
                        name: source
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("生成结果")
                            .to_owned(),
                        source,
                    })
                    .collect(),
                "导出生成结果",
            );
        }
        _ => {}
    }
}

/// Create the final name exclusively. A prior exists-check is insufficient:
/// another export (or the user) can create the name before the copy begins.
#[cfg(test)]
fn copy_unique_export(
    source: &Path,
    folder: &Path,
    display_name: &str,
    stable_id: &str,
) -> Result<PathBuf, String> {
    copy_unique_export_progress(source, folder, display_name, stable_id, None, None)?
        .ok_or_else(|| "导出已取消".into())
}

fn copy_unique_export_progress(
    source: &Path,
    folder: &Path,
    display_name: &str,
    stable_id: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    bytes_done: Option<&std::sync::atomic::AtomicU64>,
) -> Result<Option<PathBuf>, String> {
    use std::io::{self, Read, Write};
    use std::sync::atomic::Ordering;
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let stem = export_stem(display_name, 160);
    let id = export_stem(stable_id, 48);
    for index in 0..10_000 {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Ok(None);
        }
        let suffix = match index {
            0 => String::new(),
            1 => format!("-{id}"),
            _ => format!("-{id}-{}", index - 1),
        };
        let filename = if extension.is_empty() {
            format!("{stem}{suffix}")
        } else {
            format!("{stem}{suffix}.{extension}")
        };
        let destination = folder.join(filename);
        let mut output = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        };
        let copied: io::Result<bool> = (|| {
            let mut input = std::fs::File::open(source)?;
            let mut buffer = [0_u8; 256 * 1024];
            loop {
                if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                    return Ok(false);
                }
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
                if let Some(bytes_done) = bytes_done {
                    bytes_done.fetch_add(count as u64, Ordering::Relaxed);
                }
            }
            output.flush()?;
            output.sync_all()?;
            Ok(true)
        })();
        match copied {
            Ok(true) => return Ok(Some(destination)),
            Ok(false) => {
                drop(output);
                let _ = std::fs::remove_file(&destination);
                return Ok(None);
            }
            Err(error) => {
                drop(output);
                let _ = std::fs::remove_file(&destination);
                return Err(error.to_string());
            }
        }
    }
    Err("同名文件过多，请选择其他文件夹".into())
}

fn copy_export_call(cloud: &Cloud) -> Result<Value, String> {
    use std::sync::atomic::Ordering;
    let Some(batch) = cloud.export_copy.as_ref() else {
        return Err("导出任务已失效".into());
    };
    let mut completed = Vec::new();
    for item in &batch.items {
        if batch.completed.contains(&item.id) {
            continue;
        }
        match copy_unique_export_progress(
            &item.source,
            &batch.folder,
            &item.name,
            &item.id,
            Some(&batch.cancel),
            Some(&batch.bytes_done),
        ) {
            Ok(Some(_)) => completed.push(item.id.clone()),
            Ok(None) => return Ok(json!({"completed":completed,"cancelled":true})),
            Err(error) => {
                return Ok(json!({"completed":completed,"failed":item.name,"error":error}));
            }
        }
        if batch.cancel.load(Ordering::Relaxed) {
            return Ok(json!({"completed":completed,"cancelled":true}));
        }
    }
    Ok(json!({"completed":completed}))
}

fn poll_copy_export_progress(
    weak: slint::Weak<App>,
    state: Rc<RefCell<Cloud>>,
    progress: std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    slint::Timer::single_shot(Duration::from_millis(100), move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let (active, total, done) = {
            let cloud = state.borrow();
            let Some(batch) = cloud.export_copy.as_ref() else {
                return;
            };
            if !batch.running || !std::sync::Arc::ptr_eq(&batch.bytes_done, &progress) {
                return;
            }
            (batch.running, batch.bytes_total, batch.completed.len())
        };
        if active {
            let bytes = progress.load(std::sync::atomic::Ordering::Relaxed);
            let ui = app.global::<SeeCut>();
            ui.set_export_copy_progress(if total == 0 {
                0
            } else {
                ((bytes.min(total) * 100) / total) as i32
            });
            ui.set_export_copy_done(done as i32);
            poll_copy_export_progress(app.as_weak(), state, progress);
        }
    });
}

fn begin_copy_export(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    items: Vec<ExportCopyItem>,
    title: &str,
) {
    let ui = app.global::<SeeCut>();
    if items.is_empty() {
        ui.set_error("请先选择素材".into());
        return;
    }
    if state.borrow().export_copy.is_some() {
        ui.set_error("请先完成当前导出".into());
        return;
    }
    let Some(folder) = crate::platform::pick_folder(title, "") else {
        return;
    };
    let bytes_total = items
        .iter()
        .filter_map(|item| std::fs::metadata(&item.source).ok())
        .map(|metadata| metadata.len())
        .sum();
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let batch = ExportCopyJob {
        items,
        folder,
        completed: HashSet::new(),
        running: true,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        bytes_done: progress.clone(),
        bytes_total,
    };
    ui.set_export_copy_total(batch.items.len() as i32);
    ui.set_export_copy_done(0);
    ui.set_export_copy_progress(0);
    ui.set_export_copy_error("".into());
    ui.set_export_copy_running(true);
    ui.set_export_copy_open(true);
    state.borrow_mut().export_copy = Some(batch);
    job(
        app,
        state,
        "export-copy".into(),
        request("POST", "local:export-copy", Value::Null),
    );
    poll_copy_export_progress(app.as_weak(), state.clone(), progress);
}

fn export_stem(value: &str, max_bytes: usize) -> String {
    let clean = value
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    let clean = clean.trim().trim_matches('.');
    let mut limited = String::new();
    for ch in clean.chars() {
        if limited.len() + ch.len_utf8() > max_bytes {
            break;
        }
        limited.push(ch);
    }
    if limited.is_empty() {
        "素材".into()
    } else {
        limited
    }
}
fn library_root() -> Result<PathBuf, String> {
    concat_host::AppDirs::locate()
        .map(|dirs| dirs.data.join("personal-library"))
        .map_err(|error| format!("无法打开个人资产库：{error}"))
}

fn library_call(c: &Cloud, req: &Request) -> Result<Value, String> {
    let root = library_root()?;
    library_call_at(c, req, &root)
}

fn library_call_at(c: &Cloud, req: &Request, root: &std::path::Path) -> Result<Value, String> {
    crate::personal_library::with_library_write_lock(|| library_call_unlocked(c, req, root))
}

fn library_call_unlocked(
    c: &Cloud,
    req: &Request,
    root: &std::path::Path,
) -> Result<Value, String> {
    use crate::personal_library::Library;
    let mut library = Library::load(root)?;
    let id = text(&req.body, "id");
    if !id.is_empty() && library.get(&id).is_none() {
        return Err("未找到该个人资产，请刷新后重试".into());
    }
    let notice = match req.path.as_str() {
        "local:library-import" => {
            let asset = library.import(text(&req.body, "path"))?;
            let id = asset.id.clone();
            library.restore(&id)?;
            "已导入个人资产库"
        }
        "local:library-register" => {
            library
                .register_generated_with_name(text(&req.body, "path"), req.body["name"].as_str())?;
            ""
        }
        "local:library-rename" => {
            library.rename(&id, text(&req.body, "name"))?;
            "已重命名"
        }
        "local:library-trash" => {
            library.trash(&id)?;
            "已移入回收站"
        }
        "local:library-restore" => {
            library.restore(&id)?;
            "已恢复资产"
        }
        "local:library-favorite" => {
            library.set_favorite(&id, req.body["favorite"].as_bool().unwrap_or(false))?;
            if req.body["favorite"].as_bool().unwrap_or(false) {
                "已收藏"
            } else {
                "已取消收藏"
            }
        }
        "local:library-create-folder" => {
            library.create_folder(&text(&req.body, "name"))?;
            ""
        }
        "local:library-move" => {
            let ids = req.body["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let folder_id = req.body["folder_id"].as_str().filter(|id| !id.is_empty());
            let changed = library.move_to_folder(&ids, folder_id)?;
            return library_result(
                &library,
                root,
                if changed > 0 {
                    format!("已整理 {changed} 项素材")
                } else {
                    String::new()
                },
            );
        }
        "local:library-trash-selected" | "local:library-restore-selected" => {
            let ids = req.body["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let restore = req.path == "local:library-restore-selected";
            let changed = library.set_trashed_many(&ids, !restore)?;
            return library_result(
                &library,
                root,
                format!(
                    "已{} {changed} 项资产",
                    if restore { "恢复" } else { "回收" }
                ),
            );
        }
        "local:library-list" => {
            // Recover existing SeeCut outputs, including files saved before the library existed.
            match std::fs::read_dir(&c.folder) {
                Ok(entries) => {
                    for entry in entries {
                        let path = entry
                            .map_err(|error| format!("无法读取结果目录：{error}"))?
                            .path();
                        let generated = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with("gen_")
                                    && matches!(
                                        path.extension().and_then(|ext| ext.to_str()),
                                        Some("png" | "mp4")
                                    )
                            });
                        if generated && path.is_file() {
                            library.register_generated(path)?;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("无法读取结果目录：{error}")),
            }
            ""
        }
        _ => return Err("未知的个人资产操作".into()),
    };
    library_result(&library, root, notice.to_owned())
}

fn library_result(
    library: &crate::personal_library::Library,
    root: &std::path::Path,
    notice: String,
) -> Result<Value, String> {
    use crate::personal_library::AssetKind;
    let mut assets = library.items().iter().collect::<Vec<_>>();
    assets.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let items: Vec<Value> = assets
        .into_iter()
        .map(|asset| {
            let mut value = serde_json::to_value(asset).unwrap_or_default();
            let metadata = asset.path.metadata().ok().filter(|m| m.is_file());
            let available = metadata.is_some();
            value["available"] = json!(available);
            value["bytes"] = json!(metadata.map(|m| m.len()).unwrap_or(0));
            // This list is prepared by the local-library worker. Keep media
            // probing out of Slint render callbacks and search keystrokes.
            if available && let Ok(info) = concat_media::probe(&asset.path) {
                if let Some(video) = info.video {
                    value["media_width"] = json!(video.width);
                    value["media_height"] = json!(video.height);
                }
                if let Some(duration) = info.duration.map(|time| time.as_f64())
                    && duration.is_finite()
                    && duration > 0.0
                {
                    value["media_duration_ms"] = json!((duration * 1000.0).round() as u64);
                }
            }
            if !asset.trashed && asset.kind != AssetKind::Audio {
                value["thumbnail"] = json!(crate::personal_library::thumbnail(asset, root));
            }
            value
        })
        .collect();
    Ok(json!({"items":items,"folders":library.folders(),"notice":notice}))
}

fn personal_job(app: &App, state: &Rc<RefCell<Cloud>>, operation: &str, body: Value) {
    job(
        app,
        state,
        "personal".into(),
        request("POST", format!("local:library-{operation}"), body),
    );
}

fn personal_drop_move(
    ui: &SeeCut,
    cloud: &Cloud,
    asset_id: &str,
    folder_index: i32,
) -> Option<(Vec<String>, String)> {
    if ui.get_personal_trash() || ui.get_working() || ui.get_personal_dialog() > 0 {
        return None;
    }
    personal_drop_plan(cloud, asset_id, folder_index)
}

fn personal_drop_plan(
    cloud: &Cloud,
    asset_id: &str,
    folder_index: i32,
) -> Option<(Vec<String>, String)> {
    let target_id = match folder_index {
        1 => String::new(),
        index if index >= 2 => cloud
            .personal_folders
            .get(usize::try_from(index - 2).ok()?)
            .map(|folder| text(folder, "id"))?,
        _ => return None, // All assets is an aggregate, never a destination.
    };
    let source = cloud
        .personal
        .iter()
        .find(|item| text(item, "id") == asset_id)?;
    if source["trashed"].as_bool().unwrap_or(false) {
        return None;
    }
    let mut ids = if cloud.selected_personal.contains(asset_id) {
        cloud.selected_personal.iter().cloned().collect::<Vec<_>>()
    } else {
        vec![asset_id.to_owned()]
    };
    ids.sort();
    let mut changes = false;
    for id in &ids {
        let item = cloud.personal.iter().find(|item| text(item, "id") == *id)?;
        if item["trashed"].as_bool().unwrap_or(false) {
            return None;
        }
        let current = item["folder_id"].as_str().unwrap_or_default();
        changes |= current != target_id;
    }
    changes.then_some((ids, target_id))
}

fn personal_dialog_job(app: &App, state: &Rc<RefCell<Cloud>>, operation: &str, body: Value) {
    let ui = app.global::<SeeCut>();
    if ui.get_personal_dialog_busy() {
        return;
    }
    ui.set_personal_dialog_error("".into());
    ui.set_personal_dialog_busy(true);
    job(
        app,
        state,
        format!("personal-dialog-{operation}"),
        request("POST", format!("local:library-{operation}"), body),
    );
}

fn personal_asset_detail(asset: &Value) -> String {
    let kind = text(asset, "kind");
    let width = asset["media_width"].as_u64().unwrap_or(0);
    let height = asset["media_height"].as_u64().unwrap_or(0);
    let duration_ms = asset["media_duration_ms"].as_u64().unwrap_or(0);
    if kind == "image" && width > 0 && height > 0 {
        return format!("{width} × {height}");
    }
    if kind != "image" && duration_ms > 0 {
        let seconds = duration_ms.div_ceil(1000);
        return if seconds >= 3600 {
            format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            )
        } else {
            format!("{:02}:{:02}", seconds / 60, seconds % 60)
        };
    }
    if width > 0 && height > 0 {
        return format!("{width} × {height}");
    }
    format!(
        "{:.1} MB",
        asset["bytes"].as_u64().unwrap_or(0) as f64 / 1_048_576.
    )
}

fn personal_asset_status(asset: &Value) -> String {
    let kind = match text(asset, "kind").as_str() {
        "image" => "图片",
        "video" => "视频",
        _ => "音频",
    };
    let source = if text(asset, "source") == "generated" {
        "生成结果"
    } else {
        "本地导入"
    };
    format!("{kind} · {source} · {}", personal_asset_detail(asset))
}

fn render_personal(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    sync_picker_selection(&ui, state);
    // A refresh may remove an item from the current view. Keep the batch
    // scope equal to the rows the user can currently see.
    let visible_ids = visible_personal(&ui, &state.borrow())
        .into_iter()
        .map(|item| text(item, "id"))
        .collect::<HashSet<_>>();
    state
        .borrow_mut()
        .selected_personal
        .retain(|id| visible_ids.contains(id));
    let reference_search = ui.get_personal_reference_search().to_lowercase();
    let imports_into_project =
        ui.get_asset_picker_open() && ui.get_asset_picker_purpose().as_str() == "import";
    let opens_in_canvas =
        ui.get_asset_picker_open() && ui.get_asset_picker_purpose().as_str() == "canvas";
    let picker_selecting =
        ui.get_asset_picker_open() && ui.get_asset_picker_purpose().as_str() != "canvas";
    let cloud = state.borrow();
    ui.set_personal_folder_names(strings(
        [
            vec!["全部素材".to_owned(), "未分类".to_owned()],
            cloud
                .personal_folders
                .iter()
                .map(|folder| text(folder, "name"))
                .collect(),
        ]
        .concat(),
    ));
    ui.set_personal_move_folder_names(strings(
        [
            vec!["未分类".to_owned()],
            cloud
                .personal_folders
                .iter()
                .map(|folder| text(folder, "name"))
                .collect(),
        ]
        .concat(),
    ));
    let row = |v: &Value| CloudItem {
        id: text(v, "id").into(),
        name: text(v, "name").into(),
        detail: personal_asset_detail(v).into(),
        status: personal_asset_status(v).into(),
        kind: text(v, "kind").into(),
        ready: v["available"].as_bool().unwrap_or(false),
        local: true,
        date_label: local_date_label(v["created_at"].as_u64()).into(),
        favorite: v["favorite"].as_bool().unwrap_or(false),
        selected: cloud.selected_personal.contains(&text(v, "id")),
        preview: slint::Image::load_from_path(std::path::Path::new(&text(v, "thumbnail")))
            .unwrap_or_default(),
        ..Default::default()
    };
    let visible_rows = visible_personal(&ui, &cloud)
        .into_iter()
        .map(row)
        .collect::<Vec<_>>();
    let selected_count = cloud.selected_personal.len() as i32;
    ui.set_personal_selected_count(selected_count);
    ui.set_personal_assets(rows(visible_rows.clone()));
    let mut groups: Vec<PersonalAssetGroup> = Vec::new();
    for item in visible_rows {
        let group_label = if ui.get_personal_sort() == 0 {
            item.date_label.clone()
        } else {
            "全部素材".into()
        };
        if let Some(group) = groups
            .last_mut()
            .filter(|group| group.date_label == group_label)
        {
            let mut items = group.items.iter().collect::<Vec<_>>();
            items.push(item);
            group.items = rows(items);
        } else {
            groups.push(PersonalAssetGroup {
                date_label: group_label,
                items: rows(vec![item]),
            });
        }
    }
    ui.set_personal_groups(rows(groups));
    ui.set_personal_references(rows(
        cloud
            .personal
            .iter()
            .filter(|v| {
                !v["trashed"].as_bool().unwrap_or(false)
                    && v["available"] == true
                    && (imports_into_project
                        || (!opens_in_canvas && ui.get_mode() != 0)
                        || text(v, "kind") == "image")
            })
            .filter(|v| text(v, "name").to_lowercase().contains(&reference_search))
            .filter(|v| match ui.get_asset_picker_personal_filter() {
                1 => text(v, "kind") == "image",
                2 => text(v, "kind") == "video",
                3 => text(v, "kind") == "audio",
                _ => true,
            })
            .filter(|v| {
                imports_into_project
                    || reference_kind(&PathBuf::from(text(v, "path"))) != "unsupported"
            })
            .map(|value| {
                let mut item = row(value);
                item.selected =
                    picker_selecting && cloud.picker_selected_ids.contains(&text(value, "id"));
                item
            })
            .collect(),
    ));
}

fn visible_personal<'a>(ui: &SeeCut, cloud: &'a Cloud) -> Vec<&'a Value> {
    let search = ui.get_personal_search().to_lowercase();
    let kind = match ui.get_personal_filter() {
        1 => "image",
        2 => "video",
        3 => "audio",
        _ => "",
    };
    let source = match ui.get_personal_source() {
        1 => "imported",
        2 => "generated",
        _ => "",
    };
    let folder_filter = ui.get_personal_folder_filter();
    let selected_folder = usize::try_from(folder_filter - 2)
        .ok()
        .and_then(|index| cloud.personal_folders.get(index))
        .map(|folder| text(folder, "id"));
    let mut visible = cloud
        .personal
        .iter()
        .filter(|v| v["trashed"].as_bool().unwrap_or(false) == ui.get_personal_trash())
        .filter(|v| text(v, "name").to_lowercase().contains(&search))
        .filter(|v| kind.is_empty() || text(v, "kind") == kind)
        .filter(|v| source.is_empty() || text(v, "source") == source)
        .filter(|v| {
            folder_filter == 0
                || (folder_filter == 1 && v["folder_id"].is_null())
                || selected_folder
                    .as_ref()
                    .is_some_and(|id| text(v, "folder_id") == *id)
        })
        .filter(|v| !ui.get_personal_favorites() || v["favorite"].as_bool().unwrap_or(false))
        .collect::<Vec<_>>();
    match ui.get_personal_sort() {
        1 => visible.sort_by_cached_key(|v| {
            (
                text(v, "name").to_lowercase(),
                std::cmp::Reverse(v["created_at"].as_u64().unwrap_or(0)),
                text(v, "id"),
            )
        }),
        2 => visible.sort_by_cached_key(|v| {
            (
                match text(v, "kind").as_str() {
                    "image" => 0,
                    "video" => 1,
                    _ => 2,
                },
                text(v, "name").to_lowercase(),
                std::cmp::Reverse(v["created_at"].as_u64().unwrap_or(0)),
                text(v, "id"),
            )
        }),
        _ => visible.sort_by_cached_key(|v| {
            (
                std::cmp::Reverse(v["created_at"].as_u64().unwrap_or(0)),
                text(v, "id"),
            )
        }),
    }
    visible
}

fn local_date_label(timestamp: Option<u64>) -> String {
    timestamp
        .and_then(|millis| {
            std::time::UNIX_EPOCH
                .checked_add(Duration::from_millis(millis))
                .map(DateTime::<Local>::from)
        })
        .map(|date| {
            let days_ago = Local::now()
                .date_naive()
                .signed_duration_since(date.date_naive())
                .num_days();
            match days_ago {
                0 => "今天".to_owned(),
                1 => "昨天".to_owned(),
                _ => date.format("%Y-%m-%d").to_string(),
            }
        })
        .unwrap_or_else(|| "未知日期".into())
}

fn enqueue_personal_reference(
    queue: &mut VecDeque<(String, String)>,
    path: String,
    name: String,
) -> bool {
    if queue.iter().any(|(queued, _)| queued == &path) {
        false
    } else {
        queue.push_back((path, name));
        true
    }
}

#[derive(Clone, Debug)]
struct PickerSelection {
    id: String,
    name: String,
    kind: String,
    local_path: String,
    download_endpoint: String,
    download_path: PathBuf,
}

fn picker_download_request(item: &PickerSelection) -> Option<Request> {
    if item.download_endpoint.is_empty() {
        return None;
    }
    Some(request(
        "GET",
        "local:download",
        json!({
            "id": item.id,
            "endpoint": item.download_endpoint,
            "path": item.download_path,
            "intent": "picker-batch",
            "intent_team": ""
        }),
    ))
}

fn set_picker_batch_path(state: &mut Cloud, id: &str, path: &str) -> bool {
    let Some((_, selected)) = state.picker_batch.as_mut() else {
        return false;
    };
    let Some(item) = selected.iter_mut().find(|item| item.id == id) else {
        return false;
    };
    item.local_path = path.to_owned();
    true
}

fn picker_selection_items(ui: &SeeCut, state: &Cloud) -> Result<Vec<PickerSelection>, String> {
    let mut result = Vec::with_capacity(state.picker_selected_ids.len());
    for id in &state.picker_selected_ids {
        if ui.get_asset_picker_source() == 0 {
            let Some(asset) = state
                .personal
                .iter()
                .find(|item| text(item, "id") == id.as_str())
            else {
                return Err("所选素材已不可用，请重新选择".into());
            };
            let path = text(asset, "path");
            if asset["trashed"].as_bool().unwrap_or(false)
                || !asset["available"].as_bool().unwrap_or(false)
                || !PathBuf::from(&path).is_file()
            {
                return Err(format!("素材“{}”已不可用，请重新选择", text(asset, "name")));
            }
            let kind = text(asset, "kind");
            if ui.get_asset_picker_purpose() == "reference"
                && reference_kind(&PathBuf::from(&path)) == "unsupported"
            {
                return Err(format!("素材“{}”格式不受支持", text(asset, "name")));
            }
            result.push(PickerSelection {
                id: id.clone(),
                name: text(asset, "name"),
                kind,
                local_path: path,
                download_endpoint: String::new(),
                download_path: PathBuf::new(),
            });
        } else {
            let Some(asset) = state
                .assets
                .iter()
                .find(|item| text(item, "id") == id.as_str())
            else {
                return Err("所选团队素材已刷新，请重新选择".into());
            };
            let content_type = text(asset, "content_type");
            let kind = content_type
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            if !matches!(kind.as_str(), "image" | "video" | "audio") {
                return Err(format!("素材“{}”格式不受支持", text(asset, "filename")));
            }
            let local_path = state
                .local
                .get(id)
                .filter(|path| PathBuf::from(path.as_str()).is_file())
                .cloned()
                .unwrap_or_default();
            let team = team_id(ui, state);
            if team.is_empty() {
                return Err("请选择素材所属团队".into());
            }
            let filename = text(asset, "filename");
            let extension = std::path::Path::new(&filename)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("bin");
            result.push(PickerSelection {
                id: id.clone(),
                name: filename.clone(),
                kind,
                local_path,
                download_endpoint: format!("/api/teams/{team}/assets/{id}/download"),
                download_path: state.folder.join(format!("{id}.{extension}")),
            });
        }
    }
    Ok(result)
}

fn validate_picker_reference_selection(
    ui: &SeeCut,
    state: &Cloud,
    selected: &[PickerSelection],
) -> Result<(), String> {
    let model_id = selected_model(ui, state);
    let Some(model) = state.models.iter().find(|model| {
        text(model, "id") == model_id && text(model, "kind") == model_kind(ui.get_mode())
    }) else {
        return Err("请选择可用的生成模型后再添加参考".into());
    };
    let max_items = model["parameters"]["reference_asset_ids"]["max_items"]
        .as_u64()
        .unwrap_or(0) as usize;
    if state.references.len() + selected.len() > max_items {
        return Err(format!(
            "当前模型最多支持 {max_items} 项参考素材，请减少选择"
        ));
    }
    let mut counts = [
        (
            "image",
            state
                .references
                .iter()
                .filter(|v| text(v, "kind") == "image")
                .count(),
        ),
        (
            "video",
            state
                .references
                .iter()
                .filter(|v| text(v, "kind") == "video")
                .count(),
        ),
        (
            "audio",
            state
                .references
                .iter()
                .filter(|v| text(v, "kind") == "audio")
                .count(),
        ),
    ];
    let mut seen = state
        .references
        .iter()
        .flat_map(|reference| {
            [
                text(reference, "local_path"),
                text(reference, "source_id"),
                text(reference, "id"),
            ]
        })
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    for item in selected {
        if !item.local_path.is_empty()
            && reference_kind(std::path::Path::new(&item.local_path)) != item.kind
        {
            return Err(format!("素材“{}”格式不受支持", item.name));
        }
        if !accepts_reference(model, &item.kind) {
            return Err(format!(
                "当前模型不支持{}参考素材",
                reference_label(&item.kind)
            ));
        }
        if !seen.insert(if item.local_path.is_empty() {
            item.id.clone()
        } else {
            item.local_path.clone()
        }) {
            return Err(format!("参考素材“{}”已在列表中", item.name));
        }
        let Some((_, count)) = counts
            .iter_mut()
            .find(|(kind, _)| *kind == item.kind.as_str())
        else {
            return Err("参考素材类型不受支持".into());
        };
        *count += 1;
        let limit = model["parameters"]["reference_asset_ids"]["max_per_kind"][&item.kind]
            .as_u64()
            .unwrap_or(max_items as u64) as usize;
        if *count > limit {
            return Err(format!(
                "{}参考最多 {limit} 项，请减少选择",
                reference_label(&item.kind)
            ));
        }
    }
    let all_audio = state
        .references
        .iter()
        .map(|item| text(item, "kind"))
        .chain(selected.iter().map(|item| item.kind.clone()))
        .all(|kind| kind == "audio");
    if (!state.references.is_empty() || !selected.is_empty()) && all_audio {
        return Err("参考音频需要搭配至少一张图片或一段视频".into());
    }
    Ok(())
}

fn picker_toggle(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let ui = app.global::<SeeCut>();
    if !ui.get_asset_picker_open() || ui.get_asset_picker_purpose() == "canvas" {
        return;
    }
    sync_picker_selection(&ui, state);
    let valid = {
        let cloud = state.borrow();
        cloud
            .picker_selected_ids
            .iter()
            .any(|selected| selected == id)
            || if ui.get_asset_picker_source() == 0 {
                cloud.personal.iter().any(|item| {
                    text(item, "id") == id && item["available"].as_bool().unwrap_or(false)
                })
            } else {
                cloud.assets.iter().any(|item| text(item, "id") == id)
            }
    };
    if !valid {
        ui.set_error("所选素材已不可用，请重新选择".into());
        return;
    }
    let mut cloud = state.borrow_mut();
    if let Some(index) = cloud.picker_selected_ids.iter().position(|item| item == id) {
        cloud.picker_selected_ids.remove(index);
    } else {
        cloud.picker_selected_ids.push(id.to_owned());
    }
    drop(cloud);
    ui.set_error("".into());
    render_personal(app, state);
    render_assets(app, state);
}

fn picker_confirm(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    if ui.get_asset_picker_purpose() == "canvas" {
        return;
    }
    sync_picker_selection(&ui, state);
    let selected = match picker_selection_items(&ui, &state.borrow()) {
        Ok(selected) if !selected.is_empty() => selected,
        Ok(_) => {
            ui.set_error("请先选择素材".into());
            return;
        }
        Err(error) => {
            ui.set_error(error.into());
            return;
        }
    };
    let purpose = ui.get_asset_picker_purpose().to_string();
    if state.borrow().picker_batch.is_some() {
        ui.set_error("上一批素材仍在准备，请稍候".into());
        return;
    }
    let needs_catalog =
        purpose == "reference" && (!ui.get_signed_in() || state.borrow().models.is_empty());
    if purpose == "reference"
        && !needs_catalog
        && let Err(error) = validate_picker_reference_selection(&ui, &state.borrow(), &selected)
    {
        ui.set_error(error.into());
        return;
    }
    state.borrow_mut().picker_batch = Some((purpose.clone(), selected));
    ui.set_error("".into());
    if needs_catalog {
        if !ui.get_signed_in() {
            show_auth(app, state, Some(1));
        } else {
            refresh_models(app, state);
        }
        return;
    }
    continue_picker_batch(app, state);
}

// Download the complete batch before accepting any references. Slots are then
// inserted together in selection order; upload failures remain retryable slots.
fn continue_picker_batch(app: &App, state: &Rc<RefCell<Cloud>>) {
    let Some((purpose, selected)) = state.borrow().picker_batch.clone() else {
        return;
    };
    if state.borrow().active_name == "picker-batch"
        || state
            .borrow()
            .pending
            .iter()
            .any(|(name, _)| name == "picker-batch")
    {
        return;
    }
    if let Some(item) = selected.iter().find(|item| item.local_path.is_empty()) {
        download_asset(app, state, &item.id, "picker-batch");
        return;
    }
    let ui = app.global::<SeeCut>();
    if selected
        .iter()
        .any(|item| !PathBuf::from(&item.local_path).is_file())
    {
        state.borrow_mut().picker_batch = None;
        ui.set_error("所选素材文件已不可用，请重新选择".into());
        return;
    }
    let validation = if purpose == "reference" {
        validate_picker_reference_selection(&ui, &state.borrow(), &selected)
    } else {
        Ok(())
    };
    if let Err(error) = validation {
        state.borrow_mut().picker_batch = None;
        ui.set_error(error.into());
        return;
    }
    state.borrow_mut().picker_batch = None;
    clear_picker_selection(&ui, state);
    state.borrow_mut().picker_context.clear();
    ui.set_asset_picker_open(false);
    render_assets(app, state);
    if purpose == "import" {
        for item in selected {
            import_or_queue_named(app, state, item.local_path, Some(item.name));
        }
    } else {
        ui.set_page(1);
        ui.set_generation_step(0);
        for item in selected {
            upload_file(app, state, item.local_path, "generation_input");
        }
    }
}

fn personal_action(app: &App, state: &Rc<RefCell<Cloud>>, action: &str, id: &str) {
    let ui = app.global::<SeeCut>();
    match action {
        "personal-preview" => open_personal_media_preview(app, state, id),
        "personal-refresh" => personal_job(app, state, "list", Value::Null),
        "personal-register-canvas" => {
            let payload = serde_json::from_str::<Value>(id).ok();
            let path = payload
                .as_ref()
                .map(|value| text(value, "path"))
                .filter(|path| !path.is_empty())
                .unwrap_or_else(|| id.to_owned());
            let name = payload
                .as_ref()
                .map(|value| text(value, "name"))
                .unwrap_or_default();
            personal_job(app, state, "register", json!({"path":path,"name":name}));
        }
        "personal-select" => {
            let visible = visible_personal(&ui, &state.borrow())
                .into_iter()
                .any(|item| text(item, "id") == id);
            if visible {
                let mut cloud = state.borrow_mut();
                if !cloud.selected_personal.remove(id) {
                    cloud.selected_personal.insert(id.to_owned());
                }
                drop(cloud);
                render_personal(app, state);
            }
        }
        "personal-select-visible" => {
            let ids = visible_personal(&ui, &state.borrow())
                .into_iter()
                .map(|item| text(item, "id"))
                .collect::<Vec<_>>();
            let mut cloud = state.borrow_mut();
            if id == "true" {
                cloud.selected_personal.extend(ids);
            } else {
                for id in ids {
                    cloud.selected_personal.remove(&id);
                }
            }
            drop(cloud);
            render_personal(app, state);
        }
        "personal-trash-selected" | "personal-restore-selected" => {
            let ids = state
                .borrow()
                .selected_personal
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            if ids.is_empty() {
                ui.set_error("请先选择素材".into());
                return;
            }
            job(
                app,
                state,
                "personal-bulk".into(),
                request(
                    "POST",
                    format!("local:library-{}", action.trim_start_matches("personal-")),
                    json!({"ids":ids}),
                ),
            );
        }
        "personal-freeze-delete" => {
            let ids = if id.is_empty() {
                state
                    .borrow()
                    .selected_personal
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                vec![id.to_owned()]
            };
            if ids.is_empty() {
                ui.set_error("请先选择素材".into());
                return;
            }
            let known = ids.iter().all(|id| {
                state
                    .borrow()
                    .personal
                    .iter()
                    .any(|asset| text(asset, "id") == *id)
            });
            if !known {
                ui.set_error("所选素材已变化，请刷新后重试".into());
                return;
            }
            state.borrow_mut().pending_delete_ids = ids;
            ui.set_personal_delete_count(state.borrow().pending_delete_ids.len() as i32);
            ui.set_personal_dialog(5);
        }
        "personal-confirm-delete" => {
            let ids = std::mem::take(&mut state.borrow_mut().pending_delete_ids);
            if ids.is_empty() {
                return;
            }
            job(
                app,
                state,
                "personal-bulk".into(),
                request("POST", "local:library-trash-selected", json!({"ids": ids})),
            );
        }
        "personal-create-folder" => {
            if ui.get_personal_new_folder().trim().is_empty() {
                ui.set_personal_dialog_error("请输入文件夹名称".into());
                return;
            }
            personal_dialog_job(
                app,
                state,
                "create-folder",
                json!({"name": ui.get_personal_new_folder().trim()}),
            );
        }
        "personal-move" | "personal-move-selected" => {
            let index = ui.get_personal_folder_target();
            let folder_id = if index == 0 {
                String::new()
            } else {
                usize::try_from(index - 1)
                    .ok()
                    .and_then(|index| state.borrow().personal_folders.get(index).cloned())
                    .map(|folder| text(&folder, "id"))
                    .unwrap_or_default()
            };
            if index != 0 && folder_id.is_empty() {
                ui.set_personal_dialog_error("文件夹不存在，请刷新后重试".into());
                return;
            }
            let ids = if action == "personal-move" {
                vec![id.to_string()]
            } else {
                state
                    .borrow()
                    .selected_personal
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if !ids.is_empty() {
                personal_dialog_job(app, state, "move", json!({"ids":ids,"folder_id":folder_id}));
            } else {
                ui.set_personal_dialog_error("请先选择素材".into());
            }
        }
        "personal-drop-folder" => {
            let Some((asset_id, index)) = id.rsplit_once(':') else {
                return;
            };
            let Ok(index) = index.parse::<i32>() else {
                return;
            };
            let Some((ids, folder_id)) = personal_drop_move(&ui, &state.borrow(), asset_id, index)
            else {
                return;
            };
            job(
                app,
                state,
                "personal-drag-move".into(),
                request(
                    "POST",
                    "local:library-move",
                    json!({"ids":ids,"folder_id":folder_id}),
                ),
            );
        }
        "personal-reference-selected"
        | "personal-project-selected"
        | "personal-canvas-selected"
        | "personal-export-selected" => {
            let selected = {
                let cloud = state.borrow();
                cloud
                    .personal
                    .iter()
                    .filter(|asset| cloud.selected_personal.contains(&text(asset, "id")))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if selected.is_empty() {
                ui.set_error("请先选择素材".into());
                return;
            }
            if selected
                .iter()
                .any(|asset| !PathBuf::from(text(asset, "path")).is_file())
            {
                ui.set_error("所选素材包含缺失文件，请重新关联后重试".into());
                return;
            }
            match action {
                "personal-reference-selected" => {
                    if ui.get_mode() == 0
                        && selected.iter().any(|asset| text(asset, "kind") != "image")
                    {
                        ui.set_error("当前图片模型只支持图片参考".into());
                        return;
                    }
                    if ui.get_signed_in() && !state.borrow().models.is_empty() {
                        let candidates = selected
                            .iter()
                            .map(|asset| PickerSelection {
                                id: text(asset, "id"),
                                name: text(asset, "name"),
                                kind: text(asset, "kind"),
                                local_path: text(asset, "path"),
                                download_endpoint: String::new(),
                                download_path: PathBuf::new(),
                            })
                            .collect::<Vec<_>>();
                        if let Err(error) =
                            validate_picker_reference_selection(&ui, &state.borrow(), &candidates)
                        {
                            ui.set_error(error.into());
                            return;
                        }
                    }
                    ui.set_page(1);
                    if !ui.get_signed_in() || state.borrow().models.is_empty() {
                        for asset in &selected {
                            enqueue_personal_reference(
                                &mut state.borrow_mut().pending_personal_references,
                                text(asset, "path"),
                                text(asset, "name"),
                            );
                        }
                        if ui.get_signed_in() {
                            refresh_models(app, state);
                        } else {
                            show_auth(app, state, Some(1));
                        }
                    } else {
                        for asset in &selected {
                            upload_personal_reference(
                                app,
                                state,
                                text(asset, "path"),
                                text(asset, "name"),
                            );
                        }
                    }
                }
                "personal-project-selected" => {
                    let imports = selected
                        .iter()
                        .map(|asset| {
                            crate::panes::canvas::CanvasImport::named(
                                PathBuf::from(text(asset, "path")),
                                text(asset, "name"),
                            )
                        })
                        .collect();
                    begin_handoff_with_imports(app, state, "clip", imports);
                }
                "personal-canvas-selected" => {
                    if selected.iter().any(|asset| text(asset, "kind") != "image") {
                        ui.set_error("画布只支持图片素材".into());
                        return;
                    }
                    let imports = selected
                        .iter()
                        .map(|asset| {
                            crate::panes::canvas::CanvasImport::named(
                                PathBuf::from(text(asset, "path")),
                                text(asset, "name"),
                            )
                        })
                        .collect();
                    begin_handoff_with_imports(app, state, "canvas", imports);
                }
                "personal-export-selected" => {
                    begin_copy_export(
                        app,
                        state,
                        selected
                            .iter()
                            .map(|asset| ExportCopyItem {
                                id: text(asset, "id"),
                                source: PathBuf::from(text(asset, "path")),
                                name: text(asset, "name"),
                            })
                            .collect(),
                        "导出素材",
                    );
                }
                _ => {}
            }
        }
        "personal-favorite" => {
            let favorite = state
                .borrow()
                .personal
                .iter()
                .find(|item| text(item, "id") == id)
                .and_then(|item| item["favorite"].as_bool())
                .unwrap_or(false);
            personal_job(
                app,
                state,
                "favorite",
                json!({"id":id,"favorite":!favorite}),
            );
        }
        "personal-browse" => {
            if let Some(paths) = crate::platform::pick_files("导入个人素材", None) {
                for path in paths {
                    personal_job(app, state, "import", json!({"path":path}));
                }
            }
        }
        "personal-drop" => personal_job(app, state, "import", json!({"path":id})),
        "personal-folder" => match library_root() {
            Ok(path) => open_file(&path.to_string_lossy()),
            Err(error) => ui.set_error(error.into()),
        },
        "personal-teams" => {
            if ui.get_signed_in() {
                job(
                    app,
                    state,
                    "teams".into(),
                    request("GET", "/api/teams", Value::Null),
                );
            } else {
                state.borrow_mut().pending_team_picker = true;
                show_auth(app, state, None);
            }
        }
        "personal-rename" => {
            if ui.get_personal_rename().trim().is_empty() {
                ui.set_personal_dialog_error("请输入素材名称".into());
                return;
            }
            personal_dialog_job(
                app,
                state,
                "rename",
                json!({"id":id,"name":ui.get_personal_rename().trim()}),
            );
        }
        "personal-trash" | "personal-restore" => personal_job(
            app,
            state,
            action.trim_start_matches("personal-"),
            json!({"id":id}),
        ),
        _ => {
            let asset = state
                .borrow()
                .personal
                .iter()
                .find(|v| text(v, "id") == id)
                .cloned();
            let Some(asset) = asset else {
                ui.set_error("未找到该个人资产，请刷新后重试".into());
                return;
            };
            let path = text(&asset, "path");
            if !PathBuf::from(&path).is_file() {
                ui.set_error("本地文件已移动或丢失".into());
                return;
            }
            match action {
                "personal-preview" => open_personal_media_preview(app, state, id),
                "personal-project" => {
                    ui.set_asset_picker_open(false);
                    begin_handoff_with_imports(
                        app,
                        state,
                        "clip",
                        vec![crate::panes::canvas::CanvasImport::named(
                            PathBuf::from(path),
                            text(&asset, "name"),
                        )],
                    );
                }
                "personal-canvas" => {
                    if text(&asset, "kind") != "image" {
                        ui.set_error("只有图片素材可以打开到画布".into());
                        return;
                    }
                    ui.set_asset_picker_open(false);
                    begin_handoff_with_imports(
                        app,
                        state,
                        "canvas",
                        vec![crate::panes::canvas::CanvasImport::named(
                            PathBuf::from(path),
                            text(&asset, "name"),
                        )],
                    );
                }
                "personal-reference" => {
                    if !ui.get_signed_in() {
                        ui.set_asset_picker_open(false);
                        enqueue_personal_reference(
                            &mut state.borrow_mut().pending_personal_references,
                            path,
                            text(&asset, "name"),
                        );
                        show_auth(app, state, Some(1));
                        return;
                    }
                    ui.set_page(1);
                    ui.set_asset_picker_open(false);
                    if state.borrow().models.is_empty() {
                        enqueue_personal_reference(
                            &mut state.borrow_mut().pending_personal_references,
                            path,
                            text(&asset, "name"),
                        );
                        refresh_models(app, state);
                        return;
                    }
                    upload_personal_reference(app, state, path, text(&asset, "name"));
                }
                "personal-upload" => {
                    if team_id(&ui, &state.borrow()).is_empty() {
                        ui.set_error("请选择要上传的团队".into());
                        return;
                    }
                    upload_file(app, state, path, "team_asset");
                }
                _ => {}
            }
        }
    }
}

fn upload_personal_reference(app: &App, state: &Rc<RefCell<Cloud>>, path: String, name: String) {
    if matches!(
        reference_kind(std::path::Path::new(&path)),
        "video" | "audio"
    ) && app.global::<SeeCut>().get_mode() == 0
    {
        app.global::<SeeCut>().set_mode(1);
        app.global::<SeeCut>().set_model_index(0);
        update_model_options(app, state);
    }
    let count = state.borrow().references.len();
    upload_file(app, state, path, "generation_input");
    let mut cloud = state.borrow_mut();
    if cloud.references.len() > count {
        if let Some(reference) = cloud.references.last_mut() {
            reference["display_name"] = json!(name);
        }
        drop(cloud);
        render_references(app, state);
    }
}

fn queue_configuration_reference(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    reference: crate::generation_templates::TemplateReference,
) {
    let client_id = if reference.client_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        reference.client_id.clone()
    };
    let path = reference.path.to_string_lossy().into_owned();
    let available = !path.is_empty() && reference.path.is_file();
    let status = if available { "uploading" } else { "missing" };
    state.borrow_mut().references.push(json!({
        "client_id": client_id,
        "local_path": path,
        "kind": reference.kind,
        "display_name": if reference.name.is_empty() { "缺失参考素材" } else { reference.name.as_str() },
        "source_id": reference.source_id,
        "status": status,
        "missing": !available,
        "id": Value::Null,
    }));
    if available {
        job(
            app,
            state,
            format!("reference:{client_id}"),
            request(
                "POST",
                "local:upload",
                json!({
                    "path": path,
                    "client_id": client_id,
                    "purpose": "generation_input",
                    "team": ""
                }),
            ),
        );
    }
}

fn replace_reference_from_path(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    client_id: &str,
    path: &str,
) -> Result<(), String> {
    let path_buf = PathBuf::from(path);
    if !path_buf.is_file() {
        return Err("替换素材文件不可用，请重新选择".into());
    }
    let kind = reference_kind(&path_buf);
    if !matches!(kind, "image" | "video" | "audio") {
        return Err("替换素材格式不受支持".into());
    }
    let expected_kind = state
        .borrow()
        .references
        .iter()
        .find(|reference| text(reference, "client_id") == client_id)
        .map(|reference| text(reference, "kind"))
        .ok_or_else(|| "未找到待替换的参考素材".to_owned())?;
    if expected_kind != "unknown" && expected_kind != kind {
        return Err(format!(
            "替换素材类型不匹配，需要{}",
            reference_label(&expected_kind)
        ));
    }
    if let Some(reference) = state
        .borrow_mut()
        .references
        .iter_mut()
        .find(|reference| text(reference, "client_id") == client_id)
    {
        reference["local_path"] = json!(path);
        reference["kind"] = json!(kind);
        reference["display_name"] = json!(
            path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        );
        reference["status"] = json!("uploading");
        reference["missing"] = json!(false);
        reference["id"] = Value::Null;
    }
    render_references(app, state);
    sync_reference_recovery_warning(&app.global::<SeeCut>(), state);
    refresh_quote(app, state);
    job(
        app,
        state,
        format!("reference:{client_id}"),
        request(
            "POST",
            "local:upload",
            json!({
                "path": path,
                "client_id": client_id,
                "purpose": "generation_input",
                "team": ""
            }),
        ),
    );
    Ok(())
}

fn render_assets(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    sync_picker_selection(&ui, state);
    let picker_team_view = ui.get_asset_picker_open() && ui.get_asset_picker_source() == 1;
    let search = if picker_team_view {
        ui.get_asset_picker_team_search().to_lowercase()
    } else {
        ui.get_asset_search().to_lowercase()
    };
    let filter = if picker_team_view {
        ui.get_asset_picker_team_filter()
    } else {
        ui.get_asset_filter()
    };
    let imports_into_project =
        picker_team_view && ui.get_asset_picker_purpose().as_str() == "import";
    let opens_in_canvas = picker_team_view && ui.get_asset_picker_purpose().as_str() == "canvas";
    let picker_selecting = picker_team_view && ui.get_asset_picker_purpose().as_str() != "canvas";
    let cloud = state.borrow();
    ui.set_assets(rows(
        cloud
            .assets
            .iter()
            .filter(|v| text(v, "filename").to_lowercase().contains(&search))
            .filter(|v| {
                !picker_team_view
                    || imports_into_project
                    || if opens_in_canvas {
                        text(v, "content_type").starts_with("image/")
                    } else {
                        ui.get_mode() != 0 || text(v, "content_type").starts_with("image/")
                    }
            })
            .filter(|v| {
                filter == 0
                    || text(v, "content_type").starts_with(match filter {
                        1 => "image",
                        2 => "video",
                        _ => "audio",
                    })
            })
            .map(|v| CloudItem {
                id: text(v, "id").into(),
                name: text(v, "filename").into(),
                detail: text(v, "uploader_email").into(),
                kind: text(v, "content_type")
                    .split('/')
                    .next()
                    .unwrap_or("file")
                    .into(),
                ready: true,
                selected: picker_selecting && cloud.picker_selected_ids.contains(&text(v, "id")),
                preview: cloud
                    .local
                    .get(&text(v, "id"))
                    .filter(|_| text(v, "content_type").starts_with("image/"))
                    .and_then(|path| slint::Image::load_from_path(std::path::Path::new(path)).ok())
                    .unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
    ));
}
fn render_references(app: &App, state: &Rc<RefCell<Cloud>>) {
    let cloud = state.borrow();
    app.global::<SeeCut>().set_references(rows(
        cloud
            .references
            .iter()
            .enumerate()
            .map(|(index, v)| {
                let path = PathBuf::from(text(v, "preview_path"));
                CloudItem {
                    id: text(v, "client_id").into(),
                    name: text(v, "display_name").into(),
                    kind: text(v, "kind").into(),
                    detail: format!(
                        "@[{}{}]",
                        reference_label(&text(v, "kind")),
                        reference_number(&cloud.references, index)
                    )
                    .into(),
                    status: match text(v, "status").as_str() {
                        "ready" => {
                            let kind = text(v, "kind");
                            if kind == "image" {
                                "图片".to_owned()
                            } else {
                                let duration_ms = v["media_duration_ms"].as_u64().unwrap_or(0);
                                let duration = if duration_ms > 0 {
                                    let seconds = duration_ms.div_ceil(1000);
                                    format!(" · {:02}:{:02}", seconds / 60, seconds % 60)
                                } else {
                                    String::new()
                                };
                                format!(
                                    "{}{}",
                                    if kind == "video" { "视频" } else { "音频" },
                                    duration
                                )
                            }
                        }
                        "failed" => "上传失败".to_owned(),
                        "expired" => "已过期".to_owned(),
                        "missing" => "缺少文件，可替换".to_owned(),
                        _ => "上传中".to_owned(),
                    }
                    .into(),
                    local: true,
                    missing: text(v, "status") == "missing",
                    ready: text(v, "status") == "ready",
                    preview: slint::Image::load_from_path(&path).unwrap_or_default(),
                    ..Default::default()
                }
            })
            .collect(),
    ));
}

fn media_preview_image(path: &str, kind: &str) -> slint::Image {
    if path.is_empty() {
        return slint::Image::default();
    }
    if kind == "image" {
        return slint::Image::load_from_path(std::path::Path::new(path)).unwrap_or_default();
    }
    library_root()
        .ok()
        .and_then(|root| {
            let asset = crate::personal_library::Asset {
                id: String::new(),
                name: PathBuf::from(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: PathBuf::from(path),
                kind: if kind == "audio" {
                    crate::personal_library::AssetKind::Audio
                } else {
                    crate::personal_library::AssetKind::Video
                },
                source: crate::personal_library::AssetSource::Generated,
                original_path: None,
                created_at: 0,
                trashed: false,
                favorite: false,
                folder_id: None,
            };
            crate::personal_library::cached_thumbnail(&asset, &root)
                .and_then(|thumbnail| slint::Image::load_from_path(&thumbnail).ok())
        })
        .unwrap_or_default()
}

fn personal_preview_item(asset: &Value) -> CloudItem {
    let kind = text(asset, "kind");
    let path = text(asset, "path");
    let ready = asset["available"].as_bool().unwrap_or(false) && PathBuf::from(&path).is_file();
    CloudItem {
        id: text(asset, "id").into(),
        name: text(asset, "name").into(),
        detail: personal_asset_detail(asset).into(),
        status: personal_asset_status(asset).into(),
        kind: kind.clone().into(),
        preview: media_preview_image(&path, &kind),
        ready,
        local: true,
        ..Default::default()
    }
}

fn team_preview_item(asset: &Value, state: &Cloud) -> CloudItem {
    let id = text(asset, "id");
    let kind = text(asset, "content_type")
        .split('/')
        .next()
        .unwrap_or("file")
        .to_owned();
    let local_path = state
        .local
        .get(&id)
        .filter(|path| PathBuf::from(path.as_str()).is_file())
        .cloned()
        .unwrap_or_default();
    let ready = !local_path.is_empty();
    CloudItem {
        id: id.into(),
        name: text(asset, "filename").into(),
        detail: text(asset, "uploader_email").into(),
        kind: kind.clone().into(),
        preview: media_preview_image(&local_path, &kind),
        ready,
        local: ready,
        ..Default::default()
    }
}

fn reference_preview_item(reference: &Value) -> CloudItem {
    let kind = text(reference, "kind");
    let path = if !text(reference, "local_path").is_empty() {
        text(reference, "local_path")
    } else {
        text(reference, "preview_path")
    };
    let ready = text(reference, "status") == "ready" && PathBuf::from(&path).is_file();
    CloudItem {
        id: text(reference, "client_id").into(),
        name: text(reference, "display_name").into(),
        detail: text(reference, "error").into(),
        kind: kind.clone().into(),
        status: match text(reference, "status").as_str() {
            "missing" => "缺少文件，可替换",
            "failed" => "上传失败",
            _ => "",
        }
        .into(),
        preview: media_preview_image(&path, &kind),
        ready,
        local: !path.is_empty(),
        missing: text(reference, "status") == "missing",
        ..Default::default()
    }
}

fn task_preview_item(task: &Value, state: &Cloud) -> CloudItem {
    let id = text(task, "id");
    let mut item = task_item(task);
    item.refillable = state.task_snapshots.contains_key(&id) || server_task_snapshot(task).is_ok();
    item.ready = false;
    if let Some(path) = state
        .local
        .get(&id)
        .filter(|path| PathBuf::from(path.as_str()).is_file())
    {
        item.preview = media_preview_image(path, text(task, "kind").as_str());
        item.local = true;
        item.ready = true;
    }
    item
}

fn next_media_preview_token(state: &Rc<RefCell<Cloud>>) -> u64 {
    let mut cloud = state.borrow_mut();
    cloud.media_preview_token = cloud.media_preview_token.wrapping_add(1);
    cloud.media_preview_token
}

fn set_media_preview(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    source: &str,
    mut item: CloudItem,
    loading: bool,
    error: &str,
) -> u64 {
    let token = next_media_preview_token(state);
    item.selected = false;
    let ui = app.global::<SeeCut>();
    ui.set_media_preview_source(source.into());
    ui.set_media_preview_item(item);
    ui.set_media_preview_loading(loading);
    ui.set_media_preview_error(error.into());
    ui.set_media_preview_open(true);
    token
}

fn open_personal_media_preview(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let asset = state
        .borrow()
        .personal
        .iter()
        .find(|asset| text(asset, "id") == id)
        .cloned();
    let Some(asset) = asset else {
        app.global::<SeeCut>()
            .set_error("未找到该个人素材，请刷新后重试".into());
        return;
    };
    let item = personal_preview_item(&asset);
    let error = if item.ready {
        ""
    } else {
        "本机文件已移动或丢失"
    };
    set_media_preview(app, state, "personal", item, false, error);
}

fn open_team_media_preview(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let asset = state
        .borrow()
        .assets
        .iter()
        .find(|asset| text(asset, "id") == id)
        .cloned();
    let Some(asset) = asset else {
        app.global::<SeeCut>()
            .set_error("未找到该团队素材，请刷新后重试".into());
        return;
    };
    let item = team_preview_item(&asset, &state.borrow());
    if item.ready {
        set_media_preview(app, state, "asset", item, false, "");
        return;
    }
    let token = set_media_preview(app, state, "asset", item, true, "");
    download_asset_with_token(app, state, id, "media-preview", Some(token));
}

fn open_reference_media_preview(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let reference = state
        .borrow()
        .references
        .iter()
        .find(|reference| text(reference, "client_id") == id)
        .cloned();
    let Some(reference) = reference else {
        app.global::<SeeCut>()
            .set_error("未找到该参考素材，请刷新后重试".into());
        return;
    };
    let item = reference_preview_item(&reference);
    let error = if item.ready {
        ""
    } else if item.missing {
        "本机文件缺失，请先替换参考素材"
    } else {
        "参考素材尚未准备好"
    };
    set_media_preview(app, state, "reference", item, false, error);
}

fn open_task_media_preview(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) {
    let task = state
        .borrow()
        .tasks
        .iter()
        .find(|task| text(task, "id") == id)
        .cloned();
    let Some(task) = task else {
        app.global::<SeeCut>()
            .set_error("未找到该生成结果，请刷新后重试".into());
        return;
    };
    if text(&task, "status") == "failed" {
        if let Some(index) = selected_task_index(&state.borrow().tasks, id) {
            app.global::<SeeCut>().set_selected_task(index);
            app.global::<SeeCut>().set_result_preview_open(true);
        }
        return;
    }
    let item = task_preview_item(&task, &state.borrow());
    if item.ready {
        set_media_preview(app, state, "task", item, false, "");
        return;
    }
    if text(&task, "status") != "succeeded" || !task_has_downloadable_output(&task) {
        set_media_preview(
            app,
            state,
            "task",
            item,
            false,
            "生成结果文件不可用，请刷新后重试",
        );
        return;
    }
    let token = set_media_preview(app, state, "task", item, true, "");
    download_task_with_token(app, state, id, "media-preview", "", Some(token));
}

fn finish_media_preview(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    id: &str,
    path: &str,
    token: Option<u64>,
) {
    let ui = app.global::<SeeCut>();
    let current_token = state.borrow().media_preview_token;
    if token != Some(current_token)
        || !ui.get_media_preview_open()
        || ui.get_media_preview_item().id.as_str() != id
    {
        return;
    }
    let mut item = ui.get_media_preview_item();
    item.preview = media_preview_image(path, item.kind.as_str());
    item.ready = true;
    item.local = true;
    ui.set_media_preview_item(item);
    ui.set_media_preview_loading(false);
    ui.set_media_preview_error("".into());
}

fn fail_media_preview(app: &App, state: &Rc<RefCell<Cloud>>, id: &str, token: u64, message: &str) {
    let ui = app.global::<SeeCut>();
    if state.borrow().media_preview_token != token
        || !ui.get_media_preview_open()
        || ui.get_media_preview_item().id.as_str() != id
    {
        return;
    }
    ui.set_media_preview_loading(false);
    ui.set_media_preview_error(message.into());
}
fn media_preview_action(app: &App, state: &Rc<RefCell<Cloud>>, action: &str) {
    let ui = app.global::<SeeCut>();
    if !ui.get_media_preview_open() {
        return;
    }
    let item = ui.get_media_preview_item();
    let id = item.id.to_string();
    let source = ui.get_media_preview_source().to_string();
    if action == "retry" {
        match source.as_str() {
            "personal" => open_personal_media_preview(app, state, &id),
            "asset" => open_team_media_preview(app, state, &id),
            "reference" => open_reference_media_preview(app, state, &id),
            "task" => open_task_media_preview(app, state, &id),
            _ => {}
        }
        return;
    }
    ui.set_error("".into());
    if action == "refill" && source == "task" {
        if state.borrow().models.is_empty() {
            state.borrow_mut().pending_task_refill_id = Some(id);
            refresh_models(app, state);
        } else if let Err(error) = refill_task(app, state, &id) {
            ui.set_media_preview_error(error.into());
            return;
        }
        ui.set_media_preview_open(false);
        ui.set_page(1);
        return;
    }
    if !item.ready || ui.get_media_preview_loading() {
        return;
    }
    let path = {
        let cloud = state.borrow();
        match source.as_str() {
            "personal" => cloud
                .personal
                .iter()
                .find(|v| text(v, "id") == id)
                .map(|v| text(v, "path")),
            "reference" => cloud
                .references
                .iter()
                .find(|v| text(v, "client_id") == id)
                .map(|v| text(v, "local_path")),
            _ => cloud.local.get(&id).cloned(),
        }
    };
    let Some(path) = path.filter(|path| PathBuf::from(path).is_file()) else {
        ui.set_media_preview_error("本机文件已移动或丢失，请重试".into());
        return;
    };
    match action {
        "play" => open_file(&path),
        "download" => ui.set_notice("素材已保存到本机".into()),
        "reference" if source != "reference" => {
            if source == "personal" && (!ui.get_signed_in() || state.borrow().models.is_empty()) {
                personal_action(app, state, "personal-reference", &id);
                ui.set_media_preview_open(false);
                return;
            }
            if let Some(error) = reference_add_error(&ui, &state.borrow(), item.kind.as_str()) {
                ui.set_media_preview_error(error.into());
                return;
            }
            upload_file(app, state, path, "generation_input");
            ui.set_page(1);
            ui.set_generation_step(0);
            ui.set_media_preview_open(false);
        }
        "import" => {
            import_or_queue_named(app, state, path, Some(item.name.to_string()));
            ui.set_media_preview_open(false);
        }
        "canvas" if item.kind == "image" => {
            ui.set_media_preview_open(false);
            begin_handoff_with_imports(
                app,
                state,
                "canvas",
                vec![crate::panes::canvas::CanvasImport::named(
                    PathBuf::from(path),
                    item.name.to_string(),
                )],
            );
        }
        "team" if source == "task" => {
            ui.set_page(1);
            ui.set_save_team_task(id.into());
            ui.set_media_preview_open(false);
        }
        _ => {}
    }
}

fn accept_asset_list(state: &mut Cloud, value: &Value, expected_context: &str) -> bool {
    if text(value, "client_assets_context") != expected_context {
        return false;
    }
    state.assets_context = expected_context.to_owned();
    state.assets = items(value);
    true
}

fn refresh_assets(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    if !ui.get_signed_in() {
        return;
    }
    let id = team_id(&ui, &state.borrow());
    let context = format!("/api/teams/{id}/assets?trash={}", ui.get_trash_open());
    if state.borrow().assets_context != context {
        let mut cloud = state.borrow_mut();
        cloud.assets.clear();
        cloud.assets_context = context.clone();
        drop(cloud);
        render_assets(app, state);
    }
    if !id.is_empty() {
        job(
            app,
            state,
            "assets".into(),
            request("GET", context, Value::Null),
        );
    }
}
fn refresh_quote(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    ui.set_can_generate(false);
    state.borrow_mut().quote_id.clear();
    state.borrow_mut().quote_credits = None;
    ui.set_quote("".into());
    ui.set_insufficient_credits(false);
    let reference_error = reference_validation(&ui, &state.borrow());
    ui.set_reference_error(reference_error.clone().into());
    if !reference_error.is_empty() {
        return;
    }
    if selected_model(&ui, &state.borrow()).is_empty() {
        return;
    }
    if ui.get_signed_in() && !ui.get_prompt().trim().is_empty() {
        let mut body = generation_body(&ui, &state.borrow());
        body["kind"] = json!(if ui.get_mode() == 0 { "image" } else { "video" });
        job(
            app,
            state,
            "quote".into(),
            request("POST", "/api/generation/quote", body),
        );
    }
}
fn upload_file(app: &App, state: &Rc<RefCell<Cloud>>, path: String, purpose: &str) {
    let team = if purpose == "team_asset" {
        team_id(&app.global::<SeeCut>(), &state.borrow())
    } else {
        String::new()
    };
    upload_file_for_team(app, state, path, purpose, team);
}

fn reference_add_error(ui: &SeeCut, state: &Cloud, kind: &str) -> Option<String> {
    let model_id = selected_model(ui, state);
    let Some(model) = state.models.iter().find(|model| {
        text(model, "id") == model_id && text(model, "kind") == model_kind(ui.get_mode())
    }) else {
        return Some("当前模型目录尚未加载，请刷新后重试".into());
    };
    let max = model["parameters"]["reference_asset_ids"]["max_items"]
        .as_u64()
        .unwrap_or(0) as usize;
    if state.references.len() >= max {
        return Some(format!("当前模型最多支持 {max} 项参考素材"));
    }
    if !accepts_reference(model, kind) {
        return Some(if ui.get_mode() == 0 {
            "图片生成支持 PNG、JPG、WebP 参考图".into()
        } else {
            "视频参考支持 PNG、JPG、WebP、MP4、MOV、MP3、WAV".into()
        });
    }
    let kind_max = model["parameters"]["reference_asset_ids"]["max_per_kind"][kind]
        .as_u64()
        .unwrap_or(max as u64) as usize;
    if state
        .references
        .iter()
        .filter(|reference| text(reference, "kind") == kind)
        .count()
        >= kind_max
    {
        return Some(format!("{}参考最多 {kind_max} 项", reference_label(kind)));
    }
    None
}

fn has_reference_path(references: &[Value], path: &str) -> bool {
    references
        .iter()
        .any(|reference| text(reference, "local_path") == path)
}

fn selected_task_id(tasks: &[Value], selected: i32) -> Option<String> {
    tasks
        .get(usize::try_from(selected).ok()?)
        .map(|task| text(task, "id"))
        .filter(|id| !id.is_empty())
}

fn selected_task_index(tasks: &[Value], id: &str) -> Option<i32> {
    tasks
        .iter()
        .position(|task| text(task, "id") == id)
        .and_then(|index| i32::try_from(index).ok())
}

fn task_has_downloadable_output(task: &Value) -> bool {
    task["outputs"]
        .as_array()
        .and_then(|outputs| outputs.first())
        .is_some_and(|output| !text(output, "download_url").is_empty())
}

fn task_reference_source(task: &Value, local_path: Option<&str>) -> Result<Option<String>, String> {
    let kind = text(task, "kind");
    if !matches!(kind.as_str(), "image" | "video") {
        return Err("该生成结果类型不能作为参考素材".into());
    }
    if let Some(path) = local_path.filter(|path| std::path::Path::new(path).is_file()) {
        return Ok(Some(path.to_owned()));
    }
    if text(task, "status") != "succeeded" {
        return Err("该生成结果尚未完成，且本机结果文件不可用".into());
    }
    if !task_has_downloadable_output(task) {
        return Err("该生成结果文件不可用，请刷新后重试".into());
    }
    Ok(None)
}

fn upload_file_for_team(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    path: String,
    purpose: &str,
    team: String,
) {
    let ui = app.global::<SeeCut>();
    if !ui.get_signed_in() {
        if purpose == "generation_input" {
            let name = PathBuf::from(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            enqueue_personal_reference(
                &mut state.borrow_mut().pending_personal_references,
                path,
                name,
            );
            show_auth(app, state, Some(1));
        } else {
            show_auth(app, state, Some(2));
        }
        return;
    }
    if purpose == "team_asset" && team.is_empty() {
        ui.set_error("请选择要上传的团队".into());
        return;
    }
    let mut client_id = String::new();
    if purpose == "generation_input" {
        if has_reference_path(&state.borrow().references, &path) {
            ui.set_notice("该结果已在参考列表中".into());
            return;
        }
        let kind = reference_kind(std::path::Path::new(&path));
        if let Some(error) = reference_add_error(&ui, &state.borrow(), kind) {
            ui.set_error(error.into());
            return;
        }
        client_id = uuid::Uuid::new_v4().to_string();
        let display_name = {
            let c = state.borrow();
            c.local
                .iter()
                .find(|(_, local_path)| *local_path == &path)
                .and_then(|(id, _)| c.assets.iter().find(|asset| text(asset, "id") == *id))
                .map(|asset| text(asset, "filename"))
                .unwrap_or_else(|| {
                    PathBuf::from(&path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
        };
        state
            .borrow_mut()
            .references
            .push(json!({"client_id":client_id,"local_path":path,"kind":kind,"display_name":display_name,"status":"uploading"}));
        render_references(app, state);
        refresh_quote(app, state);
    }
    job(
        app,
        state,
        if purpose == "team_asset" {
            "asset-change".to_owned()
        } else {
            format!("reference:{client_id}")
        },
        request(
            "POST",
            "local:upload",
            json!({"path":path,"client_id":client_id,"purpose":purpose,"team":if purpose=="team_asset"{team}else{String::new()}}),
        ),
    );
}
fn download_task(app: &App, state: &Rc<RefCell<Cloud>>, id: &str, intent: &str, intent_team: &str) {
    download_task_with_token(app, state, id, intent, intent_team, None);
}

fn download_task_with_token(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    id: &str,
    intent: &str,
    intent_team: &str,
    preview_token: Option<u64>,
) {
    let c = state.borrow().clone();
    if let Some(path) = c
        .local
        .get(id)
        .filter(|p| std::path::Path::new(p).is_file())
    {
        match intent {
            "media-preview" => finish_media_preview(app, state, id, path, preview_token),
            "batch-output" => continue_result_batch(app, state),
            "preview" => open_file(path),
            "import" => begin_handoff(app, state, "clip", vec![PathBuf::from(path)]),
            "reference" => upload_file(app, state, path.clone(), "generation_input"),
            "canvas" => {
                begin_handoff(app, state, "canvas", vec![PathBuf::from(path)]);
            }
            "download" => app.global::<SeeCut>().set_notice("结果已保存到本机".into()),
            "save-team" => upload_file_for_team(
                app,
                state,
                path.clone(),
                "team_asset",
                intent_team.to_owned(),
            ),
            _ => {}
        }
        return;
    }
    if let Some(task) = c.tasks.iter().find(|v| text(v, "id") == id)
        && let Some(out) = task["outputs"].as_array().and_then(|v| v.first())
    {
        let ext = if text(task, "kind") == "video" {
            "mp4"
        } else {
            "png"
        };
        let path = c.folder.join(format!("{id}.{ext}"));
        job(
            app,
            state,
            if intent == "cache" {
                format!("local-cache:{id}")
            } else if intent == "reference" {
                format!("local-reference:{id}")
            } else if intent == "media-preview" {
                format!("media-preview:{id}:{}", preview_token.unwrap_or_default())
            } else {
                "local".into()
            },
            {
                let mut body = json!({"id":id,"url":out["download_url"],"path":path,"intent":intent,"intent_team":intent_team});
                if let Some(token) = preview_token {
                    body["preview_token"] = json!(token);
                }
                request("GET", "local:download", body)
            },
        );
    }
}
fn download_asset(app: &App, state: &Rc<RefCell<Cloud>>, id: &str, intent: &str) {
    download_asset_with_token(app, state, id, intent, None);
}

fn download_asset_with_token(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    id: &str,
    intent: &str,
    preview_token: Option<u64>,
) {
    let c = state.borrow().clone();
    if let Some(path) = c
        .local
        .get(id)
        .filter(|path| std::path::Path::new(path).is_file())
    {
        match intent {
            "picker-batch" => {
                set_picker_batch_path(&mut state.borrow_mut(), id, path);
                continue_picker_batch(app, state);
            }
            "media-preview" => finish_media_preview(app, state, id, path, preview_token),
            "preview" => open_file(path),
            "import" => begin_handoff(app, state, "clip", vec![PathBuf::from(path)]),
            "reference" => upload_file(app, state, path.clone(), "generation_input"),
            "canvas" => {
                app.global::<SeeCut>().set_asset_picker_open(false);
                begin_handoff(app, state, "canvas", vec![PathBuf::from(path)]);
            }
            "download" => app.global::<SeeCut>().set_notice("素材已保存到本机".into()),
            _ => {}
        }
        return;
    }
    if intent == "picker-batch" {
        let frozen_request = c
            .picker_batch
            .as_ref()
            .and_then(|(_, items)| items.iter().find(|item| item.id == id))
            .and_then(picker_download_request);
        if let Some(req) = frozen_request {
            job(app, state, "picker-batch".into(), req);
        } else {
            state.borrow_mut().picker_batch = None;
            app.global::<SeeCut>()
                .set_error("所选素材的下载信息已失效，请重新选择".into());
        }
        return;
    }
    let team = team_id(&app.global::<SeeCut>(), &c);
    if let Some(asset) = c.assets.iter().find(|v| text(v, "id") == id) {
        let filename = text(asset, "filename");
        let ext = std::path::Path::new(&filename)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("bin");
        let mut body = json!({"id":id,"endpoint":format!("/api/teams/{team}/assets/{id}/download"),"path":c.folder.join(format!("{id}.{ext}")),"intent":intent,"intent_team":""});
        if let Some(token) = preview_token {
            body["preview_token"] = json!(token);
        }
        job(
            app,
            state,
            if intent == "picker-batch" {
                "picker-batch".into()
            } else if intent == "media-preview" {
                format!("media-preview:{id}:{}", preview_token.unwrap_or_default())
            } else {
                "local".into()
            },
            request("GET", "local:download", body),
        );
    }
}
fn open_file(path: &str) {
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        let _ = opener::open(path);
    }
}

fn enqueue_pending_import(queue: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    if queue.iter().any(|queued| queued == &path) {
        false
    } else {
        queue.push(path);
        true
    }
}

fn resume_pending_imports(queue: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    std::mem::take(queue)
}

fn cancel_pending_imports(queue: &mut Vec<PathBuf>) -> usize {
    let count = queue.len();
    queue.clear();
    count
}

fn import_paths(app: &App, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    app.global::<SeeCut>().set_page(0);
    app.global::<SeeCut>().set_notice("".into());
    crate::host::on_ui(move |studio, _, _| {
        studio.handle(crate::panes::Msg::Media(
            crate::panes::media_bin::MediaMsg::Import(paths),
        ))
    });
}

fn import_named_paths(app: &App, imports: Vec<crate::panes::media_bin::MediaImport>) {
    if imports.is_empty() {
        return;
    }
    app.global::<SeeCut>().set_page(0);
    app.global::<SeeCut>().set_notice("".into());
    crate::host::on_ui(move |studio, _, _| {
        let Some(session) = studio.media_import_session() else {
            return;
        };
        studio.handle(crate::panes::Msg::Media(
            crate::panes::media_bin::MediaMsg::ImportNamed { imports, session },
        ))
    });
}

fn import_or_queue_named(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    path: String,
    display_name: Option<String>,
) {
    let ui = app.global::<SeeCut>();
    let path = PathBuf::from(path);
    if ui.get_project_open() {
        if let Some(display_name) = display_name.filter(|name| !name.trim().is_empty()) {
            import_named_paths(
                app,
                vec![crate::panes::media_bin::MediaImport {
                    path,
                    display_name: Some(display_name),
                }],
            );
        } else {
            import_paths(app, vec![path]);
        }
        return;
    }
    let count = {
        let mut cloud = state.borrow_mut();
        enqueue_pending_import(&mut cloud.pending_imports, path.clone());
        if let Some(display_name) = display_name.filter(|name| !name.trim().is_empty()) {
            cloud
                .pending_import_display_names
                .insert(path, display_name);
        }
        cloud.pending_imports.len()
    };
    ui.set_pending_import_count(count as i32);
    ui.set_page(0);
    ui.set_notice("".into());
}

fn project_ready(app: &App, state: &Rc<RefCell<Cloud>>) {
    let imports = {
        let mut cloud = state.borrow_mut();
        let display_names = std::mem::take(&mut cloud.pending_import_display_names);
        let mut imports = resume_pending_imports(&mut cloud.pending_imports)
            .into_iter()
            .map(|path| crate::panes::media_bin::MediaImport {
                display_name: display_names.get(&path).cloned(),
                path,
            })
            .collect::<Vec<_>>();
        if cloud
            .pending_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.kind == "clip" && handoff.awaiting_new_project)
        {
            imports.extend(
                cloud
                    .pending_handoff
                    .take()
                    .expect("checked")
                    .imports
                    .into_iter()
                    .map(|item| crate::panes::media_bin::MediaImport {
                        path: item.path,
                        display_name: item.display_name,
                    }),
            );
        }
        imports
    };
    let ui = app.global::<SeeCut>();
    ui.set_project_open(true);
    ui.set_pending_import_count(0);
    import_named_paths(app, imports);
}

fn cancel_project_import(app: &App, state: &Rc<RefCell<Cloud>>) {
    let mut cloud = state.borrow_mut();
    let mut count = cancel_pending_imports(&mut cloud.pending_imports);
    cloud.pending_import_display_names.clear();
    if cloud
        .pending_handoff
        .as_ref()
        .is_some_and(|handoff| handoff.kind == "clip" && handoff.awaiting_new_project)
    {
        count += cloud
            .pending_handoff
            .take()
            .map_or(0, |handoff| handoff.imports.len());
    }
    drop(cloud);
    let ui = app.global::<SeeCut>();
    ui.set_pending_import_count(0);
    if count > 0 {
        ui.set_notice("".into());
    }
}
fn task_item(v: &Value) -> CloudItem {
    let status = text(v, "status");
    let completed_spec = if text(v, "kind") == "video" {
        let duration = v["request"]["duration"].as_u64();
        let resolution = text(&v["request"], "resolution");
        match (duration, resolution.is_empty()) {
            (Some(seconds), false) => {
                format!("{:02}:{:02} · {resolution}", seconds / 60, seconds % 60)
            }
            (Some(seconds), true) => format!("{:02}:{:02}", seconds / 60, seconds % 60),
            (None, false) => resolution,
            (None, true) => "已完成".to_owned(),
        }
    } else {
        let size = text(&v["request"], "size");
        if size.is_empty() {
            "已完成".to_owned()
        } else {
            size.replace('x', " × ")
        }
    };
    CloudItem {
        id: text(v, "id").into(),
        name: text(v, "prompt").into(),
        detail: if status == "failed" {
            let message = text(&v["error"], "message");
            if message.is_empty() {
                "未返回失败原因".into()
            } else {
                message
            }
        } else {
            text(v, "model")
        }
        .into(),
        status: match status.as_str() {
            "queued" => "排队中",
            "submitting" | "provider_accepted" | "processing" => "生成中",
            "validating" => "保存结果",
            "succeeded" => &completed_spec,
            "failed" => "生成失败",
            _ => "待核实",
        }
        .into(),
        failed: status == "failed",
        kind: text(v, "kind").into(),
        ready: status == "succeeded",
        local: false,
        ..Default::default()
    }
}
fn update_model_options(app: &App, state: &Rc<RefCell<Cloud>>) {
    update_model_options_inner(app, state, false);
}

/// Apply a newly fetched catalog without interpreting the old numeric index as
/// the user's choice. The current id and raw parameter values are captured
/// before the response replaces the catalog by the caller.
fn update_model_options_preserving_catalog(app: &App, state: &Rc<RefCell<Cloud>>) {
    update_model_options_inner(app, state, true);
}

/// Render a saved template/task configuration from its stable model id and
/// raw parameter values. The old form indices are deliberately fenced off
/// while the mode is changed: a task restored into the same mode must not
/// accidentally inherit whichever model happened to be selected beforehand.
fn update_model_options_for_configuration(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    mode: i32,
    model_id: &str,
    parameters: &Value,
) {
    let ui = app.global::<SeeCut>();
    let kind = model_kind(mode);
    {
        let mut cloud = state.borrow_mut();
        cloud.updating_model_options = true;
        // Parameter changes are captured lazily at model/catalog transitions.
        // Snapshot the old form before the requested history/template state
        // replaces its selected id and draft, including cross-mode restores.
        remember_current_model_draft(&ui, &mut cloud);
        cloud.generation_mode = Some(mode);
        cloud
            .selected_model_ids
            .insert(kind.to_owned(), model_id.to_owned());
        if !model_id.is_empty() {
            cloud
                .parameter_drafts
                .insert(parameter_draft_key(mode, model_id), parameters.clone());
        }
        let available = cloud
            .models
            .iter()
            .any(|model| text(model, "kind") == kind && text(model, "id") == model_id);
        cloud.unavailable_model_id = (!available).then_some(model_id.to_owned());
        cloud.unavailable_model_kind = (!available).then_some(kind.to_owned());
    }

    // `on_changed("mode")` is intentionally ignored during this transition;
    // the explicit update below owns the complete restore operation.
    app.global::<SeeCut>().set_mode(mode);
    state.borrow_mut().updating_model_options = false;
    update_model_options_preserving_catalog(app, state);
}

fn option_index(model: &Value, name: &str, expected: Option<&Value>) -> (i32, bool) {
    if text(model, "id").is_empty() {
        return (-1, false);
    }
    let values = model["parameters"][name]["values"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(expected) = expected
        && let Some(index) = values.iter().position(|value| value == expected)
    {
        return (index as i32, false);
    }
    (parameter_default_index(model, name), expected.is_some())
}

fn update_model_options_inner(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    preserve_catalog_selection: bool,
) {
    let ui = app.global::<SeeCut>();
    let mode = ui.get_mode();
    let kind = model_kind(mode);
    let (models, desired_id, draft, unavailable) = {
        let mut cloud = state.borrow_mut();
        if cloud.updating_model_options {
            return;
        }
        cloud.updating_model_options = true;

        let previous_mode = cloud.generation_mode;
        if !preserve_catalog_selection {
            let previous_kind = previous_mode.map(model_kind);
            let previous_id = previous_kind
                .and_then(|previous_kind| cloud.selected_model_ids.get(previous_kind).cloned())
                .or_else(|| {
                    previous_mode.and_then(|previous_mode| {
                        model_id_at_index(&cloud.models, previous_mode, ui.get_model_index())
                    })
                });
            if let (Some(previous_mode), Some(previous_id)) =
                (previous_mode, previous_id.as_deref())
            {
                // A mode switch leaves the previous mode's indices in the UI
                // until this function updates them. A model change leaves the
                // old model id in selected_model_ids, so both transitions can
                // be snapshotted. Catalog refreshes skip this capture because
                // `models` has already been replaced by the new response.
                remember_model_draft(&ui, &mut cloud, previous_mode, previous_id);
            }
        }
        cloud.generation_mode = Some(mode);

        let current_index_id = model_id_at_index(&cloud.models, mode, ui.get_model_index());
        let selected_id = if preserve_catalog_selection {
            cloud.selected_model_ids.get(kind).cloned()
        } else if previous_mode == Some(mode) {
            current_index_id.or_else(|| cloud.selected_model_ids.get(kind).cloned())
        } else {
            cloud
                .selected_model_ids
                .get(kind)
                .cloned()
                .or(current_index_id)
        };
        let models = cloud
            .models
            .iter()
            .filter(|model| text(model, "kind") == kind)
            .cloned()
            .collect::<Vec<_>>();
        let desired_id = selected_id.filter(|id| !id.is_empty());
        let unavailable = if cloud.unavailable_model_kind.as_deref() == Some(kind)
            && cloud.unavailable_model_id.is_some()
            && desired_id.is_none()
        {
            true
        } else {
            desired_id.as_deref().is_some_and(|id| {
                !models.iter().any(|model| text(model, "id") == id)
                    && cloud.unavailable_model_id.as_deref() == Some(id)
            })
        };
        let draft = desired_id.as_deref().and_then(|id| {
            cloud
                .parameter_drafts
                .get(&parameter_draft_key(mode, id))
                .cloned()
        });
        (models, desired_id, draft, unavailable)
    };

    let selected_index = if unavailable {
        -1
    } else if let Some(id) = desired_id.as_deref() {
        stable_model_index(&models, mode, id)
            .or_else(|| (!models.is_empty()).then_some(0))
            .unwrap_or(-1)
    } else if models.is_empty() {
        -1
    } else {
        0
    };

    let catalog_model_changed = desired_id
        .as_deref()
        .is_some_and(|id| !models.iter().any(|model| text(model, "id") == id) && !unavailable);
    let adjusted = std::cell::Cell::new(catalog_model_changed);
    let selected_model = usize::try_from(selected_index)
        .ok()
        .and_then(|index| models.get(index))
        .cloned()
        .unwrap_or_default();
    let options = |name: &str| {
        selected_model["parameters"][name]["values"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| parameter_label(name, value))
            .collect()
    };
    let expected = |name: &str| draft.as_ref().and_then(|value| value.get(name));
    let (ratio_index, ratio_adjusted) = option_index(
        &selected_model,
        "aspect_ratio",
        if mode == 0 {
            None
        } else {
            expected("aspect_ratio")
        },
    );
    let (resolution_index, resolution_adjusted) = option_index(
        &selected_model,
        if mode == 0 { "size" } else { "resolution" },
        expected(if mode == 0 { "size" } else { "resolution" }),
    );
    let (duration_index, duration_adjusted) =
        option_index(&selected_model, "duration", expected("duration"));
    let (quality_index, quality_adjusted) =
        option_index(&selected_model, "quality", expected("quality"));
    let (quantity_index, quantity_adjusted) =
        option_index(&selected_model, "quantity", expected("quantity"));
    adjusted.set(
        adjusted.get()
            || ratio_adjusted
            || resolution_adjusted
            || duration_adjusted
            || quality_adjusted
            || quantity_adjusted,
    );
    let supports_audio = mode != 0 && selected_model["parameters"].get("generate_audio").is_some();
    let audio_expected = expected("generate_audio");
    let audio_value = audio_expected.and_then(Value::as_bool).unwrap_or_else(|| {
        selected_model["parameters"]["generate_audio"]["default"]
            .as_bool()
            .unwrap_or(true)
    });
    if audio_expected.is_some() && !supports_audio {
        adjusted.set(true);
    }
    let limit = selected_model["parameters"]["reference_asset_ids"]["max_items"]
        .as_i64()
        .unwrap_or(0);

    ui.set_model_names(strings(
        models
            .iter()
            .map(|model| {
                let label = text(model, "display_name");
                if label.is_empty() {
                    text(model, "id")
                } else {
                    label
                }
            })
            .collect(),
    ));
    ui.set_model_index(selected_index);
    ui.set_ratios(strings(options("aspect_ratio")));
    ui.set_resolutions(strings(options(if mode == 0 {
        "size"
    } else {
        "resolution"
    })));
    ui.set_durations(strings(options("duration")));
    ui.set_qualities(strings(options("quality")));
    ui.set_quantities(strings(options("quantity")));
    ui.set_ratio_index(ratio_index);
    ui.set_resolution_index(resolution_index);
    ui.set_duration_index(duration_index);
    ui.set_quality_index(quality_index);
    ui.set_quantity_index(quantity_index);
    ui.set_supports_generation_audio(supports_audio);
    ui.set_generate_audio(audio_value);
    ui.set_accepts_references(limit > 0);
    ui.set_reference_max(limit as i32);
    ui.set_reference_limit(if mode != 0 {
        format!("全能参考 · 最多 {limit} 项\n图片 PNG/JPG/WebP，视频 MP4/MOV，音频 MP3/WAV\n视频、音频各最多 3 项，各累计 2–15 秒；音频需搭配图片或视频")
    } else {
        format!("PNG / JPG / WebP · 最多 {limit} 张")
    }.into());

    {
        let mut cloud = state.borrow_mut();
        let live_id = usize::try_from(selected_index)
            .ok()
            .and_then(|index| models.get(index))
            .map(|model| text(model, "id"))
            .filter(|id| !id.is_empty());
        if let Some(id) = live_id.or(desired_id.clone()) {
            cloud.selected_model_ids.insert(kind.into(), id);
        }
        if !unavailable && selected_index >= 0 {
            cloud.unavailable_model_id = None;
        }
        cloud.updating_model_options = false;
    }
    if adjusted.get() {
        let message = if catalog_model_changed {
            "当前模型已下线，已切换到可用模型并保留可用参数"
        } else {
            "当前模型不再支持部分已保存参数，已恢复可用默认值"
        };
        set_recovery_warning(&ui, message);
    } else if !unavailable && selected_index >= 0 {
        // Choosing a replacement model resolves the model part of a history
        // recovery. Keep a warning for any unresolved reference slots, but do
        // not leave the old missing-model error visible once the form is valid.
        let has_unresolved_references = state
            .borrow()
            .references
            .iter()
            .any(|reference| text(reference, "status") != "ready");
        let previous = ui.get_recovery_warning().to_string();
        if !has_unresolved_references && previous.contains("模型") {
            clear_recovery_warning(&ui);
            if ui.get_error().as_str() == previous {
                ui.set_error("".into());
            }
        }
    }
    render_personal(app, state);
    render_assets(app, state);
}

fn clear_session(app: &App, state: &Rc<RefCell<Cloud>>, preserve_auth_intent: bool) {
    let mut cloud = state.borrow_mut();
    cloud.token.clear();
    cloud.quote_id.clear();
    cloud.quote_credits = None;
    cloud.pending.clear();
    cloud.models.clear();
    cloud.selected_model_ids.clear();
    cloud.parameter_drafts.clear();
    cloud.generation_mode = None;
    cloud.updating_model_options = false;
    cloud.unavailable_model_id = None;
    cloud.unavailable_model_kind = None;
    cloud.tasks.clear();
    cloud.assets.clear();
    cloud.assets_context.clear();
    cloud.teams.clear();
    if !preserve_auth_intent {
        cloud.picker_selected_ids.clear();
        cloud.picker_context.clear();
        cloud.picker_batch = None;
    }
    if preserve_auth_intent {
        for reference in &mut cloud.references {
            reference["id"] = Value::Null;
            reference["status"] = json!("expired");
            reference["error"] = json!("登录状态已变化，请重新上传");
        }
    } else {
        cloud.references.clear();
    }
    if !preserve_auth_intent {
        cloud.pending_personal_references.clear();
        cloud.pending_template_id = None;
        cloud.pending_task_refill_id = None;
        cloud.pending_team_picker = false;
        cloud.auth_return_page = None;
    }
    cloud.local.clear();
    cloud.task_snapshots.clear();
    cloud.task_snapshot_name.clear();
    cloud.orders.clear();
    cloud.submission = None;
    cloud.pending_task_references.clear();
    cloud.download_attempts.clear();
    cloud.media_preview_token = cloud.media_preview_token.wrapping_add(1);
    cloud.active_name.clear();
    cloud.epoch += 1;
    drop(cloud);
    let ui = app.global::<SeeCut>();
    ui.set_signed_in(false);
    ui.set_email("".into());
    ui.set_account_label("?".into());
    ui.set_balance("--".into());
    ui.set_frozen("--".into());
    ui.set_tasks(rows(Vec::new()));
    ui.set_assets(rows(vec![]));
    if preserve_auth_intent {
        render_references(app, state);
    } else {
        ui.set_references(rows(vec![]));
    }
    ui.set_members(rows(vec![]));
    ui.set_ledger(rows(vec![]));
    ui.set_orders(rows(vec![]));
    ui.set_team_names(strings(vec![]));
    ui.set_team_index(-1);
    ui.set_invite_url("".into());
    ui.set_auth_token("".into());
    ui.set_auth_password("".into());
    ui.set_auth_password_confirm("".into());
    ui.set_can_generate(false);
    ui.set_quote("".into());
    ui.set_insufficient_credits(false);
    ui.set_recovery_warning("".into());
    ui.set_asset_picker_selected_count(0);
    ui.set_media_preview_open(false);
    ui.set_media_preview_loading(false);
    ui.set_media_preview_error("".into());
    ui.set_selected_task(-1);
    ui.set_save_team_task("".into());
    if !preserve_auth_intent {
        ui.set_asset_picker_open(false);
    }
    ui.set_mention_open(false);
    ui.set_reference_error(
        if preserve_auth_intent && !state.borrow().references.is_empty() {
            "登录状态已变化，请逐项重传参考素材"
        } else {
            ""
        }
        .into(),
    );
    ui.set_busy(false);
    ui.set_working(false);
    ui.set_auth_busy(false);
}
fn preferences_path() -> Option<PathBuf> {
    concat_host::AppDirs::locate()
        .ok()
        .map(|dirs| dirs.config.join("seecut.json"))
}

fn configured_api_url(override_url: Option<&str>) -> String {
    override_url
        .and_then(|url| base_url(url).ok())
        .unwrap_or_else(|| DEFAULT_API_URL.into())
}

fn switch_auth(ui: &SeeCut, mode: i32) {
    ui.set_auth_mode(mode);
    if mode < 3 {
        ui.set_auth_resend_seconds(0);
    }
    ui.set_auth_password("".into());
    ui.set_auth_password_confirm("".into());
    ui.set_auth_token("".into());
    ui.set_error("".into());
    ui.set_notice("".into());
}

fn normalized_auth_email(raw: &str) -> Result<String, String> {
    let email = raw.trim().to_lowercase();
    if email.len() > 254
        || email.chars().any(char::is_whitespace)
        || !email.split_once('@').is_some_and(|(name, host)| {
            !name.is_empty() && host.contains('.') && !host.contains('@') && !host.ends_with('.')
        })
    {
        return Err("请输入有效的邮箱地址".into());
    }
    Ok(email)
}

fn auth_request(
    mode: i32,
    email: &str,
    password: &str,
    confirmation: &str,
    code: &str,
) -> Result<(String, Request), String> {
    let email = normalized_auth_email(email)?;
    if matches!(mode, 0 | 1 | 4) && password.is_empty() {
        return Err("请输入密码".into());
    }
    if matches!(mode, 1 | 4) {
        if !(10..=128).contains(&password.chars().count()) {
            return Err("密码需要 10 到 128 个字符".into());
        }
        if password != confirmation {
            return Err("两次输入的密码不一致".into());
        }
    }
    let code = code.trim();
    if matches!(mode, 3 | 4) && (code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit())) {
        return Err("请输入邮件中的 6 位验证码".into());
    }
    let (name, endpoint, body) = match mode {
        0 => ("login", "login", json!({"email":email,"password":password})),
        1 => (
            "register",
            "register",
            json!({"email":email,"password":password}),
        ),
        2 => (
            "password-reset-request",
            "forgot-password",
            json!({"email":email}),
        ),
        3 => (
            "verify-email",
            "verify-email",
            json!({"email":email,"token":code}),
        ),
        4 => (
            "password-reset-confirm",
            "reset-password",
            json!({"email":email,"token":code,"password":password}),
        ),
        _ => return Err("请返回登录后重试".into()),
    };
    Ok((
        name.into(),
        request("POST", format!("/api/auth/{endpoint}"), body),
    ))
}

fn submit_auth(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    if ui.get_auth_busy() {
        return;
    }
    match auth_request(
        ui.get_auth_mode(),
        ui.get_auth_email().as_str(),
        ui.get_auth_password().as_str(),
        ui.get_auth_password_confirm().as_str(),
        ui.get_auth_token().as_str(),
    ) {
        Ok((name, req)) => {
            ui.set_auth_email(text(&req.body, "email").into());
            ui.set_notice("".into());
            job(app, state, name, req);
        }
        Err(error) => {
            ui.set_notice("".into());
            ui.set_error(error.into());
        }
    }
}

fn send_auth_code(app: &App, state: &Rc<RefCell<Cloud>>, mode: i32) {
    let ui = app.global::<SeeCut>();
    if ui.get_auth_busy() || ui.get_auth_resend_seconds() > 0 {
        return;
    }
    match normalized_auth_email(ui.get_auth_email().as_str()) {
        Ok(email) => {
            ui.set_auth_email(email.clone().into());
            ui.set_notice("".into());
            let (name, endpoint) = if mode == 4 {
                ("password-reset-request", "forgot-password")
            } else {
                ("verification-resend", "resend-verification")
            };
            job(
                app,
                state,
                name.into(),
                request(
                    "POST",
                    format!("/api/auth/{endpoint}"),
                    json!({"email":email}),
                ),
            );
        }
        Err(error) => ui.set_error(error.into()),
    }
}

fn auth_countdown(weak: slint::Weak<App>) {
    slint::Timer::single_shot(Duration::from_secs(1), move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let ui = app.global::<SeeCut>();
        ui.set_auth_resend_seconds((ui.get_auth_resend_seconds() - 1).max(0));
        auth_countdown(weak);
    });
}

fn save_preferences(app: &App, state: &Rc<RefCell<Cloud>>) {
    let Some(path) = preferences_path() else {
        return;
    };
    let c = state.borrow();
    let data = json!({
        "folder": c.folder,
        "reduced_motion": app.global::<SeeCut>().get_reduced_motion(),
        "creator_mode": app.global::<SeeCut>().get_creator_mode(),
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&data) {
        let _ = std::fs::write(path, bytes);
    }
}
pub fn bind(app: &App) {
    let saved: Value = preferences_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let base = configured_api_url(std::env::var("SEECUT_API_URL").ok().as_deref());
    let folder = saved["folder"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Movies/SeeCut")
        });
    app.global::<SeeCut>()
        .set_reduced_motion(saved["reduced_motion"].as_bool().unwrap_or(false));
    app.global::<SeeCut>().set_creator_mode(
        saved["creator_mode"]
            .as_i64()
            .map(|v| v.clamp(0, 1) as i32)
            .unwrap_or(-1),
    );
    app.global::<SeeCut>()
        .set_local_folder(folder.to_string_lossy().into_owned().into());
    app.global::<SeeCut>().set_page(1);
    app.global::<SeeCut>().set_auth_open(false);
    app.global::<SeeCut>().set_pending_import_count(0);
    let state = Rc::new(RefCell::new(Cloud {
        base,
        folder,
        ..Cloud::default()
    }));
    let drop_state = state.clone();
    let drop_app = app.as_weak();
    app.global::<SeeCut>()
        .on_personal_can_drop(move |asset_id, folder_index| {
            let Some(app) = drop_app.upgrade() else {
                return false;
            };
            personal_drop_move(
                &app.global::<SeeCut>(),
                &drop_state.borrow(),
                asset_id.as_str(),
                folder_index,
            )
            .is_some()
        });
    let weak = app.as_weak();
    let shared = state.clone();
    app.global::<SeeCut>().on_action(move |name, id| { let Some(app)=weak.upgrade() else{return}; let ui=app.global::<SeeCut>(); let team=team_id(&ui,&shared.borrow()); match name.as_str() {
        "handoff-select" => select_handoff(&app, &shared, id.as_str()),
        "handoff-cancel" => { shared.borrow_mut().pending_handoff = None; ui.set_handoff_open(false); },
        "handoff-complete" => {
            shared.borrow_mut().pending_handoff = None;
            ui.set_handoff_open(false);
            ui.set_page(6);
            ui.set_canvas_gallery_open(false);
        },
        "handoff-failed" => {
            if shared.borrow().pending_handoff.is_some() {
                ui.set_page(ui.get_handoff_source_page());
                ui.set_handoff_open(true);
                if !id.is_empty() { ui.set_error(id); }
            }
        },
        "handoff-create-cancel" => {
            let restore = {
                let mut cloud = shared.borrow_mut();
                if let Some(handoff) = cloud.pending_handoff.as_mut() {
                    if handoff.awaiting_new_project { handoff.awaiting_new_project = false; true } else { false }
                } else { false }
            };
            if restore {
                ui.set_pending_import_count(shared.borrow().pending_imports.len() as i32);
                ui.set_page(ui.get_handoff_source_page());
                ui.set_handoff_open(true);
            }
        },
        "clip-project-search" => {
            ui.set_clip_project_search(id);
            crate::host::Shell::with(|shell, app| {
                shell.studio.borrow().publish(&app, &shell.models);
            });
        },
        "clip-project-sort" | "clip-project-view" => {
            crate::host::Shell::with(|shell, app| {
                shell.studio.borrow().publish(&app, &shell.models);
            });
        },
        "canvas-project-search" => {
            ui.set_canvas_project_search(id);
            render_canvas_projects(&app, &shared);
        },
        "canvas-project-sort" | "canvas-project-view" => render_canvas_projects(&app, &shared),
        "task-selection-mode" => {
            let enabled = !ui.get_task_selection_mode();
            ui.set_task_selection_mode(enabled);
            if !enabled { shared.borrow_mut().selected_tasks.clear(); }
            render_tasks(&app, &shared);
        },
        "task-select-toggle" => {
            if shared.borrow().tasks.iter().any(|task| text(task, "id") == id.as_str()) {
                let mut cloud = shared.borrow_mut();
                if !cloud.selected_tasks.remove(id.as_str()) { cloud.selected_tasks.insert(id.to_string()); }
                drop(cloud);
                render_tasks(&app, &shared);
            }
        },
        "task-batch-reference" => start_result_batch(&app, &shared, "reference"),
        "task-batch-canvas" => start_result_batch(&app, &shared, "canvas"),
        "task-batch-clip" => start_result_batch(&app, &shared, "clip"),
        "task-batch-export" => start_result_batch(&app, &shared, "export"),
        "task-batch-freeze-delete" => {
            let ids = {
                let cloud = shared.borrow();
                cloud.tasks.iter().filter(|task| cloud.selected_tasks.contains(&text(task, "id"))).map(|task| text(task, "id")).collect::<Vec<_>>()
            };
            if ids.is_empty() { ui.set_error("请先选择生成结果".into()); return; }
            if shared.borrow().tasks.iter().filter(|task| ids.contains(&text(task, "id")))
                .any(|task| !matches!(text(task, "status").as_str(), "succeeded" | "failed" | "expired")) {
                ui.set_error("生成中的结果暂不能删除".into()); return;
            }
            ui.set_task_delete_count(ids.len() as i32);
            shared.borrow_mut().pending_task_delete_ids = ids;
            ui.set_task_delete_open(true);
        },
        "task-batch-cancel-delete" => { shared.borrow_mut().pending_task_delete_ids.clear(); ui.set_task_delete_open(false); },
        "task-batch-confirm-delete" => {
            let ids = std::mem::take(&mut shared.borrow_mut().pending_task_delete_ids);
            if !ids.is_empty() {
                ui.set_task_delete_open(false);
                job(&app, &shared, "tasks-trash".into(), request("POST", "/api/generation/tasks/trash", json!({"ids": ids})));
            }
        },
        "canvas-projects-refresh" => refresh_canvas_projects(&app, &shared),
        "canvas-new" => {
            ui.set_canvas_gallery_open(false);
            ui.set_page(6);
            app.invoke_canvas_new();
        },
        "canvas-open" => {
            let path = PathBuf::from(id.as_str());
            if crate::panes::canvas::canvas_recent_paths().ok().is_some_and(|paths| paths.contains(&path))
                && path.join("manifest.json").is_file() {
                ui.set_canvas_gallery_open(false);
                ui.set_page(6);
                app.invoke_open_canvas_path(id);
            } else {
                ui.set_error("画布项目不存在，请刷新后重试".into());
                refresh_canvas_projects(&app, &shared);
            }
        },
        name if name.starts_with("personal-") => personal_action(&app,&shared,name,id.as_str()),
        name if name.starts_with("template-") => template_action(&app,&shared,name,id.as_str()),
        "auth-open" => show_auth(&app,&shared,id.parse::<i32>().ok()),
        "auth-close" if !ui.get_auth_busy() => {
            ui.set_auth_open(false);
            ui.set_error("".into());
            ui.set_notice("".into());
            if !ui.get_signed_in() && ui.get_page()==4 { ui.set_page(1); }
            ui.set_auth_password("".into());
            ui.set_auth_password_confirm("".into());
            ui.set_auth_token("".into());
            if !ui.get_signed_in() {
                let mut cloud=shared.borrow_mut();
                cloud.auth_return_page=None;
                cloud.pending_team_picker=false;
                cloud.pending_personal_references.clear();
                cloud.picker_batch = None;
            }
        },
        "auth-submit" => submit_auth(&app, &shared),
        "auth-switch" if !ui.get_auth_busy() => {
            let mode=id.parse::<i32>().unwrap_or(0).clamp(0,4);
            if mode==0 && ui.get_signed_in() && matches!(ui.get_auth_mode(),2|4) {
                ui.set_auth_open(false);
                ui.set_page(4);
                shared.borrow_mut().auth_return_page=None;
            } else { switch_auth(&ui,mode); }
        },
        "auth-resend" => send_auth_code(&app, &shared, ui.get_auth_mode()),
        "auth-verify-start" => { if let Err(error) = normalized_auth_email(ui.get_auth_email().as_str()) { ui.set_error(error.into()); } else if !ui.get_auth_busy() { switch_auth(&ui, 3); send_auth_code(&app, &shared, 3); } },
        "change-password"=>{ui.set_auth_email(ui.get_email());switch_auth(&ui,2);ui.set_auth_open(true);},
        "logout"=>job(&app,&shared,"logout".into(),request("POST","/api/auth/logout",Value::Null)),
        "wallet-refresh"=>job(&app,&shared,"wallet".into(),request("GET","/api/wallet",Value::Null)),
        "tasks-refresh"=>job(&app,&shared,"tasks".into(),request("GET","/api/generation/tasks",Value::Null)),
        "purchase"=>job(&app,&shared,"purchase".into(),request("POST","/api/orders",json!({"plan_id":id.to_string()}))),
        "order-refresh"=>job(&app,&shared,"order".into(),request("POST",format!("/api/orders/{id}/refresh"),Value::Null)),
        "navigate"=>navigate(&app,&shared,id.parse::<i32>().unwrap_or_else(|_|ui.get_page())),
        "creator-mode"=>{
            if let Ok(mode) = id.parse::<i32>() {
                ui.set_creator_mode(mode.clamp(0, 1));
                save_preferences(&app, &shared);
            }
        },
        "project-assets"=>begin_asset_picker(&app,&shared,"import",0),
        "canvas-assets"=>begin_asset_picker(&app,&shared,"canvas",0),
        "export-copy-cancel"=>{
            if let Some(batch) = shared.borrow().export_copy.as_ref() {
                batch.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
        "export-copy-close"=>{
            if shared.borrow().export_copy.as_ref().is_some_and(|batch| !batch.running) {
                shared.borrow_mut().export_copy = None;
                ui.set_export_copy_open(false);
            }
        },
        "export-copy-retry"=>{
            let progress = {
                let mut cloud = shared.borrow_mut();
                let Some(batch) = cloud.export_copy.as_mut() else { return; };
                if batch.running || batch.completed.len() == batch.items.len() { return; }
                let bytes_done: u64 = batch.items.iter().filter(|item| batch.completed.contains(&item.id))
                    .filter_map(|item| std::fs::metadata(&item.source).ok()).map(|metadata| metadata.len()).sum();
                batch.cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                batch.bytes_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(bytes_done));
                batch.running = true;
                batch.bytes_done.clone()
            };
            ui.set_export_copy_error("".into());
            ui.set_export_copy_running(true);
            job(&app,&shared,"export-copy".into(),request("POST","local:export-copy",Value::Null));
            poll_copy_export_progress(app.as_weak(),shared.clone(),progress);
        },
        "canvas-drop-batch"=>{
            match serde_json::from_str::<Vec<PathBuf>>(id.as_str()) {
                Ok(paths) if !paths.is_empty() => begin_handoff(&app,&shared,"canvas",paths),
                _ => ui.set_error("拖入的素材无效，请重新选择".into()),
            }
        },
        "asset-picker-toggle"=>picker_toggle(&app,&shared,id.as_str()),
        "asset-picker-confirm"=>picker_confirm(&app,&shared),
        "asset-picker-cancel"=>{
            shared.borrow_mut().picker_batch = None;
            clear_picker_selection(&ui,&shared);
            shared.borrow_mut().picker_context.clear();
            ui.set_asset_picker_open(false);
            ui.set_error("".into());
            render_personal(&app,&shared);
            render_assets(&app,&shared);
        },
        "project-ready"=>project_ready(&app,&shared),
        "cancel-project-import"=>cancel_project_import(&app,&shared),
        "generate"=>{
            if !ui.get_signed_in(){show_auth(&app,&shared,Some(1));return;}
            let error = reference_validation(&ui, &shared.borrow());
            if !error.is_empty() { ui.set_error(error.into()); return; }
            let mut body=generation_body(&ui,&shared.borrow());
            if shared.borrow().quote_id.is_empty()||body!=shared.borrow().quote_body{refresh_quote(&app,&shared);return}
            let previous=shared.borrow().submission.clone();
            let key=previous.filter(|submission|submission.body==body).map(|submission|submission.key).unwrap_or_else(||uuid::Uuid::new_v4().to_string());
            let snapshot=current_task_snapshot(&ui,&shared.borrow());
            shared.borrow_mut().submission=Some(PendingSubmission{body:body.clone(),key:key.clone(),snapshot});
            body["quote_id"]=json!(shared.borrow().quote_id.clone());
            let endpoint=if ui.get_mode()==0{"/api/generation/images"}else{"/api/generation/videos"};let mut req=request("POST",endpoint,body);req.idem=Some(key);job(&app,&shared,"generate".into(),req);
        },
        "team-create"=>job(&app,&shared,"team-create".into(),request("POST","/api/teams",json!({"name":ui.get_team_name().to_string()}))),
        "team-join"=>{let raw=ui.get_invite_input().trim().to_owned();let token=raw.strip_prefix("seecut://team-invite/").unwrap_or(&raw);job(&app,&shared,"team-join".into(),request("POST","/api/team-invites/accept",json!({"token":token})));},
        "invite-create" if !team.is_empty()=>job(&app,&shared,"invite".into(),request("POST",format!("/api/teams/{team}/invites"),Value::Null)),
        "invite-revoke"=>{let invite=shared.borrow().invite_id.clone();job(&app,&shared,"invite-revoke".into(),request("DELETE",format!("/api/teams/{team}/invites/{invite}"),Value::Null));},
        "member-remove"=>job(&app,&shared,"member-remove".into(),request("DELETE",format!("/api/teams/{team}/members/{id}"),Value::Null)),
        "assets-refresh"=>refresh_assets(&app,&shared),
        "members-refresh" if !team.is_empty()=>job(&app,&shared,"members".into(),request("GET",format!("/api/teams/{team}/members"),Value::Null)),
        "asset-delete"=>job(&app,&shared,"asset-change".into(),request("DELETE",format!("/api/teams/{team}/assets/{id}"),Value::Null)),
        "asset-restore"=>job(&app,&shared,"asset-change".into(),request("POST",format!("/api/teams/{team}/assets/{id}/restore"),Value::Null)),
        "reference-browse"|"asset-upload"=>{let extensions: &[&str] = if ui.get_mode()==0 { &["png","jpg","jpeg","webp"] } else { &["png","jpg","jpeg","webp","mp4","mov","mp3","wav"] }; if let Some(paths)=crate::platform::pick_files("选择素材",if name=="reference-browse"{Some(("参考素材",extensions))}else{None}){for path in paths{upload_file(&app,&shared,path.to_string_lossy().to_string(),if name=="asset-upload"{"team_asset"}else{"generation_input"});}}},
        "reference-drop"=>upload_file(&app,&shared,id.to_string(),"generation_input"),
        "reference-team"=>begin_asset_picker(&app,&shared,"reference",0),
        "reference-source"=>{
            sync_picker_selection(&ui,&shared);
            render_personal(&app,&shared);
            render_assets(&app,&shared);
            if ui.get_asset_picker_source()==1 {
                if ui.get_signed_in(){job(&app,&shared,"teams".into(),request("GET","/api/teams",Value::Null));}
                else{shared.borrow_mut().pending_team_picker=true;show_auth(&app,&shared,None);}
            } else if ui.get_asset_picker_open() {
                personal_job(&app,&shared,"list",Value::Null);
            }
        },
        "reference-remove"=>remove_reference(&app,&shared,id.as_str()),
        "reference-mention-open"=>open_reference_mention(&app,&shared),
        "reference-mention"=>insert_mention(&app,&shared,id.as_str()),
        "reference-preview"=>open_reference_media_preview(&app,&shared,id.as_str()),
        "asset-preview"=>open_team_media_preview(&app,&shared,id.as_str()),
        "task-preview-open"=>open_task_media_preview(&app,&shared,id.as_str()),
        "media-preview-action"=>media_preview_action(&app,&shared,id.as_str()),
        "reference-retry"=>{
            let item=shared.borrow().references.iter().find(|v|text(v,"client_id")==id.as_str()&&matches!(text(v,"status").as_str(),"failed"|"expired")).cloned();
            if let Some(item)=item {
                if let Some(reference)=shared.borrow_mut().references.iter_mut().find(|v|text(v,"client_id")==id.as_str()){reference["status"]=json!("uploading");}
                render_references(&app,&shared);sync_reference_recovery_warning(&app.global::<SeeCut>(),&shared);refresh_quote(&app,&shared);
                job(&app,&shared,format!("reference:{id}"),request("POST","local:upload",json!({"path":item["local_path"],"client_id":id.to_string(),"purpose":"generation_input","team":""})));
            }
        },
        // The picker UI can later pass {"client_id":"...","path":"..."}
        // through this stable action. Replacement updates the existing slot
        // in place so same-kind @ references keep their number.
        "reference-replace"=>{
            let payload=serde_json::from_str::<Value>(&id).unwrap_or_default();
            let client_id=if payload.is_object() { text(&payload,"client_id") } else { id.to_string() };
            let path=if payload.is_object() { text(&payload,"path") } else { String::new() };
            if client_id.is_empty() {
                ui.set_error("请选择要替换的参考素材".into());
            } else if !path.is_empty() {
                if let Err(error)=replace_reference_from_path(&app,&shared,&client_id,&path) {
                    ui.set_error(error.into());
                }
            } else {
                let kind=shared.borrow().references.iter().find(|reference|text(reference,"client_id")==client_id).map(|reference|text(reference,"kind"));
                let extensions: &[&str]=match kind.as_deref() {
                    Some("video")=>&["mp4","mov"],
                    Some("audio")=>&["mp3","wav"],
                    _=>&["png","jpg","jpeg","webp"],
                };
                if let Some(paths)=crate::platform::pick_files("替换参考素材",Some(("参考素材",extensions)))
                    && let Some(path)=paths.into_iter().next()
                    && let Err(error)=replace_reference_from_path(&app,&shared,&client_id,&path.to_string_lossy()) {
                    ui.set_error(error.into());
                }
            }
        },
        "task-select"=>{
            ui.set_error("".into());
            ui.set_selected_task(selected_task_index(&shared.borrow().tasks, id.as_str()).unwrap_or(-1));
            let task=shared.borrow().tasks.iter().find(|task|text(task,"id")==id.as_str()).cloned();
            let Some(task)=task else {ui.set_error("未找到该生成结果，请刷新后重试".into());ui.set_result_preview_open(false);return;};
            let local=shared.borrow().local.get(id.as_str()).filter(|path|std::path::Path::new(path).is_file()).cloned();
            if local.is_none() {
                if text(&task,"status")!="succeeded" {
                    ui.set_error("该生成结果尚未完成，且本机结果文件不可用".into());
                } else if !task_has_downloadable_output(&task) {
                    ui.set_error("该生成结果文件不可用，请刷新后重试".into());
                } else if shared.borrow_mut().download_attempts.insert(id.to_string()) {
                    ui.set_notice("正在加载生成结果".into());
                    download_task(&app,&shared,id.as_str(),"cache","");
                } else {
                    ui.set_notice("正在加载生成结果".into());
                }
            }
        },
        "task-refill"=>{
            ui.set_error("".into());
            if shared.borrow().models.is_empty() {
                shared.borrow_mut().pending_task_refill_id=Some(id.to_string());
                ui.set_notice("正在加载模型目录，加载完成后将继续回填".into());
                refresh_models(&app,&shared);
            } else if let Err(error)=refill_task(&app,&shared,id.as_str()) {
                ui.set_error(error.into());
            }
        },
        "task-reference"=>{
            ui.set_error("".into());
            let task=shared.borrow().tasks.iter().find(|task|text(task,"id")==id.as_str()).cloned();
            let Some(task)=task else {ui.set_error("未找到该生成结果，请刷新后重试".into());return;};
            let local=shared.borrow().local.get(id.as_str()).cloned();
            let source=match task_reference_source(&task,local.as_deref()) {Ok(source)=>source,Err(error)=>{ui.set_error(error.into());return;}};
            let kind=text(&task,"kind");
            if let Some(error)=reference_add_error(&ui,&shared.borrow(),&kind) {ui.set_error(error.into());return;}
            if let Some(path)=source {
                ui.set_generation_step(0);
                upload_file(&app,&shared,path,"generation_input");
            } else if shared.borrow_mut().pending_task_references.insert(id.to_string()) {
                ui.set_generation_step(0);
                download_task(&app,&shared,id.as_str(),"reference","");
            } else {
                ui.set_notice("正在准备该结果，请稍候".into());
            }
        },
        "task-preview"=>download_task(&app,&shared,id.as_str(),"preview",""),
        "task-download"=>download_task(&app,&shared,id.as_str(),"download",""),
        "task-import"=>download_task(&app,&shared,id.as_str(),"import",""),
        "task-canvas"=>{
            let is_image=shared.borrow().tasks.iter().find(|v|text(v,"id")==id.as_str()).is_some_and(|v|text(v,"kind")=="image");
            if !is_image {ui.set_error("只有图片生成结果可以打开到画布".into());return;}
            download_task(&app,&shared,id.as_str(),"canvas","");
        },
        "task-save-team"=>{if team.is_empty(){ui.set_error("请先选择要保存到的团队".into());}else{ui.set_notice("".into());download_task(&app,&shared,id.as_str(),"save-team",&team);}},
        "asset-download"|"asset-reference"|"asset-import"|"asset-canvas"=>{
            if name=="asset-reference" {
                let is_motion=shared.borrow().assets.iter().find(|v|text(v,"id")==id.as_str()).is_some_and(|v|text(v,"content_type").starts_with("video/") || text(v,"content_type").starts_with("audio/"));
                if is_motion && ui.get_mode()==0 {ui.set_mode(1);ui.set_model_index(0);update_model_options(&app,&shared);}
                if shared.borrow().references.len()>=ui.get_reference_max().max(0) as usize {ui.set_error(format!("当前模型最多支持 {} 项参考素材",ui.get_reference_max()).into());return;}
                ui.set_asset_picker_open(false);ui.set_page(1);
            }
            if name=="asset-import" {ui.set_asset_picker_open(false);}
            if name=="asset-canvas" {
                let is_image=shared.borrow().assets.iter().find(|v|text(v,"id")==id.as_str()).is_some_and(|v|text(v,"content_type").starts_with("image/"));
                if !is_image {ui.set_error("只有图片素材可以打开到画布".into());return;}
            }
            download_asset(&app,&shared,id.as_str(),match name.as_str(){"asset-reference"=>"reference","asset-import"=>"import","asset-download"=>"download","asset-canvas"=>"canvas",_=>"preview"});
        },
        "local-folder-browse"=>{#[cfg(not(any(target_os="ios",target_os="android")))]if let Some(folder)=rfd::FileDialog::new().pick_folder(){ui.set_local_folder(folder.to_string_lossy().into_owned().into());shared.borrow_mut().folder=folder;save_preferences(&app,&shared);}},
        _=>{}
    }});
    let weak = app.as_weak();
    let shared = state.clone();
    app.global::<SeeCut>().on_changed(move |field, _| {
        let Some(app) = weak.upgrade() else { return };
        if shared.borrow().updating_model_options {
            return;
        }
        if field == "reduced-motion" {
            save_preferences(&app, &shared);
        }
        if field == "mode" {
            update_model_options(&app, &shared);
        }
        if field == "model" {
            update_model_options(&app, &shared);
        }
        if field == "prompt" || field == "prompt-cursor" {
            update_mention(&app);
        }
        if matches!(
            field.as_str(),
            "mode"
                | "model"
                | "prompt"
                | "ratio"
                | "resolution"
                | "quality"
                | "duration"
                | "generate-audio"
                | "quantity"
        ) && app.global::<SeeCut>().get_signed_in()
        {
            refresh_quote(&app, &shared);
        }
        if field == "team" {
            let index = app.global::<SeeCut>().get_team_index();
            let owner = shared
                .borrow()
                .teams
                .get(index.max(0) as usize)
                .is_some_and(|v| text(v, "role") == "owner");
            app.global::<SeeCut>().set_team_owner(owner);
            app.global::<SeeCut>().set_invite_url("".into());
            shared.borrow_mut().invite_id.clear();
            refresh_assets(&app, &shared);
        }
        if matches!(
            field.as_str(),
            "asset-search"
                | "asset-filter"
                | "asset-picker-team-search"
                | "asset-picker-team-filter"
                | "asset-picker-purpose"
        ) {
            render_assets(&app, &shared);
        }
        if field == "asset-picker-purpose" {
            sync_picker_selection(&app.global::<SeeCut>(), &shared);
            render_personal(&app, &shared);
        }
        if field == "team"
            && app.global::<SeeCut>().get_asset_picker_open()
            && app.global::<SeeCut>().get_asset_picker_source() == 1
        {
            sync_picker_selection(&app.global::<SeeCut>(), &shared);
            render_assets(&app, &shared);
        }
        if field.starts_with("personal-") || field == "asset-picker-personal-filter" {
            // Managing the library is an explicit mode, including with zero
            // selected rows. A new view or mode starts with a fresh selection.
            if matches!(
                field.as_str(),
                "personal-selection-mode"
                    | "personal-trash"
                    | "personal-folder-filter"
                    | "personal-search"
                    | "personal-filter"
                    | "personal-source"
                    | "personal-favorites"
            ) {
                shared.borrow_mut().selected_personal.clear();
            }
            render_personal(&app, &shared);
        }
    });
    refresh_canvas_projects(app, &state);
    auth_countdown(app.as_weak());
    heartbeat(app.as_weak(), state);
}
fn heartbeat(weak: slint::Weak<App>, state: Rc<RefCell<Cloud>>) {
    slint::Timer::single_shot(Duration::from_secs(5), move || {
        let Some(app) = weak.upgrade() else { return };
        app.global::<SeeCut>().set_project_open(!app.get_on_start());
        if app.global::<SeeCut>().get_signed_in()
            && !app.global::<SeeCut>().get_busy()
            && app.global::<SeeCut>().get_page() == 1
        {
            job(
                &app,
                &state,
                "tasks".into(),
                request("GET", "/api/generation/tasks", Value::Null),
            );
        }
        heartbeat(weak, state);
    });
}

#[cfg(test)]
mod reference_tests {
    use super::accept_asset_list;
    use super::{
        Cloud, DEFAULT_API_URL, ExportCopyItem, ExportCopyJob, NavigationDecision, PickerSelection,
        TaskInputSnapshot, auth_request, auth_return_target, can_start_request,
        cancel_pending_imports, configured_api_url, copy_export_call, copy_unique_export,
        enqueue_pending_import, enqueue_personal_reference, has_reference_path,
        invalid_reference_prompt, is_background_job, is_cloud_identity_error, mention_start,
        navigation_decision, option_index, parameter_default_index, parameter_label,
        parameter_value, personal_drop_plan, picker_download_request, reference_number, request,
        request_requires_auth, resume_pending_imports, rewrite_mentions, selected_task_id,
        selected_task_index, server_task_snapshot, set_picker_batch_path,
        should_clear_identity_error_for_target, should_surface_job_error, stable_model_index,
        task_item, task_reference_source, validate_configuration_references,
    };
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn repeat_export_keeps_every_existing_file_and_uses_display_name() {
        let root = std::env::temp_dir().join(format!("seecut-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.png");
        std::fs::write(&source, b"new bytes").unwrap();
        for name in ["产品正面.png", "产品正面-id.png", "产品正面-id-1.png"] {
            std::fs::write(root.join(name), b"existing bytes").unwrap();
        }
        let exported = copy_unique_export(&source, &root, "产品正面", "id").unwrap();
        assert_eq!(exported.file_name().unwrap(), "产品正面-id-2.png");
        for name in ["产品正面.png", "产品正面-id.png", "产品正面-id-1.png"] {
            assert_eq!(std::fs::read(root.join(name)).unwrap(), b"existing bytes");
        }
        assert_eq!(std::fs::read(&exported).unwrap(), b"new bytes");
        let another = copy_unique_export(&source, &root, "产品正面", "id").unwrap();
        assert_eq!(another.file_name().unwrap(), "产品正面-id-3.png");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn batch_copy_reports_partial_result_for_retry_without_recopying_completed_item() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64},
        };
        let root =
            std::env::temp_dir().join(format!("seecut-export-batch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.png");
        std::fs::write(&source, b"ready").unwrap();
        let first = ExportCopyItem {
            id: "first".into(),
            source: source.clone(),
            name: "first".into(),
        };
        let second = ExportCopyItem {
            id: "second".into(),
            source: root.join("missing.png"),
            name: "second".into(),
        };
        let mut cloud = Cloud {
            export_copy: Some(ExportCopyJob {
                items: vec![first, second],
                folder: root.clone(),
                completed: Default::default(),
                running: true,
                cancel: Arc::new(AtomicBool::new(false)),
                bytes_done: Arc::new(AtomicU64::new(0)),
                bytes_total: 5,
            }),
            ..Default::default()
        };
        let partial = copy_export_call(&cloud).unwrap();
        assert_eq!(partial["completed"], json!(["first"]));
        assert!(partial["error"].as_str().is_some());
        cloud
            .export_copy
            .as_mut()
            .unwrap()
            .completed
            .insert("first".into());
        std::fs::write(root.join("missing.png"), b"recovered").unwrap();
        let retry = copy_export_call(&cloud).unwrap();
        assert_eq!(retry["completed"], json!(["second"]));
        assert_eq!(std::fs::read(root.join("first.png")).unwrap(), b"ready");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_snapshot_round_trip_preserves_ordered_local_references() {
        let snapshot = TaskInputSnapshot {
            mode: 1,
            prompt: "让 @[视频1] 跟随 @[图片1]".into(),
            model_id: "video-model".into(),
            parameters: json!({"duration": 8, "resolution": "720p"}),
            references: vec![
                crate::generation_templates::TemplateReference {
                    path: "/tmp/first.mp4".into(),
                    name: "first.mp4".into(),
                    kind: "video".into(),
                    client_id: "first".into(),
                    source_id: "asset-first".into(),
                },
                crate::generation_templates::TemplateReference {
                    path: "/tmp/second.png".into(),
                    name: "second.png".into(),
                    kind: "image".into(),
                    client_id: "second".into(),
                    source_id: "asset-second".into(),
                },
            ],
        };
        let restored: TaskInputSnapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(restored.prompt, snapshot.prompt);
        assert_eq!(restored.references, snapshot.references);
    }

    #[test]
    fn server_task_refill_recovers_known_input_and_keeps_missing_reference_slots() {
        let plain = json!({
            "kind":"image",
            "model":"image-model",
            "request":{"model":"image-model","prompt":"studio","size":"1024x1024","quality":"high"}
        });
        let restored = server_task_snapshot(&plain).unwrap();
        assert_eq!(restored.prompt, "studio");
        assert_eq!(restored.parameters["size"], "1024x1024");

        let referenced = json!({
            "kind":"video",
            "model":"video-model",
            "request":{"model":"video-model","prompt":"motion","reference_asset_ids":["gasset-1"]}
        });
        let restored = server_task_snapshot(&referenced).unwrap();
        assert_eq!(restored.prompt, "motion");
        assert_eq!(restored.references.len(), 1);
        assert_eq!(restored.references[0].source_id, "gasset-1");
        assert_eq!(restored.references[0].kind, "unknown");
        assert!(restored.references[0].path.as_os_str().is_empty());

        let incomplete = json!({
            "kind":"video",
            "model":"video-model",
            "request":{
                "model":"video-model",
                "prompt":"motion",
                "resolution":"720p",
                "duration":5,
                "aspect_ratio":"16:9"
            }
        });
        let restored = server_task_snapshot(&incomplete).unwrap();
        assert_eq!(restored.prompt, "motion");
        assert_eq!(restored.parameters["duration"], 5);
        assert!(restored.parameters.get("generate_audio").is_none());
    }

    #[test]
    fn refill_reference_validation_rejects_duplicates_kind_limits_and_audio_only() {
        use crate::generation_templates::TemplateReference;
        let model = json!({"parameters":{"reference_asset_ids":{
            "max_items":9,
            "accepted_media":["image/*","video/*","audio/*"],
            "max_per_kind":{"image":9,"video":1,"audio":3}
        }}});
        let reference = |path: &str, kind: &str| TemplateReference {
            path: path.into(),
            name: path.into(),
            kind: kind.into(),
            client_id: String::new(),
            source_id: String::new(),
        };
        let duplicate = vec![
            reference("/tmp/a.png", "image"),
            reference("/tmp/a.png", "image"),
        ];
        assert!(
            validate_configuration_references(&model, &duplicate, "历史任务")
                .unwrap_err()
                .contains("重复")
        );
        let videos = vec![
            reference("/tmp/a.mp4", "video"),
            reference("/tmp/b.mp4", "video"),
        ];
        assert_eq!(
            validate_configuration_references(&model, &videos, "历史任务").unwrap_err(),
            "历史任务中的视频参考最多 1 项"
        );
        assert_eq!(
            validate_configuration_references(
                &model,
                &[reference("/tmp/a.wav", "audio")],
                "历史任务"
            )
            .unwrap_err(),
            "历史任务中的参考音频需要搭配至少一张图片或一段视频"
        );
    }

    #[test]
    fn selected_task_tracks_id_across_refresh_and_closes_when_missing() {
        let old = vec![json!({"id":"a"}), json!({"id":"b"})];
        let reordered = vec![json!({"id":"b"}), json!({"id":"a"})];
        let selected = selected_task_id(&old, 1).unwrap();
        assert_eq!(selected, "b");
        assert_eq!(selected_task_index(&reordered, &selected), Some(0));
        assert_eq!(selected_task_index(&[json!({"id":"a"})], &selected), None);
    }

    #[test]
    fn expired_task_can_reference_real_local_result_but_missing_result_is_rejected() {
        let root = std::env::temp_dir().join(format!("seecut-result-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let result = root.join("result.png");
        std::fs::write(&result, b"image").unwrap();
        let expired = json!({"kind":"image","status":"expired","outputs":[]});
        assert_eq!(
            task_reference_source(&expired, result.to_str()).unwrap(),
            Some(result.to_string_lossy().into_owned())
        );
        assert!(
            task_reference_source(&expired, Some("/missing/result.png"))
                .unwrap_err()
                .contains("本机结果文件不可用")
        );
        let succeeded_without_output = json!({"kind":"image","status":"succeeded","outputs":[]});
        assert_eq!(
            task_reference_source(&succeeded_without_output, None).unwrap_err(),
            "该生成结果文件不可用，请刷新后重试"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_reference_path_is_idempotent() {
        let references = vec![json!({"local_path":"/tmp/result.png"})];
        assert!(has_reference_path(&references, "/tmp/result.png"));
        assert!(!has_reference_path(&references, "/tmp/other.png"));
    }

    #[test]
    fn personal_folder_drop_uses_selected_group_only_when_source_is_selected() {
        let mut cloud = Cloud {
            personal: vec![
                json!({"id":"a","folder_id":null,"trashed":false}),
                json!({"id":"b","folder_id":"first","trashed":false}),
                json!({"id":"c","folder_id":"second","trashed":false}),
            ],
            personal_folders: vec![json!({"id":"first"}), json!({"id":"second"})],
            ..Default::default()
        };
        cloud.selected_personal.extend(["a".into(), "b".into()]);
        assert_eq!(
            personal_drop_plan(&cloud, "a", 2),
            Some((vec!["a".into(), "b".into()], "first".into()))
        );
        assert_eq!(
            personal_drop_plan(&cloud, "c", 2),
            Some((vec!["c".into()], "first".into()))
        );
        assert_eq!(personal_drop_plan(&cloud, "c", 0), None);
        assert_eq!(
            personal_drop_plan(&cloud, "c", 1),
            Some((vec!["c".into()], "".into()))
        );
        assert_eq!(personal_drop_plan(&cloud, "c", 3), None);
        cloud.personal[2]["trashed"] = json!(true);
        assert_eq!(personal_drop_plan(&cloud, "c", 2), None);
        assert_eq!(personal_drop_plan(&cloud, "missing", 2), None);
    }

    #[test]
    fn personal_library_bridge_recovers_results_and_preserves_trash_without_login() {
        let root =
            std::env::temp_dir().join(format!("seecut-library-bridge-{}", uuid::Uuid::new_v4()));
        let output = root.join("results");
        let library = root.join("library");
        std::fs::create_dir_all(&output).unwrap();
        let path = output.join("gen_existing.png");
        let mut encoder = png::Encoder::new(std::fs::File::create(&path).unwrap(), 4, 4);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[160; 48])
            .unwrap();
        let cloud = super::Cloud {
            folder: output,
            ..Default::default()
        };
        let run = |operation: &str, body| {
            super::library_call_at(
                &cloud,
                &super::request("POST", format!("local:library-{operation}"), body),
                &library,
            )
            .unwrap()
        };
        let first = run("list", serde_json::Value::Null);
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        assert_eq!(first["items"][0]["available"], true);
        assert_eq!(first["items"][0]["source"], "generated");
        assert!(std::path::Path::new(first["items"][0]["thumbnail"].as_str().unwrap()).is_file());
        let id = first["items"][0]["id"].clone();
        run("rename", json!({"id":id,"name":"已完成图片"}));
        run("trash", json!({"id":id}));
        let refreshed = run("list", serde_json::Value::Null);
        assert_eq!(refreshed["items"].as_array().unwrap().len(), 1);
        assert_eq!(refreshed["items"][0]["trashed"], true);
        assert_eq!(refreshed["items"][0]["name"], "已完成图片");
        assert!(path.exists());
        let restored = run("restore", json!({"id":id}));
        assert_eq!(restored["items"][0]["trashed"], false);
        std::fs::remove_file(path).unwrap();
        let missing = run("list", serde_json::Value::Null);
        assert_eq!(missing["items"][0]["available"], false);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn canvas_registration_and_import_share_one_serial_manifest() {
        let root =
            std::env::temp_dir().join(format!("seecut-library-serial-{}", uuid::Uuid::new_v4()));
        let output = root.join("results");
        let library = root.join("library");
        let sources = root.join("sources");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::create_dir_all(&sources).unwrap();
        let canvas = output.join("canvas.png");
        let imported = sources.join("imported.png");
        std::fs::write(&canvas, b"canvas").unwrap();
        std::fs::write(&imported, b"imported").unwrap();
        let cloud = super::Cloud {
            folder: output,
            ..Default::default()
        };
        let call = |operation: &str, body| {
            super::library_call_at(
                &cloud,
                &super::request("POST", format!("local:library-{operation}"), body),
                &library,
            )
            .unwrap()
        };
        call("import", json!({"path":imported}));
        let result = call("register", json!({"path":canvas}));
        assert_eq!(result["items"].as_array().unwrap().len(), 2);
        assert!(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["source"] == "imported")
        );
        assert!(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["source"] == "generated")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_launch_uses_online_service_and_development_requires_override() {
        assert_eq!(configured_api_url(None), DEFAULT_API_URL);
        assert_eq!(configured_api_url(Some("invalid")), DEFAULT_API_URL);
        assert_eq!(
            configured_api_url(Some("http://remote.example.com")),
            DEFAULT_API_URL
        );
        assert_eq!(
            configured_api_url(Some("http://127.0.0.1:8797")),
            "http://127.0.0.1:8797"
        );
    }

    #[test]
    fn signed_out_navigation_keeps_the_current_page_and_returns_after_login() {
        assert_eq!(
            navigation_decision(1, 2, false),
            NavigationDecision::Authenticate { stay: 1, target: 2 }
        );
        assert_eq!(
            navigation_decision(1, 5, false),
            NavigationDecision::Open(5)
        );
        assert_eq!(navigation_decision(1, 3, true), NavigationDecision::Open(3));
        assert_eq!(auth_return_target(Some(2), 1), 2);
        assert_eq!(auth_return_target(None, 1), 1);
    }

    #[test]
    fn refresh_jobs_do_not_present_as_foreground_work() {
        for name in [
            "tasks",
            "quote",
            "wallet",
            "capabilities",
            "models",
            "teams",
            "assets",
            "local-cache:gen_1",
            "personal-cache",
        ] {
            assert!(is_background_job(name), "{name}");
        }
        for name in ["generate", "local", "asset-change", "personal"] {
            assert!(!is_background_job(name), "{name}");
        }
    }

    #[test]
    fn signed_out_cloud_requests_stop_before_authenticated_endpoints() {
        let models = request("GET", "/api/generation/models", serde_json::Value::Null);
        let assets = request("GET", "/api/teams/team-1/assets", serde_json::Value::Null);
        let capabilities = request("GET", "/api/capabilities", serde_json::Value::Null);
        let login = request("POST", "/api/auth/login", json!({}));
        let personal = request("POST", "local:library-list", serde_json::Value::Null);

        assert!(request_requires_auth(&models));
        assert!(request_requires_auth(&assets));
        assert!(!request_requires_auth(&capabilities));
        assert!(!request_requires_auth(&login));
        assert!(!request_requires_auth(&personal));
        assert!(!can_start_request(false, &models));
        assert!(can_start_request(true, &models));
    }

    #[test]
    fn stale_unauthenticated_errors_do_not_escape_to_local_workflows() {
        assert!(!should_surface_job_error(
            false, false, true, "models", true,
        ));
        assert!(!should_surface_job_error(
            false, true, false, "assets", true,
        ));
        assert!(should_surface_job_error(false, true, false, "login", true,));
        assert!(should_surface_job_error(
            false, false, true, "personal", false,
        ));
        assert!(is_cloud_identity_error("请先登录"));
        assert!(is_cloud_identity_error("登录状态已失效，请重新登录"));
        assert!(!is_cloud_identity_error("无法读取导入素材"));
        assert!(should_clear_identity_error_for_target(0, "请先登录"));
        assert!(should_clear_identity_error_for_target(5, "请先登录"));
        assert!(!should_clear_identity_error_for_target(1, "请先登录"));
        assert!(!should_clear_identity_error_for_target(
            0,
            "无法读取导入素材",
        ));
    }

    #[test]
    fn pending_project_imports_dedupe_cancel_and_resume_once() {
        let first = std::path::PathBuf::from("/tmp/first.png");
        let second = std::path::PathBuf::from("/tmp/second.mp4");
        let mut pending = Vec::new();
        assert!(enqueue_pending_import(&mut pending, first.clone()));
        assert!(!enqueue_pending_import(&mut pending, first.clone()));
        assert!(enqueue_pending_import(&mut pending, second.clone()));
        assert_eq!(pending.len(), 2);

        let resumed = resume_pending_imports(&mut pending);
        assert_eq!(resumed, vec![first.clone(), second.clone()]);
        assert!(pending.is_empty());
        assert!(resume_pending_imports(&mut pending).is_empty());

        assert!(enqueue_pending_import(&mut pending, first));
        assert!(enqueue_pending_import(&mut pending, second));
        assert_eq!(cancel_pending_imports(&mut pending), 2);
        assert!(pending.is_empty());
    }

    #[test]
    fn signed_out_reference_intents_keep_multiple_unique_files() {
        let mut pending = std::collections::VecDeque::new();
        assert!(enqueue_personal_reference(
            &mut pending,
            "/tmp/first.png".into(),
            "first.png".into(),
        ));
        assert!(enqueue_personal_reference(
            &mut pending,
            "/tmp/second.png".into(),
            "second.png".into(),
        ));
        assert!(!enqueue_personal_reference(
            &mut pending,
            "/tmp/first.png".into(),
            "duplicate.png".into(),
        ));
        assert_eq!(pending.len(), 2);
        assert_eq!(pending.pop_front().unwrap().1, "first.png");
        assert_eq!(pending.pop_front().unwrap().1, "second.png");
    }

    #[test]
    fn email_code_requests_are_bound_to_email_and_keep_leading_zeroes() {
        let (name, req) = auth_request(3, " User@Example.com ", "", "", " 001234 ").unwrap();
        assert_eq!(name, "verify-email");
        assert_eq!(
            req.body,
            json!({"email":"user@example.com","token":"001234"})
        );
        let (_, req) = auth_request(
            4,
            "user@example.com",
            "new-password",
            "new-password",
            "001234",
        )
        .unwrap();
        assert_eq!(req.path, "/api/auth/reset-password");
        assert_eq!(req.body["email"], "user@example.com");
        assert_eq!(req.body["password"], "new-password");
        assert!(auth_request(3, "user@example.com", "", "", "12345").is_err());
    }

    #[test]
    fn account_forms_reject_mismatched_passwords_and_invalid_email() {
        assert!(
            auth_request(1, "user@example.com", "long-password", "other-password", "").is_err()
        );
        assert!(auth_request(1, "user@example.com", "short", "short", "").is_err());
        assert!(auth_request(2, "wrong address", "", "", "").is_err());
        assert!(auth_request(0, "user@example.com", "existing", "", "").is_ok());
        let (name, req) = auth_request(2, "user@example.com", "", "", "").unwrap();
        assert_eq!(name, "password-reset-request");
        assert_eq!(req.body, json!({"email":"user@example.com"}));
    }

    #[test]
    fn friendly_image_labels_keep_exact_catalog_request_values() {
        let model = json!({"parameters": {
            "size": {"default":"auto", "values":["auto", "1024x1024", "1536x1024", "1024x1536"]},
            "quality": {"default":"high", "values":["high"]}
        }});
        for (index, raw, label) in [
            (0, "auto", "自动"),
            (1, "1024x1024", "1:1 · 1024 × 1024"),
            (2, "1536x1024", "3:2 · 1536 × 1024"),
            (3, "1024x1536", "2:3 · 1024 × 1536"),
        ] {
            let value = parameter_value(&model, "size", index);
            assert_eq!(value, json!(raw));
            assert_eq!(parameter_label("size", &value), label);
        }
        assert_eq!(parameter_value(&model, "quality", 0), json!("high"));
        assert_eq!(
            parameter_label("quality", &parameter_value(&model, "quality", 0)),
            "高清"
        );
        assert_eq!(parameter_default_index(&model, "quality"), 0);
    }

    #[test]
    fn video_parameters_keep_numbers_and_reject_non_catalog_options() {
        let model = json!({"parameters": {
            "duration": {"default":5,"values":[4,5,6,7,8,9,10,11,12,13,14,15]},
            "resolution": {"values":["720p"]},
            "aspect_ratio": {"values":["16:9","9:16","1:1"]}
        }});
        assert_eq!(parameter_value(&model, "duration", 11), json!(15));
        assert_eq!(
            parameter_label("duration", &parameter_value(&model, "duration", 11)),
            "15 秒"
        );
        assert_eq!(parameter_default_index(&model, "duration"), 1);
        assert_eq!(parameter_value(&model, "resolution", 0), json!("720p"));
        assert_eq!(
            parameter_value(&model, "resolution", 1),
            serde_json::Value::Null
        );
        assert_eq!(
            parameter_value(&model, "duration", -1),
            serde_json::Value::Null
        );
        assert_eq!(parameter_value(&model, "aspect_ratio", 1), json!("9:16"));
    }

    #[test]
    fn model_catalog_reorder_uses_stable_id_and_keeps_raw_parameter_value() {
        let models = [
            json!({"id":"image-a","kind":"image","parameters":{"size":{"default":"1:1","values":["1:1","3:2"]}}}),
            json!({"id":"image-b","kind":"image","parameters":{"size":{"default":"1:1","values":["1:1","3:2"]}}}),
        ];
        let reordered = vec![models[1].clone(), models[0].clone()];
        assert_eq!(stable_model_index(&reordered, 0, "image-a"), Some(1));
        let (index, adjusted) = option_index(&reordered[1], "size", Some(&json!("3:2")));
        assert_eq!(index, 1);
        assert!(!adjusted);
        let (fallback, adjusted) = option_index(&reordered[1], "size", Some(&json!("9:16")));
        assert_eq!(fallback, 0);
        assert!(adjusted);
    }

    #[test]
    fn image_and_video_catalog_indexes_are_kept_in_separate_namespaces() {
        let models = vec![
            json!({"id":"image-a","kind":"image"}),
            json!({"id":"video-a","kind":"video"}),
            json!({"id":"image-b","kind":"image"}),
        ];
        assert_eq!(stable_model_index(&models, 0, "image-b"), Some(1));
        assert_eq!(stable_model_index(&models, 1, "video-a"), Some(0));
        assert_eq!(stable_model_index(&models, 1, "image-b"), None);
    }

    #[test]
    fn failed_task_exposes_raw_failure_state_and_a_fallback_reason() {
        let item = task_item(&json!({"id":"task-1","kind":"image","status":"failed"}));
        assert!(item.failed);
        assert_eq!(item.detail.as_str(), "未返回失败原因");
    }

    #[test]
    fn deleted_and_out_of_range_references_block_generation() {
        assert!(invalid_reference_prompt("保持[已移除参考图片]的主体", 2));
        assert!(invalid_reference_prompt("@[图片3]", 2));
        assert!(invalid_reference_prompt("@[图片0]", 2));
        assert!(invalid_reference_prompt("@[图片1]", 0));
        assert!(!invalid_reference_prompt(
            "@[图片2]中的衣服与@[图片1]中的背景",
            2
        ));
    }

    #[test]
    fn provider_prompt_keeps_reference_numbers_and_chinese_context() {
        let result = rewrite_mentions("沿用@[图片2]的衣服，保持@[图片1]的脸", |n| {
            format!("第{n}张参考图片")
        });
        assert_eq!(result, "沿用第2张参考图片的衣服，保持第1张参考图片的脸");
    }

    #[test]
    fn mixed_reference_numbers_follow_each_media_array() {
        let references = vec![
            json!({"kind":"image"}),
            json!({"kind":"video"}),
            json!({"kind":"image"}),
            json!({"kind":"audio"}),
            json!({"kind":"video"}),
        ];
        let numbers: Vec<_> = (0..references.len())
            .map(|index| super::reference_number(&references, index))
            .collect();
        assert_eq!(numbers, [1, 1, 2, 1, 2]);
        let prompt = "用@[图片2]主体，按@[视频2]运动，跟随@[音频1]节奏";
        assert_eq!(
            super::rewrite_media_mentions(prompt, |kind, n| format!("{kind}{n}")),
            "用图片2主体，按视频2运动，跟随音频1节奏"
        );
        let removed = super::rewrite_media_mentions(prompt, |kind, n| {
            if kind == "视频" {
                format!("@[{kind}{}]", n - 1)
            } else {
                format!("@[{kind}{n}]")
            }
        });
        assert_eq!(removed, "用@[图片2]主体，按@[视频1]运动，跟随@[音频1]节奏");
        assert_eq!(
            super::rewrite_media_mentions("@[视频x] @[音频] @hello", |_, _| panic!(
                "invalid token"
            )),
            "@[视频x] @[音频] @hello"
        );
    }

    #[test]
    fn reference_types_follow_model_catalog() {
        let image = json!({"parameters":{"reference_asset_ids":{"accepted_media":["image/*"]}}});
        let video = json!({"parameters":{"reference_asset_ids":{"accepted_media":["image/*","video/*","audio/*"]}}});
        for (path, kind) in [
            ("A.PNG", "image"),
            ("clip.MOV", "video"),
            ("beat.wav", "audio"),
            ("movie.webm", "unsupported"),
        ] {
            assert_eq!(super::reference_kind(std::path::Path::new(path)), kind);
            assert_eq!(super::accepts_reference(&image, kind), kind == "image");
            assert_eq!(
                super::accepts_reference(&video, kind),
                kind != "unsupported"
            );
        }
    }

    #[test]
    fn removing_reference_does_not_cascade_number_replacements() {
        let result = rewrite_mentions("@[图片1] @[图片2] @[图片3] @[图片10]", |n| {
            if n == 1 {
                "[已移除参考图片]".into()
            } else {
                format!("@[图片{}]", n - 1)
            }
        });
        assert_eq!(result, "[已移除参考图片] @[图片1] @[图片2] @[图片9]");
    }

    #[test]
    fn missing_middle_reference_keeps_kind_number_for_later_mentions() {
        let references = vec![
            json!({"kind":"image"}),
            json!({"kind":"image", "status":"missing"}),
            json!({"kind":"image"}),
        ];
        let numbers: Vec<_> = (0..references.len())
            .map(|index| reference_number(&references, index))
            .collect();
        assert_eq!(numbers, [1, 2, 3]);
        assert_eq!(
            rewrite_mentions("主体@[图片2]，背景@[图片3]", |number| {
                format!("第{number}张参考图片")
            }),
            "主体第2张参考图片，背景第3张参考图片"
        );
    }

    #[test]
    fn mention_detection_uses_utf8_cursor_and_ignores_complete_tokens() {
        let prompt = "参考@[图片1]，搭配@图 后文";
        assert_eq!(
            mention_start(prompt, "参考@[图片1]，搭配@图".len()),
            Some("参考@[图片1]，搭配".len())
        );
        assert_eq!(mention_start(prompt, "参考@[图片1]".len()), None);
        assert_eq!(mention_start(prompt, prompt.len()), None);
        assert_eq!(mention_start(prompt, 1), None);
    }
    #[test]
    fn picker_batch_request_keeps_confirmed_team_and_destination() {
        let item = PickerSelection {
            id: "asset_a".into(),
            name: "original.png".into(),
            kind: "image".into(),
            local_path: String::new(),
            download_endpoint: "/api/teams/original/assets/asset_a/download".into(),
            download_path: PathBuf::from("/tmp/original/asset_a.png"),
        };
        let mut cloud = Cloud {
            picker_batch: Some(("reference".into(), vec![item])),
            teams: vec![json!({"id":"other-team"})],
            ..Cloud::default()
        };
        cloud.assets.clear();
        cloud.folder = PathBuf::from("/tmp/other");
        let req = picker_download_request(&cloud.picker_batch.as_ref().unwrap().1[0]).unwrap();
        assert_eq!(
            req.body["endpoint"],
            "/api/teams/original/assets/asset_a/download"
        );
        assert_eq!(req.body["path"], "/tmp/original/asset_a.png");
        assert_eq!(req.body["intent"], "picker-batch");
    }

    #[test]
    fn picker_batch_cache_hit_completes_matching_slot_without_reordering() {
        let make = |id: &str| PickerSelection {
            id: id.into(),
            name: format!("{id}.png"),
            kind: "image".into(),
            local_path: String::new(),
            download_endpoint: format!("/api/teams/t/assets/{id}/download"),
            download_path: PathBuf::from(format!("/tmp/{id}.png")),
        };
        let mut cloud = Cloud {
            picker_batch: Some(("reference".into(), vec![make("first"), make("second")])),
            ..Cloud::default()
        };
        assert!(set_picker_batch_path(
            &mut cloud,
            "first",
            "/tmp/cached.png"
        ));
        let items = &cloud.picker_batch.as_ref().unwrap().1;
        assert_eq!(items[0].id, "first");
        assert_eq!(items[0].local_path, "/tmp/cached.png");
        assert_eq!(
            items
                .iter()
                .find(|item| item.local_path.is_empty())
                .unwrap()
                .id,
            "second"
        );
        assert!(!set_picker_batch_path(
            &mut cloud,
            "unrelated",
            "/tmp/other.png"
        ));
    }
    #[test]
    fn team_asset_list_rejects_late_team_and_trash_responses() {
        let mut cloud = Cloud::default();
        let current = "/api/teams/new/assets?trash=false";
        cloud.assets_context = current.into();
        for old in [
            "/api/teams/old/assets?trash=false",
            "/api/teams/new/assets?trash=true",
        ] {
            assert!(!accept_asset_list(
                &mut cloud,
                &json!({
                    "client_assets_context": old, "items": [{"id":"old-asset"}]
                }),
                current
            ));
            assert!(cloud.assets.is_empty());
        }
        assert!(accept_asset_list(
            &mut cloud,
            &json!({
                "client_assets_context": current, "items": [{"id":"new-asset"}]
            }),
            current
        ));
        assert_eq!(cloud.assets[0]["id"], "new-asset");
    }
}
