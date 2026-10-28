//! The `bh-rtd` daemon — the Phase 2 runtime wedge server (PLAN.md §4).
//!
//! A long-lived process that embeds [`crate::infinity_host::InfinityHost`]
//! (real `infinity-agent-core` threads) behind the BH bridge protocol v1
//! (`crate::uds`): UTF-8 JSON, one value per `\n`-terminated line over a
//! Unix domain socket, **one request per connection**:
//!
//! * Request: `{"id": <string>, "method": "<name>", "params": {...}}`
//! * Response (success): `{"id": <echoed>, "ok": true, "result": {...}}`
//! * Response (failure): `{"id": <echoed-or-null>, "ok": false,
//!   "error": {"code": <int>, "message": "<string>"}}`
//!
//! The Phase 1 envelope rule is preserved: `ok: false` is reserved for
//! bridge/protocol failures (parse `-32700`, invalid params `-32602`,
//! unknown method `-32601`, internal `-32603`); *business* outcomes (spawn
//! refused at capacity, event to an unknown agent, ...) travel as
//! `ok: true` with `accepted: false` and a human-readable `error` inside
//! `result` — nothing is silently swallowed on either side.
//!
//! ## Methods
//!
//! | method | params | result |
//! |---|---|---|
//! | `health` | — | `{status, service, protocol, runtime, version, brain, tools, threads, max_threads, timers_pending}` |
//! | `spawn_thread` | `name` (required), `role?`, `instructions?` | `{accepted, agent_id, thread_id}` |
//! | `send_event` | `agent_id` (required), `kind?` (`task_assigned`\|`message_received`\|`health_tick`\|`custom`), `payload?` | `{accepted}` |
//! | `shutdown_thread` | `agent_id` (required) | `{accepted, thread_id}` |
//! | `list_threads` | — | `{threads: [{agent_id, thread_id, role, events_seen, last_event}]}` |
//! | `schedule_timer` | `delay_ms` (required > 0), `agent_id?`, `payload?` | `{accepted, timer_id}` |
//! | `cancel_timer` | `timer_id` (required) | `{accepted}` |
//! | `list_timers` | — | `{timers: [TimerRecord::to_json()]}` |
//! | `shutdown` | — | `{status: "shutting_down"}` — stops the daemon after replying |
//!
//! `shutdown_thread` detaches the host from the thread (events stop; the
//! Infinity thread idles out and its history stays in the store —
//! hibernation semantics). Durable timers are persisted via
//! [`crate::timers::JsonTimerStore`] and survive daemon restarts; a fired
//! timer whose target thread is gone (e.g. after a restart, where thread
//! registries are in-memory until Phase 3) stays `fired` in the journal
//! instead of being dropped or pretending to be delivered.
//!
//! ## Environment (owned by the `bh-rtd` binary, interpreted here)
//!
//! `BH_RTD_SOCKET` (listen path), `BH_RTD_STATE_DIR` (timer journal),
//! `BH_RTD_MAX_THREADS`, `BH_RTD_BRAIN_SOCKET` + `BH_RTD_BRAIN_MODEL`
//! (Jeumpa brain) and `BH_RTD_TOOLS_SOCKET` (`src/agent_tools/uds_server.py`
//! peer; the binary then wires [`crate::infinity_host::InfinityHost`] with
//! the `bh-mcp-bridge` tools). See `docs/02-arch/infinity-adapter-contract.md`
//! for the full table. The binary also traps `SIGTERM`/`SIGINT` and turns
//! them into the same graceful stop as the `shutdown` method.
//!
//! Trust model: the socket is created with `0600` permissions and the
//! daemon refuses to bind over a socket that is currently being served.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use infinity_provider_protocol::{
    CompletionError, CompletionRequest, ErrorClass, ModelEntry, ModelProvider,
};
use serde_json::{json, Value};

