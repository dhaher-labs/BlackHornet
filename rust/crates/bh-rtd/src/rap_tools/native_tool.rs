//! The Infinity `Tool` surface for the native RAP tools: a
//! [`NativeTool`] per tool name, executing through the shared
//! [`NativeToolServer`](super::NativeToolServer) — the Phase 4 counterpart
//! of `bh-mcp-bridge`'s `BhBridgeTool` (which rides the MCP proxy).
//!
//! Synchronous execution contract (mirrors the bridge's, which mirrors the
//! runtime's): `supports_sync() -> true` and `execute_synchronous` never
//! returns `Err` — every outcome is rendered as the tool result text the
//! model sees:
//!
//! * business success → the tool's own JSON payload (same shape the Python
//!   registry returns through the proxy);
//! * a business failure (`file`'s `success: false`, `search`'s `error`
//!   field, `shell`'s negative exit codes stay inside their payloads) →
//!   `Error: <message>` when the payload marks the failure, mirroring how
//!   `BhToolsClient` maps `success: false` onto error text;
//! * protocol misuse (unknown tool, non-object args) →
//!   `Error: <typed error>` — loud, never a fabricated result.

use async_trait::async_trait;
use infinity_agent_core::message::{InputMessage, InputMessageContent};
use infinity_agent_core::tools::{Tool, ToolContext};
use infinity_agent_core::traits::InputSender;
use infinity_provider_protocol::message::{ToolResult, ToolResultContent, UserContent};
use rap_protocol::{ProviderCallId, ToolCallId};
use serde_json::Value;
use std::sync::Arc;

use super::{NativeToolError, NativeToolServer, NativeToolSpec};

/// One native RAP tool exposed to an Infinity agent runtime. Shares the
/// daemon's [`NativeToolServer`] with the direct `tools/call` router, so
/// agent rounds and direct calls execute through exactly the same engines.
#[derive(Clone, Debug)]
pub struct NativeTool {
    spec: NativeToolSpec,
    server: Arc<NativeToolServer>,
}

impl NativeTool {
    /// Build the native `file` tool against `server`.
    pub fn file(server: Arc<NativeToolServer>) -> Self {
        Self::from_spec(super::file_spec(), server)
    }

    /// Build the native `shell.execute` tool against `server`.
    pub fn shell(server: Arc<NativeToolServer>) -> Self {
        Self::from_spec(super::shell_execute_spec(), server)
    }

    /// Build the native `search.web` tool against `server`.
    pub fn search(server: Arc<NativeToolServer>) -> Self {
        Self::from_spec(super::search_web_spec(), server)
    }

    /// Build one tool per native catalog spec, all against `server`.
    pub fn catalog(server: Arc<NativeToolServer>) -> Vec<Self> {
        super::native_tool_specs()
            .into_iter()
            .map(|spec| Self::from_spec(spec, Arc::clone(&server)))
            .collect()
    }

    /// Build from an explicit spec (the shared constructor).
    pub fn from_spec(spec: NativeToolSpec, server: Arc<NativeToolServer>) -> Self {
        Self { spec, server }
    }

    /// The server this tool executes through.
    pub fn server(&self) -> &NativeToolServer {
        &self.server
    }

    /// Run the tool and render the outcome as a `ToolResult` (see the
    /// module docs for the rendering contract).
    fn run_to_result(
        &self,
        args: &Value,
        id: &ToolCallId<str>,
        call_id: Option<&str>,
    ) -> ToolResult {
        let outcome = self.server.call(self.spec.name, args);
        let text = match outcome {
            Ok(result) => render_result(&result),
            Err(e) => format!("Error: {e}"),
        };
        ToolResult {
            id: id.as_str().to_owned(),
            call_id: call_id.map(|c| c.to_owned()),
            content: vec![ToolResultContent::text(text)],
        }
    }
}

/// Render a tool payload the way the MCP proxy bridge does: a payload that
/// marks its own business failure becomes `Error: <message>`; everything
/// else travels as the verbatim JSON the registry would have returned.
fn render_result(result: &Value) -> String {
    if result.get("success").and_then(Value::as_bool) == Some(false) {
        let message = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("tool reported failure without an error message");
        return format!("Error: {message}");
    }
    serde_json::to_string(result).unwrap_or_else(|e| format!("Error: result unserializable: {e}"))
}

