//! `bh-rtd` — the Phase 2 runtime wedge binary (PLAN.md §4).
//!
//! A thin, fail-loud composition root around [`bh_rtd::daemon::run_daemon`]:
//! it parses the environment, assembles the tool surface (Phase 4: the
//! native RAP tools — `file`, `shell.execute`, `search.web` — executed
//! in-process by `bh_rtd::rap_tools`, plus the remaining BH catalog through
//! the `bh-mcp-bridge` proxy), installs the `SIGTERM`/`SIGINT` graceful-stop
//! handler, and runs the daemon in the foreground until a graceful stop is
//! requested. Any startup failure is printed to stderr with a non-zero
//! exit — never a silent degraded start.
//!
//! ## Environment
//!
//! | variable | meaning | default |
//! |---|---|---|
//! | `BH_RTD_SOCKET` | UDS path to listen on (required) | — |
//! | `BH_RTD_STATE_DIR` | durable timer journal directory (required) | — |
//! | `BH_RTD_MAX_THREADS` | bounded sub-agent pool size | `16` |
//! | `BH_RTD_BRAIN_SOCKET` | Jeumpa brain UDS (`src/colony/jeumpa_uds_server.py`) | none → no brain |
//! | `BH_RTD_BRAIN_MODEL` | brain model id | `default` |
//! | `BH_RTD_TOOLS_SOCKET` | BH tools UDS (`src/agent_tools/uds_server.py`), proxy leg | none → no proxy tools |
//! | `BH_RTD_WORKSPACE_ROOT` | sandbox root for the native `file` / `search.web` tools | process cwd |
//!
//! `SIGTERM`/`SIGINT` flip the same cooperative stop flag as the `shutdown`
//! method, so every stop path passes through the daemon's clean shutdown
//! (socket removed, Infinity threads wound down, timer journal intact).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bh_rtd::daemon::{
    DaemonConfig, DaemonError, DaemonToolBox, DirectToolOutcome, DirectToolRouter,
    ToolCatalogEntry, DEFAULT_MAX_THREADS, ENV_BRAIN_MODEL, ENV_BRAIN_SOCKET, ENV_MAX_THREADS,
    ENV_SOCKET, ENV_STATE_DIR, ENV_TOOLS_SOCKET,
};
use bh_rtd::uds::UdsClient;
use bh_rtd::rap_tools::{is_native_tool, native_tool::NativeTool, NativeToolError,
    NativeToolServer};
use serde_json::{json, Value};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn fail(err: DaemonError) -> ExitCode {
    eprintln!("bh-rtd-daemon: fatal: {err}");
    ExitCode::FAILURE
}

/// One native RAP tool's `tools/list` entry. Classification metadata comes
/// from the static catalog (already transcribed from the Python tools with
/// the Phase 1 discipline); `search.web` is native-only (not in the proxy
/// catalog), so its metadata is transcribed here from
/// `src/agent_tools/search.py`. Schemas come from the engines themselves —
/// the catalog must describe the contract the executing engine enforces.
fn native_entry(name: &'static str, input: Value, output: Value) -> ToolCatalogEntry {
    const SEARCH_WEB_META: (&str, u8, &str) = (
        "knowledge",
        0,
        "Web, local-file, and code search. Actions: web (simulated), \
         local (files by name/content), code (regex support).",
    );
    let (category, autonomy_level, description) =
        match bh_mcp_bridge::catalog::catalog_entry(name) {
            Some(e) => (e.category.to_owned(), e.autonomy_level, e.description.to_owned()),
            None => (
                SEARCH_WEB_META.0.to_owned(),
                SEARCH_WEB_META.1,
                SEARCH_WEB_META.2.to_owned(),
            ),
        };
    ToolCatalogEntry {
        name: name.to_owned(),
        native: true,
        transport: "native-rust".to_owned(),
        category,
        autonomy_level,
        description,
        input_schema: input,
        output_schema: output,
    }
}

