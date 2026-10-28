//! Phase 3 **memory seam** — Infinity-shaped `ConversationStore` /
//! `StateStore` contracts backed by the BH memory subsystem over BH bridge
//! protocol v1 (PLAN.md §4 Phase 3: *"Memory seam — Infinity
//! ConversationStore/StateStore over BH SQLite FTS5 (+ ChromaDB
//! read-through). Exit criteria: Hibernating agents persist into BH
//! MemoryFederation."*).
//!
//! # Posture
//!
//! * **Never port the colony.** The Rust side never touches SQLite (that
//!   would be porting). All data lives in the existing Python memory stack
//!   (`src/memory/`): [`SessionMemory`](https://docs.rs/) sessions/messages
//!   (SQLite), `MemorySystem` LTM (SQLite + FTS5), `MemoryFederation`
//!   (cross-agent share/recall) and episodic memory. A Python peer,
//!   `src/memory/rtd_memory_server.py`, SHIPs that data over the same UDS
//!   JSON-lines protocol v1 used by every other bridge
//!   ([`bh_rtd::uds`]).
//! * **R10 isolation.** The traits below are bh-rtd-owned and reference **no
//!   Infinity types**. They mirror the *shape* of the verified
//!   `infinity-agent-core` 0.2.0 `ConversationStore` / `StateStore` traits
//!   (`ensure/thread_exists`, `load_history_up_to`, `append_messages`,
//!   `is_thread_closed`, `close_thread`, ...) so a thin adapter can map
//!   them 1:1 later, but an Infinity 0.3.0 API churn cannot break this
//!   seam. Infinity's traits are `async`; this seam is deliberately sync
//!   (the UDS client is sync, one short request per call) — an async
//!   adapter can wrap it without changing the wire contract.
//! * **Fail-loud.** A peer `ok:false` answer is a [`MemoryError::Server`],
//!   a result that violates the documented shape is a
//!   [`MemoryError::Protocol`], and a missing peer is a
//!   [`MemoryError::Transport`] — never an empty-but-successful answer.
//!
//! # Documented mapping deltas (Infinity 0.2.0 → BH seam)
//!
//! | Infinity 0.2.0 | BH seam / peer | Why the delta |
//! |---|---|---|
//! | `ensure_root_thread(thread_id)` with caller-chosen ids | [`ConversationStore::create_thread`] — the peer mints the id (`SessionMemory.create_session`) | BH sessions are server-id-minted; no fake "ensure with id X" |
//! | `append_messages(session_id, Vec<(InfinityMessage, String)>)` | [`ConversationStore::append_messages`] with [`MemoryMessage`] (role/content/metadata, batch) | `SessionMemory` stores role/content rows; the Infinity↔BH message adapter stays out of the seam (R10) |
//! | `load_history_up_to(session_id, start_from, up_to)` | same name and leading-row-count semantics, sliced peer-side over `SessionMemory.get_messages` | identical convention, no re-implementation client-side |
//! | `StateStore::{get,set}_metadata`, processed-ids, subscriptions, user choices | [`StateStore::{get_state,set_state,delete_state}`] — JSON values over `MemorySystem` LTM (SQLite + FTS5) | hibernation state is the Phase 3 need; Infinity's dedup/subscription tables are runtime-internal and have no real BH counterpart yet — adding them would be fabrication |
//! | — (no equivalent) | [`SharedMemoryStore::share`] / [`::recall`] | the exit criterion runs through `MemoryFederation` |
//!
//! # Post-MVP (documented, NOT faked)
//!
//! **ChromaDB read-through** is listed in PLAN.md §4 Phase 3 but is *not*
//! part of this MVP: `src/memory/vector.py` (Chroma) is not wired into the
//! peer's read path and the Rust client claims nothing about it. Wiring it
//! in without a real deployment is exactly the "fake success" the repo
//! bans; see `src/memory/rtd_memory_server.py` docstring for the same note.
//!
//! Feature gate: `bridge` (the seam is Infinity-independent; it only needs
//! the UDS transport).

use std::path::Path;

use serde_json::{json, Value};

use crate::uds::{UdsClient, UdsError};

