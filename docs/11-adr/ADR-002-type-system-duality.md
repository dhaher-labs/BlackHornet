# ADR-002: Type-system duality — `colony_types` canonical core, `bh_types` ecosystem surface

- **Status:** Accepted
- **Date:** 2026-10-28
- **Decided by:** Task 18 (carry-over completion), resolving the PLAN.md §2.1 D6 leftover flagged by the 17-a traceability audit.

## Context

The pre-rebuild audit (DROP item D6) mandated deleting `src/colony_types/` as a
duplicate of `src/bh_types/`. The clean rebuild inverted that reality:

- `src/colony_types/` is the **live core type system** — imported by ~41 files
  across `src/core/`, `src/agents/`, `src/colony/`, `src/memory/`,
  `src/security/` (agents, tasks, events, tools, colony runtime models).
- `src/bh_types/` serves the **ecosystem / agent-tools surface** —
  `ecosystem.EcosystemState` (canonical, wired to `GET /api/v1/ecosystem`),
  `tools`, `messages`, `memory` (consumed by `src/agent_tools/*`),
  and `events` (normalized together with `colony_types.Event` by
  `src/core/event_bus.py`).

Two files were genuine dead duplicates: `bh_types/colony.py` (zero importers)
and `bh_types/agent.py` (only a test import-smoke; `AgentState` lives
canonically in `colony_types/models.py`, `AgentConfig` in
`colony_types/agent.py`). Their continued presence kept the exact confusion
D6 was written to prevent.

## Decision

1. **`src/colony_types/` is the canonical core type system.** New core models
   and enums go here.
2. **`src/bh_types/` is the canonical ecosystem/agent-tools surface**
   (`ecosystem`, `tools`, `messages`, `memory`, `events`). No new core types.
3. Dead duplicates `bh_types/colony.py` and `bh_types/agent.py` are **removed**;
   `tests/test_blackhornet_units.py` migrated to the canonical imports.
4. The duality is deliberate and guarded by
   `tests/selftests/colony_types_tests.py` ("v7 type-system truth").
   Consolidation into a single package is explicitly **post-v7** scope
   (PLAN §5 R8: no scope creep); until then this ADR is the rule of record.

## Consequences

- Import rule of thumb: core/agent/colony runtime → `colony_types`;
  agent tools & ecosystem state → `bh_types`.
- `event_bus` keeps normalizing both `Event` types (documented at
  `src/core/event_bus.py:139`) until a deliberate post-v7 unification.
- Any future PR adding a type that already exists on the other side must
  fail review against this ADR.
