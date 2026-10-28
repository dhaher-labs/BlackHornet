//! Phase 3 tests for the memory seam (`bh_rtd::memory`).
//!
//! Hermetic by design: the peer is a tiny **Rust** mock UDS server spawned
//! on a thread speaking BH bridge protocol v1 — the same pattern as
//! `tests/uds.rs`. The mock is the *contract reference* for
//! `src/memory/rtd_memory_server.py`: whatever these tests script is what
//! the Python peer must answer. Everything runs through the seam traits
//! (`&dyn ConversationStore` etc.) to prove the R10 isolation: the tests
//! never need an Infinity type.

#![cfg(feature = "bridge")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bh_rtd::memory::{
    ConversationStore, MemoryError, MemoryMessage, SharedMemoryStore, StateEntry, StateStore,
    ThreadSummary, UdsMemoryClient,
};
use serde_json::{json, Value};

// ── Mock UDS peer (std-only; one request per connection) ────────────────

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Takes the raw request line, returns the raw response line (without `\n`).
type Responder = Arc<dyn Fn(&str) -> String + Send + Sync>;

struct MockServer {
    path: PathBuf,
    #[allow(dead_code)]
    handle: std::thread::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn spawn_mock_peer(responder: Responder) -> MockServer {
    let dir = std::env::temp_dir().join(format!(
        "bh-rtd-mem-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir for mock socket");
    let path = dir.join("mock-memory.sock");
    let listener = UnixListener::bind(&path).expect("bind mock uds listener");
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve_one(stream, &responder);
        }
    });
    MockServer { path, handle }
}

fn serve_one(stream: UnixStream, responder: &Responder) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let response = responder(line.trim());
    let mut bytes = response.into_bytes();
    bytes.push(b'\n');
    let _ = reader.get_mut().write_all(&bytes);
    let _ = reader.get_mut().flush();
}

fn ok_envelope(request_line: &str, result: &Value) -> String {
    let request: Value = serde_json::from_str(request_line).expect("mock got a JSON line");
    json!({
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "ok": true,
        "result": result,
    })
    .to_string()
}