// ── Errors ──────────────────────────────────────────────────────────────

/// Errors from the memory seam. Fail-loud on purpose: every variant names
/// what actually went wrong; nothing degrades into a silent empty answer.
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryError {
    /// Transport failure: the peer is unreachable (socket missing, refused,
    /// timed out) or died mid-call.
    Transport(String),
    /// The peer answered `ok:false` with a structured bridge error
    /// (unknown session, invalid params, internal failure, ...).
    Server {
        /// Machine-readable bridge error code (see `bh_rtd::uds` constants).
        code: i64,
        /// Human-readable message from the peer.
        message: String,
    },
    /// The peer answered `ok:true` but the result violates the documented
    /// result shape (missing/ill-typed fields) — a contract drift, not data.
    Protocol(String),
}

impl core::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryError::Transport(m) => write!(f, "memory peer unreachable: {m}"),
            MemoryError::Server { code, message } => {
                write!(f, "memory peer error {code}: {message}")
            }
            MemoryError::Protocol(m) => write!(f, "memory peer protocol drift: {m}"),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<UdsError> for MemoryError {
    fn from(e: UdsError) -> Self {
        match e {
            UdsError::Io(m) => MemoryError::Transport(m),
            UdsError::Protocol(m) => MemoryError::Protocol(m),
            UdsError::Server { code, message } => MemoryError::Server { code, message },
        }
    }
}

// ── Data types (BH session shapes; no Infinity types, R10) ──────────────

/// One persisted conversation message (BH `SessionMemory.Message` shape).
///
/// This is the BH-side counterpart of an Infinity history entry: the seam
/// stores the role/content view that the BH session store models, plus the
/// raw metadata JSON blob for anything richer.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryMessage {
    /// Server-minted message id (absent on append; the peer mints it).
    pub message_id: Option<String>,
    /// `user` | `assistant` | `system` | `tool` (BH session roles).
    pub role: String,
    /// Message text content.
    pub content: String,
    /// Free-form JSON metadata persisted alongside the message.
    pub metadata: Value,
    /// Token estimate (the peer estimates `len/4` when absent).
    pub token_count: Option<i64>,
    /// ISO-8601 timestamp (absent on append; the peer stamps it).
    pub timestamp: Option<String>,
}

impl MemoryMessage {
    /// Convenience constructor: role + content, everything else defaulted.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            message_id: None,
            role: role.into(),
            content: content.into(),
            metadata: json!({}),
            token_count: None,
            timestamp: None,
        }
    }

    /// Wire form for `conversation.append` (server-minted fields omitted).
    pub fn to_params(&self) -> Value {
        let mut v = json!({
            "role": self.role,
            "content": self.content,
            "metadata": if self.metadata.is_null() { json!({}) } else { self.metadata.clone() },
        });
        if let Some(tc) = self.token_count {
            v["token_count"] = json!(tc);
        }
        v
    }

    /// Decode one element of a peer `messages` array. Missing required
    /// fields or wrong types are protocol errors, never silent defaults.
    pub fn from_value(v: &Value) -> Result<Self, MemoryError> {
        let obj = v.as_object().ok_or_else(|| {
            MemoryError::Protocol(format!("message entry is not an object: {v}"))
        })?;
        let role = require_str(obj, "role")?;
        let content = require_str(obj, "content")?;
        let metadata = match obj.get("metadata") {
            None | Some(Value::Null) => json!({}),
            Some(m @ Value::Object(_)) => m.clone(),
            Some(other) => {
                return Err(MemoryError::Protocol(format!(
                    "message metadata must be an object, got {other}"
                )))
            }
        };
        let token_count = opt_i64(obj, "token_count")?;
        let message_id = opt_str(obj, "message_id")?;
        let timestamp = opt_str(obj, "timestamp")?;
        Ok(Self {
            message_id,
            role,
            content,
            metadata,
            token_count,
            timestamp,
        })
    }
}

