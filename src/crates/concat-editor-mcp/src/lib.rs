// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded Unix IPC shared by the running editor and its stdio MCP adapter.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A single IPC message is bounded before deserialization or allocation.
pub const MAX_MESSAGE: usize = 1024 * 1024;
/// UI reads should fail promptly if the app is exiting or blocked.
pub const IPC_TIMEOUT: Duration = Duration::from_secs(10);

/// One named, read-only request. No path or edit command is accepted here.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    /// The exact running App instance.
    pub instance_id: String,
    /// The client named in the trusted App grant.
    pub client_id: String,
    /// The token minted by that grant.
    pub client_token: String,
    /// One named read method.
    pub method: Method,
    #[serde(default)]
    /// Opaque project identity.
    pub project_id: Option<String>,
    #[serde(default)]
    /// Identity of this opening of a document.
    pub document_session_id: Option<String>,
    #[serde(default)]
    /// Committed document version expected by preview.
    pub revision: Option<u64>,
    #[serde(default)]
    /// Visible context version expected by preview.
    pub context_revision: Option<u64>,
    #[serde(default)]
    /// First item of a structure page.
    pub offset: Option<usize>,
    #[serde(default)]
    /// Requested structure page length.
    pub limit: Option<usize>,
    #[serde(default)]
    /// Maximum preview edge in pixels.
    pub max_edge: Option<u32>,
}

/// The only methods on the IPC boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Method {
    /// Read supported tools and limits.
    Capabilities,
    /// Read the active workspace and document context.
    Context,
    /// Read a bounded project structure page.
    Project,
    /// Read a stable image frame.
    Preview,
}

/// A status plus bounded structured output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// `ok` or a typed App/permission/version error.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Method output when status is `ok`.
    pub data: Option<Value>,
}

impl Response {
    /// A typed error with no payload.
    pub fn error(status: &str) -> Self {
        Self {
            status: status.into(),
            data: None,
        }
    }
    /// A successful structured response.
    pub fn ok(data: Value) -> Self {
        Self {
            status: "ok".into(),
            data: Some(data),
        }
    }
}

/// Match a preview to one document and context snapshot. The App calls this
/// before composing and again after its worker finishes.
pub fn preview_matches(
    request: &Request,
    project: &str,
    session: &str,
    revision: u64,
    context_revision: u64,
) -> bool {
    request.project_id.as_deref() == Some(project)
        && request.document_session_id.as_deref() == Some(session)
        && request.revision == Some(revision)
        && request.context_revision == Some(context_revision)
}

/// Read one length-prefixed JSON message without trusting a peer's length.
pub fn read_message<T: for<'a> Deserialize<'a>>(reader: &mut impl Read) -> Result<T, String> {
    let mut size = [0; 4];
    reader.read_exact(&mut size).map_err(|e| e.to_string())?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err("invalidInput: message size".into());
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| format!("invalidInput: {e}"))
}

/// Write one bounded length-prefixed JSON message.
pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_MESSAGE {
        return Err("invalidInput: response too large".into());
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(&bytes).map_err(|e| e.to_string())
}

/// An explicit instance's endpoint under a directory private to the OS user.
#[cfg(unix)]
pub fn endpoint(instance_id: &str) -> Result<PathBuf, String> {
    if instance_id.len() != 36
        || !instance_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return Err("invalidInput: instance ID".into());
    }
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = unsafe { libc::geteuid() };
    // macOS sockaddr_un has a short path limit; keep this below it even
    // under /var/folders/.../T.
    let dir = std::env::temp_dir().join(format!("sc-{uid}"));
    if let Err(error) = std::fs::symlink_metadata(&dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.to_string());
        }
        let mut builder = std::fs::DirBuilder::new();
        if let Err(error) = builder.mode(0o700).create(&dir)
            && error.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(error.to_string());
        }
    }
    let metadata = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("ioFailure: unsafe IPC directory".into());
    }
    Ok(dir.join(instance_id))
}