fn err_envelope(request_line: &str, code: i64, message: &str) -> String {
    let request: Value = serde_json::from_str(request_line).expect("mock got a JSON line");
    json!({
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "ok": false,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

/// A dispatcher-style mock: routes one request line by `method` to a
/// canned result, and records every request for assertions.
struct ScriptedPeer {
    server: MockServer,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl ScriptedPeer {
    /// `routes` maps a method to a *queue* of canned results: consecutive
    /// calls to the same method consume the queue front-to-back (so one
    /// method can script different answers per call). An exhausted queue
    /// answers -32603 instead of fabricating data.
    fn new(routes: Vec<(&'static str, Vec<Value>)>) -> Self {
        let mut route_map: std::collections::HashMap<&'static str, std::collections::VecDeque<Value>> =
            std::collections::HashMap::new();
        for (method, results) in routes {
            route_map
                .entry(method)
                .or_default()
                .extend(results);
        }
        let route_map = Arc::new(std::sync::Mutex::new(route_map));
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let responder: Responder = Arc::new(move |line| {
            let request: Value = serde_json::from_str(line).expect("mock got a JSON line");
            seen_clone.lock().unwrap().push(request.clone());
            let method = request["method"].as_str().unwrap_or("");
            let next = route_map
                .lock()
                .unwrap()
                .get_mut(method)
                .and_then(std::collections::VecDeque::pop_front);
            match next {
                // A `{"__error__": {code, message}}` route answers ok:false.
                Some(result) if result.get("__error__").is_some() => {
                    let err = &result["__error__"];
                    err_envelope(
                        line,
                        err["code"].as_i64().unwrap_or(-32603),
                        err["message"].as_str().unwrap_or("mock error"),
                    )
                }
                Some(result) => ok_envelope(line, &result),
                None => err_envelope(
                    line,
                    -32601,
                    &format!("unknown method {method:?} (or mock script exhausted)"),
                ),
            }
        });
        Self {
            server: spawn_mock_peer(responder),
            seen,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

fn canned_session(session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "agent_id": "agent-7",
        "colony_id": "bh-colony",
        "created_at": "2026-02-25T08:00:00+00:00",
        "last_active": "2026-02-25T08:05:00+00:00",
        "status": "active",
        "message_count": 2,
        "total_tokens": 24,
        "metadata": {},
    })
}

fn canned_message(role: &str, content: &str) -> Value {
    json!({
        "message_id": format!("m-{}", content.len()),
        "role": role,
        "content": content,
        "timestamp": "2026-02-25T08:01:00+00:00",
        "metadata": {},
        "token_count": content.len() as i64 / 4 + 1,
    })
}

fn client_for(peer: &ScriptedPeer) -> UdsMemoryClient {
    UdsMemoryClient::new(&peer.server.path)
}

// ── ConversationStore ───────────────────────────────────────────────────

#[test]
fn create_thread_sends_agent_and_decodes_peer_minted_id() {
    let peer = ScriptedPeer::new(vec![(
        "conversation.create",
        vec![json!({ "session": canned_session("sess42") })],
    )]);
    // Through the trait object: the seam is Infinity-type-free (R10).
    let store: &dyn ConversationStore = &client_for(&peer);

    let summary = store.create_thread("agent-7", "bh-colony").expect("create");
    assert_eq!(summary.session_id, "sess42");
    assert_eq!(summary.agent_id, "agent-7");
    assert_eq!(summary.status, "active");
    assert_eq!(summary.message_count, 2);

    let req = &peer.requests()[0];
    assert_eq!(req["method"], "conversation.create");
    assert_eq!(req["params"]["agent_id"], "agent-7");
    assert_eq!(req["params"]["colony_id"], "bh-colony");
}

#[test]
fn append_and_load_history_follow_the_documented_wire_contract() {
    let peer = ScriptedPeer::new(vec![
        ("conversation.append", vec![json!({ "appended": 2 })]),
        (
            "conversation.get",
            vec![json!({
                "messages": [
                    canned_message("user", "ping"),
                    canned_message("assistant", "pong"),
                ],
                "count": 2,
            })],
        ),
    ]);
    let store = client_for(&peer);

    let batch = vec![
        MemoryMessage::new("user", "ping"),
        MemoryMessage::new("assistant", "pong"),
    ];
    let appended = store
        .append_messages("sess42", &batch)
        .expect("append round-trips");
    assert_eq!(appended, 2);

    // Infinity leading-row-count convention: skip 1, take first 2 → [0..2).
    let history = store
        .load_history_up_to("sess42", Some(1), Some(3))
        .expect("history round-trips");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[0].content, "ping");
    assert_eq!(history[1].role, "assistant");
    assert!(history[1].message_id.is_some());
    assert!(history[1].timestamp.is_some());

    let requests = peer.requests();
    assert_eq!(requests[0]["method"], "conversation.append");
    assert_eq!(requests[0]["params"]["session_id"], "sess42");
    assert_eq!(requests[0]["params"]["messages"][0]["role"], "user");
    assert_eq!(requests[0]["params"]["messages"][0]["content"], "ping");
    assert_eq!(requests[1]["method"], "conversation.get");
    assert_eq!(requests[1]["params"]["start_from"], 1);
    assert_eq!(requests[1]["params"]["up_to"], 3);
}

#[test]
fn short_append_count_is_a_protocol_error_not_a_silent_drop() {
    // Peer claims 1 of 2 appended — the client must fail loud.
    let peer = ScriptedPeer::new(vec![(
        "conversation.append",
        vec![json!({ "appended": 1 })],
)]);
    let store = client_for(&peer);
    let err = store
        .append_messages(
            "sess42",
            &[
                MemoryMessage::new("user", "a"),
                MemoryMessage::new("assistant", "b"),
            ],
        )
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::Protocol(ref m) if m.contains("appended 1 of 2")),
        "got {err:?}"
    );
}

#[test]
fn exists_is_closed_and_list_decode_strictly() {
    let peer = ScriptedPeer::new(vec![
        ("conversation.exists", vec![json!({ "exists": true })]),
        ("conversation.is_closed", vec![json!({ "closed": false })]),
        (
            "conversation.list",
            vec![json!({ "sessions": [canned_session("sess42"),
                                       canned_session("sess43")] })],
        ),
        ("conversation.close", vec![json!({ "session": {
            "session_id": "sess42", "status": "closed", "agent_id": "agent-7",
        }})]),
    ]);
    let store: &dyn ConversationStore = &client_for(&peer);

    assert!(store.thread_exists("sess42").unwrap());
    assert!(!store.is_thread_closed("sess42").unwrap());

    let threads = store.list_threads(Some("agent-7"), Some("active")).unwrap();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[1].session_id, "sess43");

    let closed = store.close_thread("sess42", Some("going to sleep")).unwrap();
    assert_eq!(closed.status, "closed");

    let requests = peer.requests();
    assert_eq!(requests[0]["params"]["session_id"], "sess42");
    assert_eq!(requests[2]["params"]["agent_id"], "agent-7");
    assert_eq!(requests[2]["params"]["status"], "active");
    assert_eq!(requests[3]["method"], "conversation.close");
    assert_eq!(requests[3]["params"]["note"], "going to sleep");
}

// ── StateStore ──────────────────────────────────────────────────────────

#[test]
fn state_set_get_delete_roundtrip_with_found_miss() {
    let peer = ScriptedPeer::new(vec![
        (
            "state.set",
            vec![json!({ "stored": true, "key": "agent-7:position" })],
        ),
        (
            "state.get",
            vec![
                json!({
                    "found": true,
                    "entry": {
                        "id": 11,
                        "key": "agent-7:position",
                        "value": { "btc": 0.5, "mode": "hibernated" },
                        "tags": ["hibernation", "agent-7"],
                        "importance": 0.9,
                    },
                }),
                json!({ "found": false }),
            ],
        ),
        ("state.delete", vec![json!({ "deleted": true })]),
    ]);
    let store: &dyn StateStore = &client_for(&peer);

    store
        .set_state(
            "agent-7:position",
            &json!({ "btc": 0.5, "mode": "hibernated" }),
            &["hibernation", "agent-7"],
            Some(0.9),
        )
        .expect("set_state");

    let entry: StateEntry = store.get_state("agent-7:position").unwrap().expect("found");
    assert_eq!(entry.id, 11);
    assert_eq!(entry.key, "agent-7:position");
    assert_eq!(entry.value["btc"], 0.5);
    assert_eq!(entry.tags, vec!["hibernation".to_string(), "agent-7".to_string()]);
    assert!((entry.importance - 0.9).abs() < 1e-9);

    // A miss is a genuine None, not an error and not a fabricated entry.
    assert!(store.get_state("agent-7:absent").unwrap().is_none());
    assert!(store.delete_state("agent-7:position").unwrap());

    let requests = peer.requests();
    assert_eq!(requests[0]["method"], "state.set");
    assert_eq!(requests[0]["params"]["value"]["mode"], "hibernated");
    assert_eq!(requests[0]["params"]["importance"], 0.9);
    assert_eq!(requests[3]["method"], "state.delete");
}

// ── SharedMemoryStore (the exit-criterion path) ─────────────────────────

#[test]
fn federation_share_and_recall_roundtrip() {
    let peer = ScriptedPeer::new(vec![
        (
            "federation.share",
            vec![json!({ "memory": {
                "memory_id": "ab12cd34ef567890",
                "content": "agent-7 hibernated with 2 open positions",
                "source_agent": "agent-7",
                "memory_type": "fact",
                "importance": 0.8,
                "confidence": 0.5,
                "tags": ["hibernation", "agent-7"],
                "agreement_count": 1,
                "access_count": 0,
            }})],
        ),
        (
            "federation.recall",
            vec![json!({ "memories": [{
                "memory_id": "ab12cd34ef567890",
                "content": "agent-7 hibernated with 2 open positions",
                "source_agent": "agent-7",
                "memory_type": "fact",
                "importance": 0.8,
                "confidence": 0.5,
                "tags": ["hibernation", "agent-7"],
                "agreement_count": 1,
                "access_count": 1,
            }]}),],
        ),
    ]);
    let store: &dyn SharedMemoryStore = &client_for(&peer);

    let record = store
        .share(
            "agent-7 hibernated with 2 open positions",
            "agent-7",
            "fact",
            0.8,
            0.5,
            &["hibernation", "agent-7"],
        )
        .expect("share");
    assert_eq!(record.memory_id, "ab12cd34ef567890");

    let recalled = store.recall("hibernation", Some("agent-7"), 10).expect("recall");
    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0].source_agent, "agent-7");

    let requests = peer.requests();
    assert_eq!(requests[0]["method"], "federation.share");
    assert_eq!(requests[0]["params"]["memory_type"], "fact");
    assert_eq!(requests[1]["params"]["query"], "hibernation");
    assert_eq!(requests[1]["params"]["limit"], 10);
}

