# Infinity Adapter Contract (protocol v1)

Phase 0 remainder of the Rust + Infinity roadmap (PLAN.md §4), folded into
the Phase 2 runtime wedge (Task 19). This document pins the wire contracts
between the Python BlackHornet colony and the `bh-rtd` Rust runtime daemon.
It describes **what is implemented** — nothing here is aspirational; anything
not yet built is listed under "Deferred".

Posture (PLAN.md §4): *adopt the protocol, embed the runtime, never port the
colony.* `BH_RUNTIME=python` is the default; the trading money-path stays
Python until Phase 5+.

## 1. Transport — BH bridge protocol v1

Unix domain socket, UTF-8 JSON, one value per `\n`-terminated line,
**one request per connection**:

* Request:  `{"id": <string>, "method": "<name>", "params": {...}}`
* Success:  `{"id": <echoed>, "ok": true,  "result": {...}}`
* Failure:  `{"id": <echoed-or-null>, "ok": false, "error": {"code": <int>, "message": "<string>"}}`

Envelope rules (identical on every BH bridge socket — brain, tools, rtd):

* `ok: false` is reserved for **bridge/protocol failures**: parse `-32700`,
  invalid params `-32602`, unknown method `-32601`, internal `-32603`.
* **Business outcomes** travel as `ok: true` with `accepted: false` and a
  human-readable `error` inside `result`. Nothing is silently swallowed on
  either side.
* Sockets are created with `0600` permissions. The `bh-rtd` daemon refuses
  to bind over a socket that is currently being served (live-bind refusal)
  and rebinds a *stale* socket left by a dead process.

Implementations: Rust client `crates/bh-bridge-uds` (sync + tokio-async,
re-exported as `bh_rtd::uds`); Python one-shot client inside
`src/integration/rtd_launcher.py` and `src/colony/runtime_gate.py`.

## 2. Daemon control plane (`bh-rtd` binary, `BH_RTD_SOCKET`)

| method | params | result |
|---|---|---|
| `health` | — | `{status, service, protocol, runtime, version, brain, tools, threads, max_threads, timers_pending, watchdog: {stall_threshold_ms, active, parked, stalled, stalled_agents}}` — Phase 5 M1: the `watchdog` object is additive |
| `spawn_thread` | `name` (required, non-empty), `role?` (default `"worker"`), `instructions?` | `{accepted, agent_id, thread_id}` |
| `send_event` | `agent_id` (required), `kind?` (`task_assigned`\|`message_received`\|`health_tick`\|`custom`), `payload?` | `{accepted}` |
| `shutdown_thread` | `agent_id` (required) | `{accepted, thread_id}` — detaches the host from the thread (events stop; the Infinity thread idles out, history stays in the store: hibernation semantics) |
| `list_threads` | — | `{threads: [{agent_id, thread_id, role, events_seen, last_event, last_event_age_ms, parked, watch_state}]}` — Phase 5 M1: `last_event_age_ms` / `parked` / `watch_state` (`active\|parked\|stalled`) are additive watchdog fields |
| `schedule_timer` | `delay_ms` (required, > 0), `agent_id?`, `payload?` | `{accepted, timer_id}` |
| `cancel_timer` | `timer_id` (required) | `{accepted}` |
| `list_timers` | — | `{timers: [TimerRecord JSON]}` (shape below) |
| `shutdown` | — | `{status: "shutting_down"}` — graceful stop after replying; also wired to `SIGTERM`/`SIGINT` |

`spawn_thread` at pool capacity (`BH_RTD_MAX_THREADS`) is a **loud business
refusal** (`accepted: false`), never a queue and never a silent local
fallback.

### Watchdog liveness (Phase 5 M1, additive)

`health.watchdog` and the per-thread `watch_state` classify each tracked
thread from its event-flow heartbeat (`last_event_age_ms`, floored at
spawn time):

