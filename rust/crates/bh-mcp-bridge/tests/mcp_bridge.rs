//! Phase 1 tests for `bh-mcp-bridge` — the client for the BH agent_tools
//! bridge (`tools/list` / `tools/call` over BH bridge protocol v1).
//!
//! Hermetic by design: the peer is a tiny **Rust** mock UDS server speaking
//! the documented protocol — no Python, no network, no async runtime.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bh_mcp_bridge::{BhToolsClient, BhToolsError};
use serde_json::{json, Value};

// ── Mock UDS peer (std-only; one request per connection, like the client) ──

static SEQ: AtomicU64 = AtomicU64::new(0);

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

fn spawn_mock_uds_server(responder: Responder) -> MockServer {
    let dir = std::env::temp_dir().join(format!(
        "bh-mcp-bridge-test-{}-{}",
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

fn ok_envelope(request_line: &str, result: &Value) -> String {
    let request: Value = serde_json::from_str(request_line).expect("mock got a JSON line");
    json!({
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "ok": true,
        "result": result,
    })
    .to_string()
}

// ── Tests ──

#[test]
fn tools_list_parses_the_manifest() {
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let seen_clone = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |line| {
        seen_clone
            .lock()
            .unwrap()
            .push(serde_json::from_str(line).unwrap());
        ok_envelope(
            line,
            &json!({
                "tools": [
                    {
                        "name": "shell.execute",
                        "category": "compute",
                        "autonomy_level": 1,
                        "input_schema": { "type": "object" },
                        "output_schema": { "type": "object" },
                    },
                    {
                        "name": "search.web",
                        "category": "knowledge",
                        "autonomy_level": 0,
                        "input_schema": { "type": "object" },
                        "output_schema": { "type": "object" },
                    },
                ]
            }),
        )
    });
    let server = spawn_mock_uds_server(responder);
    let client = BhToolsClient::new(&server.path);

    let tools = client.list_tools().expect("manifest parses");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "shell.execute");
    assert_eq!(tools[0].category, "compute");
    assert_eq!(tools[0].autonomy_level, 1);
    assert_eq!(tools[1].name, "search.web");
    assert_eq!(tools[1].autonomy_level, 0);

    let requests = seen.lock().unwrap();
    assert_eq!(requests[0]["method"], "tools/list");
    assert_eq!(requests[0]["params"], json!({}));
}

#[test]
fn malformed_manifest_is_a_manifest_error_not_a_crash() {
    let server = spawn_mock_uds_server(Arc::new(|line| ok_envelope(line, &json!({ "nope": [] }))));
    let client = BhToolsClient::new(&server.path);

    let err = client.list_tools().unwrap_err();
    assert!(matches!(err, BhToolsError::Manifest(_)));
    assert!(err.to_string().contains("manifest"));
    let _: &dyn std::error::Error = &err;
}

#[test]
fn call_tool_returns_the_full_result_payload() {
    let responder: Responder = Arc::new(|line| {
        let request: Value = serde_json::from_str(line).unwrap();
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "test.echo");
        assert_eq!(request["params"]["args"], json!({ "message": "ping" }));
        ok_envelope(line, &json!({ "success": true, "echoed": "ping" }))
    });
    let server = spawn_mock_uds_server(responder);
    let client = BhToolsClient::new(&server.path);

    let result = client
        .call_tool("test.echo", &json!({ "message": "ping" }))
        .expect("tool call succeeds");
    assert_eq!(result["echoed"], "ping");
    assert_eq!(result["success"], true);
}

#[test]
fn business_failure_surfaces_as_tool_failed() {
    let responder: Responder = Arc::new(|line| {
        ok_envelope(
            line,
            &json!({
                "success": false,
                "error": "Tool not found: nope.execute",
                "error_code": -32601,
            }),
        )
    });
    let server = spawn_mock_uds_server(responder);
    let client = BhToolsClient::new(&server.path);

    let err = client
        .call_tool("nope.execute", &json!({}))
        .expect_err("tool-level failure must surface as an error");
    match &err {
        BhToolsError::ToolFailed { name, message } => {
            assert_eq!(name, "nope.execute");
            assert!(message.contains("Tool not found"));
        }
        other => panic!("expected ToolFailed, got {other:?}"),
    }
    assert!(err.to_string().contains("Tool not found"));
}

#[test]
fn transport_failure_wraps_the_uds_error() {
    let missing = std::env::temp_dir().join(format!(
        "bh-mcp-bridge-not-here-{}.sock",
        std::process::id()
    ));
    let client = BhToolsClient::new(&missing);

    let err = client.call_tool("test.echo", &json!({})).unwrap_err();
    assert!(matches!(err, BhToolsError::Transport(_)));
    // The error chain is preserved for callers that inspect sources.
    assert!(std::error::Error::source(&err).is_some());
}
