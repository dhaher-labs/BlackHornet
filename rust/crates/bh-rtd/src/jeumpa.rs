//! `JeumpaProvider` — Infinity [`ModelProvider`] bridged to the Python
//! Jeumpa Brain over the BH bridge protocol v1 (see [`crate::uds`]).
//!
//! Phase 1 deliverable (PLAN.md §4 "Bridges before ports"): this provider
//! does **not** re-implement the Jeumpa providers in Rust. It forwards
//! completions to `src/colony/jeumpa_uds_server.py`, which delegates to the
//! real `JeumpaBrain` (5 keyless providers, task profiles, cooldowns, cache).
//!
//! Mapping (honours the Phase 0 seam note in `infinity_adapter.rs`):
//!
//! | Infinity concept              | Jeumpa Brain concept                    |
//! |-------------------------------|-----------------------------------------|
//! | `model_id`                    | task profile (`coding`, `reasoning`, …) |
//! | `list_models()`               | `models` method → `TASK_PROFILES` keys  |
//! | `CompletionRequest.preamble`  | system message prepended to `messages`  |
//! | `invoke_model()`              | `complete` method → `brain.chat_messages` |
//! | business failure (`success:false`) | `CompletionError::ProviderError` with the server-declared retry class |
//! | bridge/transport failure      | `CompletionError::ResponseError` (transient) |
//!
//! Cooldown/rate-limit classification stays BH-side: the Python server
//! declares `error_class` (`throttled` | `transient` | `context_overflow` |
//! `fatal`) from the brain's own failure vocabulary; this provider maps it
//! onto Infinity's [`ErrorClass`] verbatim (default `Fatal` — the safe,
//! do-not-retry classification, mirroring Infinity's own `WireError`
//! default).
//!
//! ## Tools over a text-only brain (Phase 1 tool convention)
//!
//! The Python Jeumpa Brain is text-only: `chat_messages` sends `{role,
//! content}` messages upstream and returns `choices[0].message.content` —
//! it has no native tool-calling channel, and teaching it one is Python-side
//! work outside this crate. Infinity agents, however, always pass the
//! registered toolset in `CompletionRequest.tools`. Dropping those
//! definitions silently would fake capability; rejecting them outright would
//! make "an Infinity agent with 100% BH brain + tools" (the Phase 1 exit
//! criterion) unreachable. So the bridge does what a bridge is for:
//!
//! 1. **Forward** the tool definitions verbatim to the Python peer in the
//!    `complete` params (`tools: [{name, description, parameters}]`), so a
//!    future brain upgrade can switch to native tool calling without any
//!    Rust-side change.
//! 2. **Declare** the tools to the model in the system message with an exact
//!    reply convention: reply `BH_TOOL_CALL <name> <json-args>` (one line)
//!    to call a tool.
//! 3. **Translate** a conforming reply into [`StreamChunk::ToolCall`] so the
//!    Infinity runtime executes it through the registered tools (the BH
//!    tool bridge; see the `bh-mcp-bridge` crate) and loops back with the
//!    tool result. A reply that merely *mentions* the convention or is
//!    malformed passes through as plain text — never fabricated into a
//!    tool call.
//!
//! This is the standard prompt-declared tool pattern for models without
//! native function calling; the Rust side owns none of the tool *semantics*.
//!
//! Concurrency note: Phase 1 performs the UDS round-trip with blocking std
//! I/O inside the async methods. The response is fully buffered and then
//! replayed as a one-shot [`ModelStream`]. This is deliberate (no tokio
//! *dependency* in this crate — tokio remains a dev-dependency until the
//! Phase 2 runtime wedge brings the real executor) and is safe for the
//! intended Phase 1/2 usage pattern (one completion at a time per colony
//! member thread); Phase 2 will move the I/O onto the runtime's executor.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use infinity_provider_protocol::{
    AssistantContent, CompletionError, CompletionRequest, ErrorClass, FinalResponse, Message,
    ModelEntry, ModelProvider, ModelStream, StreamChunk, ToolCall, ToolDefinition,
    ToolResultContent, UserContent,
};
use serde_json::{json, Value};