/// Summary of one BH session (a conversation thread in seam terms).
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadSummary {
    /// Session id (minted by the peer; use it as the thread id everywhere).
    pub session_id: String,
    /// Owning agent id ("" when the session was created without one).
    pub agent_id: String,
    /// Colony scope the session was created under.
    pub colony_id: String,
    /// `active` | `compacted` | `archived` | `closed`.
    pub status: String,
    /// Number of messages persisted in the session.
    pub message_count: i64,
    /// Estimated total tokens across the session.
    pub total_tokens: i64,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// ISO-8601 last-activity stamp.
    pub last_active: String,
}

impl ThreadSummary {
    /// Decode one element of a peer `sessions` array (fail-loud on drift).
    pub fn from_value(v: &Value) -> Result<Self, MemoryError> {
        let obj = v
            .as_object()
            .ok_or_else(|| MemoryError::Protocol(format!("session entry is not an object: {v}")))?;
        Ok(Self {
            session_id: require_str(obj, "session_id")?,
            agent_id: default_str(obj, "agent_id")?,
            colony_id: default_str(obj, "colony_id")?,
            status: require_str(obj, "status")?,
            message_count: default_i64(obj, "message_count")?,
            total_tokens: default_i64(obj, "total_tokens")?,
            created_at: default_str(obj, "created_at")?,
            last_active: default_str(obj, "last_active")?,
        })
    }
}

/// One hibernation-state entry (BH `LongTermMemory` entry shape).
///
/// Values are JSON: the peer serialises with `json.dumps` into the LTM
/// (durable SQLite + FTS5) and deserialises on read.
#[derive(Clone, Debug, PartialEq)]
pub struct StateEntry {
    /// LTM row id.
    pub id: i64,
    /// The state key this entry was stored under.
    pub key: String,
    /// The stored JSON value.
    pub value: Value,
    /// Tags persisted with the entry (searchable via FTS5/tag index).
    pub tags: Vec<String>,
    /// Importance weight (0.0–1.0) used by BH forgetting/consolidation.
    pub importance: f64,
}

impl StateEntry {
    /// Decode a peer `entry` object (fail-loud on drift). `key_hint` is the
    /// key the caller asked for; it backfills the summary if the peer sent
    /// an empty `key` (defensive only — the peer echoes the key).
    pub fn from_value(key_hint: &str, v: &Value) -> Result<Self, MemoryError> {
        let obj = v
            .as_object()
            .ok_or_else(|| MemoryError::Protocol(format!("state entry is not an object: {v}")))?;
        let key = default_str(obj, "key")?;
        let key = if key.is_empty() {
            key_hint.to_string()
        } else {
            key
        };
        Ok(Self {
            id: default_i64(obj, "id")?,
            key,
            value: obj
                .get("value")
                .cloned()
                .ok_or_else(|| MemoryError::Protocol("state entry missing 'value'".into()))?,
            tags: default_str_list(obj, "tags")?,
            importance: default_f64(obj, "importance")?,
        })
    }
}

/// One federated memory record (`MemoryFederation.FederatedMemory` shape).
#[derive(Clone, Debug, PartialEq)]
pub struct SharedMemoryRecord {
    /// Federation id (content hash, merge key for duplicate shares).
    pub memory_id: String,
    /// The shared content.
    pub content: String,
    /// Agent that shared the memory.
    pub source_agent: String,
    /// `insight` | `pattern` | `fact` | `skill_ref` | `warning` | ...
    pub memory_type: String,
    /// Importance weight (0.0–1.0).
    pub importance: f64,
    /// Confidence weight (0.0–1.0), raised on repeated agreements.
    pub confidence: f64,
    /// Tags used by relevance scoring.
    pub tags: Vec<String>,
    /// How many agents have re-shared this content.
    pub agreement_count: i64,
    /// How often this memory has been recalled.
    pub access_count: i64,
}

