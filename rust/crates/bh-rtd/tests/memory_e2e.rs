//! Phase 3 exit-criteria proof: **hibernating agents persist into BH
//! memory** (PLAN.md §4 — "Memory seam ... Exit criteria: Hibernating
//! agents persist into BH MemoryFederation").
//!
//! This is the only Rust test in the workspace that leaves the hermetic
//! bubble, on purpose: it spawns the **real Python peer**
//! (`python3 -m src.memory.rtd_memory_server`) over the real
//! `src/memory/` stack — `SessionMemory` (SQLite), `MemorySystem` LTM
//! (SQLite + FTS5), `MemoryFederation`, episodic memory — and drives it
//! with the real seam client ([`UdsMemoryClient`]):
//!
//! 1. **Hibernate**: create a thread, append a conversation, store
//!    hibernation state, close the thread → the peer persists everything
//!    into SQLite *and* shares a hibernation record into the BH
//!    `MemoryFederation` (recalled here, in-process, before restart).
//! 2. **Restart**: the peer process is SIGKILLed (harsher than a graceful
//!    stop — SQLite commits per request, so this is the stronger
//!    durability claim) and a fresh peer starts over the **same** DB
//!    files. The conversation, the closed status and the state entries
//!    are read back through the seam — nothing was lost.
//!
//! No SQLite is ever touched from Rust (that would be porting, not
//! bridging). ChromaDB read-through is intentionally absent — see the
//! `src/memory/rtd_memory_server.py` docstring (post-MVP, not faked).

#![cfg(feature = "bridge")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bh_rtd::memory::{
    ConversationStore, MemoryMessage, SharedMemoryStore, StateStore, UdsMemoryClient,
};
use serde_json::json;

static SEQ: AtomicU64 = AtomicU64::new(0);

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

struct PeerProcess {
    child: Child,
    stderr_pipe: std::process::ChildStderr,
}

