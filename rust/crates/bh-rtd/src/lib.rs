//! `bh-rtd` — BlackHornet Rust runtime daemon (Phase 0 contracts + Phase 1
//! bridges + Phase 2 runtime wedge).
//!
//! The Rust + Infinity roadmap (PLAN.md §4) proceeds by strangler-fig:
//! *"adopt the protocol, embed the runtime, never port the colony."* The
//! Python BlackHornet stays fully working at every phase;
//! [`RuntimeFlavor::Python`] remains the default.
//!
//! What exists now:
//! * [`host`] — the [`AgentHost`](host::AgentHost) contract for colony
//!   member agents (`spawn_agent` / `send_event` / `shutdown`), plus the
//!   Phase 0 fail-loud [`ColdHost`](host::ColdHost) placeholder (kept as the
//!   honest no-backend reference).
//! * [`RuntimeFlavor`] — the `BH_RUNTIME` env gate mirroring PLAN.md's
//!   `BH_RUNTIME=python|rust` flag (`python` is the default).
//! * `uds` (feature `bridge`, implied by `infinity`) — the BH bridge
//!   protocol v1 client, re-exported from the sibling `bh-bridge-uds`
//!   crate (the transport is shared with `bh-mcp-bridge`): one JSON value
//!   per line over a UDS, one request per connection,
//!   `{id, method, params}` → `{id, ok, result|error}`. The Phase 2 daemon
//!   serves the same envelope server-side.
//! * `timers` (feature `bridge`) — the daemon's **durable timer wheel**: a
//!   minimal bh-rtd-owned implementation behind the
//!   [`timers::TimerStore`] trait (Infinity 0.2.0 ships only in-process
//!   timers and explicitly defers durable timers to the embedding platform —
//!   R10 isolation: no Infinity types here, swappable persistence backend).
//! * `infinity_adapter` (feature `infinity`) — a minimal integration stub
//!   over the **verified** `infinity-agent-core` 0.2.0 API, mapping
//!   BlackHornet concepts onto Infinity primitives per the Task 12-c
//!   researcher mapping.
//! * `infinity_host` (feature `infinity`) — [`InfinityHost`], the Phase 2
//!   [`AgentHost`](host::AgentHost) implementation embedding real
//!   `infinity-agent-core` threads: a dedicated runtime-actor thread (the
//!   Infinity local system is non-`Send`), a bounded sub-agent pool
//!   (`spawn_thread` ≈ the colony's sub_agent pool), clean shutdown,
//!   fail-loud throughout.
//! * `daemon` (feature `infinity`) — the runtime wedge server: a UDS
//!   listener speaking BH bridge protocol v1 with methods `health`,
//!   `spawn_thread`, `send_event`, `shutdown_thread`, `list_threads`,
//!   `schedule_timer`, `cancel_timer`, `list_timers`, `shutdown`; durable
//!   timers persist to a JSON journal that survives restarts. The `bh-rtd`
//!   binary (this crate's `src/bin/bh-rtd.rs`) is a thin env-configured
//!   wrapper around [`daemon::run_daemon`] that also wires the
//!   `bh-mcp-bridge` tools (`BH_RTD_TOOLS_SOCKET`) and traps
//!   `SIGTERM`/`SIGINT` into the same graceful stop. Python reaches it
//!   through the `BH_RUNTIME=rust` gate in `src/runtime_gate.py` (fail-loud
//!   when the daemon is unreachable — never a silent fallback).
//! * `jeumpa` (feature `infinity`) — [`jeumpa::JeumpaProvider`], an Infinity
//!   [`ModelProvider`] that bridges to the Python Jeumpa Brain
//!   (`src/colony/jeumpa_uds_server.py`, shipped): task profiles as models,
//!   cooldowns as provider error classes, no provider logic re-implemented
//!   in Rust. The daemon wires it as its brain when `BH_RTD_BRAIN_SOCKET`
//!   is set.
//! * `memory` (feature `bridge`) — the Phase 3 **memory seam**:
//!   Infinity-shaped [`memory::ConversationStore`] /
//!   [`memory::StateStore`] / [`memory::SharedMemoryStore`] contracts
//!   (bh-rtd-owned, zero Infinity types — R10) implemented by
//!   [`memory::UdsMemoryClient`] against the Python peer
//!   `src/memory/rtd_memory_server.py`, which wraps the existing
//!   `src/memory/` stack (SessionMemory SQLite, MemorySystem LTM
//!   SQLite+FTS5, MemoryFederation, episodic memory). Hibernating agents
//!   persist their state + conversation into BH memory and read it back —
//!   never touching SQLite from Rust.
//!
//! The MCP side of Phase 1 is the sibling crate `bh-mcp-bridge` (BH tool
//! catalog + tools client + Infinity `Tool` impl over `src/agent_tools/`);
//! the Phase 2 daemon binary links it (feature `tools`) to give member-agent
//! threads the colony's real Python tools.
//!
//! Rust + Infinity roadmap posture (PLAN.md §4): *adopt the protocol, embed
//! the runtime, never port the colony.* The trading money-path is excluded
//! from migration until Phase 5+.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod host;

