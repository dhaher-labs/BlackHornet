//! Bridge one BH tool into Infinity's tool surface: an implementation of
//! `infinity_agent_core::tools::Tool` backed by the BH tools bridge client.
//!
//! This is the runtime-facing half of the Phase 1 "infinity-mcp-bridge"
//! deliverable: an Infinity agent registers a [`BhBridgeTool`] per catalog
//! entry (or per peer-manifest entry) and the agent runtime can then call
//! the real Python `agent_tools` registry through the bridge client — no
//! tool logic is ported, the registry (circuit breaker, rate limiting,
//! permission checks) stays Python.
//!
//! ## Synchronous execution contract
//!
//! BH tools are local UDS round-trips, so the bridge implements
//! `supports_sync() -> true` and `execute_synchronous`, the path Infinity's
//! runtime uses to inject the result inline and continue the completion
//! loop without a callback. The upstream contract for sync tools is that
//! `execute_synchronous` must not return `Err` (the runtime `expect`s it),
//! so *every* failure — business (`success: false`) or transport — is
//! delivered as a `ToolResult` whose text starts with `Error:`. That
//! mirrors the RAP error policy ("a descriptive result lets the model act
//! on the failure instead of waiting forever") and keeps the conversation
//! recoverable; nothing is masked because the error text is exactly what
//! the model sees.

use async_trait::async_trait;
use infinity_agent_core::message::{InputMessage, InputMessageContent};
use infinity_agent_core::tools::{Tool, ToolContext};
use infinity_agent_core::traits::InputSender;
use infinity_provider_protocol::message::{ToolResult, ToolResultContent, UserContent};
use rap_protocol::{ProviderCallId, ToolCallId};
use serde_json::Value;
use crate::catalog::CatalogEntry;
use crate::{BhToolsClient, ToolManifest};

/// One BH tool exposed to an Infinity agent runtime.
///
/// Generic over the embedding's [`InputSender`] (Infinity's `Tool` trait is),
/// so the same tool serves the local runtime (`ChannelSender`) and any
/// platform sender.
#[derive(Clone, Debug)]
pub struct BhBridgeTool {
    name: String,
    description: String,
    parameters: Value,
    client: BhToolsClient,
}

impl BhBridgeTool {
    /// Build a tool from explicit metadata plus a bridge client.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        client: BhToolsClient,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            client,
        }
    }

    /// Build from a statically mirrored [`CatalogEntry`] (see
    /// [`crate::catalog`]).
    pub fn from_catalog(entry: &CatalogEntry, client: BhToolsClient) -> Self {
        Self::new(
            entry.name,
            entry.description,
            entry.input_schema.clone(),
            client,
        )
    }

    /// Build from a `tools/list` manifest entry served by the Python peer —
    /// the peer-driven variant (schemas stay authored Python-side).
    pub fn from_manifest(manifest: &ToolManifest, client: BhToolsClient) -> Self {
        Self::new(
            manifest.name.clone(),
            format!(
                "{} (category {}; autonomy L{})",
                manifest.name, manifest.category, manifest.autonomy_level
            ),
            manifest.input_schema.clone(),
            client,
        )
    }

    /// Build one tool per static catalog entry, all pointing at `client`.
    pub fn catalog_tools(client: BhToolsClient) -> Vec<Self> {
        crate::catalog::bh_tool_catalog()
            .iter()
            .map(|entry| Self::from_catalog(entry, client.clone()))
            .collect()
    }

    /// The bridge client this tool executes through.
    pub fn client(&self) -> &BhToolsClient {
        &self.client
    }

    /// Run the tool and render the outcome as a `ToolResult`.
    ///
    /// Never returns `Err` (see the module docs): business and transport
    /// failures both become `Error:`-prefixed text results the model can act
    /// on.
    fn run_to_result(&self, args: &Value, id: &ToolCallId<str>, call_id: Option<&str>) -> ToolResult {
        let outcome = self.client.call_tool(&self.name, args);
        let text = match outcome {
            Ok(result) => result.to_string(),
            // `ToolFailed` (registry-level business failure) and
            // `Transport` (socket/protocol failure) both surface here with
            // their actionable Display strings.
            Err(e) => format!("Error: {e}"),
        };
        ToolResult {
            id: id.as_str().to_owned(),
            call_id: call_id.map(|c| c.to_owned()),
            content: vec![ToolResultContent::text(text)],
        }
    }
}