#[async_trait]
impl<M: InputSender + 'static> Tool<M> for NativeTool {
    fn name(&self) -> &str {
        self.spec.name
    }

    fn description(&self) -> &str {
        self.spec.description
    }

    fn parameters(&self) -> Value {
        self.spec.input_schema.clone()
    }

    /// Native tools complete synchronously (an in-process engine call, no
    /// socket): the runtime injects the result and continues the same
    /// completion round.
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

/// Never constructed: proves [`NativeToolError`] stays `Send + Sync` (the
/// shared server crosses threads).
const _: Option<fn(NativeToolError) -> Box<dyn std::error::Error + Send + Sync>> =
    Some(|e| Box::new(e));

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait as at;
    use serde_json::json;
    use std::path::PathBuf;

    /// InputSender stub (`execute_synchronous` ignores the context, but
    /// the trait needs a concrete sender type).
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

    fn sandbox() -> (Arc<NativeToolServer>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "bh-rap-native-tool-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        (Arc::new(NativeToolServer::new(&dir)), dir)
    }

    fn dyn_tool(tool: &NativeTool) -> &dyn Tool<StubSender> {
        tool
    }

    #[tokio::test]
    async fn metadata_comes_from_the_mirrored_spec() {
        let (server, _dir) = sandbox();
        let tool = NativeTool::file(Arc::clone(&server));
        let tool = dyn_tool(&tool);
        assert_eq!(tool.name(), "file");
        assert_eq!(
            tool.description(),
            "Read, write, edit, list, search (glob), diff/patch files \
             with safety controls and encoding detection"
        );
        assert_eq!(tool.parameters()["required"], json!(["operation", "path"]));
        assert!(tool.supports_sync(), "native tools execute synchronously");
    }

    #[tokio::test]
    async fn execute_synchronous_returns_the_tool_payload_as_text() {
        let (server, dir) = sandbox();
        let tool = NativeTool::file(server);

        let id: ToolCallId = "tc-1".into();
        let call_id: ProviderCallId = "call-9".into();
        let result = tool
            .execute_synchronous(
                &json!({"operation": "write", "path": "a.txt", "content": "native!"}),
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
        assert!(std::path::Path::new(&dir).join("a.txt").exists());
    }

    #[tokio::test]
    async fn business_failure_becomes_an_error_tool_result_not_err() {
        let (server, _dir) = sandbox();
        let tool = NativeTool::file(server);

        let id: ToolCallId = "tc-2".into();
        let result = tool
            .execute_synchronous(
                &json!({"operation": "read", "path": "../escape.txt"}),
                &id,
                None,
                &tool_context(),
            )
            .await
            .expect("still Some");
        let ToolResultContent::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(
            text.text.starts_with("Error: Path traversal detected"),
            "got: {}",
            text.text
        );
        assert_eq!(result.call_id, None);
    }

    #[tokio::test]
    async fn unknown_tool_and_shell_payloads_render_like_the_proxy() {
        let (server, _dir) = sandbox();
        let shell = NativeTool::shell(server);

        let id: ToolCallId = "tc-3".into();
        let result = shell
            .execute_synchronous(
                &json!({"command": "printf 'native-shell'"}),
                &id,
                None,
                &tool_context(),
            )
            .await
            .expect("still Some");
        let ToolResultContent::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        // The shell payload has no `success` flag: it travels verbatim,
        // exactly like the Python registry's result through the proxy.
        let payload: Value = serde_json::from_str(&text.text).expect("payload is JSON");
        assert_eq!(payload["stdout"], "native-shell");
        assert_eq!(payload["exit_code"], 0);
    }

    #[tokio::test]
    async fn async_execute_path_enqueues_the_result_via_the_sender() {
        #[derive(Clone)]
        struct CapturingSender {
            messages: Arc<std::sync::Mutex<Vec<(InputMessage, String)>>>,
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

        let (server, _dir) = sandbox();
        let tool = NativeTool::search(server);
        let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
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
            json!({"action": "local", "query": "anything"}),
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
    fn catalog_covers_the_whole_native_set() {
        let (server, dir) = sandbox();
        let tools = NativeTool::catalog(Arc::clone(&server));
        // `Tool<M>` is implemented for every sender `M` — name the test
        // sender explicitly so the trait method call resolves.
        let names: Vec<&str> = tools
            .iter()
            .map(<NativeTool as Tool<StubSender>>::name)
            .collect();
        assert_eq!(names, vec!["file", "shell.execute", "search.web"]);
        for tool in &tools {
            assert!(Arc::ptr_eq(&tool.server, &server), "servers are shared");
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