impl SharedMemoryRecord {
    /// Decode a peer `memory` / `memories[]` object (fail-loud on drift).
    pub fn from_value(v: &Value) -> Result<Self, MemoryError> {
        let obj = v
            .as_object()
            .ok_or_else(|| MemoryError::Protocol(format!("federated memory is not an object: {v}")))?;
        Ok(Self {
            memory_id: require_str(obj, "memory_id")?,
            content: require_str(obj, "content")?,
            source_agent: require_str(obj, "source_agent")?,
            memory_type: default_str(obj, "memory_type")?,
            importance: default_f64(obj, "importance")?,
            confidence: default_f64(obj, "confidence")?,
            tags: default_str_list(obj, "tags")?,
            agreement_count: default_i64(obj, "agreement_count")?,
            access_count: default_i64(obj, "access_count")?,
        })
    }
}

// ── Field decoding helpers (fail-loud JSON contract checks) ─────────────

fn require_str(obj: &serde_json::Map<String, Value>, key: &str) -> Result<String, MemoryError> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(MemoryError::Protocol(format!(
            "field '{key}' expected string, got {other}"
        ))),
        None => Err(MemoryError::Protocol(format!(
            "peer result missing required field '{key}'"
        ))),
    }
}

fn opt_str(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Option<String>, MemoryError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(MemoryError::Protocol(format!(
            "field '{key}' expected string or null, got {other}"
        ))),
    }
}

fn default_str(obj: &serde_json::Map<String, Value>, key: &str) -> Result<String, MemoryError> {
    Ok(opt_str(obj, key)?.unwrap_or_default())
}

fn opt_i64(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Option<i64>, MemoryError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| MemoryError::Protocol(format!("field '{key}' is not an integer: {n}"))),
        Some(other) => Err(MemoryError::Protocol(format!(
            "field '{key}' expected integer or null, got {other}"
        ))),
    }
}

fn default_i64(obj: &serde_json::Map<String, Value>, key: &str) -> Result<i64, MemoryError> {
    Ok(opt_i64(obj, key)?.unwrap_or(0))
}

fn default_f64(obj: &serde_json::Map<String, Value>, key: &str) -> Result<f64, MemoryError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(0.0),
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| {
            MemoryError::Protocol(format!("field '{key}' is not a number: {n}"))
        }),
        Some(other) => Err(MemoryError::Protocol(format!(
            "field '{key}' expected number or null, got {other}"
        ))),
    }
}

fn default_str_list(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Vec<String>, MemoryError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|it| match it {
                Value::String(s) => Ok(s.clone()),
                other => Err(MemoryError::Protocol(format!(
                    "field '{key}' expected an array of strings, got element {other}"
                ))),
            })
            .collect(),
        Some(other) => Err(MemoryError::Protocol(format!(
            "field '{key}' expected an array of strings, got {other}"
        ))),
    }
}

fn expect_object(value: Value, what: &str) -> Result<serde_json::Map<String, Value>, MemoryError> {
    match value {
        Value::Object(map) => Ok(map),
        other => Err(MemoryError::Protocol(format!(
            "{what} must be an object, got {other}"
        ))),
    }
}

// ── Seam traits (bh-rtd-owned; R10: zero Infinity types) ────────────────

/// Conversation-history half of the memory seam.
///
/// Mirrors the *shape* of `infinity_agent_core::traits::ConversationStore`
/// 0.2.0 for the hibernation-relevant subset (thread creation, existence,
/// append, bounded history load, close state) against the real BH
/// `SessionMemory` via the Python peer. Mapping deltas are documented on
/// the module.
pub trait ConversationStore: Send + Sync {
    /// Create a new conversation thread (BH session) for `agent_id` and
    /// return its summary — including the peer-minted thread id.
    ///
    /// BH sessions are server-id-minted, so this replaces Infinity's
    /// caller-chosen `ensure_root_thread(thread_id)` (see module mapping).
    fn create_thread(
        &self,
        agent_id: &str,
        colony_id: &str,
    ) -> Result<ThreadSummary, MemoryError>;

    /// Whether an exact thread (session) record exists — no creation side
    /// effects (mirrors Infinity's `thread_exists`).
    fn thread_exists(&self, thread_id: &str) -> Result<bool, MemoryError>;

    /// Append a batch of messages to the thread.
    ///
    /// Mirrors Infinity's `append_messages`; the return value is the number
    /// of messages the peer actually persisted (a short count is an
    /// error-path signal the caller can inspect, not a silent drop).
    fn append_messages(
        &self,
        thread_id: &str,
        messages: &[MemoryMessage],
    ) -> Result<usize, MemoryError>;

