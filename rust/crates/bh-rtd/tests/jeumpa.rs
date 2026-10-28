//! Phase 1 tests for [`JeumpaProvider`] — the Infinity `ModelProvider`
//! bridged to the Python Jeumpa Brain over the BH bridge protocol v1.
//!
//! Hermetic: the peer is a tiny **Rust** mock UDS server speaking the
//! documented protocol (`health` / `models` / `complete`) — no Python, no
//! network. Gated behind the `infinity` feature (it exercises the real
//! `infinity-provider-protocol` 0.2.0 trait surface).

#![cfg(feature = "infinity")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bh_rtd::jeumpa::JeumpaProvider;
use futures_util::StreamExt;
use infinity_provider_protocol::{CompletionError, CompletionRequest, ErrorClass, ModelProvider, StreamChunk};
use serde_json::{json, Value};

// ── Mock UDS peer (same protocol; one request per connection) ──

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
        "bh-rtd-jeumpa-test-{}-{}",
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

/// Mock brain catalog: one entry per task profile (what the Python server
/// derives from `TASK_PROFILES`).
fn models_result() -> Value {
    json!({
        "models": [
            { "model_id": "coding", "display_name": "jeumpa/coding",
              "context_window": 16384, "max_output_tokens": null },
            { "model_id": "default", "display_name": "jeumpa/default",
              "context_window": 16384, "max_output_tokens": null },
            { "model_id": "reasoning", "display_name": "jeumpa/reasoning",
              "context_window": 16384, "max_output_tokens": null },
        ]
    })
}

fn complete_ok(content: &str) -> Value {
    json!({
        "success": true,
        "content": content,
        "provider": "pollinations",
        "model": "openai-fast",
        "latency_ms": 12.5,
        "fallbacks_used": [],
        "error": "",
        "cached": false,
    })
}

fn complete_failed(error: &str, error_class: Value) -> Value {
    json!({
        "success": false,
        "content": "",
        "provider": "",
        "model": "",
        "latency_ms": 3.0,
        "fallbacks_used": ["pollinations/openai-fast"],
        "error": error,
        "cached": false,
        "error_class": error_class,
    })
}

