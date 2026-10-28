//! Phase 2 tests for the `bh-rtd` daemon — the runtime wedge server.
//!
//! These are **real end-to-end** tests: each one spawns the actual
//! `bh-rtd` binary (via Cargo's `CARGO_BIN_EXE_*` mechanism) as a
//! child process on a private UDS + state dir, then drives it with the
//! Phase 1 `UdsClient` speaking BH bridge protocol v1 — the same path the
//! Python `BH_RUNTIME=rust` gate uses. No mocks, no Python, no network.
//!
//! Covered: health, spawn/list/send/shutdown-thread lifecycle, business
//! failures (`accepted: false`), protocol failures (`ok: false`), the
//! no-brain completion contract, durable timer persistence across daemon
//! restarts, live-bind refusal, and the graceful `shutdown` method.

#![cfg(feature = "infinity")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bh_rtd::uds::UdsClient;
use serde_json::{json, Value};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bh-rtd-test-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// ── Daemon process handle ──

struct DaemonProc {
    child: Child,
    socket: PathBuf,
    state_dir: PathBuf,
    /// When true, Drop keeps the state dir on disk — used by the
    /// restart-persistence tests, which inspect the journal *after* the
    /// first daemon has stopped (shutdown_via_protocol/hard_stop consume
    /// `self`, so Drop runs inside them).
    keep_state: bool,
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        // Safety net so a failing assert never leaks a daemon: SIGKILL and
        // reap. Clean shutdowns remove the socket file themselves.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        if !self.keep_state {
            let _ = std::fs::remove_dir_all(&self.state_dir);
        }
    }
}

impl DaemonProc {
    /// Spawn the real binary. `extra_env` is layered on top of the base
    /// socket/state-dir env (used to override `BH_RTD_MAX_THREADS`).
    fn spawn(tag: &str, extra_env: &[(&str, &str)]) -> Self {
        let dir = temp_dir(tag);
        let socket = dir.join("bh-rtd.sock");
        let state_dir = dir.join("state");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_bh-rtd"));
        cmd.env("BH_RTD_SOCKET", &socket)
            .env("BH_RTD_STATE_DIR", &state_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn bh-rtd binary");
        let proc = Self {
            child,
            socket,
            state_dir,
            keep_state: false,
        };
        wait_until_healthy(&proc.socket);
        proc
    }

    /// A client configured for this daemon's socket.
    fn client(&self) -> UdsClient {
        let mut client = UdsClient::new(&self.socket);
        client.set_timeout(Some(Duration::from_secs(30)));
        client
    }

    /// Graceful stop via the protocol's `shutdown` method; asserts exit 0.
    fn shutdown_via_protocol(mut self) {
        let result = self.client().call("shutdown", &json!({}));
        assert!(result.is_ok(), "shutdown method must answer ok");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match self.child.try_wait().expect("wait daemon") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "daemon must exit cleanly, got {status}"
                    );
                    break;
                }
                None => {
                    assert!(Instant::now() < deadline, "daemon did not exit");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        assert!(
            !self.socket.exists(),
            "graceful shutdown removes the socket file"
        );
        // Drop must not kill anything — the child is already reaped.
        self.child.wait().expect("reap");
    }

    /// Kill immediately (used for the restart-persistence scenario).
    fn hard_stop(mut self) {
        let _ = self.child.kill();
        self.child.wait().expect("reap killed daemon");
    }
}

// ── Readiness polling (real UDS `health` round-trip) ──