use crate::host::{AgentHost, AgentId, AgentSpec, ColonyEvent, ColonyEventKind, HostError};
pub use crate::infinity_host::DaemonToolBox;
use crate::infinity_host::{InfinityHost, InfinityHostConfig};
use crate::timers::{TimerRecord, TimerWheel};
use crate::uds::{
    CODE_INTERNAL_ERROR, CODE_INVALID_REQUEST, CODE_PARSE_ERROR, CODE_UNKNOWN_METHOD,
    PROTOCOL_VERSION,
};

/// Environment variable: UDS path the daemon listens on.
pub const ENV_SOCKET: &str = "BH_RTD_SOCKET";
/// Environment variable: directory for the durable timer journal.
pub const ENV_STATE_DIR: &str = "BH_RTD_STATE_DIR";
/// Environment variable: bounded sub-agent pool size (default 16).
pub const ENV_MAX_THREADS: &str = "BH_RTD_MAX_THREADS";
/// Environment variable: UDS path of the Python Jeumpa brain peer
/// (`src/colony/jeumpa_uds_server.py`). Absent → the daemon runs without a
/// brain (thread lifecycle works; completion rounds fail loud).
pub const ENV_BRAIN_SOCKET: &str = "BH_RTD_BRAIN_SOCKET";
/// Environment variable: brain model id (default `default`, the Jeumpa
/// provider's first task profile).
pub const ENV_BRAIN_MODEL: &str = "BH_RTD_BRAIN_MODEL";
/// Environment variable: UDS path of the Python tools peer
/// (`src/agent_tools/uds_server.py`). When set, the `bh-rtd` binary wires
/// the host with [`crate::infinity_host::InfinityHostConfig::tools`] built
/// from `bh-mcp-bridge`'s catalog (BH tools execute Python-side, unchanged).
pub const ENV_TOOLS_SOCKET: &str = "BH_RTD_TOOLS_SOCKET";

/// Default bounded pool size.
pub const DEFAULT_MAX_THREADS: usize = 16;

/// Daemon configuration (the `bh-rtd` binary maps env vars onto this;
/// tests construct it directly).
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// UDS path to listen on.
    pub socket_path: PathBuf,
    /// Directory holding the durable timer journal (`timers.json`).
    pub state_dir: PathBuf,
    /// Bounded sub-agent pool size (`spawn_thread` fails at capacity).
    pub max_threads: usize,
    /// Jeumpa brain peer socket, if a brain is wired.
    pub brain_socket: Option<PathBuf>,
    /// Brain model id resolved from the provider catalog.
    pub brain_model: String,
    /// BH tools peer socket, if tools are wired (health descriptor only;
    /// the tool objects themselves are passed to [`run_daemon`]).
    pub tools_socket: Option<PathBuf>,
    /// Cooperative stop flag: set by the `shutdown` method, the binary's
    /// `SIGTERM`/`SIGINT` handler, or any test/embedding that wants a
    /// graceful stop. One flag drives the accept loop, the timer ticker and
    /// (transitively) the host shutdown.
    pub stop: Arc<AtomicBool>,
}

/// Errors from [`run_daemon`]. All fatal, all reported (no silent exit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonError {
    /// The UDS could not be bound (or is already served by a live daemon).
    Socket(String),
    /// The embedded host failed to start or shut down.
    Host(String),
    /// The durable timer store failed.
    Timer(String),
    /// Misc I/O failure.
    Io(String),
}

impl core::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DaemonError::Socket(m) => write!(f, "bh-rtd socket error: {m}"),
            DaemonError::Host(m) => write!(f, "bh-rtd host error: {m}"),
            DaemonError::Timer(m) => write!(f, "bh-rtd timer error: {m}"),
            DaemonError::Io(m) => write!(f, "bh-rtd io error: {m}"),
        }
    }
}

impl std::error::Error for DaemonError {}

/// A provider that advertises the configured model but fails every
/// completion round — the honest "no brain wired" mode. Thread lifecycle
/// (spawn / send / list / detach / timers) still works; the first
/// completion fails loud with `Fatal` (no retry, no fabrication).
pub struct NoBrainProvider {
    entry: ModelEntry,
}