use crate::uds::UdsClient;

/// Method on the jeumpa UDS server that reports brain health/stats.
const METHOD_HEALTH: &str = "health";
/// Method returning the model catalog (one [`ModelEntry`] per task profile).
const METHOD_MODELS: &str = "models";
/// Method performing a completion (`params.messages` + `params.profile`).
const METHOD_COMPLETE: &str = "complete";

/// Reply-prefix convention for prompt-declared tool calls over the text-only
/// brain. Documented in the tool-declaration block emitted by
/// [`tool_declaration_block`] and parsed by [`parse_tool_call_reply`].
pub const TOOL_CALL_PREFIX: &str = "BH_TOOL_CALL";

static PROVIDER_SEQ: AtomicU64 = AtomicU64::new(0);
static TOOL_CALL_SEQ: AtomicU64 = AtomicU64::new(0);

/// Errors specific to the Jeumpa bridge (request *shaping* only; transport
/// and business errors map onto [`CompletionError`], per Infinity's own
/// provider contract).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JeumpaError {
    /// The `CompletionRequest` used a feature this bridge does not carry.
    /// Fail-loud, never silently dropped.
    UnsupportedFeature(String),
    /// The Python peer's reply did not match the documented `complete`
    /// result schema.
    BadReply(String),
}

impl core::fmt::Display for JeumpaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JeumpaError::UnsupportedFeature(m) => {
                write!(f, "jeumpa bridge does not support this request: {m}")
            }
            JeumpaError::BadReply(m) => write!(f, "jeumpa bridge reply malformed: {m}"),
        }
    }
}

impl std::error::Error for JeumpaError {}

/// Infinity [`ModelProvider`] backed by the Python Jeumpa Brain.
///
/// Construct with [`JeumpaProvider::new`] pointing at the socket path of a
/// running `src/colony/jeumpa_uds_server.py` instance.
#[derive(Clone, Debug)]
pub struct JeumpaProvider {
    client: UdsClient,
}

impl JeumpaProvider {
    /// Provider bound to the UDS socket at `socket_path`.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            client: UdsClient::new(socket_path),
        }
    }

    /// Socket path of the Python peer.
    pub fn socket_path(&self) -> &Path {
        self.client.path()
    }

    /// Probe the Python brain (`health` method) — used by daemons and tests
    /// to verify the bridge is alive before registering the provider.
    pub async fn health(&self) -> Result<Value, crate::uds::UdsError> {
        self.call(METHOD_HEALTH, &json!({}))
    }

    /// Blocking round-trip over the shared client (see module docs for the
    /// Phase 1 concurrency note).
    fn call(&self, method: &str, params: &Value) -> Result<Value, crate::uds::UdsError> {
        self.client.call(method, params)
    }
}

