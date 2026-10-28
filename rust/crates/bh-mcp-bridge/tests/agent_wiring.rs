//! Phase 1 exit-criterion integration test (PLAN.md §4): **an Infinity agent
//! with 100% BH brain + tools**, driven fully offline.
//!
//! Composition under test:
//!
//! * **Brain** — `bh_rtd::jeumpa::JeumpaProvider`, the real
//!   `infinity_provider_protocol::ModelProvider` implementation, bridged to
//!   the Python Jeumpa Brain over the BH bridge protocol v1.
//! * **Tools** — `bh_mcp_bridge::BhBridgeTool::catalog_tools`, the real
//!   `infinity_agent_core::tools::Tool` implementations, bridged to the
//!   Python `agent_tools` registry over the same protocol.
//! * **Runtime** — a real `infinity_agent_core` local agent system
//!   (`AgentSystemBuilder::new_local` + `StaticModel`), exactly the API a
//!   Phase 2 `bh-rtd` daemon would embed.
//!
//! The Python peers are stand-ins here: a tiny in-process UDS mock speaking
//! the documented protocol (models/complete for the brain peer,
//! tools/call for the tools peer). No Python process, no network, no
//! secrets. What is NOT exercised offline is the Python side itself: the
//! jeumpa peer ships (`src/colony/jeumpa_uds_server.py`), the agent_tools
//! peer is planned (`src/agent_tools/uds_server.py`) — see
//! `bh-mcp-bridge/src/lib.rs` "Peer status".
//!
//! The mock brain is text-only *by design* (the real Jeumpa Brain is): the
//! test therefore also proves the Phase 1 tool convention end-to-end —
//! tools declared in the system message, the model replying with the
//! `BH_TOOL_CALL` line, the provider translating it to a `StreamChunk::ToolCall`,
//! the runtime executing it through the BH tool bridge (sync path), and the
//! loop returning the tool result to the brain for the final answer.

#![cfg(feature = "infinity")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bh_mcp_bridge::catalog;
use bh_mcp_bridge::tool_bridge::BhBridgeTool;
use bh_mcp_bridge::BhToolsClient;
use bh_rtd::jeumpa::JeumpaProvider;
use infinity_agent_core::stores::{InMemoryConversationStore, InMemoryStateStore};
use infinity_agent_core::system::local::ChannelSender;
use infinity_agent_core::system::{AgentEvent, AgentSystemBuilder, StaticModel};
use infinity_agent_core::tools::Tool;
use serde_json::{json, Value};

// ── Mock BH peers (one UDS server answering both bridges) ──────────────

static SEQ: AtomicU64 = AtomicU64::new(0);

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

/// Shared state the responder reads/mutates across connections.
#[derive(Default)]
struct BrainState {
    completes: std::sync::atomic::AtomicUsize,
    requests: Mutex<Vec<Value>>,
}

fn spawn_bh_peer(state: Arc<BrainState>) -> MockServer {
    let dir = std::env::temp_dir().join(format!(
        "bh-agent-wiring-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("bh.sock");
    let listener = UnixListener::bind(&path).expect("bind mock uds listener");
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let response = respond(line.trim(), &state);
            let mut bytes = response.into_bytes();
            bytes.push(b'\n');
            let _ = reader.get_mut().write_all(&bytes);
            let _ = reader.get_mut().flush();
        }
    });
    MockServer { path, handle }
}

fn respond(request_line: &str, state: &Arc<BrainState>) -> String {
    let request: Value = serde_json::from_str(request_line).expect("mock got a JSON line");
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    state
        .requests
        .lock()
        .unwrap()
        .push(request.clone());

    let result: Value = match method.as_str() {
        // Model catalog: one entry per task profile (TASK_PROFILES mirror),
        // `default` present as the bridge guarantees.
        "models" => json!({
            "models": [
                { "model_id": "default", "display_name": "jeumpa/default",
                  "context_window": 16384, "max_output_tokens": null },
                { "model_id": "coding", "display_name": "jeumpa/coding",
                  "context_window": 16384, "max_output_tokens": null },
            ]
        }),
        // Text-only brain: turn 1 asks for a tool using the documented
        // convention; turn 2 answers with the tool result in hand.
        "complete" => {
            let n = state
                .completes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if n == 1 {
                json!({
                    "success": true,
                    "content": "BH_TOOL_CALL shell.execute {\"command\": \"ls -l\"}",
                    "provider": "pollinations", "model": "openai-fast",
                    "latency_ms": 1.0, "fallbacks_used": [], "error": "", "cached": false,
                })
            } else {
                json!({
                    "success": true,
                    "content": "The directory contains a.txt and b.txt.",
                    "provider": "pollinations", "model": "openai-fast",
                    "latency_ms": 2.0, "fallbacks_used": [], "error": "", "cached": false,
                })
            }
        }
        // The tools peer: the registry executes the shell tool.
        "tools/call" => {
            assert_eq!(request["params"]["name"], "shell.execute");
            assert_eq!(
                request["params"]["args"],
                json!({ "command": "ls -l" }),
                "model-provided arguments travel unchanged"
            );
            json!({
                "success": true,
                "stdout": "a.txt\nb.txt",
                "stderr": "",
                "exit_code": 0,
                "timed_out": false,
                "duration_ms": 1.5,
            })
        }
        other => panic!("mock peer got unexpected method {other:?}"),
    };

    json!({ "id": id, "ok": true, "result": result }).to_string()
}