/// The native RAP tools' catalog, in [`NATIVE_TOOL_NAMES`] order.
fn native_catalog_entries() -> Vec<ToolCatalogEntry> {
    use bh_rtd::rap_tools::{file, search, shell};
    vec![
        native_entry("file", file::input_schema(), file::output_schema()),
        native_entry("shell.execute", shell::input_schema(), shell::output_schema()),
        native_entry("search.web", search::input_schema(), search::output_schema()),
    ]
}

/// One proxy tool's `tools/list` entry (the static catalog entry, honestly
/// reporting its transport).
fn proxy_catalog_entry(entry: &bh_mcp_bridge::catalog::CatalogEntry) -> ToolCatalogEntry {
    ToolCatalogEntry {
        name: entry.name.to_owned(),
        native: false,
        transport: "mcp-proxy-python".to_owned(),
        category: entry.category.to_owned(),
        autonomy_level: entry.autonomy_level,
        description: entry.description.to_owned(),
        input_schema: entry.input_schema.clone(),
        output_schema: entry.output_schema.clone(),
    }
}

/// The daemon's direct `tools/call` router: native names execute
/// in-process, everything else rides the MCP proxy pass-through. Business
/// failures stay INSIDE the payload on both legs (mirroring
/// `src/agent_tools/uds_server.py`); only bridge-level failures (peer
/// unreachable, protocol misuse) come back as `Err`.
struct RoutedTools {
    native: Arc<NativeToolServer>,
    proxy_socket: Option<PathBuf>,
}

impl DirectToolRouter for RoutedTools {
    fn route(&self, name: &str, args: &Value) -> Result<DirectToolOutcome, String> {
        if is_native_tool(name) {
            return match self.native.call(name, args) {
                Ok(result) => Ok(DirectToolOutcome::Executed(result)),
                Err(NativeToolError::UnknownTool(_)) => Ok(DirectToolOutcome::NotFound),
                Err(e) => Err(e.to_string()),
            };
        }
        let Some(socket) = &self.proxy_socket else {
            return Ok(DirectToolOutcome::NotFound);
        };
        // Raw protocol v1 pass-through (one request per connection, exactly
        // like `BhToolsClient`): the reply is forwarded verbatim so
        // `success: false` business outcomes remain visible to the caller
        // instead of being laundered into a bridge-level error.
        let mut client = UdsClient::new(socket);
        client.set_timeout(Some(std::time::Duration::from_secs(600)));
        let reply = client
            .call("tools/call", &json!({ "name": name, "args": args }))
            .map_err(|e| e.to_string())?;
        Ok(DirectToolOutcome::Executed(reply))
    }
}

/// Assemble the full tool surface. The three native RAP tools are ALWAYS
/// wired (they execute in-process; no peer needed); the remaining BH
/// catalog rides the `bh-mcp-bridge` proxy when `BH_RTD_TOOLS_SOCKET` is
/// configured and the binary was built with the `tools` feature. Native
/// names never ride the proxy — that is the Phase 4 exit criterion.
fn assemble_surface(
    socket: Option<PathBuf>,
    workspace_root: PathBuf,
) -> (
    Vec<DaemonToolBox>,
    Vec<ToolCatalogEntry>,
    Option<Arc<dyn DirectToolRouter>>,
) {
    let server = Arc::new(NativeToolServer::new(workspace_root));
    eprintln!(
        "bh-rtd-daemon: native RAP tools (file, shell.execute, search.web) sandboxed to {}",
        server.base_dir().display()
    );

    // Infinity completion-round surface: the native tools first, then the
    // non-native proxy tools (built per static catalog entry — identical to
    // Phase 1's `catalog_tools` minus the native names, which must never
    // ride the proxy).
    let mut tools: Vec<DaemonToolBox> = NativeTool::catalog(Arc::clone(&server))
        .into_iter()
        .map(|t| -> DaemonToolBox { Box::new(t) })
        .collect();
    let mut catalog = native_catalog_entries();

    #[cfg(feature = "tools")]
    if let Some(path) = &socket {
        use bh_mcp_bridge::catalog::bh_tool_catalog;
        use bh_mcp_bridge::tool_bridge::BhBridgeTool;
        use bh_mcp_bridge::BhToolsClient;

        let client = BhToolsClient::new(path.clone());
        for entry in bh_tool_catalog() {
            if is_native_tool(entry.name) {
                continue;
            }
            tools.push(Box::new(BhBridgeTool::from_catalog(entry, client.clone())));
            catalog.push(proxy_catalog_entry(entry));
        }
        eprintln!(
            "bh-rtd-daemon: wired {} BH tool(s) via {} (native ones excluded)",
            bh_tool_catalog().len(),
            path.display()
        );
    }

    #[cfg(not(feature = "tools"))]
    if socket.is_some() {
        eprintln!(
            "bh-rtd-daemon: fatal: BH_RTD_TOOLS_SOCKET is set, but this binary was built without \
             the `tools` feature (rebuild with default features)"
        );
        std::process::exit(2);
    }

    let router: Arc<dyn DirectToolRouter> = Arc::new(RoutedTools {
        native: server,
        proxy_socket: socket,
    });
    (tools, catalog, Some(router))
}