#[async_trait]
impl ModelProvider for NoBrainProvider {
    async fn list_models(
        &self,
    ) -> Result<Vec<ModelEntry>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(vec![self.entry.clone()])
    }

    async fn invoke_model(
        &self,
        _model_id: &str,
        _request: CompletionRequest,
    ) -> Result<infinity_provider_protocol::ModelStream, CompletionError> {
        Err(CompletionError::provider(
            ErrorClass::Fatal,
            "bh-rtd has no brain configured: set BH_RTD_BRAIN_SOCKET to the \
             jeumpa_uds_server.py socket",
        ))
    }
}

/// Build the daemon's model provider from its configuration.
///
/// Returns the provider and a human-readable `brain` descriptor for
/// `health`.
pub fn build_provider(config: &DaemonConfig) -> (Arc<dyn ModelProvider>, String) {
    match &config.brain_socket {
        Some(path) => (
            Arc::new(crate::jeumpa::JeumpaProvider::new(path)),
            format!("jeumpa:{}", path.display()),
        ),
        None => (
            Arc::new(NoBrainProvider {
                entry: ModelEntry {
                    model_id: config.brain_model.clone(),
                    display_name: "bh-rtd/no-brain".into(),
                    context_window: 8192,
                    max_output_tokens: None,
                    supports_image_input: false,
                },
            }),
            "none".into(),
        ),
    }
}

/// Mutable daemon state shared by dispatchers.
pub struct DaemonState {
    stop: Arc<AtomicBool>,
    /// Human-readable brain descriptor (for `health`).
    brain: String,
    /// Human-readable tools descriptor (for `health`).
    tools: String,
    /// Configured pool limit (for `health`).
    max_threads: usize,
}

impl DaemonState {
    /// Whether a graceful stop was requested (the `shutdown` method, an
    /// external signal, or the embedding's stop flag — one shared
    /// [`AtomicBool`] for all of them).
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// The shared stop flag (the binary's signal handler stores into it).
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Build the shared state for [`run_daemon`] / [`dispatch`].
    pub fn new(
        brain: String,
        tools: String,
        max_threads: usize,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            stop,
            brain,
            tools,
            max_threads,
        }
    }
}

/// The daemon's shared host cell: `None` once the daemon is shutting down.
type SharedHost = Mutex<Option<InfinityHost>>;