impl Drop for PeerProcess {
    fn drop(&mut self) {
        // SIGKILL: durability must not depend on a graceful stop.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn repo_root() -> PathBuf {
    // .../rust/crates/bh-rtd → repo root is three levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .expect("CARGO_MANIFEST_DIR sits three levels below the repo root")
}

/// Spawn `python3 -m src.memory.rtd_memory_server` for the given work dir.
fn spawn_peer(work_dir: &Path, socket: &Path) -> PeerProcess {
    let root = repo_root();
    let data_dir = work_dir.join("memory");
    let sessions_db = work_dir.join("sessions").join("sessions.db");
    std::fs::create_dir_all(&data_dir).expect("create memory data dir");
    std::fs::create_dir_all(sessions_db.parent().unwrap()).expect("create sessions dir");

    let child = Command::new("python3")
        .current_dir(&root)
        .env("PYTHONPATH", &root)
        .arg("-m")
        .arg("src.memory.rtd_memory_server")
        .arg("--socket")
        .arg(socket)
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--sessions-db")
        .arg(&sessions_db)
        .arg("--colony-id")
        .arg("bh-colony")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn python3 peer (is python3 on PATH?): {e}"));
    let mut child = child;
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    PeerProcess { child, stderr_pipe }
}

/// Block until the peer answers `health` (or dump its stderr and panic).
fn wait_ready(client: &UdsMemoryClient, peer: &mut PeerProcess) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_err = String::new();
    while Instant::now() < deadline {
        if peer
            .child
            .try_wait()
            .map(|s| s.is_some())
            .unwrap_or(false)
        {
            let mut stderr = String::new();
            let _ = peer.stderr_pipe.read_to_string(&mut stderr);
            panic!("python memory peer exited during startup:\n{stderr}");
        }
        match client.health() {
            Ok(result) if result["status"] == "ok" => return,
            Ok(other) => last_err = format!("unexpected health result: {other}"),
            Err(e) => last_err = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    // Kill the wedged peer so the stderr pipe reaches EOF and we can
    // include it in the failure (fail loud, with evidence).
    let _ = peer.child.kill();
    let _ = peer.child.wait();
    let mut stderr = String::new();
    let _ = peer.stderr_pipe.read_to_string(&mut stderr);
    panic!(
        "peer not ready after {READY_TIMEOUT:?}; last error: {last_err}; stderr:\n{stderr}"
    );
}

fn new_client(socket: &Path) -> UdsMemoryClient {
    let mut client = UdsMemoryClient::new(socket);
    client.set_timeout(Some(CALL_TIMEOUT));
    client
}

fn fresh_work_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bh-rtd-mem-e2e-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create e2e work dir");
    dir
}

/// One agent's hibernation → restart → read-back cycle, end to end.
#[test]
fn hibernating_agent_persists_and_wakes_with_state_intact() {
    let work_dir = fresh_work_dir();
    let socket = work_dir.join("memory.sock");
    let agent = "hiber-agent";

    // ── Phase 1: hibernate through the seam ────────────────────────────
    let mut peer_a = spawn_peer(&work_dir, &socket);
    let client = new_client(&socket);
    wait_ready(&client, &mut peer_a);

    let health = client.health().expect("health");
    assert_eq!(health["service"], "memory-uds");
    assert_eq!(health["protocol"], "v1");

    // The thread the agent lives in, with its conversation.
    let thread = client
        .create_thread(agent, "bh-colony")
        .expect("create thread");
    let session_id = thread.session_id.clone();
    assert!(!session_id.is_empty());
    assert!(client.thread_exists(&session_id).expect("exists"));

    client
        .append_messages(
            &session_id,
            &[
                MemoryMessage::new(
                    "user",
                    "pre-hibernation orders: hold ETH, report at dawn",
                ),
                MemoryMessage::new("assistant", "state saved; going dormant"),
            ],
        )
        .expect("append conversation");

    // The agent's durable state snapshot.
    client
        .set_state(
            &format!("{agent}:portfolio"),
            &json!({ "eth": 12, "mode": "hibernated" }),
            &["hibernation", agent],
            Some(0.9),
        )
        .expect("set state");

    // Hibernate: close the thread → peer persists the closed session,
    // shares the hibernation record into BH MemoryFederation, records a
    // durable `hibernate` episode.
    let closed = client
        .close_thread(&session_id, Some("nightly hibernation window"))
        .expect("close thread");
    assert_eq!(closed.status, "closed");
    assert!(client.is_thread_closed(&session_id).expect("closed check"));

    // Exit criterion, live layer: the record is recallable from the
    // MemoryFederation *inside the running peer*.
    let recalled = client
        .recall("hibernation", Some(agent), 10)
        .expect("federation recall");
    assert!(
        recalled
            .iter()
            .any(|m| m.source_agent == agent && m.tags.iter().any(|t| t == "hibernation")),
        "hibernation record must be in BH MemoryFederation, got {recalled:?}"
    );

    // Durable trail: the hibernate episode landed in episodic SQLite.
    let episodes = client.episodes(agent, 20).expect("episodes");
    assert!(
        episodes.iter().any(|ep| ep["type"] == "hibernate"),
        "hibernate episode must be recorded, got {episodes:?}"
    );

    // State is readable through the seam before the restart.
    let before = client
        .get_state(&format!("{agent}:portfolio"))
        .expect("get state")
        .expect("state present");
    assert_eq!(before.value["eth"], 12);

    // ── Phase 2: hard restart over the same DB files ───────────────────
    drop(client);
    drop(peer_a); // SIGKILL — SQLite already committed per request.

    let client_b = new_client(&socket);
    let mut peer_b = spawn_peer(&work_dir, &socket); // stale socket is reused
    wait_ready(&client_b, &mut peer_b);

    // The thread survives the kill — and it is still closed.
    assert!(
        client_b.thread_exists(&session_id).expect("exists after restart"),
        "session must survive a peer restart via SQLite"
    );
    assert!(client_b.is_thread_closed(&session_id).expect("closed after restart"));

    // The conversation reads back verbatim.
    let history = client_b
        .load_history_up_to(&session_id, None, None)
        .expect("history after restart");
    let contents: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec![
            "pre-hibernation orders: hold ETH, report at dawn",
            "state saved; going dormant"
        ]
    );

    // The hibernation state survives and parses back to the same JSON.
    let after = client_b
        .get_state(&format!("{agent}:portfolio"))
        .expect("get state after restart")
        .expect("state survives restart");
    assert_eq!(after.value, json!({ "eth": 12, "mode": "hibernated" }));
    assert_eq!(after.tags, vec!["hibernation".to_string(), agent.to_string()]);
    assert!((after.importance - 0.9).abs() < 1e-9);

    // The durable hibernation trail is still queryable.
    let episodes_b = client_b.episodes(agent, 20).expect("episodes after restart");
    assert!(episodes_b.iter().any(|ep| ep["type"] == "hibernate"));

    // Honest note: the *live* MemoryFederation layer does not survive the
    // process (by its own shipped design — in-process cross-agent view);
    // what survives is everything SQLite-backed. Assert exactly that, no
    // fake continuity claims:
    let recalled_b = client_b.recall("hibernation", Some(agent), 10).expect("recall ok");
    let _ = recalled_b; // may be empty after restart — documented behaviour

    // Wake path: the closed thread is discoverable for resumption.
    let closed_threads = client_b
        .list_threads(Some(agent), Some("closed"))
        .expect("list closed threads");
    assert!(
        closed_threads.iter().any(|t| t.session_id == session_id),
        "the hibernated thread must be discoverable via list_threads"
    );

    drop(client_b);
    drop(peer_b);
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// A downed peer must be a loud error, never silent "no data".
#[test]
fn dead_peer_is_a_transport_error_not_empty_data() {
    let missing = std::env::temp_dir().join(format!(
        "bh-rtd-mem-e2e-dead-{}.sock",
        std::process::id()
    ));
    let client = new_client(&missing);
    let err = client.thread_exists("whatever").unwrap_err();
    assert!(
        err.to_string().contains("memory peer unreachable"),
        "got {err}"
    );
}