* `active` — events flowing, or age ≤ `stall_threshold_ms`;
* `parked` — the thread's most recent tool call was the passive
  `sleep_until_event_or_input` tool (the runtime's own "hibernate until
  input" idiom): exempt from stall detection by design. The flag is a
  recorded fact about the most recent tool call — sticky across non-tool
  events (the turn-ending `CompletionFinished` does not un-park it),
  cleared by any other tool call;
* `stalled` — not parked and silent for strictly longer than
  `stall_threshold_ms`; the thread's colony names are listed (sorted) in
  `health.watchdog.stalled_agents`.

Classification is evaluated at query time (pure pull) — no sweeper thread.
Subscription-aware waking of parked agents remains Phase 5 work (not faked
here).

## 3. `AgentSpec` wire form (spawn)

```json
{ "name": "radar-worker-1", "role": "worker", "instructions": "..." }
```

* `name` — colony-visible identity (required, non-empty; duplicates are a
  business refusal).
* `role` — BlackHornet role vocabulary, default `"worker"`.
* `instructions` — system instructions for the agent's LLM turns (optional).

The daemon assigns `agent_id` (colony-visible) and `thread_id` (Infinity
thread); both are returned and are the addressing keys for every later call.

## 4. `ColonyEvent` payload contract (v1)

A colony event is `{kind, payload}` where `kind` is the discriminator
(`task_assigned` | `message_received` | `health_tick` | `custom`) and
`payload` is any JSON value carried **losslessly** to the thread (the daemon
never inspects or rewrites it). Rust-side this arrives as
`ColonyEvent { kind: ColonyEventKind, payload_json: String }`.

Durable-timer delivery is a `custom` event whose payload is:

```json
{ "timer_id": "<id>", "payload": <the scheduled payload> }
```

A fired timer whose target thread is gone (e.g. after a restart — thread
registries are in-memory until Phase 3) stays `fired` in the journal: never
re-fired, never reported as delivered, always operator-visible on stderr.

## 5. Durable timer journal

* Path: `<BH_RTD_STATE_DIR>/timers.json` — a single JSON document
  `{ "timers": [ ... ] }`, rewritten atomically (temp file + rename) with
  `0600` permissions on every mutation.
* Record shape (`TimerRecord::to_json`):

```json
{ "timer_id": "<id>", "fire_at_ms": 1730000000000, "agent_id": "…|null",
  "payload": <any JSON>, "status": "pending|fired|delivered|cancelled" }
```

* Statuses: `pending` (scheduled) → `fired` (deadline passed, handed to the
  daemon; delivery unconfirmed) → `delivered` (reached a live target, or no
  target was required); `cancelled` before firing — kept in the journal for
  auditability.
* Replay on boot re-arms every `pending` record; a corrupt journal is a
  **fail-loud error**, never silently truncated.

## 6. Environment contract

| Variable | Owner | Meaning |
|---|---|---|
| `BH_RUNTIME` | both sides | `python` (default) \| `rust`; anything else fails closed (`ValueError` / `RuntimeError::UnknownRuntime`) |
| `BH_RTD_SOCKET` | daemon + Python clients | daemon control UDS path (required for the daemon and for every rust-flavor client) |
| `BH_RTD_STATE_DIR` | daemon | directory for the timer journal (default `./.bh-rtd-state`) |
| `BH_RTD_MAX_THREADS` | daemon | sub-agent pool cap (default `8`; overflow = loud refusal) |
| `BH_RTD_BRAIN_SOCKET` | daemon | Jeumpa brain peer (`src/colony/jeumpa_uds_server.py`) |
| `BH_RTD_BRAIN_MODEL` | daemon | default model for spawned threads |
| `BH_RTD_TOOLS_SOCKET` | daemon | tools peer (`src/agent_tools/uds_server.py`); wires `bh-mcp-bridge` tools onto threads |
| `BH_RTD_WATCHDOG_MS` | daemon | Phase 5 watchdog stall threshold in ms (default `30000`; must be ≥ 1) — a silent, unparked thread older than this reports `stalled` |
| `BH_RTD_BIN` | Python launcher | path to the daemon binary (default `<repo>/rust/target/release/bh-rtd`) |

## 7. Python ⇄ Rust seam mapping (Phase 2 wedge)

* `UnifiedLauncher.start_all()` step 6b: `BH_RUNTIME=rust` →
  `RtdLauncher.start()` spawns the daemon, waits (≤ 15 s) for
  `BH_RTD_SOCKET`, health-checks it over protocol v1, and registers the
  handle for `stop_all()` teardown. `python` (default) → one debug log,
  boot otherwise byte-identical. Any failure is **fail-loud** — no silent
  fallback to Python.
* `SubagentPool.submit()` (`src/colony/orchestration/pool.py`) consults
  `src/colony/runtime_gate.py`: under `rust`, the member-agent thread is
  spawned on the daemon via `spawn_thread{name: subagent.id,
  role: "worker", instructions: subagent.task}`; the local `Subagent` only
  mirrors `RUNNING`. Under `python` the gate returns `None` and the
  dispatch is exactly as before.
* An unreachable daemon raises `RustRuntimeError` — a colony that asked for
  the Rust runtime must never quietly continue on Python (repo F8 rule).

## 8. Deferred (later phases, per PLAN.md §4)

* **Phase 3** — thread registry persistence + Infinity
  `ConversationStore`/`StateStore` over BH SQLite FTS5 `MemoryFederation`
  (hibernating agents survive daemon restarts); payload schema versioning
  (`v` field) if evolution requires it.
* **Phase 4** — native Rust RAP tool servers (shell/file/search) bypassing
  the MCP proxy.
* **Phase 5** — watchdog/heartbeat via `sleep_until_event_or_input` +
  subscriptions; `view_update` UI integration.