/// Bind the daemon's UDS: refuse a socket that is currently served, unlink
/// a stale one, create with `0600`.
pub fn bind_socket(path: &Path) -> Result<UnixListener, DaemonError> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(DaemonError::Socket(format!(
                "{} is already served by a live daemon",
                path.display()
            )));
        }
        std::fs::remove_file(path)
            .map_err(|e| DaemonError::Socket(format!("stale socket {}: {e}", path.display())))?;
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DaemonError::Socket(format!("mkdir {}: {e}", parent.display())))?;
        }
    }
    let listener = UnixListener::bind(path)
        .map_err(|e| DaemonError::Socket(format!("bind {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(listener)
}

/// Run the daemon until a graceful stop is requested (the `shutdown`
/// method, an external signal flipping [`DaemonConfig::stop`], or a fatal
/// timer-store failure). Cleans up the socket and shuts the host down on
/// exit.
///
/// `tools` are the pre-assembled BH bridge tools for the Infinity tool
/// surface (built by the `bh-rtd` binary from `bh-mcp-bridge` when
/// `BH_RTD_TOOLS_SOCKET` is set; empty when no tools peer is configured).
/// The peer socket path travels in [`DaemonConfig::tools_socket`] purely as
/// the `health` descriptor.
pub fn run_daemon(config: DaemonConfig, tools: Vec<DaemonToolBox>) -> Result<(), DaemonError> {
    if config.max_threads == 0 {
        return Err(DaemonError::Host("max_threads must be >= 1".into()));
    }
    let (provider, brain) = build_provider(&config);
    let tools_desc = match &config.tools_socket {
        None => "none".to_owned(),
        Some(path) => {
            format!("bh-mcp-bridge:{} ({} tools)", path.display(), tools.len())
        }
    };
    let host: Arc<SharedHost> = Arc::new(Mutex::new(Some(
        InfinityHost::start(InfinityHostConfig {
            provider,
            model_id: config.brain_model.clone(),
            max_threads: config.max_threads,
            tools,
        })
        .map_err(|e| DaemonError::Host(e.to_string()))?,
    )));

    std::fs::create_dir_all(&config.state_dir)
        .map_err(|e| DaemonError::Io(format!("mkdir {}: {e}", config.state_dir.display())))?;
    let wheel = Arc::new(
        TimerWheel::new(Box::new(crate::timers::JsonTimerStore::new(
            config.state_dir.join("timers.json"),
        )))
        .map_err(|e| DaemonError::Timer(e.to_string()))?,
    );

    let state = Arc::new(DaemonState::new(
        brain,
        tools_desc,
        config.max_threads,
        Arc::clone(&config.stop),
    ));

    let listener = bind_socket(&config.socket_path)?;
    listener
        .set_nonblocking(true)
        .map_err(|e| DaemonError::Socket(format!("nonblocking: {e}")))?;
    eprintln!(
        "bh-rtd-daemon v{} listening on {} (brain: {}, tools: {})",
        env!("CARGO_PKG_VERSION"),
        config.socket_path.display(),
        state.brain,
        state.tools,
    );

    // Durable-timer ticker (started only after the socket binds, so a bind
    // failure cannot strand it): pops due timers and delivers them to live
    // threads. A store failure here is fatal (fail-loud): the daemon stops
    // rather than running with timers it cannot persist.
    let ticker = {
        let wheel = Arc::clone(&wheel);
        let host = Arc::clone(&host);
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("bh-rtd-timers".into())
            .spawn(move || ticker_loop(&wheel, &host, &state))
            .map_err(|e| DaemonError::Io(format!("spawn timer ticker: {e}")))?
    };

    while !state.stop_requested() {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let host = Arc::clone(&host);
                let wheel = Arc::clone(&wheel);
                let state = Arc::clone(&state);
                let _ = std::thread::Builder::new()
                    .name("bh-rtd-dispatch".into())
                    .spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
                        serve_one(stream, &host, &wheel, &state);
                    });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let err = DaemonError::Socket(format!("accept: {e}"));
                state.stop.store(true, Ordering::SeqCst);
                let _ = ticker.join();
                shutdown_quietly(&config.socket_path, &host);
                return Err(err);
            }
        }
    }

    // Stop the ticker first (it exits within one poll), then wind the host
    // down and remove the socket.
    let _ = ticker.join();
    shutdown_quietly(&config.socket_path, &host);
    Ok(())
}

fn shutdown_quietly(socket_path: &Path, host: &Arc<SharedHost>) {
    let _ = std::fs::remove_file(socket_path);
    // `take()` empties the cell so late dispatchers/ticker see a stopped
    // daemon instead of racing a half-shutdown host.
    if let Ok(mut cell) = host.lock() {
        if let Some(host) = cell.take() {
            let _ = host.shutdown();
        }
    }
}

/// One request per connection: read one line, dispatch, write one line.
fn serve_one(
    stream: UnixStream,
    host: &Arc<SharedHost>,
    wheel: &TimerWheel,
    state: &DaemonState,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return; // peer connected and left without sending (e.g. liveness probe)
    }
    let response = dispatch(line.trim(), host, wheel, state);
    let writer = reader.get_mut();
    let mut bytes = response.to_string().into_bytes();
    bytes.push(b'\n');
    let _ = writer.write_all(&bytes);
    let _ = writer.flush();
}

