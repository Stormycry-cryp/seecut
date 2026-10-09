// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI-independent, bounded JSONL bridge to an explicitly selected local runtime.
//! No process starts by default. Initialization (including credentials) is a stdin frame.
//! Desktop Unix only; the handle never joins a worker or waits for pipe I/O.

use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FRAME_BYTES: usize = 4 * 1024 * 1024;
const QUEUE_BYTES: usize = 8 * 1024 * 1024;
const QUEUE_FRAMES: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Construct only from trusted host configuration, never model/user message fields.
/// Both files must be absolute ordinary files, not symlinks. No arbitrary argv is accepted.
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub entrypoint: PathBuf,
    /// Explicit values only: TMPDIR, XDG_RUNTIME_DIR, LANG, LC_ALL, TZ.
    /// NODE_OPTIONS/NODE_PATH, tokens, provider keys and the ambient environment are excluded.
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeError {
    #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
    UnsupportedPlatform,
    InvalidLaunch,
    InvalidFrame,
    QueueBusy,
    InputBackpressure,
    OutputBackpressure,
    Closed,
    SpawnFailed,
    PipeFailed,
    ProtocolFailed,
    ShutdownTimeout,
    ExitRegistrationFailed,
}

/// No raw stderr, initialization parameters or error strings are retained.
pub enum Event {
    Response(Value),
    Runtime(Value),
    Managed(Value),
    Result(Value),
    RuntimeDiagnostic {
        code: String,
    },
    /// Emitted at most once; the child is stopped and reaped afterwards.
    ProtocolFailure(BridgeError),
    /// Emitted exactly once, after owned-child wait/reap. No automatic restart follows.
    Exit {
        code: Option<i32>,
        requested: bool,
    },
}

struct InputQueue {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    count: usize,
    shutdown_id: Option<String>,
}
struct OutputQueue {
    events: VecDeque<(Event, usize)>,
    bytes: usize,
    // Reserved terminal slots survive a full normal queue. No accepted result is silently dropped.
    failure: Option<BridgeError>,
    failure_delivered: bool,
    exit: Option<Event>,
}
struct Shared {
    input: Mutex<InputQueue>,
    output: Mutex<OutputQueue>,
    stopping: AtomicBool,
    requested: AtomicBool,
    exited: AtomicBool,
}

pub struct Handle {
    shared: Arc<Shared>,
}

impl Handle {
    /// Serializes and enqueues only; never waits for child stdin or a contended mutex.
    /// A full queue rejects this request before dispatch. The caller must not infer execution.
    pub fn try_send(&self, id: &str, method: &str, params: Value) -> Result<(), BridgeError> {
        if self.shared.stopping.load(Ordering::Acquire)
            || self.shared.exited.load(Ordering::Acquire)
        {
            return Err(BridgeError::Closed);
        }
        if id.is_empty()
            || id.chars().count() > 100
            || !params.is_object()
            || !matches!(
                method,
                "initialize"
                    | "status"
                    | "session.create"
                    | "session.restore"
                    | "session.close"
                    | "send"
                    | "stop"
                    | "approve"
                    | "reconcile"
                    | "shutdown"
            )
        {
            return Err(BridgeError::InvalidFrame);
        }
        let mut frame = serde_json::to_vec(&json!({
            "version": 1, "id": id, "method": method, "params": params
        }))
        .map_err(|_| BridgeError::InvalidFrame)?;
        if frame.len() > FRAME_BYTES {
            return Err(BridgeError::InvalidFrame);
        }
        frame.push(b'\n');
        let mut queue = self
            .shared
            .input
            .try_lock()
            .map_err(|_| BridgeError::QueueBusy)?;
        // Fence again after taking the queue so a late UI send cannot revive shutdown.
        if self.shared.stopping.load(Ordering::Acquire)
            || self.shared.exited.load(Ordering::Acquire)
            || queue.shutdown_id.is_some()
        {
            return Err(BridgeError::Closed);
        }
        if queue.count >= QUEUE_FRAMES || queue.bytes + frame.len() > QUEUE_BYTES {
            return Err(BridgeError::InputBackpressure);
        }
        queue.bytes += frame.len();
        queue.count += 1;
        if method == "shutdown" {
            queue.shutdown_id = Some(id.to_owned());
        }
        queue.frames.push_back(frame);
        Ok(())
    }