#[async_trait]
impl<M: InputSender + 'static> Tool<M> for BhBridgeTool {
    // `M::Error: Send + Sync + 'static` is already required by the
    // `InputSender` trait definition, so no extra where-clause here: adding
    // one would make this impl unusable for generic callers of `Tool`.
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    /// BH tools complete synchronously over the local UDS bridge: the
    /// runtime injects the result and continues the same completion round.
    fn supports_sync(&self) -> bool {
        true
    }

    async fn execute_synchronous(
        &self,
        args: &Value,
        id: &ToolCallId<str>,
        call_id: Option<&ProviderCallId<str>>,
        _context: &ToolContext<M>,
    ) -> Option<ToolResult> {
        Some(self.run_to_result(args, id, call_id.map(|c| c.as_str())))
    }

    /// Async dispatch path (used when a runtime chooses not to take the
    /// sync fast path): perform the call and enqueue the result like any
    /// asynchronously-dispatched tool.
    async fn execute(
        &self,
        args: Value,
        id: rap_protocol::ToolCallId,
        call_id: Option<rap_protocol::ProviderCallId>,
        context: &ToolContext<M>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let result = self.run_to_result(&args, &id, call_id.as_ref().map(|c| c.as_str()));
        let message = InputMessage {
            content: InputMessageContent::User(UserContent::ToolResult(result)),
            group_id: context.group_id.clone(),
            metadata: None,
            synthetic: None,
            display_as: None,
            subscription: false,
        };
        context
            .message_sender
            .send_to_input_queue(message, id.as_str())
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait as at;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};

    // ── Std-only mock UDS peer (same protocol; one request per connection) ──

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
            "bh-mcp-bridge-tool-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("mock.sock");
        let listener = UnixListener::bind(&path).expect("bind mock uds listener");
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let response = responder(line.trim());
                let mut bytes = response.into_bytes();
                bytes.push(b'\n');
                let _ = reader.get_mut().write_all(&bytes);
                let _ = reader.get_mut().flush();
            }
        });
        MockServer { path, handle }
    }

    fn ok_envelope(request_line: &str, result: &serde_json::Value) -> String {
        let request: serde_json::Value = serde_json::from_str(request_line).unwrap();
        serde_json::json!({
            "id": request.get("id").cloned().unwrap_or(Value::Null),
            "ok": true,
            "result": result,
        })
        .to_string()
    }

    /// InputSender stub (execute_synchronous ignores the context, but the
    /// trait needs a concrete sender type).
    #[derive(Clone)]
    struct StubSender;

    #[at]
    impl InputSender for StubSender {
        type Error = std::io::Error;
        async fn send_to_input_queue(
            &self,
            _message: InputMessage,
            _dedup_id: &str,
        ) -> Result<(), std::io::Error> {
            unreachable!("execute_synchronous never sends through the context")
        }
    }

    fn tool_context() -> ToolContext<StubSender> {
        ToolContext {
            message_sender: StubSender,
            group_id: "colony-thread-1".into(),
            callback_url: String::new(),
            user_id: None,
            thread_stack: vec!["colony-thread-1".into()],
            safe_spawn_point: None,
        }
    }

    /// `BhBridgeTool` implements `Tool<M>` for every `InputSender`; the
    /// trait-object coercion pins `M = StubSender` so methods are callable
    /// without UFCS noise.
    fn dyn_tool(tool: &BhBridgeTool) -> &dyn Tool<StubSender> {
        tool
    }

    fn shell_tool(responder: Responder) -> (BhBridgeTool, MockServer) {
        let server = spawn_mock_uds_server(responder);
        let client = BhToolsClient::new(&server.path);
        (
            BhBridgeTool::from_catalog(&crate::catalog::shell_execute(), client),
            server,
        )
    }

    #[tokio::test]
    async fn metadata_comes_from_the_catalog() {
        let (tool, _server) = shell_tool(Arc::new(|_| {
            panic!("metadata methods must not hit the socket")
        }));
        let tool = dyn_tool(&tool);
        assert_eq!(tool.name(), "shell.execute");
        assert_eq!(
            tool.description(),
            "Execute shell commands with timeout, output capture, \
             working-directory control, and environment-variable support"
        );
        assert_eq!(tool.parameters()["required"], serde_json::json!(["command"]));
        assert!(tool.supports_sync(), "BH tools execute synchronously");
    }

    #[tokio::test]
    async fn execute_synchronous_returns_the_tool_payload_as_text() {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let (tool, _server) = shell_tool(Arc::new(move |line| {
            let request: Value = serde_json::from_str(line).unwrap();
            assert_eq!(request["method"], "tools/call");
            assert_eq!(request["params"]["name"], "shell.execute");
            assert_eq!(
                request["params"]["args"],
                serde_json::json!({"command": "ls -l"})
            );
            seen_clone.lock().unwrap().push(request);
            ok_envelope(
                line,
                &serde_json::json!({
                    "success": true,
                    "stdout": "a.txt\nb.txt",
                    "exit_code": 0,
                }),
            )
        }));

        let id: ToolCallId = "tc-1".into();
        let call_id: ProviderCallId = "call-9".into();
        let result = tool
            .execute_synchronous(
                &serde_json::json!({"command": "ls -l"}),
                &id,
                Some(&call_id),
                &tool_context(),
            )
            .await
            .expect("sync tools always return Some");

        assert_eq!(result.id, "tc-1");
        assert_eq!(result.call_id.as_deref(), Some("call-9"));
        let ToolResultContent::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        let payload: Value = serde_json::from_str(&text.text).expect("payload is JSON");
        assert_eq!(payload["success"], true);
        assert_eq!(payload["stdout"], "a.txt\nb.txt");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn business_failure_becomes_an_error_tool_result_not_err() {
        // The upstream sync-tool contract is `execute_synchronous` must not
        // return Err (the runtime `expect`s it) — failures ride in the
        // result text instead.
        let (tool, _server) = shell_tool(Arc::new(|line| {
            ok_envelope(
                line,
                &serde_json::json!({
                    "success": false,
                    "error": "Tool not found: shell.execute",
                    "error_code": -32601,
                }),
            )
        }));

        let id: ToolCallId = "tc-2".into();
        let result = tool
            .execute_synchronous(&serde_json::json!({}), &id, None, &tool_context())
            .await
            .expect("still Some");
        let ToolResultContent::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(text.text.starts_with("Error:"), "got: {}", text.text);
        assert!(text.text.contains("Tool not found"));
        assert_eq!(result.call_id, None);
    }

    #[tokio::test]
    async fn transport_failure_also_becomes_an_error_tool_result() {
        let client = BhToolsClient::new("/tmp/bh-mcp-bridge-definitely-not-here.sock");
        let tool = BhBridgeTool::from_catalog(&crate::catalog::file(), client);

        let id: ToolCallId = "tc-3".into();
        let result = tool
            .execute_synchronous(&serde_json::json!({}), &id, None, &tool_context())
            .await
            .expect("still Some");
        let ToolResultContent::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(
            text.text.starts_with("Error: bh tools bridge transport error"),
            "got: {}",
            text.text
        );
    }

    #[tokio::test]
    async fn async_execute_path_enqueues_the_result_via_the_sender() {
        #[derive(Clone)]
        struct CapturingSender {
            messages: Arc<Mutex<Vec<(InputMessage, String)>>>,
        }

        #[at]
        impl InputSender for CapturingSender {
            type Error = std::io::Error;
            async fn send_to_input_queue(
                &self,
                message: InputMessage,
                dedup_id: &str,
            ) -> Result<(), std::io::Error> {
                self.messages
                    .lock()
                    .unwrap()
                    .push((message, dedup_id.to_owned()));
                Ok(())
            }
        }

        let (tool, _server) = shell_tool(Arc::new(|line| {
            ok_envelope(
                line,
                &serde_json::json!({"success": true, "stdout": "hi"}),
            )
        }));

        let messages = Arc::new(Mutex::new(Vec::new()));
        let context = ToolContext {
            message_sender: CapturingSender {
                messages: Arc::clone(&messages),
            },
            group_id: "colony-thread-1".into(),
            callback_url: String::new(),
            user_id: None,
            thread_stack: vec!["colony-thread-1".into()],
            safe_spawn_point: None,
        };

        tool.execute(
            serde_json::json!({"command": "echo hi"}),
            "tc-4".into(),
            None,
            &context,
        )
        .await
        .expect("execute succeeds");

        let sent = messages.lock().unwrap();
        assert_eq!(sent.len(), 1, "one tool result enqueued");
        assert_eq!(sent[0].1, "tc-4", "dedup id is the tool-call id");
        assert_eq!(sent[0].0.group_id.as_str(), "colony-thread-1");
        let InputMessageContent::User(UserContent::ToolResult(result)) = &sent[0].0.content
        else {
            panic!("expected a tool result message");
        };
        assert_eq!(result.id, "tc-4");
    }

    #[test]
    fn catalog_tools_cover_the_whole_catalog() {
        let client = BhToolsClient::new("/tmp/unused.sock");
        let tools = BhBridgeTool::catalog_tools(client);
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, "file");
        assert_eq!(tools[1].name, "shell.execute");
        assert_eq!(tools[2].name, "code.operations");
    }

    #[tokio::test]
    async fn manifest_driven_variant_uses_peer_schemas() {
        let manifest = ToolManifest {
            name: "memory.manage".into(),
            category: "memory".into(),
            autonomy_level: 1,
            input_schema: serde_json::json!({"type": "object", "required": ["op"]}),
            output_schema: serde_json::json!({"type": "object"}),
        };
        let client = BhToolsClient::new("/tmp/unused.sock");
        let tool = BhBridgeTool::from_manifest(&manifest, client);
        let tool = dyn_tool(&tool);
        assert_eq!(tool.name(), "memory.manage");
        assert_eq!(tool.parameters(), manifest.input_schema);
    }
}