/// Timer ticker body: pop due timers, attempt delivery, journal the result.
fn ticker_loop(wheel: &TimerWheel, host: &Arc<SharedHost>, state: &DaemonState) {
    while !state.stop_requested() {
        std::thread::sleep(Duration::from_millis(25));
        let due = match wheel.due() {
            Ok(due) => due,
            Err(e) => {
                eprintln!("bh-rtd-daemon fatal: timer store failure: {e}");
                state.stop.store(true, Ordering::SeqCst);
                return;
            }
        };
        for record in due {
            deliver_timer(wheel, host, record);
        }
    }
}

fn deliver_timer(wheel: &TimerWheel, host: &Arc<SharedHost>, record: TimerRecord) {
    let delivered = match &record.agent_id {
        None => true, // fired, no delivery target required
        Some(agent_id) => {
            let payload = json!({
                "timer_id": record.id.0,
                "payload": record.payload,
            });
            let event = ColonyEvent {
                kind: ColonyEventKind::Custom,
                payload_json: payload.to_string(),
            };
            let outcome = host.lock().ok().and_then(|mut cell| {
                cell.as_mut()
                    .map(|host| host.send_event(&AgentId(agent_id.clone()), event))
            });
            match outcome {
                Some(Ok(())) => true,
                Some(Err(e)) => {
                    eprintln!("bh-rtd-daemon: timer delivery failed: {e}");
                    false
                }
                None => {
                    eprintln!("bh-rtd-daemon: host unavailable; timer not delivered");
                    false
                }
            }
        }
    };
    if delivered {
        if let Err(e) = wheel.mark_delivered(&record.id) {
            eprintln!("bh-rtd-daemon: marking timer delivered failed: {e}");
        }
    } else {
        // Stays `fired` in the journal: honest, never re-fired, never
        // reported as delivered.
        eprintln!(
            "bh-rtd-daemon: timer {} fired but its target agent is gone",
            record.id.0
        );
    }
}

/// Dispatch one request line to one response envelope (pure function over
/// shared state; also exercised directly by unit tests).
pub fn dispatch(
    request_line: &str,
    host: &Arc<SharedHost>,
    wheel: &TimerWheel,
    state: &DaemonState,
) -> Value {
    let parsed: Value = match serde_json::from_str(request_line) {
        Ok(v) => v,
        Err(e) => {
            return error_envelope(Value::Null, CODE_PARSE_ERROR, format!("not valid JSON: {e}"))
        }
    };
    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = parsed.get("method").and_then(Value::as_str) else {
        return error_envelope(
            id,
            CODE_INVALID_REQUEST,
            "request missing string 'method'".into(),
        );
    };
    let params = parsed.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "health" => health(host, wheel, state, id),
        "spawn_thread" => spawn_thread(host, params, id),
        "send_event" => send_event(host, params, id),
        "shutdown_thread" => shutdown_thread(host, params, id),
        "list_threads" => list_threads(host, id),
        "schedule_timer" => schedule_timer(wheel, params, id),
        "cancel_timer" => cancel_timer(wheel, params, id),
        "list_timers" => list_timers(wheel, id),
        "shutdown" => {
            state.stop.store(true, Ordering::SeqCst);
            ok_envelope(id, json!({"status": "shutting_down"}))
        }
        other => error_envelope(id, CODE_UNKNOWN_METHOD, format!("unknown method {other:?}")),
    }
}

fn ok_envelope(id: Value, result: Value) -> Value {
    json!({"id": id, "ok": true, "result": result})
}

fn error_envelope(id: Value, code: i64, message: String) -> Value {
    json!({"id": id, "ok": false, "error": {"code": code, "message": message}})
}

/// Render a [`HostError`] as a business failure result (`accepted: false`).
fn business_failure(e: &HostError) -> Value {
    json!({"accepted": false, "error": e.to_string()})
}