    /// Poll from the UI. Contention returns None; call again on the next UI tick.
    pub fn try_recv(&self) -> Option<Event> {
        let mut queue = self.shared.output.try_lock().ok()?;
        if let Some((event, bytes)) = queue.events.pop_front() {
            queue.bytes -= bytes;
            return Some(event);
        }
        if !queue.failure_delivered
            && let Some(error) = queue.failure
        {
            queue.failure_delivered = true;
            return Some(Event::ProtocolFailure(error));
        }
        queue.exit.take()
    }

    /// Starts an asynchronous, at-most-three-second grace period, then owned-group kill/reap.
    /// A partial frame is abandoned by closing stdin, never replayed.
    pub fn request_shutdown(&self) {
        self.shared.requested.store(true, Ordering::Release);
        self.shared.stopping.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub fn has_exited(&self) -> bool {
        self.shared.exited.load(Ordering::Acquire)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

fn valid_file(path: &Path) -> bool {
    path.is_absolute()
        && std::fs::symlink_metadata(path)
            .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            .unwrap_or(false)
}

fn validate_launch(spec: &LaunchSpec) -> Result<(), BridgeError> {
    if !valid_file(&spec.executable) || !valid_file(&spec.entrypoint) {
        return Err(BridgeError::InvalidLaunch);
    }
    for (key, value) in &spec.environment {
        if !matches!(
            key.as_str(),
            "TMPDIR" | "XDG_RUNTIME_DIR" | "LANG" | "LC_ALL" | "TZ"
        ) || value.contains('\0')
            || value.len() > 4096
        {
            return Err(BridgeError::InvalidLaunch);
        }
        if matches!(key.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR")
            && (!Path::new(value).is_absolute() || !Path::new(value).is_dir())
        {
            return Err(BridgeError::InvalidLaunch);
        }
    }
    Ok(())
}

/// File validation is synchronous; spawning and all subsequent I/O happen on the manager thread.
/// The caller should perform first-time file selection/validation away from latency-sensitive UI.
pub fn start(spec: LaunchSpec) -> Result<Handle, BridgeError> {
    #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
    {
        let _ = spec;
        Err(BridgeError::UnsupportedPlatform)
    }
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    {
        use std::os::unix::fs::PermissionsExt;
        validate_launch(&spec)?;
        if std::fs::metadata(&spec.executable)
            .map_err(|_| BridgeError::InvalidLaunch)?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err(BridgeError::InvalidLaunch);
        }
        let shared = Arc::new(Shared {
            input: Mutex::new(InputQueue {
                frames: VecDeque::new(),
                bytes: 0,
                count: 0,
                shutdown_id: None,
            }),
            output: Mutex::new(OutputQueue {
                events: VecDeque::new(),
                bytes: 0,
                failure: None,
                failure_delivered: false,
                exit: None,
            }),
            stopping: AtomicBool::new(false),
            requested: AtomicBool::new(false),
            exited: AtomicBool::new(false),
        });
        let manager_shared = Arc::clone(&shared);
        exit_registry::register(&shared)?;
        std::thread::Builder::new()
            .name("agent-process".into())
            .spawn(move || desktop::manage(spec, manager_shared))
            .map_err(|_| BridgeError::SpawnFailed)?;
        Ok(Handle { shared })
    }
}

/// Call only during host process exit, after the UI event loop has returned.
/// Normal UI shutdown uses Handle::request_shutdown instead. Idempotent, at most four seconds
/// from the first call; libc atexit invokes the same path for Cocoa/process::exit termination.
pub fn shutdown_all_at_exit() -> bool {
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    {
        exit_registry::shutdown()
    }
    #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
    {
        true
    }
}

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
mod exit_registry {
    use super::*;
    use std::sync::{OnceLock, Weak};
    const MAX_MANAGERS: usize = 64;
    static REGISTRY: Mutex<Vec<Weak<Shared>>> = Mutex::new(Vec::new());
    static HOOK: OnceLock<Result<(), BridgeError>> = OnceLock::new();
    static EXITING: AtomicBool = AtomicBool::new(false);
    static DEADLINE: OnceLock<Instant> = OnceLock::new();

    extern "C" fn at_exit() {
        // The callback must never unwind through libc. It performs no UI, log or PID operations.
        let _ = std::panic::catch_unwind(shutdown);
    }
    pub(super) fn register(shared: &Arc<Shared>) -> Result<(), BridgeError> {
        let installed = HOOK.get_or_init(|| {
            if unsafe { libc::atexit(at_exit) } == 0 {
                Ok(())
            } else {
                Err(BridgeError::ExitRegistrationFailed)
            }
        });
        (*installed)?;
        let mut registry = REGISTRY.try_lock().map_err(|_| BridgeError::QueueBusy)?;
        if is_exiting() {
            return Err(BridgeError::Closed);
        }
        registry.retain(|entry| entry.strong_count() > 0);
        if registry.len() >= MAX_MANAGERS {
            return Err(BridgeError::ExitRegistrationFailed);
        }
        registry.push(Arc::downgrade(shared));
        Ok(())
    }
    pub(super) fn is_exiting() -> bool {
        EXITING.load(Ordering::Acquire)
    }
    pub(super) fn shutdown() -> bool {
        let deadline = *DEADLINE.get_or_init(|| Instant::now() + Duration::from_secs(4));
        EXITING.store(true, Ordering::Release);
        // Managers also observe EXITING independently, so a temporarily contended registry lock
        // cannot prevent notification. The registry itself owns only Weak references, never PIDs.
        let owners = loop {
            match REGISTRY.try_lock() {
                Ok(registry) => {
                    break registry
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect::<Vec<_>>();
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    break error
                        .into_inner()
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect::<Vec<_>>();
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(_) => return false,
            }
        };
        for owner in &owners {
            owner.requested.store(true, Ordering::Release);
            owner.stopping.store(true, Ordering::Release);
        }
        loop {
            if owners
                .iter()
                .all(|owner| owner.exited.load(Ordering::Acquire))
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn protocol_frame(line: &[u8]) -> Result<Event, BridgeError> {
    // serde_json also enforces UTF-8 and bounded recursive depth.
    let value: Value = serde_json::from_slice(line).map_err(|_| BridgeError::ProtocolFailed)?;
    if value.get("version").and_then(Value::as_u64) != Some(1) || !value.is_object() {
        return Err(BridgeError::ProtocolFailed);
    }
    let valid_id = |value: &Value| {
        value
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.chars().count() <= 100)
    };
    let event = match value.get("type").and_then(Value::as_str) {
        Some("response") => {
            let id_valid = value
                .get("id")
                .is_some_and(|id| valid_id(id) || id.is_null());
            let result = value.get("result").is_some();
            let error = value.get("error").is_some_and(|error| {
                error.get("code").and_then(Value::as_str).is_some()
                    && error.get("message").and_then(Value::as_str).is_some()
            });
            if !id_valid || result == error {
                return Err(BridgeError::ProtocolFailed);
            }
            Event::Response(value)
        }
        Some("event") => {
            let event = &value["event"];
            if event.get("schemaVersion").and_then(Value::as_u64) != Some(1)
                || event.get("seq").and_then(Value::as_u64).is_none()
                || event.get("sessionId").and_then(Value::as_str).is_none()
                || event.get("type").and_then(Value::as_str).is_none()
                || !event.get("data").is_some_and(Value::is_object)
            {
                return Err(BridgeError::ProtocolFailed);
            }
            Event::Runtime(value)
        }
        Some("result") => {
            let result = &value["result"];
            if value.get("sessionId").and_then(Value::as_str).is_none()
                || result.get("turnId").and_then(Value::as_str).is_none()
                || result.get("text").and_then(Value::as_str).is_none()
                || !matches!(
                    result.get("status").and_then(Value::as_str),
                    Some("completed" | "failed" | "interrupted")
                )
                || !value.get("toolState").is_some_and(Value::is_object)
            {
                return Err(BridgeError::ProtocolFailed);
            }
            Event::Result(value)
        }
        Some("managed.state") => {
            if line.len() > 64 * 1024
                || !value.get("sessionId").is_some_and(valid_id)
                || !value.get("modelState").is_some_and(Value::is_object)
            {
                return Err(BridgeError::ProtocolFailed);
            }
            Event::Managed(value)
        }
        Some("diagnostic") => {
            let code = value
                .get("code")
                .and_then(Value::as_str)
                .ok_or(BridgeError::ProtocolFailed)?;
            if code.len() > 100
                || !code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(BridgeError::ProtocolFailed);
            }
            Event::RuntimeDiagnostic {
                code: code.to_owned(),
            }
        }
        _ => return Err(BridgeError::ProtocolFailed),
    };
    Ok(event)
}

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
mod desktop {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    fn failure(shared: &Shared, error: BridgeError) {
        let mut output = shared
            .output
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if output.failure.is_none() {
            output.failure = Some(error);
        }
        shared.stopping.store(true, Ordering::Release);
    }
    fn exit(shared: &Shared, code: Option<i32>) {
        let mut output = shared
            .output
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        output.exit = Some(Event::Exit {
            code,
            requested: shared.requested.load(Ordering::Acquire),
        });
        shared.exited.store(true, Ordering::Release);
    }
    fn nonblocking(fd: i32) -> Result<(), BridgeError> {
        // SAFETY: fd is an owned, live child pipe; fcntl doesn't retain pointers.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(BridgeError::PipeFailed);
        }
        Ok(())
    }
    fn enqueue(shared: &Shared, event: Event, bytes: usize) -> Result<(), BridgeError> {
        let mut queue = shared
            .output
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if queue.events.len() >= QUEUE_FRAMES || queue.bytes + bytes > QUEUE_BYTES {
            return Err(BridgeError::OutputBackpressure);
        }
        queue.bytes += bytes;
        queue.events.push_back((event, bytes));
        Ok(())
    }
    fn kill_group(pid: u32) {
        // SAFETY: the child started in a fresh group whose ID equals its PID.
        // The child is still unreaped, preventing PID reuse while sending this signal.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }

    fn exited_without_reaping(pid: u32) -> Result<bool, BridgeError> {
        // Keep the child unreaped until group cleanup, preventing PID reuse while signalling.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            Ok(info.si_signo != 0)
        } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(BridgeError::PipeFailed)
        }
    }

    pub(super) fn manage(spec: LaunchSpec, shared: Arc<Shared>) {
        if shared.stopping.load(Ordering::Acquire) || exit_registry::is_exiting() {
            shared.requested.store(true, Ordering::Release);
            exit(&shared, None);
            return;
        }
        let spawn = Command::new(&spec.executable)
            .arg(&spec.entrypoint)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        let mut child = match spawn {
            Ok(child) => child,
            Err(_) => {
                failure(&shared, BridgeError::SpawnFailed);
                exit(&shared, None);
                return;
            }
        };
        let pid = child.id();
        let mut stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let pipes = [
            stdin.as_ref().unwrap().as_raw_fd(),
            stdout.as_raw_fd(),
            stderr.as_raw_fd(),
        ];
        if pipes.into_iter().any(|fd| nonblocking(fd).is_err()) {
            failure(&shared, BridgeError::PipeFailed);
            kill_group(pid);
            let _ = child.kill();
            let status = child.wait().ok();
            exit(&shared, status.and_then(|status| status.code()));
            return;
        }
        let mut pending: Option<(Vec<u8>, usize)> = None;
        let mut buffer = Vec::new();
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut shutdown_started: Option<Instant> = None;
        let mut child_done = false;
        let mut read_buffer = [0u8; 8192];
        loop {
            if exit_registry::is_exiting() {
                shared.requested.store(true, Ordering::Release);
                shared.stopping.store(true, Ordering::Release);
            }
            if shared.stopping.load(Ordering::Acquire) && shutdown_started.is_none() {
                shutdown_started = Some(Instant::now());
                // Closing the pipe cannot block, including when a partial frame is in flight.
                stdin.take();
                pending.take();
                let mut input = shared
                    .input
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                input.frames.clear();
                input.bytes = 0;
                input.count = 0;
            }
            if !child_done {
                match exited_without_reaping(pid) {
                    Ok(true) => {
                        child_done = true;
                        stdin.take();
                    }
                    Ok(false) => {}
                    Err(_) => {
                        failure(&shared, BridgeError::PipeFailed);
                    }
                }
            }
            if child_done && stdout_eof {
                break;
            }
            if shutdown_started.is_some_and(|start| start.elapsed() >= SHUTDOWN_TIMEOUT) {
                if !child_done {
                    failure(&shared, BridgeError::ShutdownTimeout);
                }
                break;
            }
            // A descendant retaining a pipe after an early child exit cannot hold the worker forever.
            if child_done && shutdown_started.is_none() {
                shutdown_started = Some(Instant::now());
            }
            if pending.is_none() && stdin.is_some() && !shared.stopping.load(Ordering::Acquire) {
                let mut input = shared
                    .input
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                pending = input.frames.pop_front().map(|frame| (frame, 0));
            }
            if let (Some(pipe), Some((frame, offset))) = (stdin.as_mut(), pending.as_mut()) {
                match pipe.write(&frame[*offset..]) {
                    Ok(0) => {
                        failure(&shared, BridgeError::PipeFailed);
                    }
                    Ok(bytes) => {
                        *offset += bytes;
                        if *offset == frame.len() {
                            let bytes = frame.len();
                            pending.take();
                            let mut input = shared
                                .input
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            input.bytes = input.bytes.saturating_sub(bytes);
                            input.count = input.count.saturating_sub(1);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => {
                        failure(&shared, BridgeError::PipeFailed);
                    }
                }
            }
            // Bound per-pass work so a continuous stderr/stdout producer cannot starve shutdown.
            for _ in 0..16 {
                if stdout_eof {
                    break;
                }
                match stdout.read(&mut read_buffer) {
                    Ok(0) => {
                        stdout_eof = true;
                        shared.stopping.store(true, Ordering::Release);
                        if !buffer.is_empty() {
                            failure(&shared, BridgeError::ProtocolFailed);
                        }
                        break;
                    }
                    Ok(bytes) => {
                        for byte in &read_buffer[..bytes] {
                            if *byte == b'\n' {
                                match protocol_frame(&buffer).and_then(|event| {
                                    if let Event::Response(value) = &event {
                                        let input = shared
                                            .input
                                            .lock()
                                            .unwrap_or_else(|error| error.into_inner());
                                        if value.get("result").is_some()
                                            && input.shutdown_id.as_deref().is_some_and(|id| {
                                                !id.is_empty()
                                                    && value.get("id").and_then(Value::as_str)
                                                        == Some(id)
                                            })
                                        {
                                            shared.requested.store(true, Ordering::Release);
                                            shared.stopping.store(true, Ordering::Release);
                                        }
                                    }
                                    enqueue(&shared, event, buffer.len())
                                }) {
                                    Ok(()) => {}
                                    Err(error) => {
                                        failure(&shared, error);
                                    }
                                }
                                buffer.clear();
                            } else if buffer.len() < FRAME_BYTES {
                                buffer.push(*byte);
                            } else {
                                failure(&shared, BridgeError::ProtocolFailed);
                                buffer.clear();
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        failure(&shared, BridgeError::PipeFailed);
                        stdout_eof = true;
                        break;
                    }
                }
            }
            for _ in 0..16 {
                if stderr_eof {
                    break;
                }
                match stderr.read(&mut read_buffer) {
                    Ok(0) => {
                        stderr_eof = true;
                        break;
                    }
                    Ok(_) => {} // Drain without retaining or displaying raw diagnostic bytes.
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        stderr_eof = true;
                        break;
                    }
                }
            }
            // poll sleeps only on the manager, and at most 20ms. It never runs on the UI thread.
            let mut fds = [
                libc::pollfd {
                    fd: if stdout_eof { -1 } else { stdout.as_raw_fd() },
                    events: if stdout_eof { 0 } else { libc::POLLIN },
                    revents: 0,
                },
                libc::pollfd {
                    fd: if stderr_eof { -1 } else { stderr.as_raw_fd() },
                    events: if stderr_eof { 0 } else { libc::POLLIN },
                    revents: 0,
                },
            ];
            unsafe {
                libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 20);
            }
        }
        kill_group(pid);
        let _ = child.kill();
        let child_status = child.wait().ok();
        exit(&shared, child_status.and_then(|status| status.code()));
    }
}

#[cfg(all(test, unix, not(any(target_os = "ios", target_os = "android"))))]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;

    struct Fixture {
        directory: PathBuf,
        handle: Option<Handle>,
    }
    impl Fixture {
        fn python(script: &str) -> Self {
            // System Python is an explicit offline fixture, never a runtime/provider dependency.
            let executable = ["/usr/bin/python3", "/usr/local/bin/python3"]
                .iter()
                .find_map(|path| fs::canonicalize(path).ok())
                .expect("Python3 fixture executable");
            static NEXT_FIXTURE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(1);
            let directory = std::env::temp_dir().join(format!(
                "agent-process-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let entrypoint = directory.join("fixture.py");
            fs::write(&entrypoint, script).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let handle = loop {
                match start(LaunchSpec {
                    executable: executable.clone(),
                    entrypoint: entrypoint.clone(),
                    environment: BTreeMap::new(),
                }) {
                    Ok(handle) => break handle,
                    // QueueBusy from registration precedes worker spawn, so this fixture
                    // can retry safely without starting or replaying a second runtime.
                    Err(BridgeError::QueueBusy) => {
                        assert!(Instant::now() < deadline, "fixture launch remained busy");
                        thread::yield_now();
                    }
                    Err(error) => panic!("fixture launch failed: {error:?}"),
                }
            };
            Self {
                directory,
                handle: Some(handle),
            }
        }
        fn handle(&self) -> &Handle {
            self.handle.as_ref().unwrap()
        }
        fn until(&self, condition: impl Fn(&Event) -> bool) -> Vec<Event> {
            let deadline = Instant::now() + Duration::from_secs(6);
            let mut events = Vec::new();
            while Instant::now() < deadline {
                if let Some(event) = self.handle().try_recv() {
                    let done = condition(&event);
                    events.push(event);
                    if done {
                        return events;
                    }
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            panic!("fixture timeout");
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.handle.take();
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn managed_frames_require_a_bounded_session_and_object() {
        let valid =
            json!({"version":1,"type":"managed.state","sessionId":"session-1","modelState":{}});
        assert!(matches!(
            protocol_frame(&serde_json::to_vec(&valid).unwrap()),
            Ok(Event::Managed(_))
        ));
        for (key, value) in [
            ("sessionId", json!("")),
            ("sessionId", json!("x".repeat(101))),
            ("modelState", json!(null)),
            ("modelState", json!({"large":"x".repeat(65536)})),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(matches!(
                protocol_frame(&serde_json::to_vec(&invalid).unwrap()),
                Err(BridgeError::ProtocolFailed)
            ));
        }
    }
    #[test]
    fn bidirectional_response_and_runtime_events() {
        let fixture = Fixture::python(
            r#"import sys,json
for line in sys.stdin:
    frame=json.loads(line)
    print(json.dumps({'version':1,'type':'response','id':frame['id'],'result':{'ok':True}}),flush=True)
    print(json.dumps({'version':1,'type':'event','event':{'schemaVersion':1,'seq':1,'sessionId':'s','type':'tool.progress','data':{'progress':1}}}),flush=True)
"#,
        );
        fixture
            .handle()
            .try_send("one", "status", json!({}))
            .unwrap();
        let events = fixture.until(|event| matches!(event, Event::Runtime(_)));
        assert!(matches!(events[0], Event::Response(_)));
        fixture.handle().request_shutdown();
        let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ProtocolFailure(_)))
        );
        assert!(fixture.handle().has_exited());
        assert!(matches!(
            fixture.handle().try_send("late", "status", json!({})),
            Err(BridgeError::Closed)
        ));
    }

    #[test]
    fn invalid_utf8_json_version_and_partial_lines_fail_once() {
        for script in [
            "import sys;sys.stdout.buffer.write(b'\\xff\\n');sys.stdout.flush()",
            "print('not-json',flush=True)",
            "print('{\"version\":2,\"type\":\"response\",\"id\":\"x\",\"result\":{}}',flush=True)",
            "import sys;sys.stdout.write('{');sys.stdout.flush()",
        ] {
            let fixture = Fixture::python(script);
            let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, Event::ProtocolFailure(_)))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn oversize_line_is_bounded_and_terminated() {
        let fixture = Fixture::python(
            "import sys,time;sys.stdout.write('x'*(4*1024*1024+1));sys.stdout.flush();time.sleep(20)",
        );
        let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ProtocolFailure(BridgeError::ProtocolFailed)))
        );
    }

    #[test]
    fn continuous_stderr_is_drained_without_content_events() {
        let fixture = Fixture::python(
            r#"import sys,json
sys.stderr.write('secret-never-retained\n'*200000);sys.stderr.flush()
print(json.dumps({'version':1,'type':'response','id':'ready','result':{}}),flush=True)
"#,
        );
        let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Response(_)))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::RuntimeDiagnostic { .. } | Event::ProtocolFailure(_)
        )));
    }

    #[test]
    fn output_backpressure_has_reserved_failure_and_exit() {
        let fixture = Fixture::python(
            r#"import sys,json,time
for i in range(100): print(json.dumps({'version':1,'type':'response','id':str(i),'result':{}}),flush=True)
time.sleep(20)
"#,
        );
        thread::sleep(Duration::from_millis(300));
        let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::ProtocolFailure(BridgeError::OutputBackpressure)
        )));
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Response(_)))
                .count()
                <= QUEUE_FRAMES
        );
    }

    #[test]
    fn blocked_stdin_and_drop_do_not_block_ui_and_child_is_reaped() {
        let mut fixture = Fixture::python("import time;time.sleep(20)");
        fixture
            .handle()
            .try_send(
                "large",
                "initialize",
                json!({"blob":"x".repeat(2*1024*1024)}),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        let shared = Arc::clone(&fixture.handle().shared);
        let now = Instant::now();
        drop(fixture.handle.take());
        assert!(now.elapsed() < Duration::from_millis(100));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.exited.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.exited.load(Ordering::Acquire));
    }

    #[test]
    fn input_backpressure_rejects_before_dispatch() {
        let fixture = Fixture::python("import time;time.sleep(20)");
        let mut rejected = false;
        for index in 0..100 {
            match fixture.handle().try_send(
                &index.to_string(),
                "initialize",
                json!({"blob":"x".repeat(256*1024)}),
            ) {
                Err(BridgeError::InputBackpressure) => {
                    rejected = true;
                    break;
                }
                Err(BridgeError::QueueBusy) | Ok(()) => {}
                other => panic!("unexpected enqueue result: {other:?}"),
            }
        }
        assert!(rejected);
        fixture.handle().request_shutdown();
        fixture.until(|event| matches!(event, Event::Exit { .. }));
    }

    #[test]
    fn shutdown_reply_closes_stdin_and_fences_new_requests() {
        let fixture = Fixture::python(
            r#"import sys,json
for line in sys.stdin:
    frame=json.loads(line)
    print(json.dumps({'version':1,'type':'response','id':frame['id'],'result':{'closed':True}}),flush=True)
"#,
        );
        fixture
            .handle()
            .try_send("bye", "shutdown", json!({}))
            .unwrap();
        assert!(matches!(
            fixture.handle().try_send("late", "status", json!({})),
            Err(BridgeError::Closed)
        ));
        let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Response(_)))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ProtocolFailure(_)))
        );
        assert!(matches!(
            events.last(),
            Some(Event::Exit {
                requested: true,
                ..
            })
        ));
    }

    #[test]
    fn null_response_id_does_not_accidentally_shutdown() {
        let fixture = Fixture::python(
            r#"import sys,json
print(json.dumps({'version':1,'type':'response','id':None,'result':{}}),flush=True)
for line in sys.stdin:
    frame=json.loads(line)
    print(json.dumps({'version':1,'type':'response','id':frame['id'],'result':{}}),flush=True)
"#,
        );
        fixture.until(|event| matches!(event,Event::Response(value) if value["id"].is_null()));
        fixture
            .handle()
            .try_send("still-open", "status", json!({}))
            .unwrap();
        fixture.until(|event| matches!(event,Event::Response(value) if value["id"]=="still-open"));
        fixture.handle().request_shutdown();
        fixture.until(|event| matches!(event, Event::Exit { .. }));
    }

    #[test]
    fn spawn_failure_has_single_failure_and_exit() {
        let fixture = Fixture::python("pass");
        let program = fixture.directory.join("not-executable-format");
        // Missing interpreter deterministically fails spawn. ENOEXEC text can trigger an
        // OS execvp shell fallback, so it is not a valid fixture for this failure branch.
        fs::write(&program, "#!/nonexistent-seecut-fixture-interpreter\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let handle = start(LaunchSpec {
            executable: program,
            entrypoint: fixture.directory.join("fixture.py"),
            environment: BTreeMap::new(),
        })
        .unwrap();
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(event) = handle.try_recv() {
                let exited = matches!(event, Event::Exit { .. });
                events.push(event);
                if exited {
                    break;
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::ProtocolFailure(BridgeError::SpawnFailed)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Exit { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn early_parent_exit_cleans_up_owned_descendant() {
        let fixture = Fixture::python(
            r#"import os,sys,json,time
pid=os.fork()
if pid==0: time.sleep(20);os._exit(0)
print(json.dumps({'version':1,'type':'response','id':'child','result':{'pid':pid}}),flush=True)
sys.exit(0)
"#,
        );
        let events = fixture.until(|event| matches!(event, Event::Exit { .. }));
        let pid = events
            .iter()
            .find_map(|event| {
                if let Event::Response(value) = event {
                    value["result"]["pid"].as_i64()
                } else {
                    None
                }
            })
            .unwrap() as i32;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut gone = false;
        while Instant::now() < deadline {
            let absent = unsafe { libc::kill(pid, 0) } < 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            let zombie = fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .is_some_and(|stat| {
                    stat.rsplit_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with('Z'))
                });
            if absent || zombie {
                gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(gone, "owned descendant remained runnable");
    }

    #[test]
    #[ignore = "auxiliary process launched only by process_exit_reaps_child_and_grandchild"]
    fn exit_helper_process() {
        let Some(root) = std::env::var_os("SEECUT_AGENT_EXIT_FIXTURE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let entrypoint = root.join("exit-fixture.py");
        fs::write(&entrypoint,r#"import os,json,time
pid=os.fork()
if pid==0: time.sleep(20);os._exit(0)
print(json.dumps({'version':1,'type':'response','id':'pids','result':{'child':os.getpid(),'grandchild':pid}}),flush=True)
time.sleep(20)
"#).unwrap();
        let handle = start(LaunchSpec {
            executable: fs::canonicalize("/usr/bin/python3").unwrap(),
            entrypoint,
            environment: BTreeMap::new(),
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let pids = loop {
            if let Some(Event::Response(value)) = handle.try_recv() {
                break value["result"].clone();
            }
            assert!(Instant::now() < deadline, "child setup timed out");
            thread::sleep(Duration::from_millis(10));
        };
        fs::write(root.join("pids.json"), serde_json::to_vec(&pids).unwrap()).unwrap();
        // Intentionally bypass Handle::drop. Only real process-exit cleanup can stop the tree.
        std::mem::forget(handle);
        if std::env::var_os("SEECUT_AGENT_EXIT_EXPLICIT").is_some() {
            assert!(shutdown_all_at_exit());
            assert!(shutdown_all_at_exit());
        }
        std::process::exit(0);
    }

    #[test]
    fn process_exit_reaps_child_and_grandchild() {
        use std::process::{Command, Stdio};
        for explicit in [false, true] {
            let directory =
                std::env::temp_dir().join(format!("agent-exit-{}-{explicit}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "agent_process::tests::exit_helper_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env_clear()
                .env("SEECUT_AGENT_EXIT_FIXTURE_ROOT", &directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    fs::File::create(directory.join("helper-stderr.txt")).unwrap(),
                ));
            // This re-executes the Cargo test binary, which on Linux links the CI's
            // FFmpeg/ONNX shared libraries outside the system loader directories.
            // The managed runtime still receives only LaunchSpec's explicit environment.
            for key in [
                "LD_LIBRARY_PATH",
                "DYLD_LIBRARY_PATH",
                "DYLD_FALLBACK_LIBRARY_PATH",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            if explicit {
                command.env("SEECUT_AGENT_EXIT_EXPLICIT", "1");
            }
            let started = Instant::now();
            let mut outer = command.spawn().unwrap();
            let status = loop {
                if let Some(status) = outer.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(8),
                    "outer process exit exceeded bound"
                );
                thread::sleep(Duration::from_millis(10));
            };
            assert!(
                status.success(),
                "exit helper failed ({status}): {}",
                fs::read_to_string(directory.join("helper-stderr.txt")).unwrap()
            );
            assert!(started.elapsed() < Duration::from_secs(5));
            let pids: Value =
                serde_json::from_slice(&fs::read(directory.join("pids.json")).unwrap()).unwrap();
            for field in ["child", "grandchild"] {
                let pid = pids[field].as_i64().unwrap() as i32;
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let absent = unsafe { libc::kill(pid, 0) } < 0
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                    let zombie = fs::read_to_string(format!("/proc/{pid}/stat"))
                        .ok()
                        .is_some_and(|stat| {
                            stat.rsplit_once(") ")
                                .is_some_and(|(_, rest)| rest.starts_with('Z'))
                        });
                    if absent || zombie {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "process-exit left owned child runnable"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn rejects_symlink_launch_and_ambient_secret_environment() {
        let fixture = Fixture::python("pass");
        let alias = fixture.directory.join("alias.py");
        std::os::unix::fs::symlink(fixture.directory.join("fixture.py"), &alias).unwrap();
        let python = fs::canonicalize("/usr/bin/python3").unwrap();
        assert!(matches!(
            start(LaunchSpec {
                executable: python.clone(),
                entrypoint: alias,
                environment: BTreeMap::new()
            }),
            Err(BridgeError::InvalidLaunch)
        ));
        assert!(matches!(
            start(LaunchSpec {
                executable: python,
                entrypoint: fixture.directory.join("fixture.py"),
                environment: BTreeMap::from([(
                    "NODE_OPTIONS".into(),
                    "--require /tmp/untrusted".into()
                )])
            }),
            Err(BridgeError::InvalidLaunch)
        ));
    }
}
