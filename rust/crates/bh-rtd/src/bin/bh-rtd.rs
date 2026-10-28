//! `bh-rtd` — the Phase 2 runtime wedge binary (PLAN.md §4).
//!
//! A thin, fail-loud composition root around [`bh_rtd::daemon::run_daemon`]:
//! it parses the environment, assembles the BH tools bridge (feature
//! `tools`), installs the `SIGTERM`/`SIGINT` graceful-stop handler, and runs
//! the daemon in the foreground until a graceful stop is requested. Any
//! startup failure is printed to stderr with a non-zero exit — never a
//! silent degraded start.
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
//! | `BH_RTD_TOOLS_SOCKET` | BH tools UDS (`src/agent_tools/uds_server.py`) | none → no tools |
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
    DaemonConfig, DaemonError, DaemonToolBox, DEFAULT_MAX_THREADS, ENV_BRAIN_MODEL,
    ENV_BRAIN_SOCKET, ENV_MAX_THREADS, ENV_SOCKET, ENV_STATE_DIR, ENV_TOOLS_SOCKET,
};

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

/// Assemble the BH tools bridge from the configured tools peer socket
/// (feature `tools`): the `bh-mcp-bridge` catalog tools execute Python-side
/// over `src/agent_tools/uds_server.py` and are offered to every member
/// agent's completion round. The Python peer validates the manifest on
/// `tools/list`, so catalog drift is surfaced by the peer, not papered over.
#[cfg(feature = "tools")]
fn assemble_tools(socket: PathBuf) -> Vec<DaemonToolBox> {
    use bh_mcp_bridge::tool_bridge::BhBridgeTool;

    let client = bh_mcp_bridge::BhToolsClient::new(socket.clone());
    let tools: Vec<DaemonToolBox> = BhBridgeTool::catalog_tools(client)
        .into_iter()
        .map(|t| -> DaemonToolBox { Box::new(t) })
        .collect();
    eprintln!(
        "bh-rtd-daemon: wired {} BH tool(s) via {}",
        tools.len(),
        socket.display()
    );
    tools
}

/// No-tools build: refuse `BH_RTD_TOOLS_SOCKET` loudly instead of silently
/// ignoring it (the binary was built without the `tools` feature).
#[cfg(not(feature = "tools"))]
fn assemble_tools(socket: PathBuf) -> Vec<DaemonToolBox> {
    let _ = socket;
    eprintln!(
        "bh-rtd-daemon: fatal: BH_RTD_TOOLS_SOCKET is set, but this binary was built without \
         the `tools` feature (rebuild with default features)"
    );
    std::process::exit(2);
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
    let tools = match &tools_socket {
        Some(path) => assemble_tools(path.clone()),
        None => Vec::new(),
    };

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
