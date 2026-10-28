//! Phase 4 **native RAP tools** — `shell` / `file` / `search` executed by
//! native Rust tool servers instead of the MCP proxy (PLAN.md §4 Phase 4:
//! *"shell/file/search as native Rust RAP tool servers (NOT
//! docker/browser). Exit criteria: Those tools bypass the MCP proxy; rest
//! keep proxy."*).
//!
//! # Posture
//!
//! * **Mirror, don't invent.** Every engine below mirrors the semantics of
//!   its existing Python tool — validation, allow/deny lists, limits, result
//!   shapes and messages — transcribed from `src/agent_tools/`:
//!   - [`shell`] ← `src/agent_tools/shell.py#ShellTool` (`shell.execute`),
//!   - [`file`]  ← `src/agent_tools/file_tool.py#FileTool` (`file`),
//!   - [`search`] ← `src/agent_tools/search.py#SearchTool` (`search.web`).
//!
//!   No new tool behaviour is fabricated; documented deltas are exactly
//!   that — documented (see each module's docs and `rust/README.md`).
//! * **Fail loud.** A path outside the workspace sandbox is a business
//!   failure (`success: false` / the tool's own error shape), a missing
//!   binary is a business failure, a timeout is a `timed_out` business
//!   failure — never an empty-but-successful answer. Protocol-level misuse
//!   (unknown tool, non-object args, missing required params) is a typed
//!   [`NativeToolError`].
//! * **Filesystem sandbox.** `file` and `search` are rooted at one
//!   workspace root (the daemon resolves `BH_RTD_WORKSPACE_ROOT`, default:
//!   the daemon's working directory). Traversal outside it is refused with
//!   the peer's own `Path traversal detected: ...` message. `shell` mirrors
//!   the Python tool exactly: it enforces the same blocked-command policy
//!   (`shell::BLOCKED_COMMANDS`) and working-directory/shell-binary checks,
//!   and intentionally gains no extra sandbox (mirroring means mirroring).
//! * **No unsafe.** The whole crate is `#![forbid(unsafe_code)]`; these
//!   engines are std-only (`std::process`, `std::fs`) plus `serde_json`.
//!
//! # Surface
//!
//! [`NativeToolServer`] dispatches one tool call — the same `{name, args}`
//! call shape the MCP proxy (`bh-mcp-bridge` → `src/agent_tools/
//! uds_server.py`) carries — to the native engine, and returns the tool's
//! own result payload. Under the `infinity` feature,
//! [`native_tool::NativeTool`] wraps the server as an
//! `infinity_agent_core::tools::Tool` so member-agent completion rounds can
//! call the native tools directly (the daemon binary wires them; see
//! `src/bin/bh-rtd.rs`).

use std::path::{Path, PathBuf};

use serde_json::Value;

pub mod file;
pub mod search;
pub mod shell;

#[cfg(feature = "infinity")]
pub mod native_tool;

/// Environment variable holding the workspace root the native `file` and
/// `search` tools are sandboxed to (resolved by the `bh-rtd` binary; the
/// default is the daemon process' working directory).
pub const ENV_WORKSPACE_ROOT: &str = "BH_RTD_WORKSPACE_ROOT";

/// Native tool names, in catalog order (mirrors the Python tools
/// `src/agent_tools/shell.py`, `src/agent_tools/file_tool.py` and
/// `src/agent_tools/search.py`).
pub const NATIVE_TOOL_NAMES: [&str; 3] = ["file", "shell.execute", "search.web"];

/// True when `name` is one of the native RAP tools ([`NATIVE_TOOL_NAMES`]).
pub fn is_native_tool(name: &str) -> bool {
    NATIVE_TOOL_NAMES.contains(&name)
}

/// Errors from [`NativeToolServer::call`]. These are *protocol-level*
/// misuse (the tool was reached wrongly, not a tool whose business outcome
/// failed — business failures travel inside the tool's own result payload,
/// exactly like the Python registry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeToolError {
    /// The tool name is not one of the native RAP tools.
    UnknownTool(String),
    /// The call arguments violate the tool's documented parameter contract.
    InvalidParams(String),
}

impl core::fmt::Display for NativeToolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NativeToolError::UnknownTool(name) => {
                write!(f, "unknown native tool {name:?}")
            }
            NativeToolError::InvalidParams(m) => write!(f, "invalid native tool params: {m}"),
        }
    }
}

impl std::error::Error for NativeToolError {}

/// One natively-served tool's static metadata (the daemon `tools/list`
/// catalog shape). Transcribed from the Python tool with the exact
/// provenance in [`NativeToolSpec::source`]; `file` and `shell.execute`
/// duplicate the `bh-mcp-bridge` catalog transcription on purpose — this
/// crate cannot depend on it (dependency cycle, see `rust/Cargo.toml`).
#[derive(Clone, Debug, PartialEq)]
pub struct NativeToolSpec {
    /// Dotted tool identifier, exactly as the Python tool's name.
    pub name: &'static str,
    /// Tool category used for discovery grouping, as the Python tool's.
    pub category: &'static str,
    /// Human-readable description (the Python tool's docstring/definition).
    pub description: &'static str,
    /// Minimum autonomy level (0-4), as the Python tool declares.
    pub autonomy_level: u8,
    /// JSON Schema of accepted args (transcribed verbatim).
    pub input_schema: Value,
    /// JSON Schema of the returned payload (transcribed verbatim).
    pub output_schema: Value,
    /// Python module + class this spec was transcribed from.
    pub source: &'static str,
}

