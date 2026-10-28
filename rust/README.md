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
  member agents (`spawn_agent` / `send_event` / `shutdown`, plus the additive
  `schedule_timer`), with `ColdHost` kept as the hermetic fail-loud
  placeholder (the repo bans fake success). The real implementation lives in
  `crates/bh-rtd/src/infinity_host.rs` (Phase 2).
- **`crates/bh-rtd/src/lib.rs`** — the `BH_RUNTIME` gate: `RuntimeFlavor`
  enum `{ Python, Rust }`, env-driven, **defaulting to `Python`**, fail-closed
  on unknown values.
- **`crates/bh-bridge-uds`** (shared crate, re-exported as `bh_rtd::uds`,
  feature `bridge`) — BH bridge protocol v1 client (sync + tokio-async):
  UTF-8 JSON, one value per `\n`-terminated line over a Unix domain socket,
  **one request per connection**; `{id, method, params}` →
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

## Phase 2 status (the runtime wedge is live)

Phase 2 deliverable — "`bh-rtd` Rust daemon embedding `infinity-agent-core`
(threads, `spawn_thread`≈sub_agent pool, durable timers)" — is complete:

- **`crates/bh-rtd/src/bin/bh-rtd.rs`** — the daemon binary (feature
  `daemon`): binds `BH_RTD_SOCKET` (`0600`), traps `SIGTERM`/`SIGINT`, and
  serves the control plane over BH bridge protocol v1: `health`,
  `spawn_thread`, `send_event`, `shutdown_thread`, `list_threads`,
  `schedule_timer`, `cancel_timer`, `list_timers`, `shutdown`. Fail-loud on
  missing env; refuses to bind a live socket; rebinds stale ones.
- **`crates/bh-rtd/src/infinity_host.rs`** — `InfinityHost`, the real
  `AgentHost` over `infinity-agent-core` 0.2.0 threads: bounded sub-agent
  pool (`BH_RTD_MAX_THREADS`, overflow = loud business refusal), brain via
  `JeumpaProvider`, tools via `bh-mcp-bridge`.
- **`crates/bh-rtd/src/timers.rs`** — durable timer wheel: JSON journal
  (`<BH_RTD_STATE_DIR>/timers.json`, atomic `0600` writes), replay on boot,
  `pending → fired → delivered` / `cancelled` statuses; a fired timer whose
  target thread is gone stays `fired` — never re-fired, never faked.
- **Python seam** (this repo): `src/colony/runtime_gate.py` routes
  `SubagentPool.submit` to the daemon when `BH_RUNTIME=rust` (fail-loud,
  no silent fallback); `src/integration/rtd_launcher.py` spawns/
  health-checks/shuts the daemon down from `UnifiedLauncher` step 6b.
  Default (`python`) boots are byte-identical to before.
- **Wire contracts** — pinned in `docs/02-arch/infinity-adapter-contract.md`
  (folds the Phase 0 adapter-contract-doc remainder).
- **Tests** — end-to-end against the real binary: `bh-rtd/tests/daemon.rs`
  (8: lifecycle, timers-across-restart incl. SIGKILL, live-bind refusal,
  protocol failures), `bh-rtd/tests/timers.rs` (journal semantics),
  `bh-rtd/tests/infinity_host.rs` (thread lifecycle). Workspace total:
  **64 Rust tests green** in both hermetic and full modes.

## Peer status (honest)

| Peer | Status |
|---|---|
| `src/colony/jeumpa_uds_server.py` (brain: `health`/`models`/`complete`) | SHIPPED (additive; wraps the real `JeumpaBrain`, `0600` socket) |
| `src/agent_tools/uds_server.py` (tools: `health`/`tools/list`/`tools/call`) | SHIPPED (Phase 1, commit 8aaba5b; wraps the real `ToolRegistry`, `0600` socket) |

## What is NOT here yet (Phases 2-5 of PLAN.md §4)

| Phase | Missing deliverable |
|---|---|
| 0 (rest) | `src/bridge/infinity/` RAP round-trip (the adapter contract doc landed in Phase 2: `docs/02-arch/infinity-adapter-contract.md`) |
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

Note: since Phase 2 `tokio` is a **real dependency** of `bh-rtd` behind the
`tokio-executor` feature (implied by `infinity`/`daemon`): JeumpaProvider's
UDS round-trips run on the tokio executor (`tokio::net::UnixStream`) and the
daemon runs a multi-thread control plane plus a dedicated colony executor.
The shared `bh-bridge-uds` crate keeps a std-only sync client so the
hermetic path stays tokio-free.

### Daemon build & run (Phase 2)

```bash
cd rust/
cargo build --release          # → rust/target/release/bh-rtd

BH_RTD_SOCKET=/tmp/bh-rtd.sock \
BH_RTD_STATE_DIR=/tmp/bh-rtd-state \
BH_RTD_BRAIN_SOCKET=/tmp/jeumpa.sock   # optional; threads need a brain
BH_RTD_TOOLS_SOCKET=/tmp/tools.sock    # optional; tools bridge
./target/release/bh-rtd
```

Python side: `BH_RUNTIME=rust` + `BH_RTD_BIN` (optional) —
`UnifiedLauncher` spawns and health-checks the daemon automatically
(`src/integration/rtd_launcher.py`). See
`docs/02-arch/infinity-adapter-contract.md` for the full env/method table.

## `BH_RUNTIME` contract

| Value | Meaning | Behaviour |
|---|---|---|
| unset / empty | default | `RuntimeFlavor::Python` — Python stays the runtime |
| `python` | Python runtime | same as default |
| `rust` | Rust wedge | the `bh-rtd` daemon is spawned by `UnifiedLauncher` (`RtdLauncher`) and colony thread spawns route to it (`runtime_gate`); fail-loud if the daemon is unreachable |
| anything else | error | `RuntimeError::UnknownRuntime` / `ValueError` (fail-closed, never silently "falls back") |

The values mirror PLAN.md's `BH_RUNTIME=python|rust` flag; the environment
variable is read by `RuntimeFlavor::from_env()`.
