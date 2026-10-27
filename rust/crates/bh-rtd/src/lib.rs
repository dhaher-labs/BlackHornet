//! `bh-rtd` — BlackHornet Rust runtime daemon (Phase 0 scaffold).
//!
//! Phase 0 of the Rust + Infinity roadmap (PLAN.md §4) is "bridges before
//! ports": this crate only fixes the *contracts* the Rust side will honour.
//! It deliberately contains almost no behaviour — nothing here replaces or
//! wraps any Python subsystem, and [`RuntimeFlavor::Python`] is the default
//! until a real wedge lands (Phase 2).
//!
//! What exists today:
//! * [`host`] — the [`AgentHost`](host::AgentHost) contract for colony member
//!   agents (`spawn_agent` / `send_event` / `shutdown`), plus a fail-loud
//!   [`ColdHost`](host::ColdHost) placeholder that refuses to pretend to work.
//! * [`RuntimeFlavor`] — the `BH_RUNTIME` env gate mirroring PLAN.md's
//!   `BH_RUNTIME=python|rust` flag (`python` is the default).
//! * `infinity_adapter` (feature `infinity`, on by default) — a minimal
//!   integration stub over the **verified** `infinity-agent-core` 0.2.0 API,
//!   mapping BlackHornet concepts onto Infinity primitives per the Task 12-c
//!   researcher mapping.
//!
//! What does NOT exist yet (honest list): the daemon binary, agent thread
//! execution, MCP bridging, memory-seam persistence, RAP tool servers,
//! watchdog — i.e. Phases 0–5 deliverables beyond this scaffold. See
//! `rust/README.md`.
//!
//! Rust + Infinity roadmap posture (PLAN.md §4): *adopt the protocol, embed
//! the runtime, never port the colony.* The trading money-path is excluded
//! from migration until Phase 5+.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod host;

#[cfg(feature = "infinity")]
pub mod infinity_adapter;

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
