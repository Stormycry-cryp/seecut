// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded Unix IPC shared by the running editor and its stdio MCP adapter.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A single IPC message is bounded before deserialization or allocation.
pub const MAX_MESSAGE: usize = 1024 * 1024;
/// IPC calls should fail promptly if the app is exiting or blocked.
pub const IPC_TIMEOUT: Duration = Duration::from_secs(10);

/// One named request to an explicit running App instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    /// The exact running App instance.
    pub instance_id: String,
    /// The client named in the trusted App grant.
    pub client_id: String,
    /// The read credential, or the independent write credential for a move.
    pub client_token: String,
    /// One named method.
    pub method: Method,
    #[serde(default)]
    /// Opaque project identity.
    pub project_id: Option<String>,
    #[serde(default)]
    /// Identity of this opening of a document.
    pub document_session_id: Option<String>,
    #[serde(default)]
    /// Committed document version expected by preview or a new move.
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
    #[serde(default)]
    /// Parameters for the sole supported edit; forbidden on read requests.
    pub move_parameters: Option<MoveParameters>,
}

/// A single atomic translation of the current single selected image layer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveParameters {
    /// Deterministic identity derived from authenticated client, session and sequence.
    pub operation_id: String,
    /// Nonzero monotonically increasing sequence within this document session.
    pub client_sequence: u64,
    /// Selection version observed before this operation.
    pub selection_revision: u64,
    /// Canonical stable image layer identity, `layer:<nonzero u64>`.
    pub object_id: String,
    /// Horizontal translation in document pixels, finite and at most 32768 in magnitude.
    pub delta_x: f64,
    /// Vertical translation in document pixels, finite and at most 32768 in magnitude.
    pub delta_y: f64,
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
    /// Move the current single selected image layer under a trusted UI write lease.
    MoveSelectedImage,
}

/// Derive the versioned operation identity using exact UTF-8 bytes and byte lengths.
/// Client and session must each contain 1..=64 bytes; sequence must be nonzero.
/// No Unicode normalization is performed. Colons in either identity are permitted.
pub fn operation_id(client: &str, session: &str, sequence: u64) -> Result<String, String> {
    if client.is_empty() || client.len() > 64 || session.is_empty() || session.len() > 64 {
        return Err("invalidInput: operation identity length".into());
    }
    if sequence == 0 {
        return Err("invalidInput: zero client sequence".into());
    }
    let id = format!(
        "scm2:1:{}:{client}:{}:{session}:{sequence}",
        client.len(),
        session.len()
    );
    if id.len() > 192 {
        return Err("invalidInput: operation ID length".into());
    }
    Ok(id)
}

/// Parse a canonical stable image layer identity without accepting aliases.
pub fn parse_layer_id(object_id: &str) -> Result<u64, String> {
    let digits = object_id.strip_prefix("layer:").unwrap_or_default();
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalidInput: object ID".into());
    }
    digits.parse().map_err(|_| "invalidInput: object ID".into())
}

/// Validate move syntax and the identity derived from the authenticated client and session.
/// Authorization, current selection, ownership and document freshness remain App checks.
pub fn validate_move_parameters(
    client: &str,
    session: &str,
    parameters: &MoveParameters,
) -> Result<(), String> {
    if parameters.operation_id != operation_id(client, session, parameters.client_sequence)? {
        return Err("invalidInput: operation ID mismatch".into());
    }
    parse_layer_id(&parameters.object_id)?;
    if !parameters.delta_x.is_finite()
        || !parameters.delta_y.is_finite()
        || parameters.delta_x.abs() > 32768.0
        || parameters.delta_y.abs() > 32768.0
    {
        return Err("invalidInput: translation".into());
    }
    Ok(())
}