    /// Load history with Infinity's leading-row-count convention: both
    /// bounds count messages from the start of the thread — `start_from`
    /// skips that many leading messages, `up_to` truncates to that many
    /// leading messages (`None` = unbounded). So `Some(a), Some(b)` yields
    /// the a-th through b-1-th messages, zero-based.
    fn load_history_up_to(
        &self,
        thread_id: &str,
        start_from: Option<i64>,
        up_to: Option<i64>,
    ) -> Result<Vec<MemoryMessage>, MemoryError>;

    /// Whether the thread is closed (mirrors Infinity's `is_thread_closed`).
    fn is_thread_closed(&self, thread_id: &str) -> Result<bool, MemoryError>;

    /// Close the thread — the *hibernate* step: the session is persisted as
    /// `closed` in SQLite, a hibernation record is shared into the BH
    /// `MemoryFederation`, and a `hibernate` episode is recorded.
    ///
    /// `hibernation_note` overrides the auto-built summary when present.
    fn close_thread(
        &self,
        thread_id: &str,
        hibernation_note: Option<&str>,
    ) -> Result<ThreadSummary, MemoryError>;

    /// List thread summaries, optionally filtered by agent and/or status
    /// (`active` | `compacted` | `archived` | `closed`). This is the wake /
    /// restore discovery path after a restart.
    fn list_threads(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<ThreadSummary>, MemoryError>;
}

/// State half of the memory seam: the hibernation key-value view.
///
/// Mirrors the *concept* of `infinity_agent_core::traits::StateStore`
/// 0.2.0 (durable per-agent state behind a store trait) but exposes the
/// BH-shaped JSON KV over the existing `MemorySystem` LTM (durable SQLite
/// with FTS5). Infinity's processed-id/subscription/user-choice tables are
/// runtime-internal state with no real BH counterpart today; they are
/// deliberately absent rather than faked.
pub trait StateStore: Send + Sync {
    /// Fetch the JSON value stored under `key` (`None` = genuine miss —
    /// distinguishable from an error, never fabricated).
    fn get_state(&self, key: &str) -> Result<Option<StateEntry>, MemoryError>;

    /// Store (upsert) `value` under `key` with optional tags/importance.
    /// Backed by the LTM, so it survives peer restarts.
    fn set_state(
        &self,
        key: &str,
        value: &Value,
        tags: &[&str],
        importance: Option<f64>,
    ) -> Result<(), MemoryError>;

    /// Delete the entry under `key` (`Ok(false)` = it did not exist).
    fn delete_state(&self, key: &str) -> Result<bool, MemoryError>;
}

/// Cross-agent federation half of the memory seam — the exit criterion
/// runs through here: hibernating agents leave a record in the BH
/// `MemoryFederation`.
///
/// `MemoryFederation` is the *live* cross-agent layer (in-process by
/// design); the durable trail of a hibernation is the closed session, the
/// LTM state entries and the `hibernate` episode — all SQLite.
pub trait SharedMemoryStore: Send + Sync {
    /// Share one memory into the federation (dedup/merge is the peer's
    /// job: identical content bumps `agreement_count` instead of adding a
    /// duplicate row).
    fn share(
        &self,
        content: &str,
        source_agent: &str,
        memory_type: &str,
        importance: f64,
        confidence: f64,
        tags: &[&str],
    ) -> Result<SharedMemoryRecord, MemoryError>;

