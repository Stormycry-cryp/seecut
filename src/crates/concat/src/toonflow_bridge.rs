//! Bun 与当前原生账户之间的私有业务桥。浏览器不接收桥令牌或账户凭据。

mod assets;

use crate::cloud::{AccountAccess, ImageClient};
use seecut_flow_core::{
    digest,
    execution::{Execution, Phase},
    graph::MediaKind,
    host::{Context, GenerationDraft},
    runner::Asset as FlowAsset,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

const MAX_FRAME: usize = 1024 * 1024;
const MAX_SNAPSHOT: u64 = 512 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 4;
type Result<T> = std::result::Result<T, RpcError>;

#[derive(Debug, Serialize)]
pub(crate) struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
}
impl RpcError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Value::Null,
        }
    }
    pub(crate) fn io(error: std::io::Error) -> Self {
        Self::new("IO_ERROR", error.to_string())
    }
    pub(crate) fn from_http(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Status(status, response) => {
                let body: Value =
                    serde_json::from_reader(response.into_reader().take(MAX_FRAME as u64))
                        .unwrap_or_default();
                let value = &body["error"];
                let code = value["code"].as_str().unwrap_or("CLOUD_ERROR");
                let mut result =
                    Self::new(code, value["message"].as_str().unwrap_or("云端请求失败"));
                result.details = value
                    .get("details")
                    .cloned()
                    .unwrap_or_else(|| json!({"status":status}));
                if code == "PROMPT_RATE_LIMITED" { result.details["retry_after"] = value.get("retry_after").cloned().unwrap_or(json!(60)); }
                if status == 404 && code == "NOT_FOUND" {
                    result.code = "FLOW_STORAGE_UNAVAILABLE".into();
                    result.message = "当前云服务尚未提供 Flow 项目存储，请升级服务端。".into();
                }
                result
            }
            ureq::Error::Transport(_) => Self::new(
                "CLOUD_UNAVAILABLE",
                "暂时无法连接云服务，请保留本地版本后重试。",
            ),
        }
    }
}
impl From<String> for RpcError {
    fn from(error: String) -> Self {
        Self::new("GENERATION_ERROR", error)
    }
}
impl From<&str> for RpcError {
    fn from(error: &str) -> Self {
        Self::from(error.to_owned())
    }
}

thread_local! { static ACCOUNT: RefCell<Option<AccountAccess>> = const { RefCell::new(None) }; }

pub(crate) struct Bridge {
    endpoint: String,
    token: String,
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    _directory_lock: Arc<File>,
}
impl Bridge {
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.listener.take() {
            let _ = thread.join();
        }
        ACCOUNT.with(|slot| slot.borrow_mut().take());
    }
}
struct State {
    root: PathBuf,
    library: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    connections: AtomicUsize,
    projects: Mutex<HashSet<String>>,
    _directory_lock: Option<Arc<File>>,
}
struct ProjectGuard<'a> {
    state: &'a State,
    key: String,
}
impl Drop for ProjectGuard<'_> {
    fn drop(&mut self) {
        self.state
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}
impl State {
    fn project_guard(&self, scope: &str, project: &str) -> Result<ProjectGuard<'_>> {
        let key = format!("{scope}:{project}");
        if !self
            .projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone())
        {
            return Err(RpcError::new(
                "BUSY",
                "此项目的上一项原生操作尚未完成，请稍后重试。",
            ));
        }
        Ok(ProjectGuard { state: self, key })
    }
    fn project(&self, project: &str) -> Result<PathBuf> {
        checked_uuid(project)?;
        let workspaces = self.root.join("workspaces");
        let path = workspaces
            .join(project)
            .canonicalize()
            .map_err(RpcError::io)?;
        if path.parent() != Some(workspaces.as_path()) || !path.is_dir() {
            return Err(RpcError::new(
                "INVALID_PATH",
                "项目目录不属于当前 Flow 数据目录。",
            ));
        }
        Ok(path)
    }
    fn project_file(&self, project: &str, raw: &str, create: bool) -> Result<PathBuf> {
        bounded_path(&self.project(project)?, raw, create)
    }
    fn ledger_path(&self, scope: &str, attempt: &str) -> Result<PathBuf> {
        checked_uuid(attempt)?;
        Ok(self
            .root
            .join("native-ledger")
            .join(scope)
            .join(format!("{attempt}.json")))
    }
}

fn lock_data_directory(data: &Path) -> std::result::Result<File, String> {
    let directory = fs::symlink_metadata(data).map_err(|e| e.to_string())?;
    if !directory.is_dir() || directory.file_type().is_symlink() {
        return Err("Flow 数据目录必须是普通目录。".into());
    }
    let path = data.join(".native-host.lock");
    let validate = |metadata: &fs::Metadata| -> std::result::Result<(), String> {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Flow 数据锁必须是普通文件，不能使用符号链接。".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err("Flow 数据锁不能使用硬链接。".into());
            }
        }
        Ok(())
    };
    match fs::symlink_metadata(&path) {
        Ok(metadata) => validate(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // 打开链接本身，再通过文件类型检查拒绝重解析点。
        options.custom_flags(0x00200000);
    }
    let file = match options.create_new(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate(&fs::symlink_metadata(&path).map_err(|e| e.to_string())?)?;
            options
                .create_new(false)
                .open(&path)
                .map_err(|e| e.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    let opened = file.metadata().map_err(|e| e.to_string())?;
    validate(&opened)?;
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => {
            "Flow 数据目录正由另一 SeeCut 实例使用，请关闭该实例后重试。".to_owned()
        }
        fs::TryLockError::Error(error) => format!("无法独占 Flow 数据目录：{error}"),
    })?;
    let current = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    validate(&current)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != current.dev() || opened.ino() != current.ino() {
            return Err("Flow 数据锁在打开期间发生变化，请重试。".into());
        }
    }
    Ok(file)
}

pub(crate) fn start(data: PathBuf, account: AccountAccess) -> std::result::Result<Bridge, String> {
    let directory_lock = Arc::new(lock_data_directory(&data)?);
    fs::create_dir_all(data.join("workspaces")).map_err(|e| e.to_string())?;
    fs::create_dir_all(data.join("native-ledger")).map_err(|e| e.to_string())?;
    let root = data.canonicalize().map_err(|e| e.to_string())?;
    let library = concat_host::AppDirs::locate()?
        .data
        .join("personal-library");
    ACCOUNT.with(|slot| *slot.borrow_mut() = Some(account));
    let listener =
        TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let endpoint = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .to_string();
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(State {
        root,
        library,
        token: token.clone(),
        stop: stop.clone(),
        connections: AtomicUsize::new(0),
        projects: Mutex::new(HashSet::new()),
        _directory_lock: Some(directory_lock.clone()),
    });
    let thread = std::thread::spawn(move || {
        while !state.stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if state.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                        state.connections.fetch_sub(1, Ordering::AcqRel);
                        drop(stream);
                        continue;
                    }
                    let state = state.clone();
                    std::thread::spawn(move || {
                        let _ = connection(stream, &state);
                        state.connections.fetch_sub(1, Ordering::AcqRel);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(_) => break,
            }
        }
    });
    Ok(Bridge {
        endpoint,
        token,
        stop,
        listener: Some(thread),
        _directory_lock: directory_lock,
    })
}

