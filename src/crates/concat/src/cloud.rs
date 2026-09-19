// SPDX-License-Identifier: AGPL-3.0-or-later
//! SeeCut's non-blocking desktop bridge. Provider credentials never enter this process.

use crate::ui::{AccountEntry, App, CloudItem, CreditPlan, SeeCut};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    net::IpAddr,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use url::Url;

const DEFAULT_API_URL: &str = "https://seecut.stormycry.cloud";

struct ClientError {
    code: String,
    message: String,
    retry_after: i32,
}

impl From<String> for ClientError {
    fn from(message: String) -> Self {
        Self {
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
    tasks: Vec<Value>,
    quote_id: String,
    quote_credits: Option<i64>,
    quote_body: Value,
    pending: VecDeque<(String, Request)>,
    assets: Vec<Value>,
    personal: Vec<Value>,
    pending_personal_reference: Option<(String, String)>,
    references: Vec<Value>,
    local: HashMap<String, String>,
    folder: PathBuf,
    history_name: String,
    invite_id: String,
    orders: Vec<Value>,
    submission: Option<(Value, String)>,
    epoch: u64,
    active_name: String,
    download_attempts: std::collections::HashSet<String>,
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
    if req.path.starts_with("local:library-") {
        return library_call(c, req).map_err(Into::into);
    }
    if req.path == "local:upload" {
        let path = PathBuf::from(text(&req.body, "path"));
        let team = text(&req.body, "team");
        let purpose = text(&req.body, "purpose");
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
        return Ok(json!({"id":req.body["id"],"path":path,"intent":req.body["intent"]}));
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
                code: error_code,
                message,
                retry_after: body
                    .pointer("/error/retry_after")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .clamp(0, 3600) as i32,
            })
        }
        Err(_) => Err("暂时无法连接 SeeCut，请检查网络后重试".into()),
    }
}