/// Poll until the daemon on `socket` answers `health`; returns a client
/// with a generous timeout. Fails loud on timeout.
fn wait_until_healthy(socket: &Path) -> UdsClient {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut client = UdsClient::new(socket);
        client.set_timeout(Some(Duration::from_secs(5)));
        if client.call("health", &json!({})).is_ok() {
            client.set_timeout(Some(Duration::from_secs(30)));
            return client;
        }
        assert!(Instant::now() < deadline, "daemon did not become ready in time");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ── Raw-line helpers (protocol-failure paths the typed client can't send) ──

fn raw_line(socket: &Path, line: &str) -> Value {
    let mut stream = UnixStream::connect(socket).expect("connect raw");
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .expect("write raw line");
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read raw reply");
    serde_json::from_str(reply.trim()).expect("raw reply is JSON")
}

/// Assert the envelope is a business success and return its `result`.
fn expect_ok(result: Result<Value, bh_rtd::uds::UdsError>, ctx: &str) -> Value {
    match result {
        Ok(v) => v,
        Err(e) => panic!("{ctx} failed: {e}"),
    }
}

fn expect_accepted(result: Result<Value, bh_rtd::uds::UdsError>, ctx: &str) -> Value {
    let v = expect_ok(result, ctx);
    assert_eq!(
        v.get("accepted").and_then(Value::as_bool),
        Some(true),
        "{ctx} must be accepted, got: {v}"
    );
    v
}

// ── The end-to-end lifecycle over a real UDS ──

#[test]
fn full_lifecycle_over_real_uds() {
    let daemon = DaemonProc::spawn("lifecycle", &[]);
    let client = daemon.client();

    // health: the daemon identifies itself honestly.
    let health = expect_ok(client.call("health", &json!({})), "health");
    assert_eq!(health.get("status").and_then(Value::as_str), Some("ok"));
    assert_eq!(health.get("service").and_then(Value::as_str), Some("bh-rtd"));
    assert_eq!(health.get("runtime").and_then(Value::as_str), Some("rust"));
    assert_eq!(health.get("protocol").and_then(Value::as_i64), Some(1));
    assert_eq!(health.get("brain").and_then(Value::as_str), Some("none"));
    assert_eq!(health.get("threads").and_then(Value::as_i64), Some(0));
    assert_eq!(health.get("max_threads").and_then(Value::as_i64), Some(16));

    // spawn_thread: accepted, ids reported.
    let spawned = expect_accepted(
        client.call("spawn_thread", &json!({"name": "worker-1", "role": "analyst"})),
        "spawn worker-1",
    );
    assert_eq!(spawned.get("agent_id").and_then(Value::as_str), Some("worker-1"));
    let thread_id = spawned
        .get("thread_id")
        .and_then(Value::as_str)
        .expect("thread_id reported")
        .to_owned();
    assert!(!thread_id.is_empty());

    // Duplicate name → business refusal, not a protocol error.
    let dupe = expect_ok(
        client.call("spawn_thread", &json!({"name": "worker-1"})),
        "duplicate spawn",
    );
    assert_eq!(dupe.get("accepted").and_then(Value::as_bool), Some(false));
    assert!(dupe
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("worker-1"));

    // list_threads reflects the member agent.
    let listed = expect_ok(client.call("list_threads", &json!({})), "list_threads");
    let threads = listed.get("threads").and_then(Value::as_array).expect("array");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].get("agent_id").and_then(Value::as_str), Some("worker-1"));
    assert_eq!(threads[0].get("role").and_then(Value::as_str), Some("analyst"));
    assert_eq!(threads[0].get("events_seen").and_then(Value::as_i64), Some(0));

    // send_event reaches the thread (Custom payload is the thread input).
    expect_accepted(
        client.call(
            "send_event",
            &json!({"agent_id": "worker-1", "payload": "hello colony"}),
        ),
        "send_event",
    );
    // The event is delivered over the input queue; stats eventually show it.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let listed = expect_ok(client.call("list_threads", &json!({})), "list_threads");
        let seen = listed.get("threads").and_then(Value::as_array).unwrap()[0]
            .get("events_seen")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if seen >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "event never observed by thread");
        std::thread::sleep(Duration::from_millis(25));
    }

    // Business failure: event to an unknown agent.
    let miss = expect_ok(
        client.call("send_event", &json!({"agent_id": "ghost"})),
        "send_event ghost",
    );
    assert_eq!(miss.get("accepted").and_then(Value::as_bool), Some(false));

    // shutdown_thread detaches; a later event is refused (business).
    let detached = expect_accepted(
        client.call("shutdown_thread", &json!({"agent_id": "worker-1"})),
        "shutdown_thread",
    );
    assert_eq!(detached.get("thread_id").and_then(Value::as_str), Some(thread_id.as_str()));
    assert_eq!(
        expect_ok(client.call("list_threads", &json!({})), "list after detach")
            .get("threads")
            .and_then(Value::as_array)
            .expect("array")
            .len(),
        0
    );
    let after = expect_ok(
        client.call("send_event", &json!({"agent_id": "worker-1"})),
        "send after detach",
    );
    assert_eq!(after.get("accepted").and_then(Value::as_bool), Some(false));

    daemon.shutdown_via_protocol();
}