fn read_frame(reader: &mut impl BufRead) -> Result<Value> {
    let mut bytes = Vec::new();
    Read::by_ref(reader)
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(RpcError::io)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME || bytes.last() != Some(&b'\n') {
        return Err(RpcError::new(
            "INVALID_FRAME",
            "RPC消息为空、不完整或超过1MiB。",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| RpcError::new("INVALID_JSON", "RPC消息不是有效JSON。"))
}
fn constant_time_equal(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}
fn write_frame(writer: &mut impl Write, value: &Value) -> Result<()> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|e| RpcError::new("INVALID_RESPONSE", e.to_string()))?;
    if bytes.len() > MAX_FRAME {
        return Err(RpcError::new("RESPONSE_TOO_LARGE", "RPC响应超过1MiB。"));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).map_err(RpcError::io)
}
fn connection(mut stream: TcpStream, state: &State) -> Result<()> {
    // macOS 的 accept 可能继承监听器的非阻塞状态；帧读取需要等待有限超时。
    stream.set_nonblocking(false).map_err(RpcError::io)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(RpcError::io)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(RpcError::io)?;
    let mut reader = BufReader::new(stream.try_clone().map_err(RpcError::io)?);
    let auth = read_frame(&mut reader)?;
    if auth["type"] != "auth"
        || !auth["token"]
            .as_str()
            .is_some_and(|token| constant_time_equal(token, &state.token))
    {
        write_frame(&mut stream, &json!({"type":"auth","ok":false}))?;
        return Ok(());
    }
    write_frame(&mut stream, &json!({"type":"auth","ok":true}))?;
    let frame = read_frame(&mut reader)?;
    let id = frame.get("id").cloned().unwrap_or(Value::Null);
    let result = if !id.is_string() || id.as_str().is_none_or(|id| id.is_empty() || id.len() > 128)
    {
        Err(RpcError::new(
            "INVALID_REQUEST",
            "RPC请求需要有界字符串id。",
        ))
    } else {
        dispatch(
            state,
            frame["method"].as_str().unwrap_or_default(),
            frame.get("params").cloned().unwrap_or_else(|| json!({})),
        )
    };
    let response = match result {
        Ok(value) => json!({"id":id,"result":value}),
        Err(error) => json!({"id":id,"error":error}),
    };
    write_frame(&mut stream, &response)
}