/// Whether a host error is a request-target outcome (→ `accepted: false`)
/// rather than a broken daemon (→ `ok: false` internal error).
fn is_business_error(e: &HostError) -> bool {
    matches!(
        e,
        HostError::UnknownAgent(_) | HostError::Capacity { .. } | HostError::DuplicateAgent(_)
    )
}

/// Run `f` with the live host; `None` after the daemon started shutting
/// down (surfaced as an internal error, never as fake success).
fn with_host<T>(
    host: &Arc<SharedHost>,
    f: impl FnOnce(&mut InfinityHost) -> Result<T, HostError>,
) -> Result<T, HostError> {
    let mut cell = host
        .lock()
        .map_err(|_| HostError::Shutdown("host mutex poisoned".into()))?;
    match cell.as_mut() {
        Some(host) => f(host),
        None => Err(HostError::Shutdown("daemon is shutting down".into())),
    }
}

fn health(host: &Arc<SharedHost>, wheel: &TimerWheel, state: &DaemonState, id: Value) -> Value {
    let threads = with_host(host, |h| h.thread_stats().map(|s| s.len()));
    let timers_pending = wheel.pending_count();
    match (threads, timers_pending) {
        (Ok(threads), Ok(timers_pending)) => ok_envelope(
            id,
            json!({
                "status": "ok",
                "service": "bh-rtd",
                "protocol": PROTOCOL_VERSION,
                "runtime": "rust",
                "version": env!("CARGO_PKG_VERSION"),
                "brain": state.brain,
                "tools": state.tools,
                "threads": threads,
                "max_threads": state.max_threads,
                "timers_pending": timers_pending,
            }),
        ),
        (Err(e), _) => {
            error_envelope(id, CODE_INTERNAL_ERROR, format!("health failed: {e}"))
        }
        (_, Err(e)) => {
            error_envelope(id, CODE_INTERNAL_ERROR, format!("health failed: {e}"))
        }
    }
}

fn spawn_thread(host: &Arc<SharedHost>, params: Value, id: Value) -> Value {
    let Some(name) = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error_envelope(
            id,
            CODE_INVALID_REQUEST,
            "spawn_thread requires non-empty string 'name'".into(),
        );
    };
    let role = params
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("worker")
        .to_owned();
    let instructions = params
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    let outcome = with_host(host, |h| {
        h.spawn_thread_full(AgentSpec {
            name: name.clone(),
            role,
            instructions,
        })
    });
    match outcome {
        Ok((agent_id, thread_id)) => ok_envelope(
            id,
            json!({
                "accepted": true,
                "agent_id": agent_id.0,
                "thread_id": thread_id,
            }),
        ),
        Err(e) if is_business_error(&e) => ok_envelope(id, business_failure(&e)),
        Err(e) => error_envelope(id, CODE_INTERNAL_ERROR, format!("spawn_thread failed: {e}")),
    }
}

fn send_event(host: &Arc<SharedHost>, params: Value, id: Value) -> Value {
    let Some(agent_id) = params
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error_envelope(
            id,
            CODE_INVALID_REQUEST,
            "send_event requires non-empty string 'agent_id'".into(),
        );
    };
    let kind = match params.get("kind").and_then(Value::as_str) {
        None | Some("custom") => ColonyEventKind::Custom,
        Some("task_assigned") => ColonyEventKind::TaskAssigned,
        Some("message_received") => ColonyEventKind::MessageReceived,
        Some("health_tick") => ColonyEventKind::HealthTick,
        Some(other) => {
            return error_envelope(
                id,
                CODE_INVALID_REQUEST,
                format!(
                    "unknown event kind {other:?} (expected task_assigned, \
                     message_received, health_tick, or custom)"
                ),
            )
        }
    };
    // A JSON string payload is delivered verbatim as the thread input text;
    // any other JSON value is serialized so its structure survives.
    let payload_json = match params.get("payload") {
        None | Some(Value::Null) => "{}".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };

    let outcome = with_host(host, |h| {
        h.send_event(
            &AgentId(agent_id),
            ColonyEvent {
                kind,
                payload_json,
            },
        )
    });
    match outcome {
        Ok(()) => ok_envelope(id, json!({"accepted": true})),
        Err(e) if is_business_error(&e) => ok_envelope(id, business_failure(&e)),
        Err(e) => error_envelope(id, CODE_INTERNAL_ERROR, format!("send_event failed: {e}")),
    }
}

