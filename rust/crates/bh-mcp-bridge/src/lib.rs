//! `bh-mcp-bridge` — the Phase 1 **infinity-mcp-bridge** deliverable
//! (PLAN.md §4 "Bridges before ports").
//!
//! This crate bridges Infinity's tool surface to BlackHornet's Python
//! `agent_tools` **without porting any tool logic**: tools keep living in
//! `src/agent_tools/` (the registry with circuit breaker, rate limiting and
//! permission checks stays Python). Two halves:
//!
//! * **Catalog** ([`catalog`], always available): the Rust-side mirror of
//!   the BH tool registry semantics — `file` (`src/agent_tools/file_tool.py`),
//!   `shell.execute` (`src/agent_tools/shell.py`), `code.operations`
//!   (`src/agent_tools/code.py`) with their exact JSON schemas, categories
//!   and autonomy levels, plus [`catalog::validate_peer_manifest`] drift
//!   checking against whatever the Python peer serves.
//! * **Client** ([`BhToolsClient`], always available): the tool-inventory /
//!   tool-invocation protocol client:
//!    - `tools/list` → manifest of available tools (metadata only; the
//!      Python peer builds it by safe introspection and never executes
//!      anything),
//!    - `tools/call` with `{name, args}` → execution result.
//! * **Runtime tool** ([`tool_bridge`], feature `infinity`):
//!   [`tool_bridge::BhBridgeTool`] implements `infinity_agent_core::tools::Tool`
//!   so an Infinity agent registers BH tools directly; calls execute
//!   synchronously over the bridge client and the result is injected into
//!   the completion loop.
//!
//! All of it travels over the BH bridge protocol v1 (see `bh_bridge_uds`,
//! re-exported by `bh-rtd` as `bh_rtd::uds`): one JSON value per
//! `\n`-terminated line over a Unix domain socket, one request per
//! connection.
//!
//! ## Peer status (honest)
//!
//! The Rust side of this bridge ships and is exercised end-to-end offline
//! against a mock UDS peer speaking the documented protocol. The Python
//! peer (`src/agent_tools/uds_server.py` wrapping the `ToolRegistry`:
//! `health` / `tools/list` / `tools/call`) ships too — Phase 1's own rule
//! is that the bridge comes *before* the BH MCP servers (PLAN.md §4), and
//! the contract it serves is specified by
//! [`BhToolsClient`] + [`ToolManifest`] + [`catalog::validate_peer_manifest`]
//! and enforced by the manifest-parity test.
//!
//! Naming note: the upstream Infinity release train publishes its own
//! `infinity-mcp-bridge =0.2.0` crate; this local crate is deliberately
//! named `bh-mcp-bridge` so the two can never be confused in `Cargo.lock`.
//! It delivers the Phase 1 "infinity-mcp-bridge wired to BH agent_tools"
//! exit criterion from the BlackHornet side.
//!
//! Error philosophy (matches `bh-rtd` conventions): transport and protocol
//! failures surface as [`BhToolsError::Transport`]; a tool call whose
//! business outcome is `success: false` surfaces as
//! [`BhToolsError::ToolFailed`] (never a plausible-looking success string);
//! manifest shape violations surface as [`BhToolsError::Manifest`]. Nothing
//! is retried or masked here.
//!
//! Trust model: the socket is local-only and filesystem-permission gated;
//! the Python peer creates it `0600` and only serves tool calls to callers
//! that can reach it. See `rust/README.md` for the full protocol table.

use std::path::{Path, PathBuf};

