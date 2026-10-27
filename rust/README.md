# bh-rtd — BlackHornet Rust runtime workspace

Phase 0 scaffold of the Rust + Infinity strangler-fig roadmap (PLAN.md §4).
Posture: **adopt the protocol, embed the runtime, never port the colony.**
The Python BlackHornet stays fully working at every phase; nothing in this
workspace replaces, wraps, or calls any Python subsystem yet.

## Phase 0 scope (what IS here)

- **`crates/bh-rtd/src/host.rs`** — the [`AgentHost`] contract for colony
  member agents: `spawn_agent` / `send_event` / `shutdown`, plus supporting
  types (`AgentSpec`, `AgentHandle`, `ColonyEvent`, `HostError`) and a
  fail-loud `ColdHost` placeholder that refuses to simulate behaviour
  (the repo bans fake success).
- **`crates/bh-rtd/src/lib.rs`** — the `BH_RUNTIME` gate: `RuntimeFlavor`
  enum `{ Python, Rust }`, env-driven, **defaulting to `Python`**, fail-closed
  on unknown values.
- **`crates/bh-rtd/src/infinity_adapter.rs`** (feature `infinity`, on by
  default) — minimal integration stub over the **verified** Infinity 0.2.0
  API (`ConversationStore`, `StateStore`, `ThreadId`,
  `InMemoryConversationStore`, `InMemoryStateStore` from
  `infinity-agent-core`; `ModelProvider` documented from
  `infinity-provider-protocol`). It constructs real Infinity types to prove
  the pinned dependency links; it persists nothing.
- **`crates/bh-rtd/tests/smoke.rs`** — hermetic smoke tests (5, no network /
  no Infinity) + infinity-gated tests (3 more) that round-trip the real
  stores.

## Phase 0 scope (what is NOT here yet)

Phases 1–5 of PLAN.md §4 are **not done**. Explicitly missing:

| Phase | Missing deliverable |
|---|---|
| 0 | adapter contract doc (message/tool/memory schemas) + `src/bridge/infinity/` RAP round-trip |
| 1 | `infinity-mcp-bridge` wired to BH `agent_tools`; `JeumpaProvider` implementing `ModelProvider` |
| 2 | the actual `bh-rtd` daemon embedding `infinity-agent-core` (threads, sub-agent pool, timers) |
| 3 | Infinity `ConversationStore`/`StateStore` backed by BH SQLite FTS5 `MemoryFederation` |
| 4 | native Rust RAP tool servers (shell/file/search) |
| 5 | watchdog/heartbeat via `sleep_until_event_or_input`; UI integration |

The trading money-path is excluded from migration until Phase 5+
(PLAN.md §4). SAMAN, swarm intelligence, Omega, vector/KG memory and the
trading stack stay Python (researcher verdict, Task 12-c).

## Dependency route (decided)

The Infinity runtime **is published on crates.io** — verified via the
crates.io API:

- `infinity-agent-core =0.2.0` (hydro-project/infinity, published 2026-10-22)
- transitive: `rap-protocol 0.2.0`, `rap-client 0.2.0`,
  `infinity-provider-protocol 0.2.0` (all same release batch)

So we use the **crates.io route pinned exactly** (`=0.2.0`), not a git
dependency. The pin guards against pre-1.0 upstream churn (risk R10). The
`infinity-mcp-bridge` crate is also published at `0.2.0` and is earmarked for
Phase 1 (not depended on yet).

The dependency is **feature-gated** (`feature = "infinity"`, on by default):
`--no-default-features` builds a hermetic std-only workspace (no network, no
Infinity). The verified API surface was cross-checked against the vendored
0.2.0 crate sources (not just the repo's `main` branch, which may drift).

## Build instructions

Requires Rust >= 1.85 (edition 2024). Toolchain used: 1.99.1.

```bash
cd rust/

# hermetic gate (no Infinity, no network after vendoring)
cargo build --no-default-features
cargo test  --no-default-features      # 5 passed / 0 failed

# full build (Infinity 0.2.0 compiled in; ~2-6 min first time; pulls
# tokio/reqwest/rustls/aws-lc-sys/rhai through infinity-agent-core)
cargo build
cargo test                              # 8 passed / 0 failed
```

Note: `tokio` is a **dev-dependency only**. The verified Infinity trait API
is async (`#[async_trait]`), so the adapter exposes async methods, but Phase
0 ships no executor; Phase 2 brings the real one via `infinity-agent-core`.

## `BH_RUNTIME` contract

| Value | Meaning | Phase 0 behaviour |
|---|---|---|
| unset / empty | default | `RuntimeFlavor::Python` — Python stays the runtime |
| `python` | Python runtime | same as default |
| `rust` | Rust wedge | selected, but no daemon exists yet (Phase 2) |
| anything else | error | `RuntimeError::UnknownRuntime` (fail-closed, never silently "falls back") |

The values mirror PLAN.md's `BH_RUNTIME=python|rust` flag; the environment
variable is read by `RuntimeFlavor::from_env()`.