async fn collect_stream(
    stream: infinity_provider_protocol::ModelStream,
) -> Vec<Result<StreamChunk, CompletionError>> {
    let mut out = Vec::new();
    let mut stream = stream;
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

// ── Tests ──

#[tokio::test]
async fn list_models_maps_task_profiles_and_defaults_first() {
    let responder: Responder = Arc::new(|line| {
        let request: Value = serde_json::from_str(line).unwrap();
        assert_eq!(request["method"], "models", "catalog comes from 'models'");
        ok_envelope(line, &models_result())
    });
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let models = provider.list_models().await.expect("list_models");
    assert_eq!(models.len(), 3);
    // Infinity contract: the first entry is the provider's default model.
    assert_eq!(models[0].model_id, "default");
    assert!(models.iter().any(|m| m.model_id == "coding"));
    assert!(models.iter().all(|m| m.context_window == 16384));
}

#[tokio::test]
async fn health_roundtrips_through_the_bridge() {
    let responder: Responder =
        Arc::new(|line| ok_envelope(line, &json!({ "status": "ok", "service": "jeumpa-uds" })));
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let health = provider.health().await.expect("health");
    assert_eq!(health["service"], "jeumpa-uds");
}

#[tokio::test]
async fn invoke_model_sends_profile_and_messages_then_streams_text_final() {
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let seen_clone = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |line| {
        seen_clone
            .lock()
            .unwrap()
            .push(serde_json::from_str(line).unwrap());
        ok_envelope(line, &complete_ok("hello from the stub brain"))
    });
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let request = CompletionRequest {
        preamble: Some("you are a colony worker".into()),
        chat_history: vec![
            infinity_provider_protocol::Message::user("hi"),
            infinity_provider_protocol::Message::assistant("hello"),
            infinity_provider_protocol::Message::user("summarise"),
        ],
        ..Default::default()
    };
    let stream = provider
        .invoke_model("coding", request)
        .await
        .expect("invoke_model");
    let chunks = collect_stream(stream).await;

    assert_eq!(
        chunks.len(),
        2,
        "buffered Jeumpa reply replays as exactly Text + Final"
    );
    assert!(matches!(
        &chunks[0],
        Ok(StreamChunk::Text(t)) if t == "hello from the stub brain"
    ));
    assert!(matches!(&chunks[1], Ok(StreamChunk::Final(_))));

    let requests = seen.lock().unwrap();
    let params = &requests[0]["params"];
    assert_eq!(params["profile"], "coding", "model_id becomes the profile");
    let messages = params["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4, "preamble + 3 history turns");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "you are a colony worker");
    assert_eq!(messages[3]["content"], "summarise");
}

fn simple_request() -> CompletionRequest {
    CompletionRequest {
        chat_history: vec![infinity_provider_protocol::Message::user("hello")],
        ..Default::default()
    }
}

#[tokio::test]
async fn business_failure_maps_to_provider_error_with_declared_class() {
    let responder: Responder = Arc::new(|line| {
        ok_envelope(
            line,
            &complete_failed(
                "All providers failed. Last error: Rate limited",
                json!("throttled"),
            ),
        )
    });
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let err = match provider.invoke_model("chat", simple_request()).await {
        Ok(_) => panic!("business failure must be an error"),
        Err(e) => e,
    };
    match &err {
        CompletionError::ProviderError { message, class } => {
            assert!(message.contains("jeumpa:"), "message is prefixed: {message}");
            assert_eq!(*class, ErrorClass::Throttled);
        }
        other => panic!("expected ProviderError, got {other:?}"),
    }
    assert_eq!(err.class(), ErrorClass::Throttled);
}

#[tokio::test]
async fn missing_error_class_defaults_to_fatal_do_not_retry() {
    let responder: Responder = Arc::new(|line| {
        ok_envelope(line, &complete_failed("HTTP 500", Value::Null))
    });
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let err = match provider.invoke_model("chat", simple_request()).await {
        Ok(_) => panic!("failure must be an error"),
        Err(e) => e,
    };
    assert_eq!(err.class(), ErrorClass::Fatal);
}

#[tokio::test]
async fn transport_failure_is_transient_response_error() {
    let missing = std::env::temp_dir().join(format!(
        "bh-rtd-jeumpa-not-here-{}.sock",
        std::process::id()
    ));
    let provider = JeumpaProvider::new(&missing);

    let err = match provider.invoke_model("chat", simple_request()).await {
        Ok(_) => panic!("no server listening"),
        Err(e) => e,
    };
    assert!(
        matches!(err, CompletionError::ResponseError(_)),
        "uds failure maps to ResponseError, got {err:?}"
    );
    assert_eq!(err.class(), ErrorClass::Transient);
}

#[tokio::test]
async fn tool_definitions_are_forwarded_and_declared_never_dropped() {
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let seen_clone = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |line| {
        seen_clone
            .lock()
            .unwrap()
            .push(serde_json::from_str(line).unwrap());
        ok_envelope(line, &complete_ok("plain answer"))
    });
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let request = CompletionRequest {
        preamble: Some("you are a colony worker".into()),
        chat_history: vec![infinity_provider_protocol::Message::user("list files")],
        tools: vec![infinity_provider_protocol::ToolDefinition {
            name: "shell.execute".into(),
            description: "run a shell command".into(),
            parameters: json!({"type": "object"}),
        }],
        ..Default::default()
    };
    let stream = provider
        .invoke_model("chat", request)
        .await
        .expect("invoke_model with tools");
    let chunks = collect_stream(stream).await;
    assert!(
        matches!(&chunks[0], Ok(StreamChunk::Text(t)) if t == "plain answer"),
        "non-conforming reply stays plain text, got {chunks:?}"
    );

    let requests = seen.lock().unwrap();
    let params = &requests[0]["params"];
    // 1. Definitions ride to the Python peer verbatim (never dropped).
    let tools = params["tools"].as_array().expect("tools param forwarded");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "shell.execute");
    assert_eq!(tools[0]["description"], "run a shell command");
    assert_eq!(tools[0]["parameters"], json!({"type": "object"}));
    // 2. The system message declares the tools with the exact reply
    //    convention the bridge parses.
    let system = params["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("you are a colony worker"));
    assert!(system.contains("`shell.execute`"));
    assert!(system.contains("run a shell command"));
    assert!(system.contains("BH_TOOL_CALL"));
    assert!(system.contains("Arguments JSON schema"));
}