/// Reject edit parameters on reads, and missing or unrelated fields on a move.
/// Existing method-specific read identity and range checks remain App checks.
pub fn validate_request_shape(request: &Request) -> Result<(), String> {
    if !matches!(request.method, Method::MoveSelectedImage) {
        return if request.move_parameters.is_none() {
            Ok(())
        } else {
            Err("invalidInput: edit parameters on read".into())
        };
    }
    let session = request
        .document_session_id
        .as_deref()
        .filter(|id| !id.is_empty());
    let project = request.project_id.as_deref().filter(|id| !id.is_empty());
    let (Some(session), Some(_), Some(_), Some(parameters)) = (
        session,
        project,
        request.revision,
        request.move_parameters.as_ref(),
    ) else {
        return Err("invalidInput: missing move identity or parameters".into());
    };
    if request.context_revision.is_some()
        || request.offset.is_some()
        || request.limit.is_some()
        || request.max_edge.is_some()
    {
        return Err("invalidInput: read fields on move".into());
    }
    validate_move_parameters(&request.client_id, session, parameters)
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
    if validate_request_shape(request).is_err() {
        return Response::error("invalidInput");
    }
    let mut frame = Vec::new();
    if write_message(&mut frame, request).is_err() {
        return Response::error("appUnavailable");
    }
    let Ok(mut stream) = UnixStream::connect(path) else {
        return Response::error("appUnavailable");
    };
    if stream.set_read_timeout(Some(IPC_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(IPC_TIMEOUT)).is_err()
    {
        return Response::error("appUnavailable");
    }
    exchange(
        &mut stream,
        &frame,
        matches!(request.method, Method::MoveSelectedImage),
    )
}

// Any sent bytes make a write failure ambiguous. The adapter cannot prove that
// the App never began a move, even when it received no usable response.
#[cfg(any(unix, test))]
fn exchange(stream: &mut (impl Read + Write), frame: &[u8], is_move: bool) -> Response {
    let mut sent = 0;
    while sent < frame.len() {
        match stream.write(&frame[sent..]) {
            Ok(0) => return send_failure(is_move, sent),
            Ok(count) => sent += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return send_failure(is_move, sent),
        }
    }
    read_message(stream).unwrap_or_else(|error| {
        if is_move {
            return Response::error("outcomeUnknown");
        }
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

#[cfg(any(unix, test))]
fn send_failure(is_move: bool, sent: usize) -> Response {
    Response::error(if is_move && sent != 0 {
        "outcomeUnknown"
    } else {
        "appUnavailable"
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
                Ok(request) if validate_request_shape(&request).is_ok() => handle(request),
                Ok(_) => Response::error("invalidInput"),
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

    fn move_request() -> Request {
        Request {
            instance_id: "app".into(),
            client_id: "client".into(),
            client_token: "write-credential".into(),
            method: Method::MoveSelectedImage,
            project_id: Some("project".into()),
            document_session_id: Some("session".into()),
            revision: Some(7),
            context_revision: None,
            offset: None,
            limit: None,
            max_edge: None,
            move_parameters: Some(MoveParameters {
                operation_id: operation_id("client", "session", 17).unwrap(),
                client_sequence: 17,
                selection_revision: 11,
                object_id: "layer:42".into(),
                delta_x: 4.0,
                delta_y: -3.0,
            }),
        }
    }

    #[test]
    fn operation_identity_is_versioned_canonical_and_length_delimited() {
        let session = "12345678-1234-1234-1234-123456789abc";
        assert_eq!(
            operation_id("abc", session, 17).unwrap(),
            format!("scm2:1:3:abc:36:{session}:17")
        );
        assert_ne!(operation_id("a:b", "c", 1), operation_id("a", "b:c", 1));
        assert_eq!(
            operation_id("客户端", "文档", 1).unwrap(),
            "scm2:1:9:客户端:6:文档:1"
        );
        assert_ne!(operation_id("é", "s", 1), operation_id("e\u{301}", "s", 1));
        let maximum = operation_id(&"c".repeat(64), &"s".repeat(64), u64::MAX).unwrap();
        assert!(maximum.ends_with(":18446744073709551615"));
        assert!(maximum.len() <= 192);
        assert!(operation_id(&"汉".repeat(21), "s", 1).is_ok());
        for (client, session, sequence) in [
            ("".to_owned(), "s".to_owned(), 1),
            ("c".to_owned(), "".to_owned(), 1),
            ("c".repeat(65), "s".to_owned(), 1),
            ("c".to_owned(), "汉".repeat(22), 1),
            ("c".to_owned(), "s".to_owned(), 0),
        ] {
            assert!(operation_id(&client, &session, sequence).is_err());
        }
    }

    #[test]
    fn layer_identity_rejects_zero_aliases_and_overflow() {
        assert_eq!(parse_layer_id("layer:42"), Ok(42));
        assert_eq!(parse_layer_id("layer:18446744073709551615"), Ok(u64::MAX));
        for invalid in [
            "layer:0",
            "layer:00",
            "layer:01",
            "layer:+1",
            "layer:-1",
            "layer: 1",
            "layer:1 ",
            "layer:١",
            "layer:18446744073709551616",
            "layer:",
            "Layer:1",
            "1",
        ] {
            assert!(parse_layer_id(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn move_requires_canonical_operation_and_finite_bounded_deltas() {
        let mut parameters = move_request().move_parameters.unwrap();
        parameters.delta_x = -32768.0;
        parameters.delta_y = 32768.0;
        assert!(validate_move_parameters("client", "session", &parameters).is_ok());
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            32768.01,
            -32768.01,
        ] {
            let mut bad = parameters.clone();
            bad.delta_x = invalid;
            assert!(validate_move_parameters("client", "session", &bad).is_err());
            bad.delta_x = parameters.delta_x;
            bad.delta_y = invalid;
            assert!(validate_move_parameters("client", "session", &bad).is_err());
        }
        for operation in [
            "scm2:1:6:client:7:session:017",
            "scm2:2:6:client:7:session:17",
            "scm2:1:6:client:7:session:18",
        ] {
            parameters.operation_id = operation.into();
            assert!(validate_move_parameters("client", "session", &parameters).is_err());
        }
        parameters.operation_id = operation_id("client", "session", 17).unwrap();
        assert!(validate_move_parameters("other", "session", &parameters).is_err());
        assert!(validate_move_parameters("client", "other", &parameters).is_err());
        parameters.client_sequence = 0;
        assert!(validate_move_parameters("client", "session", &parameters).is_err());
    }

    #[test]
    fn move_wire_shape_rejects_unknown_missing_and_unrelated_fields() {
        let request = move_request();
        assert!(validate_request_shape(&request).is_ok());
        let value = serde_json::to_value(&request).unwrap();
        let mut unknown = value.clone();
        unknown["moveParameters"]["grant"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Request>(unknown).is_err());
        let mut unknown = value.clone();
        unknown["path"] = serde_json::json!("/tmp/other");
        assert!(serde_json::from_value::<Request>(unknown).is_err());
        let overflow = serde_json::to_string(&request).unwrap().replace(
            "\"clientSequence\":17",
            "\"clientSequence\":18446744073709551616",
        );
        assert!(serde_json::from_str::<Request>(&overflow).is_err());
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("moveParameters");
        assert!(validate_request_shape(&serde_json::from_value(missing).unwrap()).is_err());
        for key in [
            "projectId",
            "documentSessionId",
            "revision",
            "moveParameters",
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[key] = serde_json::Value::Null;
            assert!(
                validate_request_shape(&serde_json::from_value(value).unwrap()).is_err(),
                "{key}"
            );
        }
        for key in ["contextRevision", "offset", "limit", "maxEdge"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[key] = serde_json::json!(1);
            assert!(
                validate_request_shape(&serde_json::from_value(value).unwrap()).is_err(),
                "{key}"
            );
        }
        for method in [
            Method::Capabilities,
            Method::Context,
            Method::Project,
            Method::Preview,
        ] {
            let mut read = request.clone();
            read.method = method;
            assert!(validate_request_shape(&read).is_err());
            read.move_parameters = None;
            assert!(validate_request_shape(&read).is_ok());
        }
    }

    struct Transport {
        response: std::io::Cursor<Vec<u8>>,
        read_error: Option<std::io::ErrorKind>,
        write_limit: Option<usize>,
        sent: usize,
    }

    impl Read for Transport {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if let Some(kind) = self.read_error {
                return Err(std::io::Error::from(kind));
            }
            self.response.read(bytes)
        }
    }

    impl Write for Transport {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let count = self.write_limit.map_or(bytes.len(), |limit| {
                bytes.len().min(limit.saturating_sub(self.sent))
            });
            if count == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.sent += count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn transport() -> Transport {
        Transport {
            response: std::io::Cursor::new(Vec::new()),
            read_error: None,
            write_limit: None,
            sent: 0,
        }
    }

    #[test]
    fn sent_move_timeout_disconnect_and_invalid_reply_are_unknown() {
        let mut frame = Vec::new();
        write_message(&mut frame, &move_request()).unwrap();
        for kind in [
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::ConnectionReset,
        ] {
            let mut peer = transport();
            peer.read_error = Some(kind);
            assert_eq!(exchange(&mut peer, &frame, true).status, "outcomeUnknown");
            assert_eq!(peer.sent, frame.len());
        }
        let mut closed = transport();
        assert_eq!(exchange(&mut closed, &frame, true).status, "outcomeUnknown");
        let mut malformed = transport();
        malformed.response = std::io::Cursor::new(vec![0, 0, 0, 1, b'{']);
        assert_eq!(
            exchange(&mut malformed, &frame, true).status,
            "outcomeUnknown"
        );
    }

    #[test]
    fn zero_sent_is_unavailable_partial_move_is_unknown_and_app_results_are_preserved() {
        let mut frame = Vec::new();
        write_message(&mut frame, &move_request()).unwrap();
        let mut no_send = transport();
        no_send.write_limit = Some(0);
        assert_eq!(
            exchange(&mut no_send, &frame, true).status,
            "appUnavailable"
        );
        let mut partial = transport();
        partial.write_limit = Some(5);
        assert_eq!(
            exchange(&mut partial, &frame, true).status,
            "outcomeUnknown"
        );
        for status in ["ok", "busy", "notAuthorized", "unchanged", "outcomeUnknown"] {
            let mut peer = transport();
            let mut response = Vec::new();
            write_message(&mut response, &Response::error(status)).unwrap();
            peer.response = std::io::Cursor::new(response);
            assert_eq!(exchange(&mut peer, &frame, true).status, status);
        }
    }

    #[test]
    fn reads_keep_timeout_disconnect_and_partial_send_statuses() {
        let mut timeout = transport();
        timeout.read_error = Some(std::io::ErrorKind::TimedOut);
        assert_eq!(exchange(&mut timeout, &[1], false).status, "busy");
        let mut closed = transport();
        assert_eq!(exchange(&mut closed, &[1], false).status, "appUnavailable");
        let mut partial = transport();
        partial.write_limit = Some(1);
        assert_eq!(
            exchange(&mut partial, &[1, 2], false).status,
            "appUnavailable"
        );
    }
    #[test]
    fn rejects_oversized_ipc_before_allocating_body() {
        let mut bytes = ((MAX_MESSAGE + 1) as u32).to_be_bytes().to_vec();
        assert!(read_message::<Request>(&mut bytes.as_slice()).is_err());
        bytes = (MAX_MESSAGE as u32).to_be_bytes().to_vec();
        assert!(read_message::<Request>(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn unrelated_write_methods_are_not_in_the_wire_protocol() {
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
            move_parameters: None,
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
            move_parameters: None,
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
