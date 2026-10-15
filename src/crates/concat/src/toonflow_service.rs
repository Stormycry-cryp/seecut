//! 实验工作区的本地服务。子进程、数据和日志均由当前应用实例管理。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

struct StartupState {
    cancelled: AtomicBool,
    service: Mutex<Option<Service>>,
}

pub(crate) struct Startup {
    state: Arc<StartupState>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Startup {
    fn drop(&mut self) {
        self.state.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.state
            .service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

pub(crate) fn start_async(
    bridge: &crate::toonflow_bridge::Bridge,
    app: &crate::ui::App,
) -> Startup {
    let state = Arc::new(StartupState {
        cancelled: AtomicBool::new(false),
        service: Mutex::new(None),
    });
    let worker_state = state.clone();
    let endpoint = bridge.endpoint().to_owned();
    let token = bridge.token().to_owned();
    let weak = app.as_weak();
    let worker = std::thread::spawn(move || {
        let result = start(&endpoint, &token, &worker_state.cancelled);
        if worker_state.cancelled.load(Ordering::Acquire) {
            return;
        }
        let endpoint = match result {
            Ok(service) => {
                let endpoint = service.url().to_owned();
                *worker_state
                    .service
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(service);
                Ok(endpoint)
            }
            Err(error) => Err(error),
        };
        let callback_state = worker_state.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if !callback_state.cancelled.load(Ordering::Acquire)
                && let Some(app) = weak.upgrade()
            {
                crate::toonflow_webview::configure(&app, endpoint);
            }
        });
    });
    Startup {
        state,
        worker: Some(worker),
    }
}

pub struct Service {
    url: String,
    child: Option<Child>,
}

impl Service {
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            #[cfg(unix)]
            {
                // 先让 Bun 清理 MCP 运行记录和监听器，再收回仍未退出的进程。
                unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn loopback_url(value: &str) -> Result<String, String> {
    let parsed = url::Url::parse(value).map_err(|_| "Toonflow 地址格式无效".to_owned())?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("Toonflow 实验服务必须使用带端口的本机回环地址".into());
    }
    Ok(value.to_owned())
}

fn runtime_root() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("SEECUT_TOONFLOW_RUNTIME") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(contents) = exe.parent().and_then(Path::parent) {
        let bundled = contents.join("Resources/toonflow");
        if bundled.join("build/server/seecutHost.js").is_file() {
            return Ok(bundled);
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    Ok(root.join("external/toonflow"))
}

pub(crate) fn data_root() -> Result<PathBuf, String> {
    let data = match std::env::var_os("SEECUT_TOONFLOW_DATA_DIR") {
        Some(path) => PathBuf::from(path),
        None => concat_host::AppDirs::locate()?
            .data
            .join("toonflow-experiment"),
    };
    std::fs::create_dir_all(&data).map_err(|e| format!("无法创建 Toonflow 实验目录：{e}"))?;
    data.canonicalize().map_err(|error| error.to_string())
}

fn start(endpoint: &str, token: &str, cancelled: &AtomicBool) -> Result<Service, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("Flow 启动已取消。".into());
    }
    if let Ok(url) = std::env::var("SEECUT_TOONFLOW_URL") {
        return Ok(Service {
            url: loopback_url(&url)?,
            child: None,
        });
    }
    if !cfg!(any(target_os = "macos", target_os = "windows")) {
        return Err("本轮 Toonflow 嵌入实验仅支持 macOS 和 Windows 桌面".into());
    }
    let root = runtime_root()?;
    let entry = root.join("build/server/seecutHost.js");
    if !entry.is_file() {
        return Err("Toonflow 运行资源尚未构建，请执行 scripts/build-toonflow.py".into());
    }
    let bundled_bun = root.join(if cfg!(windows) {
        "bin/bun.exe"
    } else {
        "bin/bun"
    });
    let bun = std::env::var_os("SEECUT_BUN")
        .map(PathBuf::from)
        .or_else(|| bundled_bun.is_file().then_some(bundled_bun))
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../.toonflow/bin/bun")
        });
    let data = data_root()?;
    let ready = data.join(format!("ready-{}.json", uuid::Uuid::new_v4()));
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("service.log"))
        .map_err(|e| e.to_string())?;
    let stderr = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(bun);
    command.env_clear();
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATH",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "TEMP",
        "SystemRoot",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("Flow 启动已取消。".into());
    }
    let child = command
        .arg("--no-install")
        .arg("--no-env-file")
        .arg(&entry)
        .current_dir(&root)
        .env("NODE_ENV", "production")
        .env("SEECUT_HOST_ENDPOINT", endpoint)
        .env("SEECUT_HOST_TOKEN", token)
        .env("TOONFLOW_DATA_DIR", &data)
        .env("SEECUT_TOONFLOW_READY_FILE", &ready)
        .env("SEECUT_PARENT_PID", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("无法启动 Toonflow 本地服务：{e}"))?;
    let mut service = Service {
        url: String::new(),
        child: Some(child),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = std::fs::remove_file(&ready);
            return Err("Flow 启动已取消。".into());
        }
        if ready.is_file() {
            let result = std::fs::read(&ready)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| e.to_string())
                })
                .and_then(|value| {
                    value
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                        .ok_or_else(|| "服务就绪信息缺少地址".into())
                })
                .and_then(|url| loopback_url(&url));
            let _ = std::fs::remove_file(&ready);
            service.url = result?;
            return Ok(service);
        }
        if let Some(status) = service
            .child
            .as_mut()
            .expect("owned child")
            .try_wait()
            .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "Toonflow 服务提前退出（{status}），详情见实验目录 service.log"
            ));
        }
        if Instant::now() >= deadline {
            return Err("Toonflow 服务启动超时，详情见实验目录 service.log".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