    /// Relevance-scored recall from the federation, optionally scoped to
    /// one agent.
    fn recall(
        &self,
        query: &str,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SharedMemoryRecord>, MemoryError>;
}

// ── Concrete impl: UDS protocol v1 client ───────────────────────────────

/// Wire method names served by `src/memory/rtd_memory_server.py`.
///
/// Kept in one place so the Rust client and the Python peer can be
/// reviewed against each other (the peer is the authority; drift shows up
/// as [`MemoryError::Protocol`]).
mod method {
    pub(super) const HEALTH: &str = "health";
    pub(super) const CONVERSATION_CREATE: &str = "conversation.create";
    pub(super) const CONVERSATION_EXISTS: &str = "conversation.exists";
    pub(super) const CONVERSATION_APPEND: &str = "conversation.append";
    pub(super) const CONVERSATION_GET: &str = "conversation.get";
    pub(super) const CONVERSATION_IS_CLOSED: &str = "conversation.is_closed";
    pub(super) const CONVERSATION_CLOSE: &str = "conversation.close";
    pub(super) const CONVERSATION_LIST: &str = "conversation.list";
    pub(super) const STATE_SET: &str = "state.set";
    pub(super) const STATE_GET: &str = "state.get";
    pub(super) const STATE_DELETE: &str = "state.delete";
    pub(super) const FEDERATION_SHARE: &str = "federation.share";
    pub(super) const FEDERATION_RECALL: &str = "federation.recall";
    pub(super) const MEMORY_EPISODES: &str = "memory.episodes";
}

/// [`ConversationStore`] + [`StateStore`] + [`SharedMemoryStore`] over one
/// BH bridge protocol v1 socket to `src/memory/rtd_memory_server.py`.
///
/// Thin by design: every method is one protocol-v1 request via
/// [`UdsClient`] plus fail-loud result decoding. No caching, no retries,
/// no fake-success fallbacks — a downed peer is an error, always.
#[derive(Clone, Debug)]
pub struct UdsMemoryClient {
    client: UdsClient,
}

impl UdsMemoryClient {
    /// Build a client for the peer listening on `path`.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            client: UdsClient::new(path),
        }
    }

    /// Socket path this client connects to.
    pub fn socket_path(&self) -> &Path {
        self.client.path()
    }

    /// Set an optional per-call read/write timeout (see [`UdsClient`]).
    pub fn set_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.client.set_timeout(timeout);
    }

    /// Peer health/version probe (`health`): returns the raw result object
    /// (service name, protocol version, memory stats).
    pub fn health(&self) -> Result<Value, MemoryError> {
        self.call(method::HEALTH, &json!({}))
    }

    /// Durable hibernation trail: episodic-memory records for `agent_id`
    /// (`memory.episodes` → `MemorySystem.get_episodes`). Each record is an
    /// object `{id, type, summary, outcome, timestamp}`; the peer's shape
    /// is passed through, missing array → protocol error.
    pub fn episodes(&self, agent_id: &str, limit: usize) -> Result<Vec<Value>, MemoryError> {
        let result = self.call(
            method::MEMORY_EPISODES,
            &json!({ "agent_id": agent_id, "limit": limit }),
        );
        match result? {
            Value::Object(map) => match map.get("episodes") {
                Some(Value::Array(items)) => Ok(items.clone()),
                Some(other) => Err(MemoryError::Protocol(format!(
                    "'episodes' must be an array, got {other}"
                ))),
                None => Err(MemoryError::Protocol(
                    "episodes result missing 'episodes' array".into(),
                )),
            },
            other => Err(MemoryError::Protocol(format!(
                "episodes result must be an object, got {other}"
            ))),
        }
    }

    /// One protocol-v1 request with error mapping and envelope checks.
    fn call(&self, method_name: &str, params: &Value) -> Result<Value, MemoryError> {
        self.client.call(method_name, params).map_err(MemoryError::from)
    }
}

impl ConversationStore for UdsMemoryClient {
    fn create_thread(
        &self,
        agent_id: &str,
        colony_id: &str,
    ) -> Result<ThreadSummary, MemoryError> {
        let result = self.call(
            method::CONVERSATION_CREATE,
            &json!({ "agent_id": agent_id, "colony_id": colony_id }),
        )?;
        let map = expect_object(result, "conversation.create result")?;
        let session = map
            .get("session")
            .ok_or_else(|| {
                MemoryError::Protocol("conversation.create result missing 'session'".into())
            })?;
        ThreadSummary::from_value(session)
    }