#[test]
fn protocol_failures_are_envelope_errors() {
    let daemon = DaemonProc::spawn("protocol", &[]);
    let client = daemon.client();

    // Unknown method → ok:false -32601.
    match client.call("teleport_agent", &json!({})) {
        Err(bh_rtd::uds::UdsError::Server { code, message }) => {
            assert_eq!(code, -32601);
            assert!(message.contains("teleport_agent"));
        }
        other => panic!("unknown method must be a server error, got {other:?}"),
    }

    // Malformed params → ok:false -32602 (missing required name).
    match client.call("spawn_thread", &json!({})) {
        Err(bh_rtd::uds::UdsError::Server { code, .. }) => assert_eq!(code, -32602),
        other => panic!("missing name must be a server error, got {other:?}"),
    }

    // send_event with an unknown kind → -32602 (validated server-side).
    match client.call(
        "send_event",
        &json!({"agent_id": "a", "kind": "mind_control"}),
    ) {
        Err(bh_rtd::uds::UdsError::Server { code, .. }) => assert_eq!(code, -32602),
        other => panic!("unknown event kind must be a server error, got {other:?}"),
    }

    // Not-JSON line → ok:false -32700, with the id echoed as null.
    let reply = raw_line(&daemon.socket, "definitely not json");
    assert_eq!(reply.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        reply.pointer("/error/code").and_then(Value::as_i64),
        Some(-32700)
    );

    // Missing 'method' → -32602.
    let reply = raw_line(&daemon.socket, r#"{"id": "q1"}"#);
    assert_eq!(
        reply.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602)
    );
    assert_eq!(reply.get("id").and_then(Value::as_str), Some("q1"));

    daemon.shutdown_via_protocol();
}

#[test]
fn schedule_cancel_list_timers_over_uds() {
    let daemon = DaemonProc::spawn("timers", &[]);
    let client = daemon.client();

    let scheduled = expect_accepted(
        client.call(
            "schedule_timer",
            &json!({"delay_ms": 60_000, "agent_id": "worker-1", "payload": {"n": 1}}),
        ),
        "schedule_timer",
    );
    let timer_id = scheduled
        .get("timer_id")
        .and_then(Value::as_str)
        .expect("timer id")
        .to_owned();

    let health = expect_ok(client.call("health", &json!({})), "health");
    assert_eq!(health.get("timers_pending").and_then(Value::as_i64), Some(1));

    let listed = expect_ok(client.call("list_timers", &json!({})), "list_timers");
    let timers = listed.get("timers").and_then(Value::as_array).expect("array");
    assert_eq!(timers.len(), 1);
    assert_eq!(timers[0].get("timer_id").and_then(Value::as_str), Some(timer_id.as_str()));
    assert_eq!(timers[0].get("status").and_then(Value::as_str), Some("pending"));
    assert_eq!(timers[0].get("agent_id").and_then(Value::as_str), Some("worker-1"));

    expect_accepted(
        client.call("cancel_timer", &json!({"timer_id": timer_id})),
        "cancel_timer",
    );
    let health = expect_ok(client.call("health", &json!({})), "health after cancel");
    assert_eq!(health.get("timers_pending").and_then(Value::as_i64), Some(0));

    // Invalid delay (0) → -32602.
    match client.call("schedule_timer", &json!({"delay_ms": 0})) {
        Err(bh_rtd::uds::UdsError::Server { code, .. }) => assert_eq!(code, -32602),
        other => panic!("delay_ms=0 must be a server error, got {other:?}"),
    }

    daemon.shutdown_via_protocol();
}