use bh_bridge_uds::{UdsClient, UdsError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod catalog;

#[cfg(feature = "infinity")]
pub mod tool_bridge;

/// Method returning the tool manifest (metadata only, no execution).
const METHOD_TOOLS_LIST: &str = "tools/list";
/// Method invoking one tool: `{name, args, context?}` → result.
const METHOD_TOOLS_CALL: &str = "tools/call";

/// Manifest entry for one BH agent tool, as returned by `tools/list`.
///
/// Deliberately schema-light and metadata-only: `input_schema` /
/// `output_schema` stay opaque [`Value`]s (JSON Schema authored Python-side)
/// so the bridge never has to know a tool's semantics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolManifest {
    /// Dotted tool identifier, e.g. `shell.execute`.
    pub name: String,
    /// Tool category used for discovery grouping, e.g. `compute`.
    pub category: String,
    /// Minimum autonomy level (0–4) required to invoke this tool.
    pub autonomy_level: u8,
    /// JSON Schema of accepted args (opaque; authored Python-side).
    #[serde(default)]
    pub input_schema: Value,
    /// JSON Schema of the returned payload (opaque; authored Python-side).
    #[serde(default)]
    pub output_schema: Value,
}

/// Errors from the BH tools bridge client.
#[derive(Debug)]
pub enum BhToolsError {
    /// Socket/protocol failure talking to `src/agent_tools/uds_server.py`.
    Transport(UdsError),
    /// The tool executed but reported a business failure
    /// (`result.success == false`); `message` is the tool-side error.
    ToolFailed {
        /// Name of the tool that was invoked.
        name: String,
        /// Tool-side error message.
        message: String,
    },
    /// The `tools/list` reply did not match the documented manifest shape.
    Manifest(String),
}

impl core::fmt::Display for BhToolsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BhToolsError::Transport(e) => write!(f, "bh tools bridge transport error: {e}"),
            BhToolsError::ToolFailed { name, message } => {
                write!(f, "tool {name:?} failed: {message}")
            }
            BhToolsError::Manifest(m) => write!(f, "bh tools manifest malformed: {m}"),
        }
    }
}

impl std::error::Error for BhToolsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BhToolsError::Transport(e) => Some(e),
            _ => None,
        }
    }
}

impl From<UdsError> for BhToolsError {
    fn from(e: UdsError) -> Self {
        BhToolsError::Transport(e)
    }
}

/// Client for the BH agent_tools bridge (Phase 1 `infinity-mcp-bridge`
/// surface). Synchronous by design: the calls are local UDS round-trips and
/// the tool execution time is governed by the Python registry's own limits.
#[derive(Clone, Debug)]
pub struct BhToolsClient {
    client: UdsClient,
}

impl BhToolsClient {
    /// Client pointing at the socket of a running
    /// `src/agent_tools/uds_server.py` instance.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            client: UdsClient::new(socket_path),
        }
    }

    /// Socket path of the Python peer.
    pub fn socket_path(&self) -> &Path {
        self.client.path()
    }

    /// List the available tools (safe inventory; the Python peer never
    /// executes anything while building the manifest).
    pub fn list_tools(&self) -> Result<Vec<ToolManifest>, BhToolsError> {
        let reply = self.client.call(METHOD_TOOLS_LIST, &json!({}))?;
        let tools = reply
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| BhToolsError::Manifest("reply missing 'tools' array".into()))?;
        tools
            .iter()
            .map(|t| {
                serde_json::from_value(t.clone())
                    .map_err(|e| BhToolsError::Manifest(format!("bad tool manifest entry: {e}")))
            })
            .collect()
    }

    /// Invoke one tool by name with `args`.
    ///
    /// * Transport/protocol failure → [`BhToolsError::Transport`].
    /// * Tool executed and returned `success: false` →
    ///   [`BhToolsError::ToolFailed`] (the registry's permission/rate-limit/
    ///   circuit-breaker rejections arrive in this shape).
    /// * Otherwise → `Ok(result)` with the full tool result payload.
    pub fn call_tool(&self, name: &str, args: &Value) -> Result<Value, BhToolsError> {
        let reply = self.client.call(
            METHOD_TOOLS_CALL,
            &json!({ "name": name, "args": args }),
        )?;
        if reply.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(BhToolsError::ToolFailed {
                name: name.to_owned(),
                message: reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("tool reported failure without an error message")
                    .to_string(),
            });
        }
        Ok(reply)
    }
}
