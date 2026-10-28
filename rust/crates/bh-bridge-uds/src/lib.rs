//! BH bridge protocol v1 — Unix-domain-socket JSON-lines transport.
//!
//! This is the shared wire protocol for the Phase 1 "bridges before ports"
//! deliverables (PLAN.md §4): the Rust side never re-implements a Python
//! subsystem, it calls it over this socket protocol.
//!
//! The transport lives in its own tiny crate (std + `serde_json` only) so
//! both `bh-rtd` and `bh-mcp-bridge` can depend on it without creating a
//! dependency cycle: `bh-rtd` re-exports it as `bh_rtd::uds`, and the Phase 2
//! daemon binary needs `bh-mcp-bridge` on the opposite edge (tool assembly),
//! which cargo forbids if both crates went through `bh-rtd`.
//!
//! ## Wire format (protocol v1)
//!
//! * UTF-8 JSON, exactly one JSON value per `\n`-terminated line.
//! * **One request per connection**: the client connects, sends exactly one
//!   request line, reads exactly one response line, then closes. (Same
//!   pattern as Infinity's own `remote` provider transport; concurrent
//!   requests simply use concurrent connections, so no framing state or
//!   mutex is needed and `UdsClient` is `Send + Sync`.)
//! * Request: `{"id": <string>, "method": "<name>", "params": {...}}`
//! * Response (success): `{"id": <echoed>, "ok": true, "result": {...}}`
//! * Response (failure): `{"id": <echoed-or-null>, "ok": false,
//!   "error": {"code": <int>, "message": "<string>"}}`
//!
//! Error codes (JSON-RPC-flavoured, subset):
//!
//! | code   | meaning                                          |
//! |--------|--------------------------------------------------|
//! | -32700 | request line was not valid JSON                  |
//! | -32602 | valid JSON but malformed request/params          |
//! | -32601 | unknown method                                   |
//! | -32603 | internal error while serving a known method      |
//!
//! `ok: false` is reserved for *bridge/protocol* failures. Business-level
//! failures (e.g. "all LLM providers failed", "tool returned success=false")
//! travel as `ok: true` with the failure expressed inside `result` — see the
//! individual bridges.
//!
//! Peers (Python side, additive files only):
//!
//! * `src/colony/jeumpa_uds_server.py` — SHIPPED. Methods `health`,
//!   `models`, `complete` (wraps `JeumpaBrain`).
//! * `src/agent_tools/uds_server.py` — SHIPPED (Phase 1, wrapping the
//!   `ToolRegistry`). Serves `health`, `tools/list`, `tools/call`; the
//!   exact contract is pinned by the `bh-mcp-bridge` client and its tests
//!   (`catalog::validate_peer_manifest` guards manifest drift).
//! * `bh-rtd` itself — SHIPPED (Phase 2). The daemon serves the same
//!   envelope server-side; see `bh_rtd::daemon` for its method table.
//!
//! Trust model: a Unix domain socket is filesystem-permission gated and
//! local-only; any local process that can reach the socket may call these
//! methods, so the Python peers create the socket with `0600` permissions
//! and refuse to clobber non-socket paths.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

/// Protocol version advertised by this client (`v1`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Request line was not valid JSON (mirrors JSON-RPC -32700).
pub const CODE_PARSE_ERROR: i64 = -32700;
/// Valid JSON but malformed request/params (mirrors JSON-RPC -32602).
pub const CODE_INVALID_REQUEST: i64 = -32602;
/// Unknown method (mirrors JSON-RPC -32601).
pub const CODE_UNKNOWN_METHOD: i64 = -32601;
/// Internal error while serving a known method (mirrors JSON-RPC -32603).
pub const CODE_INTERNAL_ERROR: i64 = -32603;

static CLIENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Errors from the BH bridge protocol client.
///
/// Follows the existing `bh-rtd` error conventions: plain enum,
/// actionable `Display`, `std::error::Error`, no hidden retries or
/// fake-success fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UdsError {
    /// Socket connect/IO failure (server not running, permissions, ...).
    Io(String),
    /// The peer answered, but the reply violates the protocol envelope.
    Protocol(String),
    /// The peer answered `ok: false` with a structured error.
    Server {
        /// Machine-readable error code (see the code constants above).
        code: i64,
        /// Human-readable message from the server.
        message: String,
    },
}

impl core::fmt::Display for UdsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UdsError::Io(m) => write!(f, "bridge uds io error: {m}"),
            UdsError::Protocol(m) => write!(f, "bridge protocol violation: {m}"),
            UdsError::Server { code, message } => {
                write!(f, "bridge server error {code}: {message}")
            }
        }
    }
}

impl std::error::Error for UdsError {}

/// Client for the BH bridge protocol v1 (one request per connection).
///
/// `Send + Sync + Clone`-friendly: no connection state is kept between
/// calls, so a single client can be shared across threads/tasks.
#[derive(Clone, Debug)]
pub struct UdsClient {
    path: PathBuf,
    timeout: Option<Duration>,
    seq: u64,
}

impl UdsClient {
    /// Build a client that talks to the server listening on `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            timeout: None,
            seq: CLIENT_SEQ.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Socket path this client connects to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Set an optional read/write timeout applied to every call
    /// (`None` = block indefinitely, the default; the Python peers enforce
    /// their own upstream timeouts, e.g. the Jeumpa task-profile timeouts).
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    /// Issue one `method` call with `params` and return the `result` value.
    ///
    /// * `ok: true` → `Ok(result)`.
    /// * `ok: false` → `Err(UdsError::Server { code, message })`.
    /// * transport/IO failure → `Err(UdsError::Io)`; protocol violation →
    ///   `Err(UdsError::Protocol)`. Nothing is retried or masked here.
    pub fn call(&self, method: &str, params: &Value) -> Result<Value, UdsError> {
        let mut stream =
            UnixStream::connect(&self.path).map_err(|e| UdsError::Io(e.to_string()))?;
        stream
            .set_read_timeout(self.timeout)
            .map_err(|e| UdsError::Io(e.to_string()))?;
        stream
            .set_write_timeout(self.timeout)
            .map_err(|e| UdsError::Io(e.to_string()))?;

        let request = json!({
            "id": format!("bh-{}-{}", self.seq, CLIENT_SEQ.fetch_add(1, Ordering::Relaxed)),
            "method": method,
            "params": params,
        });
        let mut line =
            serde_json::to_string(&request).map_err(|e| UdsError::Protocol(e.to_string()))?;
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .map_err(|e| UdsError::Io(e.to_string()))?;
        stream
            .flush()
            .map_err(|e| UdsError::Io(e.to_string()))?;

        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .map_err(|e| UdsError::Io(e.to_string()))?;
        if response_line.trim().is_empty() {
            return Err(UdsError::Protocol(
                "server closed the connection without a response line".into(),
            ));
        }

        let envelope: Value = serde_json::from_str(response_line.trim())
            .map_err(|e| UdsError::Protocol(format!("response line is not valid JSON: {e}")))?;
        let obj = envelope
            .as_object()
            .ok_or_else(|| UdsError::Protocol("response is not a JSON object".into()))?;

        match obj.get("ok").and_then(Value::as_bool) {
            Some(true) => obj
                .get("result")
                .cloned()
                .ok_or_else(|| UdsError::Protocol("ok:true response missing 'result'".into())),
            Some(false) => {
                let err = obj.get("error").cloned().unwrap_or(Value::Null);
                let code = err
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or(CODE_INTERNAL_ERROR);
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unspecified bridge error")
                    .to_string();
                Err(UdsError::Server { code, message })
            }
            None => Err(UdsError::Protocol(
                "response envelope missing boolean 'ok'".into(),
            )),
        }
    }
}