    fn thread_exists(&self, thread_id: &str) -> Result<bool, MemoryError> {
        let result = self.call(
            method::CONVERSATION_EXISTS,
            &json!({ "session_id": thread_id }),
        )?;
        let map = expect_object(result, "conversation.exists result")?;
        match map.get("exists") {
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(MemoryError::Protocol(format!(
                "'exists' must be a boolean, got {other}"
            ))),
            None => Err(MemoryError::Protocol(
                "conversation.exists result missing 'exists'".into(),
            )),
        }
    }

    fn append_messages(
        &self,
        thread_id: &str,
        messages: &[MemoryMessage],
    ) -> Result<usize, MemoryError> {
        let payload: Vec<Value> = messages.iter().map(MemoryMessage::to_params).collect();
        let result = self.call(
            method::CONVERSATION_APPEND,
            &json!({ "session_id": thread_id, "messages": payload }),
        )?;
        let map = expect_object(result, "conversation.append result")?;
        let appended = match map.get("appended") {
            Some(Value::Number(n)) => n.as_i64().ok_or_else(|| {
                MemoryError::Protocol(format!("'appended' is not an integer: {n}"))
            })?,
            Some(other) => {
                return Err(MemoryError::Protocol(format!(
                    "'appended' must be an integer, got {other}"
                )))
            }
            None => {
                return Err(MemoryError::Protocol(
                    "conversation.append result missing 'appended'".into(),
                ))
            }
        };
        // Fail-loud: the peer must report *every* message persisted.
        if appended != messages.len() as i64 {
            return Err(MemoryError::Protocol(format!(
                "peer appended {appended} of {} messages",
                messages.len()
            )));
        }
        Ok(appended as usize)
    }

    fn load_history_up_to(
        &self,
        thread_id: &str,
        start_from: Option<i64>,
        up_to: Option<i64>,
    ) -> Result<Vec<MemoryMessage>, MemoryError> {
        let result = self.call(
            method::CONVERSATION_GET,
            &json!({
                "session_id": thread_id,
                "start_from": start_from,
                "up_to": up_to,
            }),
        )?;
        let map = expect_object(result, "conversation.get result")?;
        let messages = match map.get("messages") {
            Some(Value::Array(items)) => items,
            Some(other) => {
                return Err(MemoryError::Protocol(format!(
                    "'messages' must be an array, got {other}"
                )))
            }
            None => {
                return Err(MemoryError::Protocol(
                    "conversation.get result missing 'messages'".into(),
                ))
            }
        };
        messages.iter().map(MemoryMessage::from_value).collect()
    }

    fn is_thread_closed(&self, thread_id: &str) -> Result<bool, MemoryError> {
        let result = self.call(
            method::CONVERSATION_IS_CLOSED,
            &json!({ "session_id": thread_id }),
        )?;
        let map = expect_object(result, "conversation.is_closed result")?;
        match map.get("closed") {
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(MemoryError::Protocol(format!(
                "'closed' must be a boolean, got {other}"
            ))),
            None => Err(MemoryError::Protocol(
                "conversation.is_closed result missing 'closed'".into(),
            )),
        }
    }

    fn close_thread(
        &self,
        thread_id: &str,
        hibernation_note: Option<&str>,
    ) -> Result<ThreadSummary, MemoryError> {
        let result = self.call(
            method::CONVERSATION_CLOSE,
            &json!({
                "session_id": thread_id,
                "note": hibernation_note.unwrap_or(""),
            }),
        )?;
        let map = expect_object(result, "conversation.close result")?;
        let session = map
            .get("session")
            .ok_or_else(|| {
                MemoryError::Protocol("conversation.close result missing 'session'".into())
            })?;
        ThreadSummary::from_value(session)
    }