#[test]
fn timers_persist_across_daemon_restart() {
    let tag = "restart";
    // First boot: schedule a long timer, then stop the daemon cleanly.
    let socket;
    let state_dir;
    {
        let mut daemon = DaemonProc::spawn(tag, &[]);
        socket = daemon.socket.clone();
        state_dir = daemon.state_dir.clone();
        expect_accepted(
            daemon
                .client()
                .call("schedule_timer", &json!({"delay_ms": 60_000})),
            "schedule before restart",
        );
        // Keep the state dir: this test verifies the journal outlives the
        // first daemon (Drop would otherwise wipe it inside the stop call).
        daemon.keep_state = true;
        daemon.shutdown_via_protocol();
    }

    // The journal is on disk right now, between the two runs.
    let journal = state_dir.join("timers.json");
    assert!(journal.exists(), "timer journal must exist between restarts");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(&journal).expect("read journal"))
            .expect("journal JSON");
    assert_eq!(
        doc.get("timers").and_then(Value::as_array).expect("array")[0]
            .get("status")
            .and_then(Value::as_str),
        Some("pending")
    );

    // Second boot over the same state dir: the timer is reloaded, not lost.
    let dir = socket.parent().unwrap().to_path_buf();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bh-rtd"));
    cmd.env("BH_RTD_SOCKET", &socket)
        .env("BH_RTD_STATE_DIR", &state_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("respawn daemon");
    let client = wait_until_healthy(&socket);

    let health = expect_ok(client.call("health", &json!({})), "health after restart");
    assert_eq!(health.get("timers_pending").and_then(Value::as_i64), Some(1));

    // Cancel it so the second boot's own state is clean, then stop.
    let listed = expect_ok(client.call("list_timers", &json!({})), "list after restart");
    let timer_id = listed.get("timers").and_then(Value::as_array).expect("array")[0]
        .get("timer_id")
        .and_then(Value::as_str)
        .expect("id")
        .to_owned();
    expect_accepted(client.call("cancel_timer", &json!({"timer_id": timer_id})), "cancel");

    let second = DaemonProc {
        child,
        socket: socket.clone(),
        state_dir: state_dir.clone(),
        keep_state: false,
    };
    second.shutdown_via_protocol();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fired_timer_for_gone_agent_stays_fired_not_delivered() {
    // Schedule a short timer targeting an agent that will not exist in the
    // next boot; restart; the ticker fires it and journals `fired` — honest
    // non-delivery, never a fake `delivered`.
    let mut daemon = DaemonProc::spawn("fired", &[]);
    expect_accepted(
        daemon
            .client()
            .call("schedule_timer", &json!({"delay_ms": 30, "agent_id": "ghost"})),
        "schedule ghost timer",
    );
    let socket = daemon.socket.clone();
    let state_dir = daemon.state_dir.clone();
    // SIGKILL: the journal must survive the crash and the restarted daemon
    // must fire the overdue timer on its first tick.
    daemon.keep_state = true;
    daemon.hard_stop();

    // Restart over the same state dir. The 30 ms deadline passes while the
    // daemon is down: the timer must fire on the first tick of the new boot.
    let daemon2 = DaemonProc {
        child: Command::new(env!("CARGO_BIN_EXE_bh-rtd"))
            .env("BH_RTD_SOCKET", &socket)
            .env("BH_RTD_STATE_DIR", &state_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("respawn daemon"),
        socket: socket.clone(),
        state_dir: state_dir.clone(),
        keep_state: false,
    };
    let client = wait_until_healthy(&socket);

    let status = wait_for_status(&client, "fired", Duration::from_secs(10));
    assert_eq!(status, "fired");

    // It must NOT be re-fired nor marked delivered: exactly one record,
    // terminal status `fired`.
    let listed = expect_ok(client.call("list_timers", &json!({})), "list");
    let timers = listed.get("timers").and_then(Value::as_array).expect("array");
    assert_eq!(timers.len(), 1);
    assert_eq!(timers[0].get("status").and_then(Value::as_str), Some("fired"));

    daemon2.shutdown_via_protocol();
}

fn wait_for_status(client: &UdsClient, target: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let listed = expect_ok(client.call("list_timers", &json!({})), "list poll");
        let status = listed.get("timers").and_then(Value::as_array).expect("array")[0]
            .get("status")
            .and_then(Value::as_str)
            .expect("status")
            .to_owned();
        if status == target {
            return status;
        }
        assert!(Instant::now() < deadline, "timer never reached {target:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn daemon_refuses_to_bind_a_live_socket() {
    let daemon = DaemonProc::spawn("bind-refusal", &[]);
    // Second process, same socket: must fail loud, not steal the socket.
    let mut second = Command::new(env!("CARGO_BIN_EXE_bh-rtd"))
        .env("BH_RTD_SOCKET", &daemon.socket)
        .env("BH_RTD_STATE_DIR", temp_dir("bind-refusal-other-state"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn second daemon");
    let output_deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = second.try_wait().expect("wait second") {
            break status;
        }
        assert!(Instant::now() < output_deadline, "second daemon hung");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(!status.success(), "second daemon must exit non-zero");
    // The original daemon is untouched and still answers.
    let health = expect_ok(daemon.client().call("health", &json!({})), "health after refusal");
    assert_eq!(health.get("status").and_then(Value::as_str), Some("ok"));
    daemon.shutdown_via_protocol();
}

#[test]
fn max_threads_env_is_enforced_at_capacity() {
    let daemon = DaemonProc::spawn("capacity", &[("BH_RTD_MAX_THREADS", "1")]);
    let client = daemon.client();

    let health = expect_ok(client.call("health", &json!({})), "health");
    assert_eq!(health.get("max_threads").and_then(Value::as_i64), Some(1));

    expect_accepted(
        client.call("spawn_thread", &json!({"name": "only"})),
        "spawn only",
    );
    let refused = expect_ok(
        client.call("spawn_thread", &json!({"name": "extra"})),
        "spawn over capacity",
    );
    assert_eq!(refused.get("accepted").and_then(Value::as_bool), Some(false));
    assert!(refused
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("capacity"));

    daemon.shutdown_via_protocol();
}

#[test]
fn missing_required_env_fails_loud_before_binding() {
    // No BH_RTD_SOCKET → immediate non-zero exit with an actionable message.
    let mut child = Command::new(env!("CARGO_BIN_EXE_bh-rtd"))
        .env("BH_RTD_STATE_DIR", temp_dir("env-missing"))
        .env_remove("BH_RTD_SOCKET")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon without socket env");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        assert!(Instant::now() < deadline, "daemon hung without env");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(stderr.contains("BH_RTD_SOCKET"), "stderr: {stderr}");
    let _ = child.wait();
}

#[test]
fn native_rap_tools_serve_tools_list_and_direct_calls() {
    // Phase 4: the three native RAP tools are served by the daemon itself —
    // `tools/list` flags them native, `tools/call` executes them in-process
    // inside the workspace sandbox, and non-native names report not-found
    // when no MCP proxy peer is wired.
    let ws = temp_dir("tools-ws");
    let daemon = DaemonProc::spawn(
        "tools",
        &[(
            "BH_RTD_WORKSPACE_ROOT",
            Box::leak(ws.to_string_lossy().into_owned().into_boxed_str()) as &str,
        )],
    );
    let client = daemon.client();

    // tools/list: exactly the native set, honestly flagged.
    let listed = expect_ok(client.call("tools/list", &json!({})), "tools/list");
    let tools = listed
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array");
    assert_eq!(tools.len(), 3, "native-only when no proxy peer is wired");
    for tool in tools {
        assert_eq!(tool.get("native").and_then(Value::as_bool), Some(true));
        assert_eq!(
            tool.get("transport").and_then(Value::as_str),
            Some("native-rust")
        );
    }
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["file", "shell.execute", "search.web"]);

    // Direct native call: write + read round-trip through the real engine.
    let written = expect_ok(
        client.call(
            "tools/call",
            &json!({"name": "file", "args": {"operation": "write", "path": "note.txt", "content": "hello phase 4\n"}}),
        ),
        "native write",
    );
    assert_eq!(written["success"], true, "{written}");
    let read = expect_ok(
        client.call(
            "tools/call",
            &json!({"name": "file", "args": {"operation": "read", "path": "note.txt"}}),
        ),
        "native read",
    );
    assert_eq!(read["output"], "hello phase 4\n");
    assert!(ws.join("note.txt").exists(), "the write stayed in the sandbox");

    // Blocked shell command: the peer's own failure shape — exit_code -2
    // plus the policy message in stderr, never an execution.
    let blocked = expect_ok(
        client.call(
            "tools/call",
            &json!({"name": "shell.execute", "args": {"command": "rm -rf /"}}),
        ),
        "blocked shell",
    );
    assert_eq!(blocked["exit_code"], -2, "{blocked}");
    assert!(blocked["stderr"]
        .as_str()
        .expect("stderr")
        .starts_with("Command blocked by safety policy"));

    // Non-native name with no proxy peer wired → not found (-32601).
    let missing = expect_ok(
        client.call("tools/call", &json!({"name": "code.operations", "args": {}})),
        "proxy-less call",
    );
    assert_eq!(missing["success"], false);
    assert_eq!(missing["error_code"], -32601);

    daemon.shutdown_via_protocol();
    let _ = std::fs::remove_dir_all(&ws);
}