// ── Fail-loud guarantees ────────────────────────────────────────────────

#[test]
fn server_errors_become_typed_errors_never_empty_data() {
    // Unknown session on append: ok:false → MemoryError::Server.
    let peer = ScriptedPeer::new(vec![(
        "conversation.append",
        vec![err_result_value(-32602, "unknown session 'nope'")],
    )]);
    let store = client_for(&peer);
    let err = store
        .append_messages("nope", &[MemoryMessage::new("user", "hi")])
        .unwrap_err();
    assert_eq!(
        err,
        MemoryError::Server {
            code: -32602,
            message: "unknown session 'nope'".into(),
        }
    );
    assert!(err.to_string().contains("unknown session"));

    let _: &dyn std::error::Error = &err;
}

/// Helper to place an `ok:false` route value into `ScriptedPeer`.
fn err_result_value(code: i64, message: &str) -> Value {
    json!({ "__error__": { "code": code, "message": message } })
}

#[test]
fn result_shape_drift_is_a_protocol_error() {
    // thread_exists without 'exists'.
    let peer = ScriptedPeer::new(vec![("conversation.exists", vec![json!({ "nope": 1 })])]);
    let err = client_for(&peer).thread_exists("x").unwrap_err();
    assert!(matches!(err, MemoryError::Protocol(ref m) if m.contains("'exists'")), "got {err:?}");

    // create_thread without 'session'.
    let peer2 = ScriptedPeer::new(vec![("conversation.create", vec![json!({})])]);
    let err2 = client_for(&peer2).create_thread("a", "c").unwrap_err();
    assert!(
        matches!(err2, MemoryError::Protocol(ref m) if m.contains("'session'")),
        "got {err2:?}"
    );

    // load history: 'messages' is not an array.
    let peer3 = ScriptedPeer::new(vec![(
        "conversation.get",
        vec![json!({ "messages": 7 })],
    )]);
    let err3 = client_for(&peer3)
        .load_history_up_to("x", None, None)
        .unwrap_err();
    assert!(
        matches!(err3, MemoryError::Protocol(ref m) if m.contains("'messages'")),
        "got {err3:?}"
    );

    // message entry missing 'content'.
    let peer4 = ScriptedPeer::new(vec![(
        "conversation.get",
        vec![json!({ "messages": [{ "role": "user" }] })],
    )]);
    let err4 = client_for(&peer4)
        .load_history_up_to("x", None, None)
        .unwrap_err();
    assert!(
        matches!(err4, MemoryError::Protocol(ref m) if m.contains("'content'")),
        "got {err4:?}"
    );

    // found:false carrying an entry — fabricated data, rejected.
    let peer5 = ScriptedPeer::new(vec![(
        "state.get",
        vec![json!({ "found": false, "entry": { "key": "k", "value": 1 } })],
    )]);
    let err5 = client_for(&peer5).get_state("k").unwrap_err();
    assert!(
        matches!(err5, MemoryError::Protocol(ref m) if m.contains("drift")),
        "got {err5:?}"
    );

    // session summary missing 'session_id'.
    let peer6 = ScriptedPeer::new(vec![(
        "conversation.create",
        vec![json!({ "session": { "status": "active" } })],
    )]);
    let err6 = client_for(&peer6).create_thread("a", "c").unwrap_err();
    assert!(
        matches!(err6, MemoryError::Protocol(ref m) if m.contains("'session_id'")),
        "got {err6:?}"
    );
}

