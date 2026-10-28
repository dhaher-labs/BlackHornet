# bh-rtd — BlackHornet Rust runtime workspace

Rust side of the Rust + Infinity strangler-fig roadmap (PLAN.md §4).
Posture: **adopt the protocol, embed the runtime, never port the colony.**
The Python BlackHornet stays fully working at every phase; `BH_RUNTIME=python`
remains the default, and nothing in this workspace replaces, wraps, or calls
any Python subsystem except through the documented UDS bridge protocol.

## Phase 1 status (what IS here now)

Phase 1 deliverable — "bridges before ports: `infinity-mcp-bridge` before BH
MCP servers + `JeumpaProvider` implementing `ModelProvider`" — is complete on
the Rust side, and its exit criterion ("an Infinity agent with 100% BH brain
+ tools") is exercised **fully offline** by
`crates/bh-mcp-bridge/tests/agent_wiring.rs`: a real `infinity-agent-core`
0.2.0 local agent system with `JeumpaProvider` as the brain and the BH tool
catalog as its tools performs a complete tool-calling turn (declare →
`BH_TOOL_CALL` reply → `StreamChunk::ToolCall` → sync tool execution over the
bridge → result looped back → final answer).

- **`crates/bh-rtd/src/host.rs`** — the [`AgentHost`] contract for colony
  member agents (`spawn_agent` / `send_event` / `shutdown`), plus a fail-loud
  `ColdHost` placeholder (the repo bans fake success). Phase 2 brings the
  real daemon behind it.
- **`crates/bh-rtd/src/lib.rs`** — the `BH_RUNTIME` gate: `RuntimeFlavor`
  enum `{ Python, Rust }`, env-driven, **defaulting to `Python`**, fail-closed
  on unknown values.
- **`crates/bh-rtd/src/uds.rs`** (feature `bridge`) — BH bridge protocol v1
  client: UTF-8 JSON, one value per `\n`-terminated line over a Unix domain
  socket, **one request per connection**; `{id, method, params}` →
  `{id, ok, result|error}` with JSON-RPC-flavoured error codes
  (-32700/-32602/-32601/-32603). `ok: false` is reserved for bridge/protocol
  failures; business failures travel as `ok: true` with the failure inside
  `result`.
- **`crates/bh-rtd/src/infinity_adapter.rs`** (feature `infinity`) — memory
  seam over the verified Infinity 0.2.0 API (`ConversationStore`,
  `StateStore`, `ThreadId`, `InMemory*Store`); constructs real Infinity
  types, persists nothing (Phase 3 replaces the in-memory stores with BH
  MemoryFederation-backed implementations).
- **`crates/bh-rtd/src/jeumpa.rs`** (feature `infinity`) —
  [`JeumpaProvider`]: the real
  `infinity_provider_protocol::ModelProvider` (`list_models` +
  `invoke_model` bridged to `JeumpaBrain.chat_messages` via the Python peer
  `src/colony/jeumpa_uds_server.py`). One model per task profile; business
  failures arrive with a server-declared `error_class` mapped onto
  Infinity's `ErrorClass` (unknown/missing → `Fatal`, never guessed).
  Tool definitions are **never dropped**: they are forwarded to the peer in
  the `complete` params and declared to the text-only model in the system
  message; a conforming `BH_TOOL_CALL <name> <json-args>` reply is
  translated into `StreamChunk::ToolCall` so the Infinity runtime executes
  it and loops the result back. Malformed or partial uses of the prefix pass
  through as plain text — nothing is fabricated.
- **`crates/bh-mcp-bridge`** — the Phase 1 `infinity-mcp-bridge` deliverable
  (local crate name; the upstream `infinity-mcp-bridge =0.2.0` crate is a
  different artifact):
  - `catalog` — the BH tool catalog mirrored from `src/agent_tools/`
    (`file` ← `file_tool.py#FileTool`, `shell.execute` ← `shell.py#ShellTool`,
    `code.operations` ← `code.py#CodeTool`) with exact JSON schemas,
    categories, autonomy levels, per-entry provenance, and
    `validate_peer_manifest()` drift checking.
  - `BhToolsClient` — `tools/list` / `tools/call` over BH bridge protocol v1;
    business failures surface as `BhToolsError::ToolFailed`, never masked.
  - `tool_bridge` (feature `infinity`) — `BhBridgeTool`, an
    `infinity_agent_core::tools::Tool` implementation (sync path via
    `execute_synchronous`; failures become `Error:`-prefixed tool results the
    model can act on, mirroring the RAP error policy).
- **Tests** — all hermetic (Rust mock UDS peers; no Python process, no
  network, no secrets): `bh-rtd/tests/smoke.rs` (5 + 3 infinity-gated),
  `bh-rtd/tests/uds.rs` (4), `bh-rtd/tests/jeumpa.rs` (10),
  `bh-mcp-bridge/tests/mcp_bridge.rs` (5),
  `bh-mcp-bridge/tests/agent_wiring.rs` (3), plus catalog and tool-bridge
  unit tests.

## Peer status (honest)

| Peer | Status |
|---|---|
| `src/colony/jeumpa_uds_server.py` (brain: `health`/`models`/`complete`) | SHIPPED (additive; wraps the real `JeumpaBrain`, `0600` socket) |
| `src/agent_tools/uds_server.py` (tools: `health`/`tools/list`/`tools/call`) | PLANNED, not yet written — Phase 1's own rule is that the bridge comes *before* the BH MCP servers. The contract it must serve is pinned by `BhToolsClient` + `ToolManifest` + `validate_peer_manifest` and the mock-peer tests. |

## What is NOT here yet (Phases 2-5 of PLAN.md §4)

| Phase | Missing deliverable |
|---|---|
| 0 (rest) | adapter contract doc + `src/bridge/infinity/` RAP round-trip |
| 2 | the actual `bh-rtd` daemon embedding `infinity-agent-core` (threads, sub-agent pool, timers) behind `ColdHost` |
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

We use the **crates.io route pinned exactly** (`=0.2.0`), not a git
dependency. The pin guards against pre-1.0 upstream churn (risk R10).
`rap-protocol` is additionally declared as a direct workspace dependency
only because the bridge's `Tool` implementation must *name* its
`ToolCallId`/`ProviderCallId` types; it adds no new package to
`Cargo.lock`.

The Infinity integration is **feature-gated** (`feature = "infinity"`, on by
default): `--no-default-features` builds a hermetic std-only workspace.
The verified API surface was cross-checked against the vendored 0.2.0 crate
sources (not just the repo's `main` branch, which may drift).

## Build instructions

Requires Rust >= 1.85 (edition 2024). Toolchain used: 1.99.1.

```bash
cd rust/

# hermetic gate (no Infinity, no network after vendoring)
cargo build --no-default-features
cargo test  --no-default-features

# full build (Infinity 0.2.0 compiled in; ~2-6 min first time; pulls
# tokio/reqwest/rustls/aws-lc-sys/rhai through infinity-agent-core)
cargo build
cargo test

# bridge crate alone, with the Infinity tool surface
cargo test -p bh-mcp-bridge --features infinity
```

Note: `tokio` is a **dev-dependency / test-only** tool in this workspace.
The verified Infinity trait API is async (`#[async_trait]`); the Jeumpa
provider performs its UDS round-trip with blocking std I/O inside the async
methods (documented, deliberate — Phase 2 moves it onto the runtime's
executor).

## `BH_RUNTIME` contract

| Value | Meaning | Phase 1 behaviour |
|---|---|---|
| unset / empty | default | `RuntimeFlavor::Python` — Python stays the runtime |
| `python` | Python runtime | same as default |
| `rust` | Rust wedge | selected, but no daemon exists yet (Phase 2) |
| anything else | error | `RuntimeError::UnknownRuntime` (fail-closed, never silently "falls back") |

The values mirror PLAN.md's `BH_RUNTIME=python|rust` flag; the environment
variable is read by `RuntimeFlavor::from_env()`.