fn job(app: &App, state: &Rc<RefCell<Cloud>>, name: String, req: Request) {
    if app.global::<SeeCut>().get_busy() {
        let mut c = state.borrow_mut();
        if matches!(name.as_str(), "quote" | "tasks" | "wallet") {
            c.pending.retain(|(n, _)| n != &name);
        }
        c.pending.push_back((name, req));
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
    app.global::<SeeCut>().set_busy(true);
    app.global::<SeeCut>().set_error("".into());
    std::thread::spawn(move || {
        let result = call(&snapshot, &req).map(|mut value| {
            if req.path == "/api/generation/quote" {
                value["client_request_body"] = request_body;
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
                    refresh_quote(&app, &state);
                }
                if name == "generate" {
                    refresh_quote(&app, &state);
                }
                ui.set_error(error.message.into());
            }
        }
        let next = state.borrow_mut().pending.pop_front();
        if let Some((name, req)) = next {
            job(&app, &state, name, req)
        }
    });
}

fn selected_model(ui: &SeeCut, state: &Cloud) -> String {
    let index = ui.get_model_index().max(0) as usize;
    state
        .models
        .iter()
        .filter(|m| text(m, "kind") == if ui.get_mode() == 0 { "image" } else { "video" })
        .nth(index)
        .map(|m| text(m, "id"))
        .unwrap_or_default()
}

// Rewrite complete mention tokens in one pass so renumbering cannot cascade.
fn rewrite_mentions(prompt: &str, mut replacement: impl FnMut(usize) -> String) -> String {
    let mut result = String::new();
    let mut rest = prompt;
    while let Some(start) = rest.find("@[图片") {
        result.push_str(&rest[..start]);
        let token = &rest[start..];
        if let Some(end) = token.find(']')
            && let Ok(number) = token["@[图片".len()..end].parse::<usize>()
        {
            result.push_str(&replacement(number));
            rest = &token[end + 1..];
        } else {
            result.push('@');
            rest = &token[1..];
        }
    }
    result.push_str(rest);
    result
}

fn reference_validation(ui: &SeeCut, state: &Cloud) -> String {
    if state
        .references
        .iter()
        .any(|v| text(v, "status") == "expired")
    {
        return "参考图片已过期，请重新上传或移除".into();
    }
    if state.references.len() > ui.get_reference_max().max(0) as usize {
        return format!(
            "当前模型最多支持 {} 张参考图片，请移除多余素材",
            ui.get_reference_max()
        );
    }
    if state
        .references
        .iter()
        .any(|v| text(v, "status") == "failed")
    {
        return "参考图片上传失败，请重试或移除".into();
    }
    if state
        .references
        .iter()
        .any(|v| text(v, "status") != "ready")
    {
        return "参考图片上传中".into();
    }
    if invalid_reference_prompt(&ui.get_prompt(), state.references.len()) {
        "提示词包含已移除或无效的素材引用，请修改后生成".into()
    } else {
        String::new()
    }
}

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
    state.borrow_mut().references.remove(index);
    state
        .borrow_mut()
        .pending
        .retain(|(name, _)| name != &format!("reference:{id}"));
    let ui = app.global::<SeeCut>();
    let prompt = rewrite_mentions(&ui.get_prompt(), |n| {
        if n == index + 1 {
            "[已移除参考图片]".into()
        } else {
            format!("@[图片{}]", if n > index + 1 { n - 1 } else { n })
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
    let token = format!("@[图片{}] ", index + 1);
    prompt.replace_range(start..cursor, &token);
    ui.set_prompt(prompt.into());
    ui.set_prompt_cursor((start + token.len()) as i32);
    ui.set_mention_open(false);
    refresh_quote(app, state);
}

fn generation_body(ui: &SeeCut, state: &Cloud) -> Value {
    let video = ui.get_mode() != 0;
    let prompt = rewrite_mentions(ui.get_prompt().trim(), |number| {
        format!("第{number}张参考图片")
    });
    let mut body =
        json!({"model": selected_model(ui, state), "operation":"generate", "prompt":prompt});
    let model_id = selected_model(ui, state);
    let model = state
        .models
        .iter()
        .find(|model| text(model, "id") == model_id)
        .cloned()
        .unwrap_or_default();
    if video {
        body["resolution"] = parameter_value(&model, "resolution", ui.get_resolution_index());
        body["duration"] = parameter_value(&model, "duration", ui.get_duration_index());
        body["aspect_ratio"] = parameter_value(&model, "aspect_ratio", ui.get_ratio_index());
    } else {
        body["size"] = parameter_value(&model, "size", ui.get_resolution_index());
        body["quality"] = parameter_value(&model, "quality", ui.get_quality_index());
    }
    let references: Vec<_> = state.references.iter().map(|v| v["id"].clone()).collect();
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

fn publish(app: &App, state: &Rc<RefCell<Cloud>>, name: &str, value: Value) {
    let ui = app.global::<SeeCut>();
    match name {
        "personal" => {
            state.borrow_mut().personal = items(&value);
            render_personal(app, state);
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
            clear(app, state);
            state.borrow_mut().token = token;
            ui.set_signed_in(true);
            ui.set_email(ui.get_auth_email());
            ui.set_auth_password("".into());
            ui.set_page(1);
            let uid = text(&value["user"], "id");
            let history_name = format!("{uid}-history.json");
            let history = state.borrow().folder.join(&history_name);
            state.borrow_mut().history_name = history_name;
            state.borrow_mut().local = std::fs::read(history)
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
            refresh_models(app, state);
            job(
                app,
                state,
                "tasks".into(),
                request("GET", "/api/generation/tasks", Value::Null),
            );
        }
        "logout" => clear(app, state),
        "register" => {
            switch_auth(&ui, 3);
            email_notice(app, &value);
            if value["email_delivery"] == "sent" {
                ui.set_auth_resend_seconds(60);
            }
        }
        "verify-email" | "password-reset-confirm" => {
            if name == "password-reset-confirm" {
                clear(app, state);
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
            state.borrow_mut().models = value
                .get("models")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| items(&value));
            update_model_options(app, state);
            let reference = state.borrow_mut().pending_personal_reference.take();
            if let Some((path, name)) = reference {
                upload_personal_reference(app, state, path, name);
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
            state.borrow_mut().submission = None;
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
            state.borrow_mut().tasks = tasks.clone();
            render_tasks(app, state);
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
                    download_task(app, state, &id, "save");
                }
            }
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
            state.borrow_mut().assets = items(&value);
            render_assets(app, state);
        }
        "asset-change" => {
            let path = text(&value, "local_path");
            if !path.is_empty() {
                state.borrow_mut().local.insert(text(&value, "id"), path);
                ui.set_notice("已上传到团队资产库".into());
            }
            refresh_assets(app, state);
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
                *item = value;
                item["display_name"] = display_name;
            }
            render_references(app, state);
            refresh_quote(app, state);
        }
        "local" => {
            let path = text(&value, "path");
            let id = text(&value, "id");
            let generated = id.starts_with("gen_");
            state.borrow_mut().local.insert(id, path.clone());
            let folder = state.borrow().folder.clone();
            let _ = std::fs::create_dir_all(&folder);
            if let Ok(bytes) = serde_json::to_vec(&state.borrow().local) {
                let _ = std::fs::write(folder.join(&state.borrow().history_name), bytes);
            }
            render_tasks(app, state);
            render_assets(app, state);
            match text(&value, "intent").as_str() {
                "preview" => open_file(&path),
                "import" => import_file(app, path.clone()),
                "reference" => upload_file(app, state, path.clone(), "generation_input"),
                _ => {}
            }
            if generated {
                personal_job(app, state, "register", json!({"path":path}));
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
    app.global::<SeeCut>().set_tasks(rows(
        c.tasks
            .iter()
            .map(|v| {
                let mut item = task_item(v);
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
                    }
                }
                item
            })
            .collect(),
    ));
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
    use crate::personal_library::{AssetKind, Library};
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
            library.register_generated(text(&req.body, "path"))?;
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
    let items: Vec<Value> = library
        .items()
        .iter()
        .rev()
        .map(|asset| {
            let mut value = serde_json::to_value(asset).unwrap_or_default();
            let metadata = asset.path.metadata().ok().filter(|m| m.is_file());
            value["available"] = json!(metadata.is_some());
            value["bytes"] = json!(metadata.map(|m| m.len()).unwrap_or(0));
            if !asset.trashed && asset.kind != AssetKind::Audio {
                value["thumbnail"] = json!(crate::personal_library::thumbnail(asset, root));
            }
            value
        })
        .collect();
    Ok(json!({"items":items,"notice":notice}))
}

fn personal_job(app: &App, state: &Rc<RefCell<Cloud>>, operation: &str, body: Value) {
    job(
        app,
        state,
        "personal".into(),
        request("POST", format!("local:library-{operation}"), body),
    );
}

fn render_personal(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let search = ui.get_personal_search().to_lowercase();
    let reference_search = ui.get_personal_reference_search().to_lowercase();
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
    let cloud = state.borrow();
    let row = |v: &Value| CloudItem {
        id: text(v, "id").into(),
        name: text(v, "name").into(),
        detail: format!(
            "{} · {} · {:.1} MB",
            match text(v, "kind").as_str() {
                "image" => "图片",
                "video" => "视频",
                _ => "音频",
            },
            if text(v, "source") == "generated" {
                "生成结果"
            } else {
                "本地导入"
            },
            v["bytes"].as_u64().unwrap_or(0) as f64 / 1_048_576.
        )
        .into(),
        kind: text(v, "kind").into(),
        ready: v["available"].as_bool().unwrap_or(false),
        local: true,
        preview: slint::Image::load_from_path(std::path::Path::new(&text(v, "thumbnail")))
            .unwrap_or_default(),
        ..Default::default()
    };
    ui.set_personal_assets(rows(
        cloud
            .personal
            .iter()
            .filter(|v| v["trashed"].as_bool().unwrap_or(false) == ui.get_personal_trash())
            .filter(|v| text(v, "name").to_lowercase().contains(&search))
            .filter(|v| kind.is_empty() || text(v, "kind") == kind)
            .filter(|v| source.is_empty() || text(v, "source") == source)
            .map(row)
            .collect(),
    ));
    ui.set_personal_references(rows(
        cloud
            .personal
            .iter()
            .filter(|v| {
                !v["trashed"].as_bool().unwrap_or(false)
                    && v["available"] == true
                    && text(v, "kind") == "image"
            })
            .filter(|v| text(v, "name").to_lowercase().contains(&reference_search))
            .filter(|v| {
                PathBuf::from(text(v, "path"))
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(ext.to_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp")
                    })
            })
            .map(row)
            .collect(),
    ));
}

fn personal_action(app: &App, state: &Rc<RefCell<Cloud>>, action: &str, id: &str) {
    let ui = app.global::<SeeCut>();
    match action {
        "personal-refresh" => personal_job(app, state, "list", Value::Null),
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
            }
        }
        "personal-rename" | "personal-trash" | "personal-restore" => personal_job(
            app,
            state,
            action.trim_start_matches("personal-"),
            json!({"id":id,"name":ui.get_personal_rename().trim()}),
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
                "personal-preview" => open_file(&path),
                "personal-project" => import_file(app, path),
                "personal-reference" => {
                    if !ui.get_signed_in() {
                        ui.set_error("登录后可使用生成服务".into());
                        ui.set_page(4);
                        return;
                    }
                    ui.set_page(1);
                    ui.set_asset_picker_open(false);
                    if state.borrow().models.is_empty() {
                        state.borrow_mut().pending_personal_reference =
                            Some((path, text(&asset, "name")));
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

fn render_assets(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let search = ui.get_asset_search().to_lowercase();
    let filter = ui.get_asset_filter();
    let cloud = state.borrow();
    ui.set_assets(rows(
        cloud
            .assets
            .iter()
            .filter(|v| text(v, "filename").to_lowercase().contains(&search))
            .filter(|v| {
                !ui.get_asset_picker_open() || text(v, "content_type").starts_with("image/")
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
    app.global::<SeeCut>().set_references(rows(
        state
            .borrow()
            .references
            .iter()
            .enumerate()
            .map(|(index, v)| {
                let path = PathBuf::from(text(v, "local_path"));
                CloudItem {
                    id: text(v, "client_id").into(),
                    name: text(v, "display_name").into(),
                    kind: "image".into(),
                    detail: format!("@[图片{}]", index + 1).into(),
                    status: match text(v, "status").as_str() {
                        "ready" => "已上传",
                        "failed" => "上传失败",
                        "expired" => "已过期",
                        _ => "上传中",
                    }
                    .into(),
                    local: true,
                    ready: text(v, "status") == "ready",
                    preview: slint::Image::load_from_path(&path).unwrap_or_default(),
                }
            })
            .collect(),
    ));
}
fn refresh_assets(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let id = team_id(&ui, &state.borrow());
    if !id.is_empty() {
        job(
            app,
            state,
            "assets".into(),
            request(
                "GET",
                format!("/api/teams/{id}/assets?trash={}", ui.get_trash_open()),
                Value::Null,
            ),
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
    let ui = app.global::<SeeCut>();
    if !ui.get_signed_in() {
        ui.set_error("请先登录再上传素材".into());
        return;
    }
    let mut client_id = String::new();
    if purpose == "generation_input" {
        let model_id = selected_model(&ui, &state.borrow());
        let max = state
            .borrow()
            .models
            .iter()
            .find(|v| text(v, "id") == model_id)
            .and_then(|v| v["parameters"]["reference_asset_ids"]["max_items"].as_u64())
            .unwrap_or(0) as usize;
        if state.borrow().references.len() >= max {
            ui.set_error(format!("当前模型最多支持 {max} 张参考图").into());
            return;
        }
        if !PathBuf::from(&path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"))
        {
            ui.set_error("当前模型仅支持 PNG、JPG、WebP 参考图片，不支持视频参考".into());
            return;
        }
        if slint::Image::load_from_path(std::path::Path::new(&path)).is_err() {
            ui.set_error("无法读取参考图片，请检查文件是否完整".into());
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
            .push(json!({"client_id":client_id,"local_path":path,"display_name":display_name,"status":"uploading"}));
        render_references(app, state);
        refresh_quote(app, state);
    }
    let team = team_id(&app.global::<SeeCut>(), &state.borrow());
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
fn download_task(app: &App, state: &Rc<RefCell<Cloud>>, id: &str, intent: &str) {
    let c = state.borrow().clone();
    if let Some(path) = c
        .local
        .get(id)
        .filter(|p| std::path::Path::new(p).is_file())
    {
        match intent {
            "preview" => open_file(path),
            "import" => import_file(app, path.clone()),
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
            "local".into(),
            request(
                "GET",
                "local:download",
                json!({"id":id,"url":out["download_url"],"path":path,"intent":intent}),
            ),
        );
    }
}
fn download_asset(app: &App, state: &Rc<RefCell<Cloud>>, id: &str, intent: &str) {
    let c = state.borrow().clone();
    let team = team_id(&app.global::<SeeCut>(), &c);
    if let Some(asset) = c.assets.iter().find(|v| text(v, "id") == id) {
        let filename = text(asset, "filename");
        let ext = std::path::Path::new(&filename)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("bin");
        job(
            app,
            state,
            "local".into(),
            request(
                "GET",
                "local:download",
                json!({"id":id,"endpoint":format!("/api/teams/{team}/assets/{id}/download"),"path":c.folder.join(format!("{id}.{ext}")),"intent":intent}),
            ),
        );
    }
}
fn open_file(path: &str) {
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        let _ = opener::open(path);
    }
}
fn import_file(app: &App, path: String) {
    app.global::<SeeCut>().set_page(0);
    crate::host::on_ui(move |studio, _, _| {
        studio.handle(crate::panes::Msg::Media(
            crate::panes::media_bin::MediaMsg::Import(vec![PathBuf::from(path)]),
        ))
    });
}
fn task_item(v: &Value) -> CloudItem {
    let status = text(v, "status");
    CloudItem {
        id: text(v, "id").into(),
        name: text(v, "prompt").into(),
        detail: if status == "failed" {
            text(&v["error"], "message")
        } else {
            text(v, "model")
        }
        .into(),
        status: match status.as_str() {
            "queued" => "排队中",
            "submitting" | "provider_accepted" | "processing" => "生成中",
            "validating" => "保存结果",
            "succeeded" => "已完成",
            "failed" => "生成失败",
            _ => "待核实",
        }
        .into(),
        kind: text(v, "kind").into(),
        ready: status == "succeeded",
        local: false,
        ..Default::default()
    }
}
fn update_model_options(app: &App, state: &Rc<RefCell<Cloud>>) {
    let ui = app.global::<SeeCut>();
    let video = ui.get_mode() != 0;
    let models: Vec<_> = state
        .borrow()
        .models
        .iter()
        .filter(|m| text(m, "kind") == if video { "video" } else { "image" })
        .cloned()
        .collect();
    ui.set_model_names(strings(
        models
            .iter()
            .map(|m| {
                let label = text(m, "display_name");
                if label.is_empty() {
                    text(m, "id")
                } else {
                    label
                }
            })
            .collect(),
    ));
    ui.set_model_index(if models.is_empty() {
        -1
    } else {
        ui.get_model_index().max(0).min(models.len() as i32 - 1)
    });
    let model = models
        .get(ui.get_model_index().max(0) as usize)
        .cloned()
        .unwrap_or_default();
    let options = |name: &str| {
        model["parameters"][name]["values"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| parameter_label(name, v))
            .collect()
    };
    ui.set_ratios(strings(options("aspect_ratio")));
    ui.set_resolutions(strings(options(if video { "resolution" } else { "size" })));
    ui.set_durations(strings(options("duration")));
    ui.set_qualities(strings(options("quality")));
    ui.set_quantities(strings(vec![]));
    ui.set_ratio_index(parameter_default_index(&model, "aspect_ratio"));
    ui.set_resolution_index(parameter_default_index(
        &model,
        if video { "resolution" } else { "size" },
    ));
    ui.set_duration_index(parameter_default_index(&model, "duration"));
    ui.set_quality_index(parameter_default_index(&model, "quality"));
    let limit = model["parameters"]["reference_asset_ids"]["max_items"]
        .as_i64()
        .unwrap_or(0);
    ui.set_accepts_references(limit > 0);
    ui.set_reference_max(limit as i32);
    ui.set_reference_limit(format!("PNG / JPG / WebP · 最多 {limit} 张").into());
}

fn clear(app: &App, state: &Rc<RefCell<Cloud>>) {
    let mut cloud = state.borrow_mut();
    cloud.token.clear();
    cloud.quote_id.clear();
    cloud.quote_credits = None;
    cloud.pending.clear();
    cloud.tasks.clear();
    cloud.assets.clear();
    cloud.teams.clear();
    cloud.references.clear();
    cloud.pending_personal_reference = None;
    cloud.local.clear();
    cloud.orders.clear();
    cloud.submission = None;
    cloud.download_attempts.clear();
    cloud.active_name.clear();
    cloud.epoch += 1;
    drop(cloud);
    let ui = app.global::<SeeCut>();
    ui.set_signed_in(false);
    ui.set_email("".into());
    ui.set_balance("--".into());
    ui.set_frozen("--".into());
    ui.set_tasks(rows(Vec::new()));
    ui.set_assets(rows(vec![]));
    ui.set_references(rows(vec![]));
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
    ui.set_selected_task(-1);
    ui.set_save_team_task("".into());
    ui.set_asset_picker_open(false);
    ui.set_mention_open(false);
    ui.set_reference_error("".into());
    ui.set_busy(false);
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
    if ui.get_busy() {
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
    if ui.get_busy() || ui.get_auth_resend_seconds() > 0 {
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
    let data =
        json!({"folder":c.folder,"reduced_motion":app.global::<SeeCut>().get_reduced_motion()});
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
    app.global::<SeeCut>()
        .set_local_folder(folder.to_string_lossy().into_owned().into());
    let state = Rc::new(RefCell::new(Cloud {
        base,
        folder,
        ..Cloud::default()
    }));
    let weak = app.as_weak();
    let shared = state.clone();
    app.global::<SeeCut>().on_action(move |name, id| { let Some(app)=weak.upgrade() else{return}; let ui=app.global::<SeeCut>(); let team=team_id(&ui,&shared.borrow()); match name.as_str() {
        name if name.starts_with("personal-") => personal_action(&app,&shared,name,id.as_str()),
        "auth-submit" => submit_auth(&app, &shared),
        "auth-switch" if !ui.get_busy() => { switch_auth(&ui, id.parse::<i32>().unwrap_or(0).clamp(0, 4)); },
        "auth-resend" => send_auth_code(&app, &shared, ui.get_auth_mode()),
        "auth-verify-start" => { if let Err(error) = normalized_auth_email(ui.get_auth_email().as_str()) { ui.set_error(error.into()); } else if !ui.get_busy() { switch_auth(&ui, 3); send_auth_code(&app, &shared, 3); } },
        "change-password"=>{ui.set_auth_email(ui.get_email());switch_auth(&ui,2);},
        "logout"=>job(&app,&shared,"logout".into(),request("POST","/api/auth/logout",Value::Null)),
        "wallet-refresh"=>job(&app,&shared,"wallet".into(),request("GET","/api/wallet",Value::Null)),
        "tasks-refresh"=>job(&app,&shared,"tasks".into(),request("GET","/api/generation/tasks",Value::Null)),
        "purchase"=>job(&app,&shared,"purchase".into(),request("POST","/api/orders",json!({"plan_id":id.to_string()}))),
        "order-refresh"=>job(&app,&shared,"order".into(),request("POST",format!("/api/orders/{id}/refresh"),Value::Null)),
        "navigate"=>match id.as_str(){"5"=>personal_job(&app,&shared,"list",Value::Null),"1"=>{refresh_models(&app,&shared);if ui.get_signed_in(){job(&app,&shared,"tasks".into(),request("GET","/api/generation/tasks",Value::Null));}},"2"=>job(&app,&shared,"teams".into(),request("GET","/api/teams",Value::Null)),"3"=>{job(&app,&shared,"plans".into(),request("GET","/api/credit-plans",Value::Null));job(&app,&shared,"wallet".into(),request("GET","/api/wallet",Value::Null));job(&app,&shared,"orders".into(),request("GET","/api/orders",Value::Null));},_=>{}},
        "generate"=>{
            let error = reference_validation(&ui, &shared.borrow());
            if !error.is_empty() { ui.set_error(error.into()); return; }
            let mut body=generation_body(&ui,&shared.borrow());
            if shared.borrow().quote_id.is_empty()||body!=shared.borrow().quote_body{refresh_quote(&app,&shared);return}
            let previous=shared.borrow().submission.clone();
            let key=previous.filter(|(b,_)|b==&body).map(|(_,k)|k).unwrap_or_else(||uuid::Uuid::new_v4().to_string());
            shared.borrow_mut().submission=Some((body.clone(),key.clone()));
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
        "reference-browse"|"asset-upload"=>{if let Some(paths)=crate::platform::pick_files("选择素材",if name=="reference-browse"{Some(("图片",&["png","jpg","jpeg","webp"]))}else{None}){for path in paths{upload_file(&app,&shared,path.to_string_lossy().to_string(),if name=="asset-upload"{"team_asset"}else{"generation_input"});}}},
        "reference-drop"=>upload_file(&app,&shared,id.to_string(),"generation_input"),
        "reference-team"=>{ui.set_asset_picker_open(true);ui.set_asset_picker_source(0);ui.set_personal_reference_search("".into());ui.set_trash_open(false);ui.set_asset_search("".into());ui.set_asset_filter(1);render_assets(&app,&shared);personal_job(&app,&shared,"list",Value::Null);},
        "reference-source" if ui.get_asset_picker_source()==1 => job(&app,&shared,"teams".into(),request("GET","/api/teams",Value::Null)),
        "reference-remove"=>remove_reference(&app,&shared,id.as_str()),
        "reference-mention"=>insert_mention(&app,&shared,id.as_str()),
        "reference-preview"=>{if let Some(item)=shared.borrow().references.iter().find(|v|text(v,"client_id")==id.as_str()){open_file(&text(item,"local_path"));}},
        "reference-retry"=>{
            let item=shared.borrow().references.iter().find(|v|text(v,"client_id")==id.as_str()&&matches!(text(v,"status").as_str(),"failed"|"expired")).cloned();
            if let Some(item)=item {
                if let Some(reference)=shared.borrow_mut().references.iter_mut().find(|v|text(v,"client_id")==id.as_str()){reference["status"]=json!("uploading");}
                render_references(&app,&shared);refresh_quote(&app,&shared);
                job(&app,&shared,format!("reference:{id}"),request("POST","local:upload",json!({"path":item["local_path"],"client_id":id.to_string(),"purpose":"generation_input","team":""})));
            }
        },
        "task-select"=>{},
        "task-preview"|"task-download"|"task-import"=>download_task(&app,&shared,id.as_str(),if name=="task-import"{"import"}else{"preview"}),
        "task-save-team"=>{let path=shared.borrow().local.get(id.as_str()).cloned();if team.is_empty(){ui.set_error("请先在团队资产中选择团队".into());}else if let Some(path)=path{upload_file(&app,&shared,path,"team_asset");}else{download_task(&app,&shared,id.as_str(),"save");ui.set_notice("下载完成后可保存到团队".into());}},
        "asset-preview"|"asset-download"|"asset-reference"|"asset-import"=>{
            if name=="asset-reference" {
                let is_image=shared.borrow().assets.iter().find(|v|text(v,"id")==id.as_str()).is_some_and(|v|text(v,"content_type").starts_with("image/"));
                if !is_image {ui.set_error("当前模型仅支持参考图片，不支持视频或音频参考".into());return;}
                if shared.borrow().references.len()>=ui.get_reference_max().max(0) as usize {ui.set_error(format!("当前模型最多支持 {} 张参考图片",ui.get_reference_max()).into());return;}
                ui.set_asset_picker_open(false);ui.set_page(1);
            }
            download_asset(&app,&shared,id.as_str(),match name.as_str(){"asset-reference"=>"reference","asset-import"=>"import",_=>"preview"});
        },
        "local-folder-browse"=>{#[cfg(not(any(target_os="ios",target_os="android")))]if let Some(folder)=rfd::FileDialog::new().pick_folder(){ui.set_local_folder(folder.to_string_lossy().into_owned().into());shared.borrow_mut().folder=folder;save_preferences(&app,&shared);}},
        _=>{}
    }});
    let weak = app.as_weak();
    let shared = state.clone();
    app.global::<SeeCut>().on_changed(move |field, _| {
        let Some(app) = weak.upgrade() else { return };
        if field == "reduced-motion" {
            save_preferences(&app, &shared);
        }
        if field == "mode" {
            app.global::<SeeCut>().set_model_index(0);
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
        if field == "asset-search" || field == "asset-filter" {
            render_assets(&app, &shared);
        }
        if field.starts_with("personal-") {
            render_personal(&app, &shared);
        }
    });
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
    use super::{
        DEFAULT_API_URL, auth_request, configured_api_url, invalid_reference_prompt, mention_start,
        parameter_default_index, parameter_label, parameter_value, rewrite_mentions,
    };
    use serde_json::json;

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
            "duration": {"default":10,"values":[5,10]},
            "resolution": {"values":["720p"]},
            "aspect_ratio": {"values":["16:9","9:16","1:1"]}
        }});
        assert_eq!(parameter_value(&model, "duration", 1), json!(10));
        assert_eq!(
            parameter_label("duration", &parameter_value(&model, "duration", 1)),
            "10 秒"
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
}