#[cfg(feature = "bridge")]
pub mod timers;

#[cfg(feature = "bridge")]
pub mod memory;

#[cfg(feature = "bridge")]
pub use bh_bridge_uds as uds;

#[cfg(feature = "infinity")]
pub mod daemon;

#[cfg(feature = "infinity")]
pub mod infinity_adapter;

#[cfg(feature = "infinity")]
pub mod infinity_host;

#[cfg(feature = "infinity")]
pub mod jeumpa;

/// Runtime selector driven by the `BH_RUNTIME` environment variable.
///
/// Mirrors the PLAN.md §4 contract: every wedge is gated behind
/// `BH_RUNTIME=python|rust` and **`BH_RUNTIME=python` is the default**.
/// The Python runtime stays fully working at every phase; flipping to
/// `rust` before Phase 2 deliverables land is not meaningful yet, and this
/// crate does not pretend otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeFlavor {
    /// The Python BlackHornet runtime (the default; always available).
    Python,
    /// The Rust runtime wedge (`bh-rtd`). Phase 0: contract only, no daemon.
    Rust,
}

impl RuntimeFlavor {
    /// Environment variable consulted by [`RuntimeFlavor::from_env`].
    pub const ENV_VAR: &'static str = "BH_RUNTIME";

    /// Read `BH_RUNTIME` from the environment.
    ///
    /// * unset / empty → [`RuntimeFlavor::Python`] (the documented default)
    /// * `"python"` / `"rust"` (case-insensitive, trimmed) → that flavor
    /// * anything else → error (fail-closed: the repo bans silently
    ///   "falling back to something plausible" on bad input)
    pub fn from_env() -> Result<RuntimeFlavor, RuntimeError> {
        match std::env::var(Self::ENV_VAR) {
            Ok(raw) if raw.trim().is_empty() => Ok(RuntimeFlavor::Python),
            Ok(raw) => Self::parse(&raw),
            Err(std::env::VarError::NotPresent) => Ok(RuntimeFlavor::Python),
            Err(e) => Err(RuntimeError::Env(e.to_string())),
        }
    }

    /// Parse a `BH_RUNTIME` value. Same rules as [`RuntimeFlavor::from_env`].
    pub fn parse(raw: &str) -> Result<RuntimeFlavor, RuntimeError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "python" => Ok(RuntimeFlavor::Python),
            "rust" => Ok(RuntimeFlavor::Rust),
            other => Err(RuntimeError::UnknownRuntime(other.to_string())),
        }
    }
}

impl Default for RuntimeFlavor {
    /// `BH_RUNTIME=python` is the documented default (PLAN.md §4).
    fn default() -> Self {
        RuntimeFlavor::Python
    }
}

/// Errors from the [`RuntimeFlavor`] env gate. Fail-closed on purpose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// The `BH_RUNTIME` variable existed but held an unrecognised value.
    UnknownRuntime(String),
    /// The environment variable could not be read (e.g. non-UTF-8).
    Env(String),
}

impl core::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RuntimeError::UnknownRuntime(v) => write!(
                f,
                "unsupported BH_RUNTIME value {v:?} (expected \"python\" or \"rust\")"
            ),
            RuntimeError::Env(e) => write!(f, "failed to read BH_RUNTIME: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
