use crate::{HostError, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Upstream request-line budget, including its newline.
pub(crate) const MAX_REQUEST: usize = 1 << 20;
/// Upstream reply-line budget, including its newline.
pub(crate) const MAX_REPLY: usize = 8 << 20;

struct RequestBytes(Vec<u8>);
impl Write for RequestBytes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(buf.len()) > MAX_REQUEST {
            return Err(std::io::Error::other("request budget exceeded"));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_request(id: u64, method: &str, params: &Value) -> Result<Vec<u8>> {
    let mut output = RequestBytes(Vec::new());
    let encode = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        write!(output, "{{\"id\":{id},\"method\":")?;
        serde_json::to_writer(&mut output, method)?;
        output.write_all(b",\"params\":")?;
        serde_json::to_writer(&mut output, params)?;
        output.write_all(b"}\n")?;
        Ok(())
    })();
    if encode.is_err() {
        return Err(HostError::NotSent(
            "request exceeds control budget or cannot be encoded",
        ));
    }
    Ok(output.0)
}

/// One authenticated, owned-editor JSON Lines connection.
pub(crate) struct Connection {
    reader: BufReader<TcpStream>,
}

impl Connection {
    /// Connect to loopback and verify the exact upstream authentication envelope.
    pub(crate) fn connect(
        addr: SocketAddr,
        token: &str,
        id: u64,
        timeout: Duration,
    ) -> Result<Self> {
        if !addr.ip().is_loopback() || addr.port() == 0 {
            return Err(HostError::NotSent("invalid loopback endpoint"));
        }
        let stream = TcpStream::connect_timeout(&addr, timeout)
            .map_err(|_| HostError::NotSent("control connection failed"))?;
        let mut conn = Self {
            reader: BufReader::new(stream),
        };
        let auth = conn
            .exchange(id, "auth", &json!({"token":token}), timeout, false)
            .map_err(|_| HostError::NotSent("control authentication failed"))?;
        if auth != json!({"authenticated":true}) {
            return Err(HostError::NotSent("invalid authentication acknowledgement"));
        }
        Ok(conn)
    }

    /// Exchange one bounded frame with an absolute deadline and no resend.
    pub(crate) fn exchange(
        &mut self,
        id: u64,
        method: &str,
        params: &Value,
        timeout: Duration,
        write: bool,
    ) -> Result<Value> {
        let frame = encode_request(id, method, params)?;
        let deadline = Instant::now() + timeout;
        let uncertain = |reason| {
            if write {
                HostError::Unknown(reason)
            } else {
                HostError::ReadFailed(reason)
            }
        };
        let mut offset = 0;
        // Once any write is attempted, an error cannot prove the editor received nothing.
        while offset < frame.len() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| uncertain("control deadline elapsed"))?;
            self.reader
                .get_ref()
                .set_write_timeout(Some(remaining))
                .map_err(|_| uncertain("control timeout setup failed"))?;
            let n = self
                .reader
                .get_mut()
                .write(&frame[offset..])
                .map_err(|_| uncertain("control write failed"))?;
            if n == 0 {
                return Err(uncertain("control write disconnected"));
            }
            offset += n;
        }
        let mut bytes = Vec::new();
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| uncertain("control deadline elapsed"))?;
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .map_err(|_| uncertain("control timeout setup failed"))?;
            let available = self
                .reader
                .fill_buf()
                .map_err(|_| uncertain("control read failed or timed out"))?;
            if available.is_empty() {
                return Err(uncertain("control reply disconnected"));
            }
            let n = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |n| n + 1);
            if bytes.len() + n > MAX_REPLY {
                return Err(uncertain("control reply exceeds budget"));
            }
            let done = available[n - 1] == b'\n';
            bytes.extend_from_slice(&available[..n]);
            self.reader.consume(n);
            if done {
                break;
            }
        }
        let reply: Value =
            serde_json::from_slice(&bytes).map_err(|_| uncertain("invalid control reply JSON"))?;
        if reply.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(uncertain("control reply identity mismatch"));
        }
        match reply.get("ok").and_then(Value::as_bool) {
            Some(true) if reply.get("result").is_some() && reply.get("error").is_none() => {
                Ok(reply["result"].clone())
            }
            Some(false)
                if reply.get("error").and_then(Value::as_str).is_some()
                    && reply.get("result").is_none() =>
            {
                if write {
                    // The protocol has no pre-admission rejection bit or transaction rollback
                    // guarantee. Even a normal error may follow partial side effects.
                    Err(HostError::Unknown(
                        "editor returned an error after dispatch",
                    ))
                } else {
                    // Do not return arbitrary remote error strings, which can include secrets,
                    // paths or document data. The caller can inspect the live UI separately.
                    Err(HostError::RemoteRejected)
                }
            }
            _ => Err(uncertain("invalid control reply envelope")),
        }
    }
}