fn shutdown_thread(host: &Arc<SharedHost>, params: Value, id: Value) -> Value {
    let Some(agent_id) = params
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error_envelope(
            id,
            CODE_INVALID_REQUEST,
            "shutdown_thread requires non-empty string 'agent_id'".into(),
        );
    };
    let outcome = with_host(host, |h| h.detach_agent(&AgentId(agent_id)));
    match outcome {
        Ok(thread_id) => ok_envelope(id, json!({"accepted": true, "thread_id": thread_id})),
        Err(e) if is_business_error(&e) => ok_envelope(id, business_failure(&e)),
        Err(e) => {
            error_envelope(id, CODE_INTERNAL_ERROR, format!("shutdown_thread failed: {e}"))
        }
    }
}

fn list_threads(host: &Arc<SharedHost>, id: Value) -> Value {
    let outcome = with_host(host, |h| h.thread_stats());
    match outcome {
        Ok(stats) => {
            let threads: Vec<Value> = stats
                .into_iter()
                .map(|s| {
                    json!({
                        "agent_id": s.agent_id,
                        "thread_id": s.thread_id,
                        "role": s.role,
                        "events_seen": s.events_seen,
                        "last_event": s.last_event,
                    })
                })
                .collect();
            ok_envelope(id, json!({ "threads": threads }))
        }
        Err(e) => error_envelope(id, CODE_INTERNAL_ERROR, format!("list_threads failed: {e}")),
    }
}

fn schedule_timer(wheel: &TimerWheel, params: Value, id: Value) -> Value {
    let Some(delay_ms) = params.get("delay_ms").and_then(Value::as_u64).filter(|d| *d > 0) else {
        return error_envelope(
            id,
            CODE_INVALID_REQUEST,
            "schedule_timer requires u64 'delay_ms' > 0".into(),
        );
    };
    let agent_id = params
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);

    match wheel.schedule(delay_ms, agent_id, payload) {
        Ok(timer_id) => ok_envelope(id, json!({"accepted": true, "timer_id": timer_id.0})),
        Err(e) => error_envelope(id, CODE_INTERNAL_ERROR, format!("schedule_timer failed: {e}")),
    }
}

fn cancel_timer(wheel: &TimerWheel, params: Value, id: Value) -> Value {
    let Some(timer_id) = params
        .get("timer_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error_envelope(
            id,
            CODE_INVALID_REQUEST,
            "cancel_timer requires non-empty string 'timer_id'".into(),
        );
    };
    match wheel.cancel(&crate::timers::TimerId(timer_id.clone())) {
        Ok(true) => ok_envelope(id, json!({"accepted": true})),
        Ok(false) => ok_envelope(
            id,
            json!({"accepted": false, "error": format!("timer {timer_id:?} already fired/cancelled")}),
        ),
        Err(crate::timers::TimerError::UnknownTimer(_)) => ok_envelope(
            id,
            json!({"accepted": false, "error": format!("unknown timer id {timer_id:?}")}),
        ),
        Err(e) => error_envelope(id, CODE_INTERNAL_ERROR, format!("cancel_timer failed: {e}")),
    }
}

fn list_timers(wheel: &TimerWheel, id: Value) -> Value {
    match wheel.snapshot() {
        Ok(timers) => {
            let timers: Vec<Value> = timers.iter().map(TimerRecord::to_json).collect();
            ok_envelope(id, json!({ "timers": timers }))
        }
        Err(e) => error_envelope(id, CODE_INTERNAL_ERROR, format!("list_timers failed: {e}")),
    }
}