#[async_trait]
impl ModelProvider for JeumpaProvider {
    /// List the models offered by the brain: one entry per task profile.
    /// The first entry is the provider's default model (Infinity contract);
    /// the Python server guarantees `default` is present and we sort it
    /// first deterministically here.
    async fn list_models(
        &self,
    ) -> Result<Vec<ModelEntry>, Box<dyn std::error::Error + Send + Sync>> {
        let reply = self.call(METHOD_MODELS, &json!({}))?;
        let models = reply
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| JeumpaError::BadReply("`models` reply missing 'models' array".into()))?;
        let mut entries: Vec<ModelEntry> = models
            .iter()
            .map(|m| {
                serde_json::from_value(m.clone())
                    .map_err(|e| JeumpaError::BadReply(format!("bad ModelEntry: {e}")))
            })
            .collect::<Result<_, _>>()?;
        // Infinity: "The first entry is the provider's default model."
        if let Some(pos) = entries.iter().position(|m| m.model_id == "default") {
            let default = entries.remove(pos);
            entries.insert(0, default);
        }
        Ok(entries)
    }

    /// Invoke one task profile (`model_id`) with the given completion
    /// request. Tool definitions are forwarded to the peer and declared to
    /// the model (see the module docs' tool convention); a conforming
    /// `BH_TOOL_CALL` reply is translated into a [`StreamChunk::ToolCall`].
    async fn invoke_model(
        &self,
        model_id: &str,
        request: CompletionRequest,
    ) -> Result<ModelStream, CompletionError> {
        let params = completion_to_params(model_id, &request)?;
        let reply = self
            .call(METHOD_COMPLETE, &params)
            .map_err(transport_error)?;

        let success = reply
            .get("success")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                CompletionError::ResponseError(
                    JeumpaError::BadReply(
                        "'complete' reply missing boolean 'success'".into(),
                    )
                    .to_string(),
                )
            })?;
        if !success {
            let message = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unspecified jeumpa failure")
                .to_string();
            let class = parse_error_class(reply.get("error_class"));
            return Err(CompletionError::provider(
                class,
                format!("jeumpa: {message}"),
            ));
        }
        let content = reply
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CompletionError::ResponseError(
                    JeumpaError::BadReply("successful 'complete' reply missing 'content'".into())
                        .to_string(),
                )
            })?
            .to_string();

        // Tool-convention translation: a conforming `BH_TOOL_CALL` reply
        // becomes a ToolCall chunk the runtime executes; anything else is
        // plain assistant text. Never both, never fabricated.
        let chunks: Vec<Result<StreamChunk, CompletionError>> =
            match parse_tool_call_reply(&content) {
                Some((name, args)) => vec![
                    Ok(StreamChunk::ToolCall(ToolCall::new(
                        format!("jeumpa-tc-{}", TOOL_CALL_SEQ.fetch_add(1, Ordering::Relaxed)),
                        name,
                        args,
                    ))),
                    Ok(StreamChunk::Final(FinalResponse { usage: None })),
                ],
                None => vec![
                    Ok(StreamChunk::Text(content)),
                    Ok(StreamChunk::Final(FinalResponse { usage: None })),
                ],
            };
        Ok(Box::pin(futures_util::stream::iter(chunks)))
    }
}

