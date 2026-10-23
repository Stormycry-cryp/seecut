//! Private native-editor host. This crate does not authorize Agent actions: the existing
//! SeeCut MCP/Task permission and budget gates must run before any internal call.
//! Blocking methods belong on a host worker, never on the GUI event thread.

mod provenance;
mod transport;
pub use provenance::VerifiedBuild;

use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use transport::Connection;
pub use uuid::Uuid;

/// Native host result.
pub type Result<T> = std::result::Result<T, HostError>;

/// Errors intentionally omit raw child logs, credentials and remote payloads.
#[derive(Debug)]
pub enum HostError {
    /// The operating system rejected local I/O or process management.
    Io(std::io::Error),
    /// The build receipt or its binaries do not match the pinned complete build.
    InvalidBuild(&'static str),
    /// Trusted launch configuration violates the host contract.
    InvalidOptions(&'static str),
    /// Process ownership is not implemented for the current platform.
    Unsupported,
    /// This host still owns a live editor, including a failed startup.
    AlreadyRunning,
    /// The requested identity is absent, replaced or already exited.
    StaleInstance,
    /// The editor has not authenticated and completed its initial inspection.
    NotReady,
    /// The application request was rejected before it could be sent.
    NotSent(&'static str),
    /// A read-only operation failed to produce a validated reply.
    ReadFailed(&'static str),
    /// A mutating request may have been applied. Never automatically resend it.
    Unknown(&'static str),
    /// The editor explicitly rejected a read-only operation.
    RemoteRejected,
    /// The requested primitive could bypass close or physical-focus safeguards.
    ForbiddenPrimitive,
}
impl From<std::io::Error> for HostError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for HostError {}

/// Trusted application configuration. Roots must come from the existing SeeCut gate.
#[derive(Clone, Debug)]
pub struct LaunchOptions {
    /// Trusted local receipt from the complete desktop and CLI build.
    pub provenance_path: PathBuf,
    /// Private persistent SeeCut data directory for configuration and recovery.
    pub state_dir: PathBuf,
    /// Optional filesystem read capability granted by the existing application gate.
    pub read_root: Option<PathBuf>,
    /// Optional filesystem write capability granted by the existing application gate.
    pub write_root: Option<PathBuf>,
    /// Maximum wait for a readiness line on the owned child's standard error.
    pub startup_timeout: Duration,
    /// Absolute deadline for each control exchange, at most 55 seconds.
    pub request_timeout: Duration,
}
impl LaunchOptions {
    /// Private state persists recovery files; do not put it in a disposable cache.
    pub fn new(provenance_path: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            provenance_path,
            state_dir,
            read_root: None,
            write_root: None,
            startup_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(55),
        }
    }
}

/// Last observed lifecycle state; `NativeHost::current` refreshes process exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceStatus {
    /// The child has spawned but has not completed readiness and authentication.
    Starting,
    /// Authentication and the initial upstream inspection succeeded.
    Ready,
    /// Startup failed after spawning; the child remains owned for safe recovery.
    StartupFailed,
    /// The host observed and reaped the child process exit.
    Exited,
}

/// Host process identity, independent of upstream transient document indices.
#[derive(Clone, Debug)]
pub struct InstanceInfo {
    /// Fresh host identity; requests from a previous launch cannot reuse it.
    pub id: Uuid,
    /// PID of the child spawned and owned by this host.
    pub pid: u32,
    /// Attempt identity from the validated local build receipt.
    pub build_attempt: String,
    /// Whether startup authenticated and inspected this still-live editor.
    pub ready: bool,
    /// Latest process lifecycle observation.
    pub status: InstanceStatus,
}

/// A close request only succeeds after observing the actual child exit.
#[derive(Debug)]
pub enum CloseOutcome {
    /// The owned child has actually exited and been reaped.
    Exited,
    /// The editor remains alive, possibly waiting for its existing save prompt.
    NeedsUser {
        /// Original upstream inspection, when a follow-up read was available.
        inspection: Option<Value>,
    },
}

struct Running {
    child: Child,
    info: InstanceInfo,
    addr: Option<SocketAddr>,
    token: String,
    token_path: PathBuf,
    runtime_dir: PathBuf,
    conn: Option<Connection>,
    next_id: u64,
    timeout: Duration,
}
impl Drop for Running {
    fn drop(&mut self) {
        // Child::drop detaches. Never kill a user document as a side effect of host drop.
        self.conn = None;
        let _ = fs::remove_file(&self.token_path);
        let _ = fs::remove_dir(&self.runtime_dir);
    }
}

/// One owned complete editor per host. Application integration must hold one shared host.
#[derive(Default)]
pub struct NativeHost {
    running: Option<Running>,
}
impl NativeHost {
    /// Create an empty host; the application must retain one shared owner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh the child status; failed startup still reports its owned PID and identity.
    pub fn current(&mut self) -> Result<Option<InstanceInfo>> {
        if let Some(r) = self.running.as_mut() {
            if r.child.try_wait()?.is_some() {
                r.info.status = InstanceStatus::Exited;
                r.info.ready = false;
                r.conn = None;
                r.addr = None;
                let _ = fs::remove_file(&r.token_path);
            }
            return Ok(Some(r.info.clone()));
        }
        Ok(None)
    }

    /// Launch the verified complete desktop, without document arguments or global profiles.
    /// Failure after spawn retains the child in this host; inspect `current` before recovery.
    #[cfg(unix)]
    pub fn start(&mut self, options: LaunchOptions) -> Result<InstanceInfo> {
        use std::os::unix::process::CommandExt;
        if self
            .current()?
            .is_some_and(|i| i.status != InstanceStatus::Exited)
        {
            return Err(HostError::AlreadyRunning);
        }
        if !options.state_dir.is_absolute()
            || options.startup_timeout.is_zero()
            || options.startup_timeout > Duration::from_secs(300)
            || options.request_timeout.is_zero()
            || options.request_timeout > Duration::from_secs(55)
        {
            return Err(HostError::InvalidOptions(
                "absolute state directory and bounded nonzero timeouts required",
            ));
        }
        let build = VerifiedBuild::from_provenance(&options.provenance_path)?;
        private_dir(&options.state_dir)?;
        let profile = options.state_dir.join("photocraft-profile");
        private_dir(&profile)?;
        let runtime_parent = options.state_dir.join("runtime");
        private_dir(&runtime_parent)?;
        let id = Uuid::new_v4();
        let runtime_dir = runtime_parent.join(id.to_string());
        private_dir(&runtime_dir)?;
        let token_path = runtime_dir.join("control.token");
        let token = match create_token(&token_path) {
            Ok(token) => token,
            Err(error) => {
                let _ = fs::remove_file(&token_path);
                let _ = fs::remove_dir(&runtime_dir);
                return Err(error);
            }
        };
        let mut cmd = Command::new(&build.editor);
        cmd.args(["--control", "0", "--control-token-file"])
            .arg(&token_path)
            .env("PHOTOCRAFT_CONFIG_DIR", &profile)
            .env_remove("SEECUT_PHOTO_HOST_INSTANCE_ID")
            .env("SEECUT_PHOTO_HOST_INSTANCE_ID", id.to_string())
            .env_remove("PHOTOCRAFT_CONTROL_TOKEN")
            .env_remove("PHOTOCRAFT_CONTROL_TOKEN_FILE")
            .env_remove("PHOTOCRAFT_CONTROL_PORT")
            .env_remove("PHOTOCRAFT_AUTOMATION_READ_ROOT")
            .env_remove("PHOTOCRAFT_AUTOMATION_WRITE_ROOT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0);
        for (flag, root) in [
            ("--automation-read-root", &options.read_root),
            ("--automation-write-root", &options.write_root),
        ] {
            if let Some(root) = root {
                if !root.is_absolute() || !root.is_dir() {
                    let _ = fs::remove_file(&token_path);
                    let _ = fs::remove_dir(&runtime_dir);
                    return Err(HostError::InvalidOptions(
                        "automation roots must be existing absolute directories",
                    ));
                }
                cmd.arg(flag).arg(root);
            }
        }
        if let Err(error) = build.recheck_editor() {
            let _ = fs::remove_file(&token_path);
            let _ = fs::remove_dir(&runtime_dir);
            return Err(error);
        }
        let child_result = cmd.spawn();
        let mut child = match child_result {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&token_path);
                let _ = fs::remove_dir(&runtime_dir);
                return Err(error.into());
            }
        };
        let stderr = child.stderr.take().expect("piped stderr");
        let ready = watch_stderr(stderr);
        self.running = Some(Running {
            info: InstanceInfo {
                id,
                pid: child.id(),
                build_attempt: build.attempt,
                ready: false,
                status: InstanceStatus::Starting,
            },
            child,
            addr: None,
            token,
            token_path,
            runtime_dir,
            conn: None,
            next_id: 1,
            timeout: options.request_timeout,
        });
        let result = (|| {
            let addr = ready.recv_timeout(options.startup_timeout).map_err(|_| {
                HostError::NotSent("owned editor readiness timed out or disconnected")
            })??;
            let r = self.running.as_mut().unwrap();
            if r.child.try_wait()?.is_some() {
                return Err(HostError::NotSent("owned editor exited during startup"));
            }
            r.addr = Some(addr);
            // The readiness log is not proof of a working editor. Authenticate and inspect.
            let inspection = call(r, "ui.inspect", json!({}), false)?;
            validate_inspection(&inspection)?;
            r.info.ready = true;
            r.info.status = InstanceStatus::Ready;
            Ok(r.info.clone())
        })();
        if result.is_err() {
            self.running.as_mut().unwrap().info.status = InstanceStatus::StartupFailed;
        }
        result
    }

    #[cfg(not(unix))]
    /// Refuse launches on platforms without implemented process-group ownership.
    pub fn start(&mut self, _options: LaunchOptions) -> Result<InstanceInfo> {
        Err(HostError::Unsupported)
    }

    fn instance(&mut self, id: Uuid) -> Result<&mut Running> {
        self.current()?;
        let r = self.running.as_mut().ok_or(HostError::StaleInstance)?;
        if r.info.id != id || r.info.status == InstanceStatus::Exited {
            return Err(HostError::StaleInstance);
        }
        Ok(r)
    }

    /// Original upstream inspect value. It is not an atomic image/revision snapshot.
    pub fn inspect(&mut self, id: Uuid) -> Result<Value> {
        let r = self.instance(id)?;
        if !r.info.ready {
            return Err(HostError::NotReady);
        }
        let result = call(r, "ui.inspect", json!({}), false)?;
        validate_inspection(&result)?;
        Ok(result)
    }

    /// Internal transport only. The caller must already pass existing MCP/Task gates.
    /// Treat every generic call as a possible write, with no automatic retry.
    pub fn call_internal_after_gate(
        &mut self,
        id: Uuid,
        method: &str,
        mut params: Value,
    ) -> Result<Value> {
        if matches!(method, "auth" | "app.quit" | "ui.focus") {
            return Err(HostError::ForbiddenPrimitive);
        }
        if method == "ui.screenshot" {
            let object = params.as_object_mut().ok_or(HostError::InvalidOptions(
                "screenshot params must be an object",
            ))?;
            object.insert("focus".into(), false.into());
        }
        let r = self.instance(id)?;
        if !r.info.ready {
            return Err(HostError::NotReady);
        }
        call(r, method, params, true)
    }

    /// Use the actual menu close flow. Never answer discard prompts or force close here.
    pub fn close(&mut self, id: Uuid, wait: Duration) -> Result<CloseOutcome> {
        if wait > Duration::from_secs(10) {
            return Err(HostError::InvalidOptions("close wait exceeds 10 seconds"));
        }
        self.call_internal_after_gate(
            id,
            "ui.menu.invoke",
            json!({"command":"file.exit","params":{}}),
        )?;
        let deadline = Instant::now() + wait;
        loop {
            let info = self.current()?.ok_or(HostError::StaleInstance)?;
            if info.status == InstanceStatus::Exited {
                return Ok(CloseOutcome::Exited);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(
                Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        Ok(CloseOutcome::NeedsUser {
            inspection: self.inspect(id).ok(),
        })
    }

    /// Destructive operation: the application caller must have explicit confirmation
    /// covering this instance and possible loss of unsaved documents before calling.
    #[cfg(unix)]
    pub fn terminate_confirmed(&mut self, id: Uuid) -> Result<()> {
        let r = self.instance(id)?;
        // A live owned Child created its own process group; no external PID is accepted.
        let status = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", r.child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() && r.child.try_wait()?.is_none() {
            return Err(HostError::ReadFailed(
                "owned process group termination failed",
            ));
        }
        r.child.wait()?;
        r.info.status = InstanceStatus::Exited;
        r.info.ready = false;
        r.conn = None;
        r.addr = None;
        let _ = fs::remove_file(&r.token_path);
        Ok(())
    }
    #[cfg(not(unix))]
    /// Refuse forced termination on platforms without implemented ownership.
    pub fn terminate_confirmed(&mut self, _id: Uuid) -> Result<()> {
        Err(HostError::Unsupported)
    }
}

fn call(r: &mut Running, method: &str, params: Value, write: bool) -> Result<Value> {
    // No request is ever retried, including reads. Reconnect only before a new call.
    if r.conn.is_none() {
        let id = r.next_id;
        r.next_id += 1;
        r.conn = Some(Connection::connect(
            r.addr.ok_or(HostError::NotReady)?,
            &r.token,
            id,
            r.timeout.min(Duration::from_secs(5)),
        )?);
    }
    let id = r.next_id;
    r.next_id += 1;
    let result = r
        .conn
        .as_mut()
        .unwrap()
        .exchange(id, method, &params, r.timeout, write);
    if result.is_err() {
        r.conn = None;
    }
    result.map(|mut v| {
        redact(&mut v, &r.token);
        v
    })
}

fn redact(value: &mut Value, token: &str) {
    match value {
        Value::String(s) => *s = s.replace(token, "[redacted]"),
        Value::Array(a) => a.iter_mut().for_each(|v| redact(v, token)),
        Value::Object(o) => {
            for (key, mut v) in std::mem::take(o) {
                redact(&mut v, token);
                o.insert(key.replace(token, "[redacted]"), v);
            }
        }
        _ => {}
    }
}

fn validate_inspection(value: &Value) -> Result<()> {
    if !value.is_object()
        || !value["session"]["documents"].is_array()
        || !value["dialogs"].is_array()
        || !value["window"].is_object()
    {
        return Err(HostError::ReadFailed("invalid ui.inspect shape"));
    }
    Ok(())
}

#[cfg(unix)]
fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    // Reject symlinks in existing ancestors, without changing permissions on caller paths.
    for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(meta) = fs::symlink_metadata(ancestor)
            && meta.file_type().is_symlink()
        {
            return Err(HostError::InvalidOptions("state path contains a symlink"));
        }
    }
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(HostError::InvalidOptions(
            "state directory must be private mode 0700",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn create_token(path: &Path) -> Result<String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut entropy = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut entropy)?;
    let token: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
        return Err(HostError::InvalidOptions("token file must be mode 0600"));
    }
    writeln!(file, "{token}")?;
    file.sync_all()?;
    Ok(token)
}

fn parse_ready(line: &[u8]) -> Option<SocketAddr> {
    let line = std::str::from_utf8(line)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    let addr: SocketAddr = line
        .strip_prefix("photocraft: control server listening on ")?
        .parse()
        .ok()?;
    (addr.ip() == std::net::Ipv4Addr::LOCALHOST && addr.port() != 0).then_some(addr)
}

fn watch_stderr(stderr: std::process::ChildStderr) -> Receiver<Result<SocketAddr>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        let mut bytes_seen = 0usize;
        let mut finished = false;
        loop {
            let Ok(buf) = reader.fill_buf() else {
                break;
            };
            if buf.is_empty() {
                break;
            }
            let n = buf
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |n| n + 1);
            let ended = buf[n - 1] == b'\n';
            bytes_seen = bytes_seen.saturating_add(n);
            if !finished {
                if bytes_seen > 1024 * 1024 || line.len() + n > 8192 {
                    let _ = tx.try_send(Err(HostError::NotSent(
                        "owned editor readiness log exceeds budget",
                    )));
                    finished = true;
                } else {
                    line.extend_from_slice(&buf[..n]);
                    if ended {
                        if let Some(addr) = parse_ready(&line) {
                            let _ = tx.try_send(Ok(addr));
                            finished = true;
                        }
                        line.clear();
                    }
                }
            }
            reader.consume(n);
            // Continue draining bounded chunks after readiness, never forward raw stderr.
        }
        if !finished {
            let _ = tx.try_send(Err(HostError::NotSent(
                "owned editor ended without a readiness frame",
            )));
        }
    });
    rx
}

#[cfg(all(test, unix))]
mod tests;