/// Install the SIGTERM/SIGINT handler: a tiny one-shot runtime on a helper
/// thread flips the shared stop flag, exactly like the `shutdown` method.
fn install_signal_handler(stop: Arc<AtomicBool>) -> Result<(), DaemonError> {
    std::thread::Builder::new()
        .name("bh-rtd-signals".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("bh-rtd-daemon: signal handler runtime failed: {e}");
                    return;
                }
            };
            rt.block_on(async {
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(mut term) => {
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {}
                            _ = term.recv() => {}
                        }
                    }
                    Err(e) => {
                        // No SIGTERM on this platform — SIGINT (ctrl_c) still works.
                        eprintln!("bh-rtd-daemon: SIGTERM handler unavailable ({e}); SIGINT only");
                        let _ = tokio::signal::ctrl_c().await;
                    }
                }
            });
            eprintln!("bh-rtd-daemon: signal received; shutting down");
            stop.store(true, Ordering::SeqCst);
        })
        .map(|_| ())
        .map_err(|e| DaemonError::Io(format!("spawn signal handler: {e}")))
}

fn main() -> ExitCode {
    let Some(socket_path) = env_path(ENV_SOCKET) else {
        eprintln!("bh-rtd-daemon: fatal: {ENV_SOCKET} is required (UDS path to listen on)");
        return ExitCode::FAILURE;
    };
    let Some(state_dir) = env_path(ENV_STATE_DIR) else {
        eprintln!(
            "bh-rtd-daemon: fatal: {ENV_STATE_DIR} is required (durable timer journal directory)"
        );
        return ExitCode::FAILURE;
    };

    let max_threads = match std::env::var(ENV_MAX_THREADS) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n,
            Ok(_) => {
                eprintln!("bh-rtd-daemon: fatal: {ENV_MAX_THREADS}=0 (must be >= 1)");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("bh-rtd-daemon: fatal: {ENV_MAX_THREADS}={raw:?}: {e}");
                return ExitCode::FAILURE;
            }
        },
        Err(_) => DEFAULT_MAX_THREADS,
    };

    let tools_socket = env_path(ENV_TOOLS_SOCKET);
    let workspace_root = env_path(bh_rtd::rap_tools::ENV_WORKSPACE_ROOT)
        .unwrap_or_else(|| {
            std::env::current_dir().expect("current directory for the native tool sandbox")
        });
    let (tools, tool_catalog, direct_tools) = assemble_surface(
        tools_socket.clone(),
        workspace_root,
    );

    let stop = Arc::new(AtomicBool::new(false));
    if let Err(err) = install_signal_handler(Arc::clone(&stop)) {
        eprintln!("bh-rtd-daemon: fatal: {err}");
        return ExitCode::FAILURE;
    }

    let config = DaemonConfig {
        socket_path,
        state_dir,
        max_threads,
        brain_socket: env_path(ENV_BRAIN_SOCKET),
        brain_model: std::env::var(ENV_BRAIN_MODEL)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "default".to_owned()),
        tools_socket,
        tool_catalog,
        direct_tools,
        stop,
    };

    match bh_rtd::daemon::run_daemon(config, tools) {
        Ok(()) => {
            eprintln!("bh-rtd-daemon: clean shutdown");
            ExitCode::SUCCESS
        }
        Err(err) => fail(err),
    }
}
