//! The [`AgentHost`] contract — how BlackHornet colony member agents will be
//! driven from Rust.
//!
//! Phase 0 scope: the trait and its data types only. There is **no**
//! implementation that actually spawns anything yet (the shipped
//! [`ColdHost`] fails loud on every call by design). The Phase 2 runtime
//! wedge will provide a real host backed by `infinity-agent-core` threads;
//! the Python side keeps running the colony in the meantime.
//!
//! Naming is aligned with BlackHornet concepts: the unit of work is a
//! *colony member agent* (what `src/colony` orchestrates today), events are
//! the existing colony event flow (`EcosystemOrchestrator` / `MeshBus`
//! vocabulary), not Infinity's internals — the adapter layer translates.

/// Opaque identity of a colony member agent inside a host.
///
/// Phase 0 uses the colony's existing string identifiers; the Phase 2
/// adapter maps these onto `infinity_agent_core::ThreadId` when agents run
/// as Infinity conversation threads.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AgentId(pub String);

/// Declarative description of a colony member agent to spawn.
///
/// Deliberately schema-light: the full message/tool/memory adapter contract
/// is a Phase 0 deliverable (`PLAN.md` §4, "adapter contract doc") and will
/// supersede this placeholder shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSpec {
    /// Colony-visible name (e.g. `"queen"`, `"radar-worker-1"`).
    pub name: String,
    /// Colony role (BlackHornet role vocabulary, e.g. `"queen"`, `"worker"`).
    pub role: String,
    /// System instructions for the agent's LLM turns.
    pub instructions: String,
}

/// Handle returned by a successful [`AgentHost::spawn_agent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHandle {
    /// The host-assigned identity; use it to address later events.
    pub id: AgentId,
}

/// A colony event delivered to a member agent.
///
/// `payload_json` is a wire-format placeholder: Phase 0's adapter-contract
/// doc defines the real message/tool/memory schemas before anything reads
/// this field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColonyEvent {
    /// Coarse event category (colony vocabulary).
    pub kind: ColonyEventKind,
    /// JSON payload (schema intentionally unspecified until the Phase 0
    /// adapter contract doc lands).
    pub payload_json: String,
}

/// Coarse colony event categories (BlackHornet vocabulary).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColonyEventKind {
    /// A task was assigned to the member agent (orchestrator → agent).
    TaskAssigned,
    /// A peer message arrived (MeshBus / radar vocabulary).
    MessageReceived,
    /// Periodic health/heartbeat tick (SelfNest vocabulary).
    HealthTick,
    /// Anything else; `payload_json` carries the discriminator.
    Custom,
}

/// Errors returned by host operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// The host backend does not support this operation yet (Phase 0).
    Unsupported(&'static str),
    /// No member agent with the given id is alive in this host.
    UnknownAgent(String),
    /// The host is shutting down or already shut down.
    Shutdown(String),
    /// The bounded sub-agent pool is full (Phase 2: `max_threads` reached).
    Capacity {
        /// The configured pool limit that was exceeded.
        limit: usize,
    },
    /// A member agent with this colony name already exists in the host.
    DuplicateAgent(String),
    /// The host backend could not start (configuration, provider, runtime).
    Startup(String),
}

impl core::fmt::Display for HostError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HostError::Unsupported(why) => write!(f, "unsupported operation: {why}"),
            HostError::UnknownAgent(id) => write!(f, "unknown agent id {id:?}"),
            HostError::Shutdown(why) => write!(f, "host shutdown: {why}"),
            HostError::Capacity { limit } => {
                write!(f, "host at capacity: bounded pool limit {limit} reached")
            }
            HostError::DuplicateAgent(name) => {
                write!(f, "agent name already in use: {name:?}")
            }
            HostError::Startup(reason) => write!(f, "host failed to start: {reason}"),
        }
    }
}

impl std::error::Error for HostError {}

/// Contract for a host that runs BlackHornet colony member agents.
///
/// Synchronous by design in Phase 0 (object-safe, runtime-free). The Phase 2
/// infinity-backed implementation will be async internally (`tokio` via
/// `infinity-agent-core`) — that change is additive behind this same trait.
pub trait AgentHost {
    /// Spawn a colony member agent from `spec`.
    fn spawn_agent(&mut self, spec: AgentSpec) -> Result<AgentHandle, HostError>;

    /// Deliver `event` to the member agent identified by `agent`.
    fn send_event(&mut self, agent: &AgentId, event: ColonyEvent) -> Result<(), HostError>;

    /// Shut the host down: stop member agents, release resources. Idempotent
    /// implementations must still report a clean result on repeat calls.
    fn shutdown(self) -> Result<(), HostError>;
}

/// Fail-loud Phase 0 placeholder host.
///
/// Every operation returns [`HostError::Unsupported`]. This is the honest
/// inverse of the "fake success" pattern the repo bans (PLAN.md F8): the
/// scaffold refuses to simulate agent behaviour until a real backend exists.
#[derive(Clone, Copy, Debug, Default)]
pub struct ColdHost;

impl AgentHost for ColdHost {
    fn spawn_agent(&mut self, _spec: AgentSpec) -> Result<AgentHandle, HostError> {
        Err(HostError::Unsupported(
            "bh-rtd Phase 0 scaffold: no agent host backend is wired yet \
             (Infinity thread execution lands in Phase 2 — see rust/README.md)",
        ))
    }

    fn send_event(&mut self, agent: &AgentId, _event: ColonyEvent) -> Result<(), HostError> {
        Err(HostError::UnknownAgent(format!(
            "{:?} — ColdHost never spawns agents (Phase 0 scaffold)",
            agent.0
        )))
    }

    fn shutdown(self) -> Result<(), HostError> {
        Ok(()) // nothing was ever started; a no-op shutdown is truthful, not fake
    }
}