    fn list_threads(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<ThreadSummary>, MemoryError> {
        let result = self.call(
            method::CONVERSATION_LIST,
            &json!({
                "agent_id": agent_id.unwrap_or(""),
                "status": status.unwrap_or(""),
            }),
        )?;
        let map = expect_object(result, "conversation.list result")?;
        let sessions = match map.get("sessions") {
            Some(Value::Array(items)) => items,
            Some(other) => {
                return Err(MemoryError::Protocol(format!(
                    "'sessions' must be an array, got {other}"
                )))
            }
            None => {
                return Err(MemoryError::Protocol(
                    "conversation.list result missing 'sessions'".into(),
                ))
            }
        };
        sessions.iter().map(ThreadSummary::from_value).collect()
    }
}

impl StateStore for UdsMemoryClient {
    fn get_state(&self, key: &str) -> Result<Option<StateEntry>, MemoryError> {
        let result = self.call(method::STATE_GET, &json!({ "key": key }))?;
        let map = expect_object(result, "state.get result")?;
        match map.get("found") {
            Some(Value::Bool(true)) => {
                let entry = map
                    .get("entry")
                    .ok_or_else(|| {
                        MemoryError::Protocol("state.get found:true missing 'entry'".into())
                    })?;
                StateEntry::from_value(key, entry).map(Some)
            }
            Some(Value::Bool(false)) => {
                // A genuine miss must NOT carry a fabricated entry.
                if map.get("entry").is_some() {
                    return Err(MemoryError::Protocol(
                        "state.get found:true/false drift: 'entry' present on found:false".into(),
                    ));
                }
                Ok(None)
            }
            Some(other) => Err(MemoryError::Protocol(format!(
                "'found' must be a boolean, got {other}"
            ))),
            None => Err(MemoryError::Protocol(
                "state.get result missing 'found'".into(),
            )),
        }
    }

    fn set_state(
        &self,
        key: &str,
        value: &Value,
        tags: &[&str],
        importance: Option<f64>,
    ) -> Result<(), MemoryError> {
        let result = self.call(
            method::STATE_SET,
            &json!({
                "key": key,
                "value": value,
                "tags": tags,
                "importance": importance,
            }),
        )?;
        let map = expect_object(result, "state.set result")?;
        match map.get("stored") {
            Some(Value::Bool(true)) => Ok(()),
            Some(other) => Err(MemoryError::Protocol(format!(
                "'stored' must be true, got {other}"
            ))),
            None => Err(MemoryError::Protocol(
                "state.set result missing 'stored'".into(),
            )),
        }
    }

    fn delete_state(&self, key: &str) -> Result<bool, MemoryError> {
        let result = self.call(method::STATE_DELETE, &json!({ "key": key }))?;
        let map = expect_object(result, "state.delete result")?;
        match map.get("deleted") {
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(MemoryError::Protocol(format!(
                "'deleted' must be a boolean, got {other}"
            ))),
            None => Err(MemoryError::Protocol(
                "state.delete result missing 'deleted'".into(),
            )),
        }
    }
}

impl SharedMemoryStore for UdsMemoryClient {
    fn share(
        &self,
        content: &str,
        source_agent: &str,
        memory_type: &str,
        importance: f64,
        confidence: f64,
        tags: &[&str],
    ) -> Result<SharedMemoryRecord, MemoryError> {
        let result = self.call(
            method::FEDERATION_SHARE,
            &json!({
                "content": content,
                "source_agent": source_agent,
                "memory_type": memory_type,
                "importance": importance,
                "confidence": confidence,
                "tags": tags,
            }),
        )?;
        let map = expect_object(result, "federation.share result")?;
        let memory = map
            .get("memory")
            .ok_or_else(|| {
                MemoryError::Protocol("federation.share result missing 'memory'".into())
            })?;
        SharedMemoryRecord::from_value(memory)
    }

    fn recall(
        &self,
        query: &str,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SharedMemoryRecord>, MemoryError> {
        let result = self.call(
            method::FEDERATION_RECALL,
            &json!({
                "query": query,
                "agent_id": agent_id.unwrap_or(""),
                "limit": limit,
            }),
        )?;
        let map = expect_object(result, "federation.recall result")?;
        let memories = match map.get("memories") {
            Some(Value::Array(items)) => items,
            Some(other) => {
                return Err(MemoryError::Protocol(format!(
                    "'memories' must be an array, got {other}"
                )))
            }
            None => {
                return Err(MemoryError::Protocol(
                    "federation.recall result missing 'memories'".into(),
                ))
            }
        };
        memories.iter().map(SharedMemoryRecord::from_value).collect()
    }
}