#[tokio::test]
async fn conforming_tool_call_reply_translates_to_a_tool_call_chunk() {
    let server = spawn_mock_uds_server(Arc::new(|line| {
        ok_envelope(
            line,
            &complete_ok("BH_TOOL_CALL shell.execute {\"command\": \"ls -l\"}"),
        )
    }));
    let provider = JeumpaProvider::new(&server.path);

    let stream = provider
        .invoke_model("chat", simple_request())
        .await
        .expect("invoke_model");
    let chunks = collect_stream(stream).await;

    assert_eq!(chunks.len(), 2, "ToolCall + Final, no text duplication");
    match &chunks[0] {
        Ok(StreamChunk::ToolCall(call)) => {
            assert_eq!(call.function.name, "shell.execute");
            assert_eq!(call.function.arguments, json!({ "command": "ls -l" }));
            assert!(call.id.starts_with("jeumpa-tc-"), "bridge-scoped id");
        }
        other => panic!("expected ToolCall chunk, got {other:?}"),
    }
    assert!(matches!(&chunks[1], Ok(StreamChunk::Final(_))));
}

#[tokio::test]
async fn malformed_tool_call_reply_stays_plain_text_never_fabricated() {
    let cases = [
        // Malformed JSON arguments.
        "BH_TOOL_CALL shell.execute {not json}",
        // Arguments are not a JSON object.
        "BH_TOOL_CALL shell.execute [1, 2, 3]",
        // No tool name.
        "BH_TOOL_CALL {\"command\": \"ls\"}",
        // Mentions the prefix mid-text only.
        "you could try BH_TOOL_CALL later",
    ];
    for content in cases {
        let server = spawn_mock_uds_server(Arc::new(move |line| {
            ok_envelope(line, &complete_ok(content))
        }));
        let provider = JeumpaProvider::new(&server.path);
        let stream = provider
            .invoke_model("chat", simple_request())
            .await
            .expect("invoke_model");
        let chunks = collect_stream(stream).await;
        assert!(
            matches!(&chunks[0], Ok(StreamChunk::Text(t)) if t == content),
            "reply {content:?} must pass through as text, got {chunks:?}"
        );
    }
}

#[tokio::test]
async fn tool_results_and_past_calls_are_forwarded_losslessly_in_history() {
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let seen_clone = Arc::clone(&seen);
    let responder: Responder = Arc::new(move |line| {
        seen_clone
            .lock()
            .unwrap()
            .push(serde_json::from_str(line).unwrap());
        ok_envelope(line, &complete_ok("done"))
    });
    let server = spawn_mock_uds_server(responder);
    let provider = JeumpaProvider::new(&server.path);

    let request = CompletionRequest {
        chat_history: vec![
            infinity_provider_protocol::Message::user("list files"),
            infinity_provider_protocol::Message::Assistant {
                content: vec![infinity_provider_protocol::AssistantContent::ToolCall(
                    infinity_provider_protocol::ToolCall::new(
                        "jeumpa-tc-0",
                        "shell.execute",
                        json!({"command": "ls"}),
                    ),
                )],
            },
            infinity_provider_protocol::Message::User {
                content: vec![infinity_provider_protocol::UserContent::ToolResult(
                    infinity_provider_protocol::ToolResult {
                        id: "jeumpa-tc-0".into(),
                        call_id: None,
                        content: vec![infinity_provider_protocol::ToolResultContent::text(
                            "file-a.txt\nfile-b.txt",
                        )],
                    },
                )],
            },
        ],
        ..Default::default()
    };
    let stream = provider
        .invoke_model("chat", request)
        .await
        .expect("invoke_model");
    let _ = collect_stream(stream).await;

    let requests = seen.lock().unwrap();
    let messages = requests[0]["params"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "list files");
    assert_eq!(messages[1]["role"], "assistant");
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("[called tool shell.execute with arguments {\"command\":\"ls\"}]"),
        "past call recorded: {}",
        messages[1]["content"]
    );
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(
        messages[2]["content"],
        "file-a.txt\nfile-b.txt",
        "tool result content is forwarded verbatim, not a placeholder"
    );
}