/// Send one request to an explicitly named App instance.
#[cfg(unix)]
pub fn call(path: &Path, request: &Request) -> Response {
    use std::os::unix::net::UnixStream;
    let Ok(mut stream) = UnixStream::connect(path) else {
        return Response::error("appUnavailable");
    };
    let _ = stream.set_read_timeout(Some(IPC_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IPC_TIMEOUT));
    if write_message(&mut stream, request).is_err() {
        return Response::error("appUnavailable");
    }
    read_message(&mut stream).unwrap_or_else(|error| {
        if error.contains("timed out") || error.contains("would block") {
            Response::error("busy")
        } else if error.contains("failed to fill whole buffer")
            || error.contains("Connection reset")
        {
            Response::error("appUnavailable")
        } else {
            Response::error("ioFailure")
        }
    })
}

/// Bind an endpoint before advertising it in the App UI.
#[cfg(unix)]
pub fn bind(path: &Path) -> Result<std::os::unix::net::UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    let listener = UnixListener::bind(path).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    Ok(listener)
}

/// Run a bound listener. The callback must return on App exit and must never
/// access Slint state from the IPC thread.
#[cfg(unix)]
pub fn serve(
    listener: std::os::unix::net::UnixListener,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: impl Fn(Request) -> Response + Send + Sync + 'static,
) {
    use std::sync::atomic::Ordering;
    let _ = listener.set_nonblocking(true);
    let handle = std::sync::Arc::new(handle);
    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    while !stop.load(Ordering::Acquire) {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(_) => break,
        };
        if active.fetch_add(1, Ordering::AcqRel) >= 8 {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = write_message(&mut stream, &Response::error("busy"));
            continue;
        }
        let handle = handle.clone();
        let active = active.clone();
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(IPC_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IPC_TIMEOUT));
            let response = match read_message::<Request>(&mut stream) {
                Ok(request) => handle(request),
                Err(_) => Response::error("invalidInput"),
            };
            let _ = write_message(&mut stream, &response);
            active.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_oversized_ipc_before_allocating_body() {
        let mut bytes = ((MAX_MESSAGE + 1) as u32).to_be_bytes().to_vec();
        assert!(read_message::<Request>(&mut bytes.as_slice()).is_err());
        bytes = (MAX_MESSAGE as u32).to_be_bytes().to_vec();
        assert!(read_message::<Request>(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn write_methods_are_not_in_the_wire_protocol() {
        let json = r#"{"instanceId":"i","clientId":"c","clientToken":"t","method":"delete","projectId":"p"}"#;
        assert!(serde_json::from_str::<Request>(json).is_err());
    }

    #[test]
    fn preview_rejects_each_stale_axis() {
        let mut request = Request {
            instance_id: "app".into(),
            client_id: "c".into(),
            client_token: "t".into(),
            method: Method::Preview,
            project_id: Some("p".into()),
            document_session_id: Some("s".into()),
            revision: Some(7),
            context_revision: Some(11),
            offset: None,
            limit: None,
            max_edge: Some(128),
        };
        assert!(preview_matches(&request, "p", "s", 7, 11));
        assert!(!preview_matches(&request, "p", "s", 8, 11));
        assert!(!preview_matches(&request, "p", "s", 7, 12));
        assert!(!preview_matches(&request, "p", "new-session", 7, 11));
        request.project_id = Some("another".into());
        assert!(!preview_matches(&request, "p", "s", 7, 11));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_instance_and_missing_app_have_distinct_results() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let path = std::env::temp_dir().join(format!("sc-test-{}", uuid::Uuid::new_v4()));
        let first = Request {
            instance_id: "first".into(),
            client_id: "c".into(),
            client_token: "t".into(),
            method: Method::Context,
            project_id: None,
            document_session_id: None,
            revision: None,
            context_revision: None,
            offset: None,
            limit: None,
            max_edge: None,
        };
        assert_eq!(call(&path, &first).status, "appUnavailable");
        let listener = bind(&path).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let server = std::thread::spawn(move || {
            serve(listener, stopping, |request| {
                if request.instance_id != "first" {
                    Response::error("appUnavailable")
                } else if request.client_token != "approved" {
                    Response::error("notAuthorized")
                } else {
                    Response::ok(serde_json::json!({"instance": "first"}))
                }
            })
        });
        assert_eq!(call(&path, &first).status, "notAuthorized");
        let mut wrong = first.clone();
        wrong.instance_id = "second".into();
        wrong.client_token = "approved".into();
        assert_eq!(call(&path, &wrong).status, "appUnavailable");
        let mut approved = first;
        approved.client_token = "approved".into();
        assert_eq!(call(&path, &approved).data.unwrap()["instance"], "first");
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
