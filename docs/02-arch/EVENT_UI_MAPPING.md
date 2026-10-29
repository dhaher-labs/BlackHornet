# Event → UI Mapping Design (Phase 5 M2+)

Status: design (no code change in this document) · Scope: `EventBus` → `WebSocketHandler` → browser/TUI clients
Related: `src/core/event_bus.py`, `src/ui/websocket_handler.py`, `src/colony_types/events.py`, `src/colony_types/models.py`, `src/swarm/parked_agents.py`

## 1. Current pipeline (as built)

```
EventBus.publish_event(Event)            # key = event_type.value (enum-normalized)
   └─ subscribers[key] + subscribers["*"]        # wildcard "*" always included
        └─ WebSocketHandler._on_event(event)     # registered via subscribe("*", ...)
             └─ broadcast("ecosystem.event", data)   # fan-out to ALL connected clients
```

The bus already guarantees:

- Enum event types are normalized once (`et.value if hasattr(et, "value") else str(et)`),
  so publishers may pass `EventType` members or plain strings interchangeably.
- Wildcard `"*"` subscribers receive every event (used by the UI bridge).
- Handler exceptions are logged and swallowed — one bad client bridge cannot kill
  the dispatch loop.

## 2. Known mismatches (to fix in a follow-up code slice)

1. **Event type lost in transit.** `_on_event` reads `getattr(event, "type", "unknown")`,
   but `Event` exposes `event_type` (an `EventType` member). Every UI message today
   carries `"event_type": "unknown"`. Correct mapping:
   `et = event.event_type; key = et.value if hasattr(et, "value") else str(et)`.
2. **Client subscriptions are stored but never honored.** `WebSocketClient.subscriptions`
   exists, yet `_on_event` fans out to every client. The design below defines the
   filtering contract; the follow-up slice implements it.

No tests pin the current `"unknown"` behaviour (`tests/` has no WebSocketHandler
coverage), so both fixes are unconstrained.

## 3. Canonical mapping (target state)

Wire format per message: `{"type": <ui_type>, "data": {...}, "timestamp": <epoch>}`.

| EventBus key (`EventType.value`) | UI `type` | `data` fields | Notes |
|---|---|---|---|
| `agent_spawned` | `agent.spawned` | `agent_id`, `source` | from `Event.data` |
| `agent_terminated` | `agent.terminated` | `agent_id`, `source` | |
| `agent_state_changed` | `agent.state` | `agent_id`, `from`, `to` | |
| `task_submitted` | `task.submitted` | `task_id`, `colony` | |
| `task_completed` | `task.completed` | `task_id`, `result_summary` | |
| `task_failed` | `task.failed` | `task_id`, `error` | |
| `tool_called` / `tool_result` | `tool.called` / `tool.result` | `tool`, `duration_ms` | |
| `memory_stored` / `memory_compacted` | `memory.stored` / `memory.compacted` | `key`, `bytes` | |
| `a2a_message` | `a2a.message` | `sender`, `recipient`, `channel` | |
| `heartbeat` | `agent.heartbeat` | `agent_id` | low priority; may be dropped under load |
| `custom` | `ecosystem.event` | passthrough | fallback, unchanged |

Envelope rule: UI never sees enum internals — only normalized string keys.

## 4. Client subscription contract (target state)

- On connect a client MAY send `{"type": "subscribe", "topics": ["agent.*", "task.*"]}`.
- Topics are prefix globs matched against the UI `type` (not the bus key).
- Empty subscription set = receive everything (today's behaviour) — backwards
  compatible; the TUI keeps this default.
- `_on_event` then filters per client: `type` matches if any topic pattern matches.
- Heartbeats (`agent.heartbeat`) are the designated low-priority class: dropped
  first when the outbound buffer exceeds `_max_buffer` (1000).

## 5. Parked agents on the UI (M2+ view)

`ParkedAgentRegistry` (src/swarm/parked_agents.py) already wakes dormant agents
from bus events. To surface dormancy in the UI:

- Registry mutators emit ordinary bus events: `agent_state_changed` with
  `data["to"] = "parked" | "active"` (park/wake/unpark) — the UI needs **no new
  transport**, only the mapping above.
- Suggested UI rendering: badge on agent card (`PARKED`), count in the colony
  header, and a "wake" affordance that publishes a matching task event — closing
  the loop with the registry's subscription model.

## 6. Implementation order

1. Fix `_on_event` mapping (drop-in, one line + normalization helper) — no client change.
2. Implement per-client topic filtering + `subscribe` control message.
3. Emit park/wake events from `ParkedAgentRegistry` usage sites (lifecycle wiring).
4. Optional: backpressure policy for `agent.heartbeat` under buffer pressure.