struct Client {
    image: ImageClient,
    epoch: u64,
}
fn current_client() -> Result<Option<Client>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    slint::invoke_from_event_loop(move || {
        ACCOUNT.with(|slot| {
            let value = slot.borrow().as_ref().and_then(|account| {
                Some(Client {
                    image: account.image_output_client()?,
                    epoch: account.epoch()?,
                })
            });
            let _ = sender.send(value);
        })
    })
    .map_err(|_| RpcError::new("APP_UNAVAILABLE", "原生窗口暂不可用。"))?;
    receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| RpcError::new("APP_BUSY", "原生窗口正在处理操作，请稍后重试。"))
}
impl Client {
    fn key(&self) -> String {
        digest(&self.image.scope)
    }
    fn recheck(&self) -> Result<()> {
        let current = current_client()?.ok_or_else(|| {
            RpcError::new("FLOW_ACCOUNT_CHANGED", "账户会话已结束，请重新登录后操作。")
        })?;
        if current.epoch != self.epoch || current.image.scope != self.image.scope {
            return Err(RpcError::new(
                "FLOW_ACCOUNT_CHANGED",
                "账户或云服务已切换，请重新打开项目后重试。",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Ledger {
    project_id: String,
    input_digest: String,
    execution: Execution,
    dispatched: bool,
    quote: Value,
    tasks: BTreeMap<String, Value>,
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| RpcError::new("INVALID_PATH", "账本路径无效。"))?;
    fs::create_dir_all(parent).map_err(RpcError::io)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(RpcError::io)?;
    serde_json::to_writer(temporary.as_file_mut(), value)
        .map_err(|e| RpcError::new("INVALID_DATA", e.to_string()))?;
    temporary.as_file_mut().flush().map_err(RpcError::io)?;
    temporary.as_file().sync_all().map_err(RpcError::io)?;
    temporary
        .persist(path)
        .map_err(|error| RpcError::io(error.error))?;
    Ok(())
}
fn load_ledger(path: &Path) -> Result<Ledger> {
    load_ledger_with_missing(path, "ATTEMPT_NOT_FOUND")
}
fn ledger_open_error(error: std::io::Error, missing_code: &str) -> RpcError {
    if error.kind() == std::io::ErrorKind::NotFound {
        RpcError::new(missing_code, "当前账户没有这条生成记录。")
    } else {
        RpcError::io(error)
    }
}
fn load_ledger_with_missing(path: &Path, missing_code: &str) -> Result<Ledger> {
    let file = File::open(path).map_err(|error| ledger_open_error(error, missing_code))?;
    if file.metadata().map_err(RpcError::io)?.len() > 32 * 1024 * 1024 {
        return Err(RpcError::new("INVALID_LEDGER", "生成账本超过大小上限。"));
    }
    let ledger: Ledger = serde_json::from_reader(file)
        .map_err(|_| RpcError::new("INVALID_LEDGER", "生成账本损坏，原文件已保留。"))?;
    ledger
        .execution
        .validate(&ledger.project_id, &ledger.project_id)?;
    if ledger.execution.attempts.len() != 1 {
        return Err(RpcError::new(
            "INVALID_LEDGER",
            "原生生成记录必须只含一个冻结attempt。",
        ));
    }
    Ok(ledger)
}
fn ledger_receipt(ledger: &Ledger) -> Value {
    let attempt = &ledger.execution.attempts[0];
    json!({"attemptId":attempt.id,"phase":if !ledger.dispatched { "prepared".into() } else { format!("{:?}",attempt.phase).to_lowercase() },"dispatched":ledger.dispatched,"readyToSubmit":attempt.body.is_some(),"quoteId":attempt.body.as_ref().and_then(|body|body["quote_id"].as_str()),"quote":ledger.quote,"requestDigest":attempt.request_digest,"scopeKey":digest(&attempt.draft.context.scope),"tasks":ledger.tasks.values().collect::<Vec<_>>(),"error":attempt.error,"outcomeUnknown":ledger.dispatched && attempt.tasks.is_empty() && attempt.phase == Phase::Intent})
}
fn argument<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params[key]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .ok_or_else(|| RpcError::new("INVALID_PARAMS", format!("缺少有效参数{key}。")))
}
fn generation_prompt(params: &Value) -> Result<&str> {
    params["prompt"]
        .as_str()
        .filter(|prompt| !prompt.trim().is_empty() && prompt.chars().count() <= 12000)
        .ok_or_else(|| {
            RpcError::new(
                "INVALID_PARAMS",
                "生成提示词不能为空，且最多支持12000个字符。",
            )
        })
}
fn checked_uuid(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value).is_ok_and(|uuid| uuid.hyphenated().to_string() == value) {
        Ok(())
    } else {
        Err(RpcError::new(
            "INVALID_PROJECT_ID",
            "项目或操作标识必须是标准UUID。",
        ))
    }
}
fn bounded_path(root: &Path, raw: &str, create: bool) -> Result<PathBuf> {
    let value = Path::new(raw);
    if value
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(RpcError::new("INVALID_PATH", "文件路径不能越过项目目录。"));
    }
    let candidate = if value.is_absolute() {
        value.to_owned()
    } else {
        root.join(value)
    };
    let checked = if create && !candidate.exists() {
        let parent = candidate
            .parent()
            .ok_or_else(|| RpcError::new("INVALID_PATH", "文件没有有效父目录。"))?
            .canonicalize()
            .map_err(RpcError::io)?;
        if !parent.starts_with(root) {
            return Err(RpcError::new("INVALID_PATH", "文件父目录不属于当前项目。"));
        }
        parent.join(
            candidate
                .file_name()
                .ok_or_else(|| RpcError::new("INVALID_PATH", "文件名无效。"))?,
        )
    } else {
        candidate.canonicalize().map_err(RpcError::io)?
    };
    if !checked.starts_with(root) || checked == root {
        return Err(RpcError::new("INVALID_PATH", "文件不属于当前项目。"));
    }
    if !create && !checked.is_file() {
        return Err(RpcError::new("INVALID_PATH", "输入必须是项目内普通文件。"));
    }
    Ok(checked)
}
fn file_sha(path: &Path) -> Result<String> {
    let mut input = File::open(path).map_err(RpcError::io)?;
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let size = input.read(&mut bytes).map_err(RpcError::io)?;
        if size == 0 {
            break;
        }
        hash.update(&bytes[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn kind(value: &str) -> Result<MediaKind> {
    match value {
        "image" => Ok(MediaKind::Image),
        "video" => Ok(MediaKind::Video),
        "audio" => Ok(MediaKind::Audio),
        _ => Err(RpcError::new("INVALID_KIND", "素材类型无效。")),
    }
}
fn asset_value(asset: &crate::personal_library::Asset) -> Value {
    json!({"assetId":asset.id,"version":asset.version,"path":asset.path,"type":match asset.kind { crate::personal_library::AssetKind::Image=>"image",crate::personal_library::AssetKind::Video=>"video",crate::personal_library::AssetKind::Audio=>"audio" },"name":asset.name})
}

fn check_expected_scope(method: &str, params: &Value, current: &str) -> Result<()> {
    let required = method.starts_with("prompts.") || method.starts_with("generation.")
        || method.starts_with("projects.cloud.")
        || method.starts_with("assets.");
    match params.get("expectedScopeKey") {
        Some(Value::String(expected)) if expected == current => Ok(()),
        None if !required => Ok(()),
        _ => Err(RpcError::new(
            "FLOW_ACCOUNT_CHANGED",
            "账户或云服务范围已变化，请刷新后重新操作。",
        )),
    }
}

fn dispatch(state: &State, method: &str, params: Value) -> Result<Value> {
    if !matches!(
        method,
        "account.status"
            | "prompts.context" | "prompts.recommend" | "prompts.rewrite"
            | "generation.catalog"
            | "generation.prepare"
            | "generation.find"
            | "generation.submit"
            | "generation.lookup"
            | "generation.task"
            | "generation.register"
            | "generation.exportProject"
            | "generation.importProject"
            | "projects.cloud.list"
            | "projects.cloud.upload"
            | "projects.cloud.download"
            | "assets.list"
            | "assets.resolve"
            | "assets.thumbnail"
            | "assets.copyToProject"
            | "assets.registerProjectMedia"
    ) {
        return Err(RpcError::new("METHOD_NOT_FOUND", "原生桥不支持此方法。"));
    }
    if method.starts_with("assets.") {
        return assets::dispatch(state, method, &params);
    }
    let current = current_client()?;
    if method == "account.status" {
        return Ok(match current {
            Some(client) => {
                json!({"signedIn":true,"scope":client.image.scope,"scopeKey":client.key(),"epoch":client.epoch})
            }
            None => json!({"signedIn":false,"scope":null,"scopeKey":"local"}),
        });
    }
    let client = current.ok_or_else(|| {
        if params.get("expectedScopeKey").is_some() {
            RpcError::new("FLOW_ACCOUNT_CHANGED", "账户会话已结束，请重新登录后操作。")
        } else {
            RpcError::new("SIGNED_OUT", "请先在SeeCut账户页登录。")
        }
    })?;
    check_expected_scope(method, &params, &client.key())?;
    let mut result = match method {
        "prompts.context" | "prompts.recommend" | "prompts.rewrite" => {
            let operation = method.trim_start_matches("prompts.");
            let key = if operation == "context" { "" } else { argument(&params, "requestId")? };
            { let mut body = params.get("body").cloned().unwrap_or(Value::Null);
              if body.is_object() { body["templates"] = crate::cloud::prompt_assistance::candidates(); }
              client.image.prompt_assistance(operation, body, key) }
        }
        "generation.catalog" => {
            let mut catalog = client.image.catalog()?;
            client.recheck()?;
            catalog["scopeKey"] = json!(client.key());
            Ok(catalog)
        }
        "generation.prepare" => prepare(state, &client, &params),
        "generation.find" => {
            let project = argument(&params, "projectId")?;
            state.project(project)?;
            let request = argument(&params, "requestId")?;
            let ledger = load_ledger_with_missing(
                &state.ledger_path(&client.key(), request)?,
                "GENERATION_NOT_FOUND",
            )?;
            if ledger.project_id != project {
                return Err(RpcError::new(
                    "ATTEMPT_NOT_FOUND",
                    "当前项目没有这条生成记录。",
                ));
            }
            client.recheck()?;
            Ok(ledger_receipt(&ledger))
        }
        "generation.exportProject" => export_project(state, &client, &params),
        "generation.importProject" => import_project(state, &client, &params),
        "generation.submit" | "generation.lookup" | "generation.task" | "generation.register" => {
            attempt_call(state, &client, method, &params)
        }
        "projects.cloud.list" => {
            let result = client.image.bridge_cloud_json("/api/flow/projects")?;
            client.recheck()?;
            Ok(result)
        }
        "projects.cloud.upload" => cloud_upload(state, &client, &params),
        "projects.cloud.download" => cloud_download(state, &client, &params),
        _ => Err(RpcError::new("METHOD_NOT_FOUND", "原生桥不支持此方法。")),
    }?;
    client.recheck()?;
    if let Some(object) = result.as_object_mut() {
        object.insert("scopeKey".into(), json!(client.key()));
    }
    Ok(result)
}

fn prepare(state: &State, client: &Client, params: &Value) -> Result<Value> {
    let prompt = generation_prompt(params)?;
    let project = argument(params, "projectId")?;
    state.project(project)?;
    let request = argument(params, "requestId")?;
    checked_uuid(request)?;
    let _guard = state.project_guard(&client.key(), project)?;
    let path = state.ledger_path(&client.key(), request)?;
    let input_digest = digest(params);
    let mut ledger = if path.is_file() {
        let ledger = load_ledger(&path)?;
        if ledger.project_id != project || ledger.input_digest != input_digest {
            return Err(RpcError::new(
                "REQUEST_CONFLICT",
                "原requestId已绑定另一份生成输入，请保留原记录并使用新的requestId。",
            ));
        }
        if ledger.execution.attempts[0].body.is_some() {
            return Ok(ledger_receipt(&ledger));
        }
        ledger
    } else {
        let media_kind = kind(argument(params, "kind")?)?;
        if media_kind == MediaKind::Audio {
            return Err(RpcError::new(
                "UNSUPPORTED_KIND",
                "当前原生生成只支持图片和视频。",
            ));
        }
        let model_id = argument(params, "modelId")?;
        let catalog = client.image.catalog()?;
        let model = catalog["models"]
            .as_array()
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| model["id"] == model_id && model["kind"] == params["kind"])
            })
            .ok_or_else(|| RpcError::new("MODEL_UNAVAILABLE", "模型不在当前账户能力目录中。"))?;
        let mut references = Vec::new();
        if let Some(values) = params["references"].as_array() {
            if values.len() > 16 {
                return Err(RpcError::new(
                    "INVALID_REFERENCES",
                    "参考素材不能超过16项。",
                ));
            }
            for reference in values {
                let file = state.project_file(project, argument(reference, "path")?, false)?;
                let asset = assets::reference_asset(state, project, &file)?;
                let actual = crate::cloud::media_kind(asset.kind);
                if actual != kind(argument(reference, "kind")?)? {
                    return Err(RpcError::new(
                        "INVALID_REFERENCES",
                        "参考文件类型与声明不一致。",
                    ));
                }
                references.push(FlowAsset {
                    id: asset.id,
                    version: serde_json::to_string(&asset.version.ok_or("参考素材没有不可变版本")?)
                        .map_err(|e| RpcError::new("INVALID_DATA", e.to_string()))?,
                    kind: actual,
                    simulated: false,
                });
            }
        }
        let draft = GenerationDraft {
            context: Context {
                document_id: project.into(),
                graph_id: params["graphId"].as_str().unwrap_or("main").into(),
                node_id: params["nodeId"].as_str().unwrap_or(request).into(),
                revision: params["revision"].as_u64().unwrap_or(0),
                scope: client.image.scope.clone(),
            },
            model_id: model_id.into(),
            capability_version: digest(model),
            media_kind,
            prompt: prompt.into(),
            parameters: params
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({})),
            references,
        };
        let mut execution = Execution::new(project.into(), project.into());
        let generated = execution.begin(draft, digest(params))?;
        let attempt = execution.attempt_mut(&generated)?;
        attempt.id = request.into();
        attempt.key = request.into();
        let ledger = Ledger {
            project_id: project.into(),
            input_digest,
            execution,
            dispatched: false,
            quote: Value::Null,
            tasks: BTreeMap::new(),
        };
        atomic_json(&path, &ledger)?;
        ledger
    };
    client.recheck()?;
    let (body, quote) = client
        .image
        .prepare_with_quote(&ledger.execution.attempts[0].draft)?;
    ledger.execution.intent(request, body)?;
    ledger.quote = quote;
    atomic_json(&path, &ledger)?;
    client.recheck()?;
    Ok(ledger_receipt(&ledger))
}
fn observe(ledger: &mut Ledger, client: &Client, task: Value) -> Result<()> {
    let attempt = ledger.execution.attempts[0].id.clone();
    ledger
        .execution
        .observe(&attempt, &client.image.scope, &task)?;
    let id = task["id"].as_str().ok_or("任务缺少身份")?.to_owned();
    ledger.tasks.insert(id, task);
    Ok(())
}
fn attempt_call(state: &State, client: &Client, method: &str, params: &Value) -> Result<Value> {
    let id = argument(params, "attemptId")?;
    let path = state.ledger_path(&client.key(), id)?;
    let mut ledger = load_ledger(&path)?;
    state.project(&ledger.project_id)?;
    let _guard = state.project_guard(&client.key(), &ledger.project_id)?;
    // 获得独占操作权后重读，避免使用锁前快照覆盖后续状态。
    ledger = load_ledger(&path)?;
    if ledger.execution.attempts[0].draft.context.scope != client.image.scope {
        return Err(RpcError::new(
            "FLOW_ACCOUNT_CHANGED",
            "生成记录不属于当前账户。",
        ));
    }
    if method == "generation.submit" && !ledger.dispatched {
        if ledger.execution.attempts[0].body.is_none() {
            return Err(RpcError::new("NOT_PREPARED", "生成请求尚未完成报价。"));
        }
        client.recheck()?;
        ledger.dispatched = true;
        atomic_json(&path, &ledger)?;
        match client.image.submit(&ledger.execution.attempts[0]) {
            Ok(task) => observe(&mut ledger, client, task)?,
            Err(failure) => ledger
                .execution
                .submission_failed(id, &client.image.scope, failure)?,
        }
    } else if ledger.dispatched && ledger.tasks.is_empty() {
        match client.image.lookup(&ledger.execution.attempts[0]) {
            Ok(task) => observe(&mut ledger, client, task)?,
            Err(failure) => {
                if ledger.execution.attempts[0].phase == Phase::Intent {
                    ledger
                        .execution
                        .submission_failed(id, &client.image.scope, failure)?;
                }
            }
        }
    } else if matches!(method, "generation.task" | "generation.register") {
        let tasks = ledger.tasks.keys().cloned().collect::<Vec<_>>();
        for task_id in tasks {
            client.recheck()?;
            let task = client.image.task(&task_id)?;
            observe(&mut ledger, client, task)?;
        }
    }
    atomic_json(&path, &ledger)?;
    if method == "generation.register" {
        let mut assets = Vec::new();
        let tasks = ledger.tasks.values().cloned().collect::<Vec<_>>();
        for task in tasks {
            if task["status"] != "succeeded" {
                continue;
            }
            let task_id = task["id"].as_str().ok_or("任务身份缺失")?;
            let output_ids = ledger.execution.attempts[0]
                .tasks
                .get(task_id)
                .ok_or("任务尚未冻结")?
                .outputs
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            for output in output_ids {
                if params["outputId"]
                    .as_str()
                    .is_some_and(|wanted| wanted != output)
                {
                    continue;
                }
                client.recheck()?;
                let asset = client
                    .image
                    .register(&ledger.execution.attempts[0], &task, &output)?
                    .asset;
                let value = asset_value(&asset);
                ledger.execution.registered(
                    id,
                    task_id,
                    &output,
                    FlowAsset {
                        id: asset.id,
                        version: serde_json::to_string(
                            &asset.version.ok_or("登记结果没有不可变版本")?,
                        )
                        .map_err(|e| RpcError::new("INVALID_DATA", e.to_string()))?,
                        kind: crate::cloud::media_kind(asset.kind),
                        simulated: false,
                    },
                )?;
                atomic_json(&path, &ledger)?;
                assets.push(value);
            }
        }
        if assets.is_empty() {
            return Err(RpcError::new("OUTPUT_NOT_READY", "尚无可登记的成功输出。"));
        }
        client.recheck()?;
        return Ok(json!({"attemptId":id,"assets":assets}));
    }
    client.recheck()?;
    Ok(ledger_receipt(&ledger))
}
fn cloud_upload(state: &State, client: &Client, params: &Value) -> Result<Value> {
    let project = argument(params, "projectId")?;
    let file = state.project_file(project, argument(params, "path")?, false)?;
    if file.extension().is_none_or(|extension| extension != "zip") {
        return Err(RpcError::new("INVALID_SNAPSHOT", "项目快照必须是zip文件。"));
    }
    let size = file.metadata().map_err(RpcError::io)?.len();
    if size == 0 || size > MAX_SNAPSHOT {
        return Err(RpcError::new(
            "REQUEST_TOO_LARGE",
            "项目快照为空或超过512MiB。",
        ));
    }
    let sha = argument(params, "sha256")?;
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) || file_sha(&file)? != sha {
        return Err(RpcError::new(
            "FLOW_HASH_MISMATCH",
            "项目快照SHA-256校验失败。",
        ));
    }
    let name = argument(params, "name")?;
    if name.chars().count() > 128 {
        return Err(RpcError::new("INVALID_PARAMS", "项目名不能超过128字。"));
    }
    let base = params["baseRevision"]
        .as_u64()
        .ok_or_else(|| RpcError::new("INVALID_PARAMS", "缺少baseRevision。"))?;
    let request = argument(params, "requestId")?;
    checked_uuid(request)?;
    let _guard = state.project_guard(&client.key(), project)?;
    let record = state
        .root
        .join("native-ledger")
        .join(client.key())
        .join("cloud")
        .join(format!("{request}.json"));
    let fingerprint =
        digest(&json!({"project":project,"name":name,"base":base,"sha256":sha,"size":size}));
    if record.exists() {
        let old: Value = serde_json::from_reader(File::open(&record).map_err(RpcError::io)?)
            .map_err(|_| RpcError::new("INVALID_LEDGER", "同步记录损坏。"))?;
        if old["fingerprint"] != fingerprint {
            return Err(RpcError::new(
                "FLOW_IDEMPOTENCY_CONFLICT",
                "原同步requestId已绑定另一份快照。",
            ));
        }
    } else {
        atomic_json(
            &record,
            &json!({"fingerprint":fingerprint,"requestId":request,"projectId":project,"sha256":sha}),
        )?;
    }
    let mut url = url::Url::parse("http://127.0.0.1").unwrap();
    url.set_path(&format!("/api/flow/projects/{project}/snapshot"));
    url.query_pairs_mut()
        .append_pair("base_revision", &base.to_string())
        .append_pair("name", name);
    client.recheck()?;
    let result = client.image.bridge_cloud_upload(
        &format!("{}?{}", url.path(), url.query().unwrap()),
        &file,
        sha,
        request,
    )?;
    client.recheck()?;
    Ok(result)
}
fn cloud_download(state: &State, client: &Client, params: &Value) -> Result<Value> {
    let project = argument(params, "projectId")?;
    let target = state.project_file(project, argument(params, "path")?, true)?;
    let _guard = state.project_guard(&client.key(), project)?;
    let endpoint = match params["revision"].as_u64() {
        Some(revision) => format!("/api/flow/projects/{project}?revision={revision}"),
        None => format!("/api/flow/projects/{project}"),
    };
    let metadata = client.image.bridge_cloud_json(&endpoint)?;
    let expected = metadata["sha256"]
        .as_str()
        .filter(|sha| sha.len() == 64)
        .ok_or_else(|| RpcError::new("INVALID_RESPONSE", "云端项目缺少可信摘要。"))?;
    let size = metadata["size_bytes"].as_u64().ok_or("云端项目缺少大小")?;
    if size > MAX_SNAPSHOT {
        return Err(RpcError::new("REQUEST_TOO_LARGE", "云端快照超过512MiB。"));
    }
    let temporary = target.with_file_name(format!(".host-{}.zip", uuid::Uuid::new_v4()));
    client.recheck()?;
    let result = (|| {
        client
            .image
            .bridge_download(argument(&metadata, "download_url")?, &temporary)?;
        if temporary.metadata().map_err(RpcError::io)?.len() != size
            || file_sha(&temporary)? != expected
        {
            return Err(RpcError::new(
                "FLOW_HASH_MISMATCH",
                "下载的项目快照校验失败。",
            ));
        }
        client.recheck()?;
        fs::rename(&temporary, &target).map_err(RpcError::io)?;
        Ok(json!({"metadata":metadata,"path":target,"verifiedSha256":expected}))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Serialize, Deserialize)]
struct ProjectJournal {
    format: u32,
    project_id: String,
    scope: seecut_flow_core::runner::Scope,
    ledgers: Vec<Ledger>,
}
// 云任务身份可跨机器恢复；个人库的登记身份只能在本机验证后沿用。
fn detach_unavailable_bindings(ledger: &mut Ledger, mut available: impl FnMut(&FlowAsset) -> bool) {
    for attempt in &mut ledger.execution.attempts {
        let mut needs_registration = false;
        for task in attempt.tasks.values_mut() {
            for output in task.outputs.values_mut() {
                if output.asset.as_ref().is_some_and(|asset| !available(asset)) {
                    output.asset = None;
                    needs_registration |= matches!(task.status.as_str(), "succeeded" | "expired");
                }
            }
        }
        if attempt.selected.as_ref().is_some_and(|(task, output)| {
            attempt
                .tasks
                .get(task)
                .and_then(|t| t.outputs.get(output))
                .is_none_or(|o| o.asset.is_none())
        }) {
            attempt.selected = None;
        }
        if attempt.phase == Phase::Finished && needs_registration {
            attempt.phase = Phase::Accepted;
        }
    }
}
fn export_project(state: &State, client: &Client, params: &Value) -> Result<Value> {
    let project = argument(params, "projectId")?;
    let root = state.project(project)?;
    let _guard = state.project_guard(&client.key(), project)?;
    let directory = root.join(".seecut");
    fs::create_dir_all(&directory).map_err(RpcError::io)?;
    let target = state.project_file(project, ".seecut/native-generation.json", true)?;
    let mut ledgers = Vec::new();
    let folder = state.root.join("native-ledger").join(client.key());
    if folder.is_dir() {
        for entry in fs::read_dir(folder).map_err(RpcError::io)?.take(10001) {
            let entry = entry.map_err(RpcError::io)?;
            if !entry.file_type().map_err(RpcError::io)?.is_file()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let ledger = load_ledger(&entry.path())?;
            if ledger.project_id == project {
                ledgers.push(ledger);
            }
            if ledgers.len() > 1000 {
                return Err(RpcError::new(
                    "JOURNAL_TOO_LARGE",
                    "项目生成记录超过导出上限。",
                ));
            }
        }
    }
    ledgers.sort_by(|a, b| a.execution.attempts[0].id.cmp(&b.execution.attempts[0].id));
    if ledgers.is_empty() && target.exists() {
        let previous: ProjectJournal =
            serde_json::from_reader(File::open(&target).map_err(RpcError::io)?).map_err(|_| {
                RpcError::new("INVALID_JOURNAL", "已有项目生成导出无效，原文件已保留。")
            })?;
        if previous.format != 1
            || previous.project_id != project
            || previous.scope != client.image.scope
        {
            return Err(RpcError::new(
                "JOURNAL_SCOPE_MISMATCH",
                "已有生成导出不属于当前项目与账户，原文件已保留。",
            ));
        }
        return Ok(
            json!({"path":target,"sha256":file_sha(&target)?,"attempts":previous.ledgers.len(),"unchanged":true}),
        );
    }
    client.recheck()?;
    let count = ledgers.len();
    let journal = ProjectJournal {
        format: 1,
        project_id: project.into(),
        scope: client.image.scope.clone(),
        ledgers,
    };
    let bytes =
        serde_json::to_vec(&journal).map_err(|e| RpcError::new("INVALID_DATA", e.to_string()))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(RpcError::new(
            "JOURNAL_TOO_LARGE",
            "项目生成记录超过32MiB。",
        ));
    }
    if fs::read(&target).is_ok_and(|existing| existing == bytes) {
        return Ok(
            json!({"path":target,"sha256":file_sha(&target)?,"attempts":count,"unchanged":true}),
        );
    }
    atomic_json(&target, &journal)?;
    Ok(json!({"path":target,"sha256":file_sha(&target)?,"attempts":count}))
}
fn import_project(state: &State, client: &Client, params: &Value) -> Result<Value> {
    let project = argument(params, "projectId")?;
    state.project(project)?;
    let _guard = state.project_guard(&client.key(), project)?;
    if params.get("path").is_none()
        && !state
            .project(project)?
            .join(".seecut/native-generation.json")
            .exists()
    {
        return Ok(json!({"imported":0,"lookupOnly":true}));
    }
    let source = state.project_file(
        project,
        params["path"]
            .as_str()
            .unwrap_or(".seecut/native-generation.json"),
        false,
    )?;
    let file = File::open(&source).map_err(RpcError::io)?;
    if file.metadata().map_err(RpcError::io)?.len() > 32 * 1024 * 1024 {
        return Err(RpcError::new(
            "JOURNAL_TOO_LARGE",
            "项目生成记录超过32MiB。",
        ));
    }
    let journal: ProjectJournal = serde_json::from_reader(file)
        .map_err(|_| RpcError::new("INVALID_JOURNAL", "项目生成记录格式无效。"))?;
    if journal.format != 1
        || journal.project_id != project
        || journal.scope != client.image.scope
        || journal.ledgers.len() > 1000
    {
        return Err(RpcError::new(
            "JOURNAL_SCOPE_MISMATCH",
            "项目生成记录不属于当前项目与账户。",
        ));
    }
    // 先验证全部记录，再采用；导入绝不赋予提交新收费请求的能力。
    for ledger in &journal.ledgers {
        ledger.execution.validate(project, project)?;
        if ledger.project_id != project
            || ledger.execution.attempts.len() != 1
            || ledger.execution.attempts[0].draft.context.scope != client.image.scope
        {
            return Err(RpcError::new("INVALID_JOURNAL", "项目生成记录身份无效。"));
        }
        checked_uuid(&ledger.execution.attempts[0].id)?;
    }
    client.recheck()?;
    let mut imported = 0;
    for mut ledger in journal.ledgers {
        let id = &ledger.execution.attempts[0].id;
        let path = state.ledger_path(&client.key(), id)?;
        if path.exists() {
            let old = load_ledger(&path)?;
            if old.project_id != project
                || old.input_digest != ledger.input_digest
                || old.execution.attempts[0].fingerprint != ledger.execution.attempts[0].fingerprint
                || old.execution.attempts[0].body != ledger.execution.attempts[0].body
            {
                return Err(RpcError::new(
                    "JOURNAL_CONFLICT",
                    "本机已有不同的原生成记录，原文件已保留。",
                ));
            }
            continue;
        }
        ledger.dispatched = true;
        detach_unavailable_bindings(&mut ledger, |asset| {
            crate::cloud::library_asset(&asset.id, Some(&asset.version)).is_ok()
        });
        ledger.execution.validate(project, project)?;
        atomic_json(&path, &ledger)?;
        imported += 1;
    }
    Ok(json!({"imported":imported,"lookupOnly":true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_prompt_counts_unicode_characters_instead_of_utf8_bytes() {
        let params = json!({"prompt":"中".repeat(12000)});
        assert_eq!(generation_prompt(&params).unwrap().chars().count(), 12000);
        assert!(generation_prompt(&json!({"prompt":"中".repeat(12001)})).is_err());
        assert!(generation_prompt(&json!({"prompt":"a".repeat(12000)})).is_ok());
        for prompt in [json!(null), json!(7), json!(""), json!(" \n\t")] {
            assert_eq!(
                generation_prompt(&json!({"prompt":prompt}))
                    .unwrap_err()
                    .code,
                "INVALID_PARAMS"
            );
        }
    }
    #[test]
    fn generation_find_only_marks_actual_not_found_as_definitely_absent() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            ledger_open_error(Error::from(ErrorKind::NotFound), "GENERATION_NOT_FOUND").code,
            "GENERATION_NOT_FOUND"
        );
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::NotADirectory,
            ErrorKind::Interrupted,
            ErrorKind::Other,
        ] {
            assert_eq!(
                ledger_open_error(Error::from(kind), "GENERATION_NOT_FOUND").code,
                "IO_ERROR"
            );
        }
    }

    #[test]
    fn bridge_data_lock_child_probe() {
        if let Some(root) = std::env::var_os("SEECUT_TEST_LOCK_ROOT") {
            assert!(lock_data_directory(Path::new(&root)).is_err());
        }
    }
    #[test]
    fn bridge_data_lock_rejects_another_process_then_releases_without_truncation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(".native-host.lock");
        fs::write(&path, b"preserve-lock-content").unwrap();
        let first = lock_data_directory(temporary.path()).unwrap();
        assert!(lock_data_directory(temporary.path()).is_err());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("bridge_data_lock_child_probe")
            .env("SEECUT_TEST_LOCK_ROOT", temporary.path())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read(&path).unwrap(), b"preserve-lock-content");
        drop(first);
        assert!(lock_data_directory(temporary.path()).is_ok());
    }
    #[test]
    fn bridge_data_lock_rejects_nonregular_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("directory-lock");
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join(".native-host.lock")).unwrap();
        assert!(lock_data_directory(&directory).is_err());
        let file = temporary.path().join("ordinary-file");
        fs::write(&file, b"data").unwrap();
        assert!(lock_data_directory(&file).is_err());
        #[cfg(unix)]
        {
            let root = temporary.path().join("symlink-lock");
            fs::create_dir(&root).unwrap();
            std::os::unix::fs::symlink(&file, root.join(".native-host.lock")).unwrap();
            assert!(lock_data_directory(&root).is_err());
            assert_eq!(fs::read(&file).unwrap(), b"data");
        }
    }

    fn imported_completed_ledger() -> Ledger {
        let project = "00000000-0000-0000-0000-000000000001";
        let scope = seecut_flow_core::runner::Scope::synthetic();
        let mut execution = Execution::new(project.into(), project.into());
        let id = execution
            .begin(
                GenerationDraft {
                    context: Context {
                        document_id: project.into(),
                        graph_id: "main".into(),
                        node_id: "node".into(),
                        revision: 1,
                        scope: scope.clone(),
                    },
                    model_id: "synthetic-model".into(),
                    capability_version: "catalog-revision".into(),
                    media_kind: MediaKind::Image,
                    prompt: "synthetic".into(),
                    parameters: json!({}),
                    references: vec![],
                },
                "a".repeat(64),
            )
            .unwrap();
        let body = json!({"model":"synthetic-model","prompt":"synthetic","quote_id":"original-quote","quantity":1});
        execution.intent(&id, body.clone()).unwrap();
        let task = json!({"id":"gen_original_task","kind":"image","model":"synthetic-model","request":body,"status":"succeeded","captured_credits":3,"outputs":[{"id":"original-output","sha256":"b".repeat(64)}]});
        execution.observe(&id, &scope, &task).unwrap();
        execution
            .registered(
                &id,
                "gen_original_task",
                "original-output",
                FlowAsset {
                    id: "old-machine-asset".into(),
                    version:
                        json!({"sha256":"b".repeat(64),"registration_id":"original-registration"})
                            .to_string(),
                    kind: MediaKind::Image,
                    simulated: false,
                },
            )
            .unwrap();
        execution
            .select(&id, "gen_original_task", "original-output", &scope)
            .unwrap();
        execution.validate(project, project).unwrap();
        Ledger {
            project_id: project.into(),
            input_digest: "c".repeat(64),
            execution,
            dispatched: true,
            quote: json!({"id":"original-quote","credits":3}),
            tasks: BTreeMap::from([("gen_original_task".into(), task)]),
        }
    }
    #[test]
    fn imported_missing_asset_can_register_locally_without_changing_request_or_task() {
        let mut ledger = imported_completed_ledger();
        let mut expected = serde_json::to_value(&ledger).unwrap();
        detach_unavailable_bindings(&mut ledger, |_| false);
        let attempt = &ledger.execution.attempts[0];
        assert_eq!(attempt.phase, Phase::Accepted);
        assert!(attempt.selected.is_none());
        assert!(
            attempt.tasks["gen_original_task"].outputs["original-output"]
                .asset
                .is_none()
        );
        ledger
            .execution
            .validate(&ledger.project_id, &ledger.project_id)
            .unwrap();
        let id = attempt.id.clone();
        let local = FlowAsset {
            id: "new-machine-asset".into(),
            version: json!({"sha256":"b".repeat(64),"registration_id":"local-registration"})
                .to_string(),
            kind: MediaKind::Image,
            simulated: false,
        };
        ledger
            .execution
            .registered(&id, "gen_original_task", "original-output", local.clone())
            .unwrap();
        assert_eq!(ledger.execution.attempts[0].phase, Phase::Finished);
        ledger
            .execution
            .validate(&ledger.project_id, &ledger.project_id)
            .unwrap();
        expected["execution"]["attempts"][0]["selected"] = Value::Null;
        expected["execution"]["attempts"][0]["tasks"]["gen_original_task"]["outputs"]["original-output"]
            ["asset"] = serde_json::to_value(local).unwrap();
        assert_eq!(serde_json::to_value(&ledger).unwrap(), expected);
    }
    #[test]
    fn imported_valid_local_binding_is_preserved() {
        let mut ledger = imported_completed_ledger();
        let before = serde_json::to_value(&ledger).unwrap();
        detach_unavailable_bindings(&mut ledger, |_| true);
        assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
    }
    #[test]
    fn imported_failed_or_rejected_attempt_keeps_its_terminal_phase() {
        let mut ledger = imported_completed_ledger();
        let task = ledger.execution.attempts[0]
            .tasks
            .get_mut("gen_original_task")
            .unwrap();
        task.status = "failed".into();
        task.outputs.clear();
        ledger.execution.attempts[0].selected = None;
        detach_unavailable_bindings(&mut ledger, |_| false);
        assert_eq!(ledger.execution.attempts[0].phase, Phase::Finished);
        let attempt = &mut ledger.execution.attempts[0];
        attempt.phase = Phase::Rejected;
        attempt.tasks.clear();
        attempt.submission_failure = Some(seecut_flow_core::execution::SubmissionFailure {
            status: 409,
            code: "QUOTE_MISMATCH".into(),
            message: "synthetic refusal".into(),
            original_key_absent: true,
        });
        let before = serde_json::to_value(&ledger).unwrap();
        detach_unavailable_bindings(&mut ledger, |_| false);
        assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
        ledger
            .execution
            .validate(&ledger.project_id, &ledger.project_id)
            .unwrap();
    }

    #[test]
    fn bridge_scope_rejects_cross_rpc_account_switch_before_any_business_work() {
        let frozen = json!({"expectedScopeKey":"account-a"});
        for method in [
            "generation.catalog",
            "generation.prepare",
            "generation.find",
            "generation.submit",
            "generation.lookup",
            "generation.task",
            "generation.register",
            "generation.exportProject",
            "generation.importProject",
            "projects.cloud.list",
            "projects.cloud.upload",
            "projects.cloud.download",
            "assets.list",
            "assets.resolve",
        ] {
            assert_eq!(
                check_expected_scope(method, &frozen, "account-b")
                    .unwrap_err()
                    .code,
                "FLOW_ACCOUNT_CHANGED",
                "{method}"
            );
        }
    }
    #[test]
    fn bridge_scope_requires_frozen_identity_for_generation_and_cloud() {
        for method in [
            "generation.catalog",
            "generation.prepare",
            "generation.find",
            "generation.submit",
            "generation.lookup",
            "generation.task",
            "generation.register",
            "generation.exportProject",
            "generation.importProject",
            "projects.cloud.list",
            "projects.cloud.upload",
            "projects.cloud.download",
        ] {
            for params in [
                json!({}),
                json!({"expectedScopeKey":null}),
                json!({"expectedScopeKey":7}),
            ] {
                assert_eq!(
                    check_expected_scope(method, &params, "account-a")
                        .unwrap_err()
                        .code,
                    "FLOW_ACCOUNT_CHANGED",
                    "{method}"
                );
            }
            assert!(
                check_expected_scope(
                    method,
                    &json!({"expectedScopeKey":"account-a"}),
                    "account-a"
                )
                .is_ok(),
                "{method}"
            );
        }
    }
    #[test]
    fn bridge_scope_asset_reads_require_current_identity() {
        assert!(check_expected_scope("assets.list", &json!({}), "account-a").is_err());
        assert!(
            check_expected_scope(
                "assets.resolve",
                &json!({"expectedScopeKey":"account-a"}),
                "account-a"
            )
            .is_ok()
        );
        assert_eq!(
            check_expected_scope(
                "assets.resolve",
                &json!({"expectedScopeKey":"account-b"}),
                "account-a"
            )
            .unwrap_err()
            .code,
            "FLOW_ACCOUNT_CHANGED"
        );
    }
    fn state(root: &Path) -> State {
        State {
            root: root.canonicalize().unwrap(),
            library: root.join("library"),
            token: "private-test-token".into(),
            stop: Arc::new(AtomicBool::new(false)),
            connections: AtomicUsize::new(0),
            projects: Mutex::new(HashSet::new()),
            _directory_lock: None,
        }
    }
    #[test]
    fn bridge_framing_rejects_truncated_and_oversized_requests() {
        assert_eq!(
            read_frame(&mut std::io::Cursor::new(b"{}".to_vec()))
                .unwrap_err()
                .code,
            "INVALID_FRAME"
        );
        assert_eq!(
            read_frame(&mut std::io::Cursor::new(vec![b' '; MAX_FRAME + 1]))
                .unwrap_err()
                .code,
            "INVALID_FRAME"
        );
        assert_eq!(
            read_frame(&mut std::io::Cursor::new(b"not-json\n".to_vec()))
                .unwrap_err()
                .code,
            "INVALID_JSON"
        );
        assert_eq!(
            read_frame(&mut std::io::Cursor::new(b"{\"id\":\"one\"}\n".to_vec())).unwrap()["id"],
            "one"
        );
    }
    #[test]
    fn bridge_private_handshake_rejects_wrong_token_and_never_dispatches() {
        let temporary = tempfile::tempdir().unwrap();
        let state = state(temporary.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            connection(stream, &state).unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(b"{\"type\":\"auth\",\"token\":\"wrong\"}\n{\"id\":\"one\",\"method\":\"generation.submit\",\"params\":{}}\n").unwrap();
        let mut reader = BufReader::new(stream);
        let result = read_frame(&mut reader).unwrap();
        assert_eq!(result, json!({"type":"auth","ok":false}));
        let mut remaining = String::new();
        reader.read_to_string(&mut remaining).unwrap();
        assert!(remaining.is_empty());
        worker.join().unwrap();
    }
    #[test]
    fn bridge_nonblocking_listener_waits_for_delayed_auth_and_request_frames() {
        let temporary = tempfile::tempdir().unwrap();
        let state = state(temporary.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            // 在所有测试平台模拟 accept 继承非阻塞，避免此回归仅在 macOS 可见。
            stream.set_nonblocking(true).unwrap();
            connection(stream, &state).unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        stream
            .write_all(b"{\"type\":\"auth\",\"token\":\"private-test-token\"}\n")
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        assert_eq!(
            read_frame(&mut reader).unwrap(),
            json!({"type":"auth","ok":true})
        );
        std::thread::sleep(Duration::from_millis(50));
        stream
            .write_all(b"{\"id\":\"delayed\",\"method\":\"filesystem.readAny\",\"params\":{}}\n")
            .unwrap();
        let result = read_frame(&mut reader).unwrap();
        assert_eq!(result["id"], "delayed");
        assert_eq!(result["error"]["code"], "METHOD_NOT_FOUND");
        worker.join().unwrap();
    }
    #[test]
    fn bridge_private_rpc_has_one_call_and_an_explicit_allowlist() {
        let temporary = tempfile::tempdir().unwrap();
        let state = state(temporary.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            connection(stream, &state).unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(b"{\"type\":\"auth\",\"token\":\"private-test-token\"}\n{\"id\":\"one\",\"method\":\"filesystem.readAny\",\"params\":{}}\n").unwrap();
        let mut reader = BufReader::new(stream);
        assert_eq!(
            read_frame(&mut reader).unwrap(),
            json!({"type":"auth","ok":true})
        );
        let result = read_frame(&mut reader).unwrap();
        assert_eq!(result["id"], "one");
        assert_eq!(result["error"]["code"], "METHOD_NOT_FOUND");
        worker.join().unwrap();
    }
    #[test]
    fn bridge_paths_reject_cross_project_traversal_and_outside_absolute_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let state = state(temporary.path());
        let first = "00000000-0000-0000-0000-000000000001";
        let second = "00000000-0000-0000-0000-000000000002";
        let a = state.root.join("workspaces").join(first);
        let b = state.root.join("workspaces").join(second);
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("image.png"), b"inside").unwrap();
        fs::write(b.join("image.png"), b"outside").unwrap();
        assert!(state.project_file(first, "image.png", false).is_ok());
        assert!(
            state
                .project_file(first, &format!("../{second}/image.png"), false)
                .is_err()
        );
        assert!(
            state
                .project_file(first, b.join("image.png").to_str().unwrap(), false)
                .is_err()
        );
        assert!(state.project_file(first, "new.zip", true).is_ok());
        assert!(checked_uuid("00000000-0000-0000-0000-00000000000A").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn bridge_paths_reject_symlink_escape_for_upload_and_download() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let state = state(temporary.path());
        let project = "00000000-0000-0000-0000-000000000001";
        let inside = state.root.join("workspaces").join(project);
        let outside = state.root.join("outside");
        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.png"), b"outside").unwrap();
        symlink(&outside, inside.join("escape")).unwrap();
        assert!(
            state
                .project_file(project, "escape/secret.png", false)
                .is_err()
        );
        assert!(state.project_file(project, "escape/new.zip", true).is_err());
    }
}
