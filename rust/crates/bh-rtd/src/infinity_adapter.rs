//! Minimal integration stub over the **verified** Infinity 0.2.0 API.
//!
//! Every item referenced here was checked against the published
//! `infinity-agent-core` / `infinity-provider-protocol` 0.2.0 sources
//! (crates.io, 2026-10-22 batch release of hydro-project/infinity):
//!
//! * `infinity_agent_core::ThreadId` — re-export of `rap_protocol::ThreadId`
//!   (the wire-level `group_id`).
//! * `infinity_agent_core::traits::{ConversationStore, StateStore,
//!   SpawnContext}` — `ConversationStore: Send + Sync + Clone` with an
//!   associated `Error` type; `StateStore: Send + Sync + Clone` for processed
//!   ids / metadata / subscriptions / pending user choices.
//! * `infinity_agent_core::stores::{InMemoryConversationStore,
//!   InMemoryStateStore}` — in-memory implementations of those traits.
//! * `infinity_provider_protocol::ModelProvider` — the provider trait
//!   (`Send + Sync`) that Phase 1's `JeumpaProvider` will implement.
//!
//! Researcher mapping honoured (worklog Task 12-c, PLAN.md §4):
//!
//! | BlackHornet concept | Infinity primitive | Phase |
//! |---------------------|--------------------|-------|
//! | `src/colony/jeumpa_brain.py` (keyless LLM layer) | `ModelProvider` (`infinity-provider-protocol`) | 1 |
//! | `src/agent_tools/` + MCP servers | `infinity-mcp-bridge` crate (zero-rewrite MCP proxy) | 1 |
//! | `MemoryFederation` (SQLite FTS5) | `ConversationStore` / `StateStore` traits backed by BH memory | 3 |
//! | colony member agent threads | `ThreadId` threads + `SpawnContext` (sub-agent pool, Phase 2) | 2 |
//!
//! This module is a *skeleton*: it constructs real Infinity types to prove
//! the pinned dependency links and the trait seams line up, but persists
//! nothing and touches no Python. TODO markers below mark exactly what the
//! later phases must implement.

/// Concrete Infinity in-memory store implementations (re-exported so the
/// seam's public signatures are nameable without a direct infinity dep).
pub use infinity_agent_core::stores::{InMemoryConversationStore, InMemoryStateStore};

/// The Infinity store traits (re-exported: callers implementing or calling
/// them need the trait in scope).
pub use infinity_agent_core::traits::{ConversationStore, StateStore};

/// The Infinity thread identifier (re-exported from `infinity-agent-core`,
/// originally `rap_protocol::ThreadId`) so downstream code can address
/// colony-member threads without depending on infinity crates directly.
pub use infinity_agent_core::ThreadId;

/// Errors from the Phase 0 adapter stub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    /// A store operation failed; Phase 0 only surfaces the message.
    Store(String),
}

impl core::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AdapterError::Store(m) => write!(f, "infinity store error: {m}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Phase 0 proof-of-linkage: wires the two real Infinity in-memory stores
/// together under BlackHornet naming.
///
/// TODO(Phase 3): replace the `InMemory*` stores with implementations of
/// `ConversationStore` / `StateStore` backed by BH `MemoryFederation`
/// (SQLite FTS5; ChromaDB read-through) per PLAN.md §4.
pub struct BhMemorySeam {
    conversation: InMemoryConversationStore,
    state: InMemoryStateStore,
}

impl BhMemorySeam {
    /// Build the in-memory seam (the only backend Phase 0 ships).
    pub fn in_memory() -> Self {
        Self {
            conversation: InMemoryConversationStore::new(),
            state: InMemoryStateStore::new(),
        }
    }

    /// Register a root thread for a colony member agent and report whether
    /// the thread record existed before.
    ///
    /// Async because the verified `ConversationStore` trait methods are
    /// `#[async_trait]`; Phase 0 ships no executor — callers (and the smoke
    /// tests) bring their own (`tokio` is a dev-dependency only).
    ///
    /// TODO(Phase 2): `spawn_agent` will call this when colony member agents
    /// become Infinity threads; TODO(Phase 3): back it with MemoryFederation.
    pub async fn ensure_colony_thread(&self, colony_member: &str) -> Result<bool, AdapterError> {
        let thread_id = ThreadId::from(colony_member.to_owned());
        let existed = self
            .conversation
            .thread_exists(&thread_id)
            .await
            .map_err(|e| AdapterError::Store(e.to_string()))?;
        if !existed {
            self.conversation
                .ensure_root_thread(&thread_id)
                .await
                .map_err(|e| AdapterError::Store(e.to_string()))?;
        }
        Ok(existed)
    }

    /// Shared access to the conversation store (for tests and Phase 2 wiring).
    pub fn conversation_store(&self) -> &InMemoryConversationStore {
        &self.conversation
    }

    /// Shared access to the state store.
    pub fn state_store(&self) -> &InMemoryStateStore {
        &self.state
    }

    /// Record message ids already handled for a colony member agent
    /// (deduplication seed — the BH ecosystem already keys on message ids).
    ///
    /// TODO(Phase 3): persist through MemoryFederation-backed `StateStore`.
    pub async fn mark_events_processed(
        &self,
        colony_member: &str,
        message_ids: Vec<String>,
    ) -> Result<(), AdapterError> {
        let thread_id = ThreadId::from(colony_member.to_owned());
        self.state
            .add_processed_message_ids(&thread_id, message_ids)
            .await
            .map_err(|e| AdapterError::Store(e.to_string()))
    }
}

/// Reminder seam for Phase 1 (no behaviour yet, by design):
///
/// `JeumpaProvider` must implement `infinity_provider_protocol::ModelProvider`
/// (a `Send + Sync` trait) over `src/colony/jeumpa_brain.py` semantics:
/// task-profile routing expressed as provider/model selection, cooldowns as
/// provider error classes, response caching kept BH-side.
///
/// TODO(Phase 1): implement the trait once the UDS/remote provider decision
/// (PLAN.md §4 Phase 1: "UDS provider may stay Python") is taken; the MCP
/// side of Phase 1 is NOT this crate — it is the `infinity-mcp-bridge`
/// crate (also published at 0.2.0) pointed at BH `agent_tools` MCP servers.
#[allow(dead_code)]
pub const PHASE_1_SEAM: &str =
    "Jeumpa Brain -> infinity_provider_protocol::ModelProvider; \
     agent_tools -> infinity-mcp-bridge (Phase 1)";
