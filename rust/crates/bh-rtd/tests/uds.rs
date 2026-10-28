//! Phase 1 tests for the BH bridge protocol client (`bh_rtd::uds`).
//!
//! Hermetic by design: the peer is a tiny **Rust** mock UDS server spawned
//! on a thread speaking the same protocol — no Python, no network, no
//! async runtime. Gated behind the `bridge` feature.

#![cfg(feature = "bridge")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bh_rtd::uds::{UdsClient, UdsError};
use serde_json::{json, Value};

// ── Mock UDS peer (std-only; one request per connection, like the client) ──

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
        // Best-effort cleanup; the accept-loop thread dies with the process.
        let _ = std::fs::remove_file(&self.path);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn spawn_mock_uds_server(responder: Responder) -> MockServer {
    let dir = std::env::temp_dir().join(format!(
        "bh-rtd-uds-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir for mock socket");
    let path = dir.join("mock.sock");
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

/// Build a well-formed `ok: true` envelope echoing the request id.
fn ok_envelope(request_line: &str, result: &Value) -> String {
    let request: Value = serde_json::from_str(request_line).expect("mock got a JSON line");
    json!({
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "ok": true,
        "result": result,
    })
    .to_string()
}

/// Build a well-formed `ok: false` envelope echoing the request id.
fn err_envelope(request_line: &str, code: i64, message: &str) -> String {
    let request: Value = serde_json::from_str(request_line).expect("mock got a JSON line");
    json!({
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "ok": false,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

// ── Tests ──

#[test]
fn roundtrips_result_and_sends_documented_request_shape() {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |line| {
        seen_clone
            .lock()
            .unwrap()
            .push(serde_json::from_str(line).unwrap());
        ok_envelope(line, &json!({ "status": "ok", "service": "mock" }))
    });
    let server = spawn_mock_uds_server(responder);

    let client = UdsClient::new(&server.path);
    let result = client.call("health", &json!({})).expect("call succeeds");
    assert_eq!(result["status"], "ok");

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1, "one request per connection");
    let first = &requests[0];
    assert!(first["id"].as_str().unwrap().starts_with("bh-"));
    assert_eq!(first["method"], "health");
    assert_eq!(first["params"], json!({}));
}

#[test]
fn server_error_envelope_maps_to_server_error() {
    let responder: Responder =
        Arc::new(|line| err_envelope(line, -32601, "unknown method 'nope'"));
    let server = spawn_mock_uds_server(responder);

    let client = UdsClient::new(&server.path);
    let err = client.call("nope", &json!({})).unwrap_err();
    assert_eq!(
        err,
        UdsError::Server {
            code: -32601,
            message: "unknown method 'nope'".into()
        }
    );
    assert!(err.to_string().contains("unknown method"), "actionable display");
    let _: &dyn std::error::Error = &err;
}

#[test]
fn protocol_violations_are_reported_not_masked() {
    // Envelope missing `ok`.
    let server = spawn_mock_uds_server(Arc::new(|_| json!({ "hello": 1 }).to_string()));
    let err = UdsClient::new(&server.path).call("health", &json!({})).unwrap_err();
    assert!(
        matches!(err, UdsError::Protocol(ref m) if m.contains("ok")),
        "missing-ok must be a protocol error, got {err:?}"
    );

    // Response line is not JSON at all.
    let server2 = spawn_mock_uds_server(Arc::new(|_| "this is not json".to_string()));
    let err2 = UdsClient::new(&server2.path).call("health", &json!({})).unwrap_err();
    assert!(
        matches!(err2, UdsError::Protocol(ref m) if m.contains("not valid JSON")),
        "non-JSON reply must be a protocol error, got {err2:?}"
    );

    // Server closes without answering.
    let server3 = spawn_mock_uds_server(Arc::new(|_| String::new()));
    let err3 = UdsClient::new(&server3.path).call("health", &json!({})).unwrap_err();
    assert!(
        matches!(err3, UdsError::Protocol(ref m) if m.contains("without a response")),
        "silent close must be a protocol error, got {err3:?}"
    );
}

#[test]
fn missing_socket_is_an_io_error_fail_loud() {
    let missing = std::env::temp_dir().join(format!(
        "bh-rtd-definitely-not-here-{}.sock",
        std::process::id()
    ));
    let client = UdsClient::new(&missing);
    let err = client.call("health", &json!({})).unwrap_err();
    assert!(matches!(err, UdsError::Io(_)));
    assert!(err.to_string().starts_with("bridge uds io error"));
}