#[test]
fn unreachable_peer_is_a_transport_error_fail_loud() {
    let missing = std::env::temp_dir().join(format!(
        "bh-rtd-mem-not-here-{}.sock",
        std::process::id()
    ));
    let client = UdsMemoryClient::new(&missing);
    let err = client.thread_exists("whatever").unwrap_err();
    assert!(matches!(err, MemoryError::Transport(_)), "got {err:?}");
    assert!(err.to_string().starts_with("memory peer unreachable"));
}

#[test]
fn health_and_episodes_pass_through() {
    let peer = ScriptedPeer::new(vec![
        (
            "health",
            vec![json!({ "status": "ok", "service": "memory-uds", "protocol": "v1" })],
        ),
        (
            "memory.episodes",
            vec![json!({ "episodes": [
                { "id": 3, "type": "hibernate", "summary": "nightly",
                  "outcome": "", "timestamp": 1740000000.0 }
            ]})],
        ),
    ]);
    let client = client_for(&peer);
    let health = client.health().expect("health");
    assert_eq!(health["service"], "memory-uds");

    let episodes = client.episodes("agent-7", 20).expect("episodes");
    assert_eq!(episodes[0]["type"], "hibernate");

    // Episodes result without the array → protocol error (no fake empty).
    let peer2 = ScriptedPeer::new(vec![("memory.episodes", vec![json!({})])]);
    let err = client_for(&peer2).episodes("agent-7", 5).unwrap_err();
    assert!(
        matches!(err, MemoryError::Protocol(ref m) if m.contains("'episodes'")),
        "got {err:?}"
    );
}

#[test]
fn trait_objects_compile_against_all_three_seam_traits() {
    // R10 isolation proof: all three contracts are object-safe and usable
    // without any Infinity type in scope.
    fn exercise(
        conversation: &dyn ConversationStore,
        _state: &dyn StateStore,
        _federation: &dyn SharedMemoryStore,
    ) -> ThreadSummary {
        conversation.create_thread("a", "c").expect("create")
    }
    let peer = ScriptedPeer::new(vec![(
        "conversation.create",
        vec![json!({ "session": canned_session("sess1") })],
    )]);
    let client = client_for(&peer);
    let summary = exercise(&client, &client, &client);
    assert_eq!(summary.session_id, "sess1");
}