/// `file` spec — `src/agent_tools/file_tool.py#FileTool` (identical
/// transcription to `bh_mcp_bridge::catalog::file`; see [`NativeToolSpec`]).
pub fn file_spec() -> NativeToolSpec {
    NativeToolSpec {
        name: "file",
        category: "file",
        description: "Read, write, edit, list, search (glob), diff/patch files \
                      with safety controls and encoding detection",
        autonomy_level: 1,
        input_schema: file::input_schema(),
        output_schema: file::output_schema(),
        source: "src/agent_tools/file_tool.py#FileTool",
    }
}

/// `shell.execute` spec — `src/agent_tools/shell.py#ShellTool` (identical
/// transcription to `bh_mcp_bridge::catalog::shell_execute`).
pub fn shell_execute_spec() -> NativeToolSpec {
    NativeToolSpec {
        name: "shell.execute",
        category: "compute",
        description: "Execute shell commands with timeout, output capture, \
                      working-directory control, and environment-variable support",
        autonomy_level: 1,
        input_schema: shell::input_schema(),
        output_schema: shell::output_schema(),
        source: "src/agent_tools/shell.py#ShellTool",
    }
}

/// `search.web` spec — `src/agent_tools/search.py#SearchTool`.
pub fn search_web_spec() -> NativeToolSpec {
    NativeToolSpec {
        name: "search.web",
        category: "knowledge",
        description: "Web, local-file, and code search",
        autonomy_level: 0,
        input_schema: search::input_schema(),
        output_schema: search::output_schema(),
        source: "src/agent_tools/search.py#SearchTool",
    }
}

/// The native tool catalog, deterministic order (same order as
/// [`NATIVE_TOOL_NAMES`]).
pub fn native_tool_specs() -> [NativeToolSpec; 3] {
    [file_spec(), shell_execute_spec(), search_web_spec()]
}

/// The native RAP tool server: dispatches `{name, args}` tool calls to the
/// native engines. `Send + Sync + Clone` (stateless engines + one sandbox
/// root), so a single server is shared by every tool object and by the
/// daemon's direct `tools/call` router.
#[derive(Clone, Debug)]
pub struct NativeToolServer {
    base_dir: PathBuf,
}

impl NativeToolServer {
    /// Build a server whose `file`/`search` sandbox is rooted at
    /// `base_dir` (made absolute; canonicalized when it exists).
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: file::resolve_base_dir(base_dir.into()),
        }
    }

    /// The resolved sandbox root (absolute; what `file`/`search` paths are
    /// validated against).
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Dispatch one tool call. Business outcomes (including the tools'
    /// own failure shapes — blocked command, traversal, empty query, ...)
    /// come back as `Ok(result)`; only protocol-level misuse is
    /// `Err(NativeToolError)`.
    pub fn call(&self, name: &str, args: &Value) -> Result<Value, NativeToolError> {
        if !args.is_object() {
            return Err(NativeToolError::InvalidParams(
                "tool args must be a JSON object".into(),
            ));
        }
        match name {
            "file" => file::FileEngine::new(&self.base_dir).execute(args),
            "shell.execute" => shell::ShellEngine.execute(args),
            "search.web" => search::SearchEngine::new(&self.base_dir).execute(args),
            other => Err(NativeToolError::UnknownTool(other.to_owned())),
        }
    }
}

/// Render a rounded `duration_ms` field the way Python's `round(x, 2)` does
/// (the engines stamp one into their results).
pub(crate) fn round2(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_names_are_the_mirrored_python_tools() {
        assert_eq!(
            NATIVE_TOOL_NAMES,
            ["file", "shell.execute", "search.web"],
            "the three PLAN.md §4 Phase 4 tools, catalog order"
        );
        assert!(is_native_tool("file"));
        assert!(is_native_tool("shell.execute"));
        assert!(is_native_tool("search.web"));
        // The proxy-only tool from the Phase 1 catalog is NOT native.
        assert!(!is_native_tool("code.operations"));
        assert!(!is_native_tool(""));
    }

    #[test]
    fn specs_are_transcribed_with_provenance_and_schemas() {
        for spec in native_tool_specs() {
            assert!(is_native_tool(spec.name), "{} must be native", spec.name);
            assert!(
                spec.input_schema.is_object() && spec.output_schema.is_object(),
                "{}: schemas must be JSON objects",
                spec.name
            );
            assert!(spec.autonomy_level <= 4);
            assert!(spec.source.starts_with("src/agent_tools/"));
        }
        assert_eq!(file_spec().name, "file");
        assert_eq!(file_spec().autonomy_level, 1);
        assert_eq!(shell_execute_spec().name, "shell.execute");
        assert_eq!(shell_execute_spec().autonomy_level, 1);
        assert_eq!(search_web_spec().name, "search.web");
        assert_eq!(search_web_spec().autonomy_level, 0);
        assert_eq!(
            search_web_spec().input_schema["required"],
            json!(["action", "query"])
        );
    }

    #[test]
    fn server_routes_by_name_and_rejects_misuse() {
        let dir = std::env::temp_dir().join(format!("bh-rap-tools-srv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let server = NativeToolServer::new(&dir);

        // Unknown tool → typed error, never a fabricated result.
        assert_eq!(
            server.call("code.operations", &json!({})),
            Err(NativeToolError::UnknownTool("code.operations".into()))
        );
        // Non-object args → typed error.
        assert!(matches!(
            server.call("file", &json!(["not", "an", "object"])),
            Err(NativeToolError::InvalidParams(_))
        ));
        // A real call routes to the engine (business failure inside Ok).
        let outcome = server
            .call("file", &json!({"operation": "read", "path": "nope.txt"}))
            .expect("file tool executes");
        assert_eq!(outcome["success"], false);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn server_sandbox_root_is_absolute() {
        let server = NativeToolServer::new(".");
        assert!(server.base_dir().is_absolute());
    }
}