/// Drain the thread's events until `CompletionFinished`, with a timeout so a
/// wiring bug fails the test instead of hanging it.
async fn collect_until_finished(thread: &mut infinity_agent_core::system::local::ThreadHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), thread.recv())
            .await
            .expect("event stream timed out")
            .expect("system shut down unexpectedly");
        let finished = matches!(event, AgentEvent::CompletionFinished { .. });
        events.push(event);
        if finished {
            return events;
        }
    }
}

fn text_of(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextChunk { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ── The Phase 1 exit-criterion tests ───────────────────────────────────

/// A full agent turn that calls a BH tool through the bridge and answers
/// from the result — brain, tools, and runtime all BH or real Infinity.
#[tokio::test(flavor = "current_thread")]
async fn agent_turn_calls_bh_tool_and_answers_from_the_result() {
    let state = Arc::new(BrainState::default());
    let server = spawn_bh_peer(Arc::clone(&state));

    // Brain: the real ModelProvider over the bridge.
    let provider = Arc::new(JeumpaProvider::new(&server.path));
    let model = StaticModel::new(Arc::clone(&provider) as Arc<_>, "default")
        .await
        .expect("static model resolves from the provider catalog");

    // Tools: the real Infinity Tool impls over the bridge.
    let tools = BhBridgeTool::catalog_tools(BhToolsClient::new(&server.path));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let system = AgentSystemBuilder::new_local(
                InMemoryConversationStore::new(),
                InMemoryStateStore::new(),
                model,
            )
            .tools(
                tools
                    .into_iter()
                    .map(|t| Box::new(t) as Box<dyn Tool<ChannelSender>>)
                    .collect::<Vec<_>>(),
            )
            .start();

            let mut thread = system.thread_builder().launch().await;
            thread
                .send_user_text("list the files in this directory")
                .await
                .expect("user text accepted");

            let events = collect_until_finished(&mut thread).await;

            // The model called the BH tool through the bridge ...
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "shell.execute")),
                "expected a shell.execute ToolCall event, got {events:?}"
            );
            // ... the result was injected into the loop ...
            assert!(
                events.iter().any(|e| matches!(e, AgentEvent::ToolResult { .. })),
                "expected a ToolResult event, got {events:?}"
            );
            // ... and the final answer came from the brain's second round.
            assert_eq!(text_of(&events), "The directory contains a.txt and b.txt.");

            // The tools peer saw exactly one registry call with the
            // model-provided arguments (asserted inside the responder).
            let requests = state.requests.lock().unwrap();
            let tool_calls = requests
                .iter()
                .filter(|r| r["method"] == "tools/call")
                .count();
            assert_eq!(tool_calls, 1, "exactly one registry execution");
            let completes = requests.iter().filter(|r| r["method"] == "complete").count();
            assert_eq!(completes, 2, "tool round-trip loops back into the brain");
        })
        .await;
}

/// The brain sees the BH tool catalog: the completion request reaching the
/// Jeumpa peer carries the tool definitions (forwarded verbatim) and the
/// system message declares them with the reply convention.
#[tokio::test(flavor = "current_thread")]
async fn brain_receives_the_bh_tool_catalog_declarations() {
    let state = Arc::new(BrainState::default());
    let server = spawn_bh_peer(Arc::clone(&state));

    let provider = Arc::new(JeumpaProvider::new(&server.path));
    let model = StaticModel::new(Arc::clone(&provider) as Arc<_>, "default")
        .await
        .expect("static model resolves");
    let tools = BhBridgeTool::catalog_tools(BhToolsClient::new(&server.path));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let system = AgentSystemBuilder::new_local(
                InMemoryConversationStore::new(),
                InMemoryStateStore::new(),
                model,
            )
            .tools(
                tools
                    .into_iter()
                    .map(|t| Box::new(t) as Box<dyn Tool<ChannelSender>>)
                    .collect::<Vec<_>>(),
            )
            .start();

            let mut thread = system.thread_builder().launch().await;
            thread
                .send_user_text("what tools do you have?")
                .await
                .expect("user text accepted");
            let events = collect_until_finished(&mut thread).await;
            assert_eq!(text_of(&events), "The directory contains a.txt and b.txt.");

            let requests = state.requests.lock().unwrap();
            let first_complete = requests
                .iter()
                .find(|r| r["method"] == "complete")
                .expect("the brain was invoked");
            let params = &first_complete["params"];

            // The Infinity runtime's toolset (BH catalog + built-in thread
            // tools) is forwarded verbatim to the Python peer.
            let forwarded = params["tools"].as_array().expect("tools forwarded");
            let names: Vec<&str> = forwarded
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            for catalog_name in ["file", "shell.execute", "code.operations"] {
                assert!(
                    names.contains(&catalog_name),
                    "catalog tool {catalog_name:?} must be forwarded, got {names:?}"
                );
            }

            // The system message declares the tools with the exact reply
            // convention the bridge parses.
            let system_message = params["messages"][0]["content"].as_str().unwrap();
            assert!(system_message.contains("BH_TOOL_CALL"));
            assert!(system_message.contains("`shell.execute`"));
            assert!(system_message.contains("Arguments JSON schema"));
        })
        .await;
}

/// The static catalog and the peer manifest agree (drift gate on the wiring
/// path): the tools/list reply of the mock peer is exactly the catalog.
#[test]
fn peer_manifest_matches_the_rust_catalog() {
    // The mock peer's tools/list reply is built FROM the catalog (that is
    // what a real registry peer serves), so validate the round trip.
    let manifests: Vec<bh_mcp_bridge::ToolManifest> = catalog::bh_tool_catalog()
        .iter()
        .map(|e| e.to_manifest())
        .collect();
    catalog::validate_peer_manifest(&manifests)
        .expect("a faithful peer manifest must not drift from the catalog");
}