/// Translate an Infinity [`CompletionRequest`] into the `complete` params of
/// the BH bridge protocol: `{messages, profile, tools?, ...}`.
///
/// Tool definitions are never dropped: they ride in the `tools` param
/// (verbatim, for future native brain-side use) and are declared to the
/// model in the system message via [`tool_declaration_block`].
fn completion_to_params(
    model_id: &str,
    request: &CompletionRequest,
) -> Result<Value, CompletionError> {
    let mut messages: Vec<Value> = Vec::new();
    let preamble = match &request.preamble {
        Some(p) if request.tools.is_empty() => p.clone(),
        Some(p) => format!("{}\n\n{}", p, tool_declaration_block(&request.tools)),
        None if request.tools.is_empty() => String::new(),
        None => tool_declaration_block(&request.tools),
    };
    if !preamble.is_empty() {
        messages.push(json!({ "role": "system", "content": preamble }));
    }
    for message in &request.chat_history {
        let (role, text) = match message {
            Message::User { content } => {
                let text = content
                    .iter()
                    .map(|block| match block {
                        UserContent::Text(t) => t.text.clone(),
                        // Tool results are the model's only view of what a
                        // tool returned: forward their text content
                        // losslessly (multi-block results join with
                        // newlines). Non-text blocks degrade to placeholders.
                        UserContent::ToolResult(r) => {
                            let parts: Vec<String> = r
                                .content
                                .iter()
                                .map(|c| match c {
                                    ToolResultContent::Text(t) => t.text.clone(),
                                    ToolResultContent::Image(_) => {
                                        "[image tool result omitted]".to_string()
                                    }
                                })
                                .collect();
                            if parts.is_empty() {
                                format!("[tool result {} with no text content]", r.id)
                            } else {
                                parts.join("\n")
                            }
                        }
                        UserContent::Image(_) => "[image content omitted]".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ("user", text)
            }
            Message::Assistant { content } => {
                let text = content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Text(t) => Some(t.text.clone()),
                        // History keeps a complete record of calls the model
                        // made, so the model can see what it already did.
                        AssistantContent::ToolCall(c) => Some(format!(
                            "[called tool {} with arguments {}]",
                            c.function.name, c.function.arguments
                        )),
                        AssistantContent::Reasoning(r) => {
                            let display = r.display_text();
                            (!display.is_empty()).then_some(display)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ("assistant", text)
            }
        };
        if !text.is_empty() {
            messages.push(json!({ "role": role, "content": text }));
        }
    }
    if messages.is_empty() {
        return Err(CompletionError::RequestError(Box::new(
            JeumpaError::UnsupportedFeature(
                "request carries no text content to complete".into(),
            ),
        )));
    }

    let mut params = json!({
        "messages": messages,
        "profile": model_id,
        "request_id": format!("jeumpa-{}", PROVIDER_SEQ.fetch_add(1, Ordering::Relaxed)),
    });
    if let Some(max_tokens) = request.max_tokens {
        params["max_tokens"] = json!(max_tokens);
    }
    // Tool definitions ride to the Python peer verbatim (Phase 1: the brain
    // ignores them; a native-tool-call brain upgrade reads them from here).
    if !request.tools.is_empty() {
        params["tools"] = json!(request
            .tools
            .iter()
            .map(|t| json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }))
            .collect::<Vec<_>>());
    }
    // Provider-specific parameters ride through unchanged (e.g.
    // `temperature`, `use_cache`) — the Python server validates them.
    if let Some(Value::Object(extra)) = &request.additional_params {
        for (key, value) in extra {
            params[key.as_str()] = value.clone();
        }
    }
    Ok(params)
}

/// Render the prompt-declared tool block appended to the system message when
/// the request carries tool definitions. Deterministic (declaration order),
/// and ends with the exact reply convention parsed by
/// [`parse_tool_call_reply`].
fn tool_declaration_block(tools: &[ToolDefinition]) -> String {
    let mut block = String::from("# Available tools\n\nYou can use these tools:\n");
    for tool in tools {
        block.push_str(&format!("\n- `{}`: {}\n", tool.name, tool.description));
        block.push_str(&format!("  Arguments JSON schema: {}\n", tool.parameters));
    }
    block.push_str(&format!(
        "\nTo call a tool, reply with exactly one line and nothing else:\n\
         {TOOL_CALL_PREFIX} <tool-name> <json-object-of-arguments>\n\
         Example: {TOOL_CALL_PREFIX} shell.execute {{\"command\": \"ls -l\"}}\n\
         If you do not need a tool, reply with plain text only.\n"
    ));
    block
}

/// Parse a model reply against the [`TOOL_CALL_PREFIX`] convention.
///
/// Returns `Some((tool_name, arguments))` only for a *conforming* reply: the
/// trimmed content starts with `BH_TOOL_CALL `, names a tool, and carries a
/// JSON object of arguments. Everything else (including malformed or
/// partial uses of the prefix) is `None`, so the reply flows through as
/// plain assistant text — the bridge never fabricates a tool call.
fn parse_tool_call_reply(content: &str) -> Option<(String, Value)> {
    let trimmed = content.trim();
    let rest = trimmed.strip_prefix(TOOL_CALL_PREFIX)?.trim_start();
    let (name, args_raw) = rest.split_once(' ')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let args: Value = serde_json::from_str(args_raw.trim()).ok()?;
    args.as_object()?;
    Some((name.to_owned(), args))
}

/// Map bridge/transport failures onto Infinity's error model: the peer may
/// come back (restart, cooldown expiry), so these are transient
/// (`ResponseError`), matching `CompletionError::class()`'s own mapping.
fn transport_error(e: crate::uds::UdsError) -> CompletionError {
    CompletionError::ResponseError(e.to_string())
}

/// Parse the server-declared retry class. Unknown/missing → `Fatal`
/// (do-not-retry), the safe default also used by Infinity's `WireError`.
fn parse_error_class(value: Option<&Value>) -> ErrorClass {
    match value.and_then(Value::as_str) {
        Some("throttled") => ErrorClass::Throttled,
        Some("transient") => ErrorClass::Transient,
        Some("context_overflow") => ErrorClass::ContextOverflow,
        _ => ErrorClass::Fatal,
    }
}
