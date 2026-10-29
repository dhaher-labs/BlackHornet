"""Subscription-aware parked agent registry (Phase 5 M2+).

Long-lived swarm/colony agents can *park* (become dormant) when they have no
work. Each parked agent declares the event types that should wake it. The
registry hooks those subscriptions into the :class:`~src.core.event_bus.EventBus`
on the agent's behalf, so publishing a matching event wakes the agent
automatically. Agents can also be woken explicitly via
:meth:`ParkedAgentRegistry.wake` (manual/external scheduling).

Design notes:
- Purely additive: no existing module is modified to adopt this registry.
- Works with or without an EventBus. Without one, drive wakeups manually via
  :meth:`ParkedAgentRegistry.on_event` (identical matching rules).
- Wake handlers are sync callables (async work should be scheduled from within
  the handler on the agent's own loop). This keeps the registry free of
  fire-and-forget tasks and makes behaviour trivially testable.
- Wildcard subscriptions: an agent parked with ``"*"`` wakes on every event.
- Re-parking an agent replaces its previous subscription set and detaches the
  old bus handlers, so stale subscriptions can never wake a replaced entry.
- Mutators emit ordinary ``agent_state_changed`` bus events (``to`` =
  ``"parked" | "active"``) so the UI can surface dormancy without a new
  transport (docs/02-arch/EVENT_UI_MAPPING.md §5). Emissions are best-effort
  telemetry scheduled only when a running loop exists; they never break
  parking/waking and are skipped silently in sync contexts.
"""

from __future__ import annotations

import asyncio
import inspect
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable, Optional

from src.colony_types.events import Event, EventType
from src.config.logging_config import get_logger
from src.core.event_bus import EventBus

logger = get_logger(__name__)

#: A wake handler receives the event that matched (or ``None`` for manual
#: wakes) and returns nothing. Must be a sync callable.
WakeHandler = Callable[[Optional[Event]], None]

WILDCARD = "*"


@dataclass
class ParkedAgent:
    """A parked agent entry with its wakeup subscriptions."""

    agent_id: str
    event_types: frozenset[str]
    parked_at: float
    wake: WakeHandler
    metadata: dict[str, Any] = field(default_factory=dict)
    # event_type -> bus handler (stable object identity, required for
    # EventBus.unsubscribe which compares handlers by equality)
    handlers: dict[str, Callable[[Event], Any]] = field(default_factory=dict)


class ParkedAgentRegistry:
    """Registry of parked agents with subscription-based wakeup.

    Args:
        bus: Optional EventBus. When provided, parking subscribes a per-agent
            handler for each declared event type and waking unsubscribes them.
            When ``None``, wakeups are driven by calling :meth:`on_event`.
    """

    def __init__(self, bus: Optional[EventBus] = None) -> None:
        self._bus = bus
        self._parked: dict[str, ParkedAgent] = {}
        # In-flight state-change emission tasks (kept alive until done so the
        # loop cannot garbage-collect them mid-flight).
        self._pending: set[asyncio.Task[Any]] = set()

    # ── parking / waking ───────────────────────────────────────────────

    def park(
        self,
        agent_id: str,
        event_types: Iterable[str],
        wake: WakeHandler,
        metadata: Optional[dict[str, Any]] = None,
    ) -> None:
        """Park an agent; replaces any previous entry (idempotent re-park).

        Args:
            agent_id: Identifier of the agent to park.
            event_types: Event types that should wake the agent
                (``"*"`` wakes on every event).
            wake: Sync callback invoked with the matching event (or ``None``
                for manual wakes).
            metadata: Optional free-form context (capabilities, last task...).

        Raises:
            ValueError: If ``wake`` is an async callable.
        """
        if inspect.iscoroutinefunction(wake):
            raise ValueError(
                "wake handler must be a sync callable; schedule async work "
                "from within it (e.g. loop.create_task)"
            )

        old = self._parked.get(agent_id)
        if old is not None:
            self._detach(old)

        types = frozenset(t.strip() for t in event_types if t and t.strip())
        entry = ParkedAgent(
            agent_id=agent_id,
            event_types=types,
            parked_at=time.time(),
            wake=wake,
            metadata=dict(metadata or {}),
        )

        if self._bus is not None:
            for et in types:
                handler = self._make_handler(agent_id)
                entry.handlers[et] = handler
                self._bus.subscribe(et, handler)

        self._parked[agent_id] = entry
        logger.info("agent_parked", agent_id=agent_id, event_types=sorted(types))
        self._schedule_state_change(agent_id, "parked")

    def wake(self, agent_id: str, event: Optional[Event] = None) -> bool:
        """Wake a parked agent and remove it from the registry.

        Args:
            agent_id: Identifier of the agent to wake.
            event: The event that triggered the wake, if any.

        Returns:
            ``True`` if the agent was parked (and has now been woken),
            ``False`` if it was not parked. Exceptions raised by the wake
            handler are logged and swallowed so the dispatch loop never dies.
        """
        entry = self._parked.pop(agent_id, None)
        if entry is None:
            return False
        self._detach(entry)
        try:
            entry.wake(event)
        except Exception as e:
            logger.error("parked_agent_wake_error", agent_id=agent_id, error=str(e))
        self._schedule_state_change(agent_id, "active")
        return True

    def unpark(self, agent_id: str) -> bool:
        """Remove a parked agent WITHOUT invoking its wake handler.

        Use when an agent is shut down or garbage-collected while parked:
        its bus subscriptions are detached but nothing is woken.

        Args:
            agent_id: Identifier of the agent to remove.

        Returns:
            ``True`` if the agent was parked, ``False`` otherwise.
        """
        entry = self._parked.pop(agent_id, None)
        if entry is None:
            return False
        self._detach(entry)
        self._schedule_state_change(agent_id, "active")
        return True

    async def on_event(self, event: Event) -> list[str]:
        """Dispatch an event to all matching parked agents (manual mode).

        Use this to drive wakeups when the registry was created without a bus
        (or from an external scheduler). Wake semantics are identical to the
        bus path: matching agents are removed from the registry first, then
        their handlers run.

        Args:
            event: The published event.

        Returns:
            IDs of the agents woken by this event, in registry order.
        """
        et = event.event_type
        key = et.value if hasattr(et, "value") else str(et)
        woken: list[str] = []
        for agent_id, entry in list(self._parked.items()):
            if key in entry.event_types or WILDCARD in entry.event_types:
                if self.wake(agent_id, event):
                    woken.append(agent_id)
        return woken

    # ── queries ────────────────────────────────────────────────────────

    def is_parked(self, agent_id: str) -> bool:
        """Return ``True`` if the agent is currently parked."""
        return agent_id in self._parked

    def parked_ids(self) -> list[str]:
        """Return IDs of all currently parked agents."""
        return list(self._parked.keys())

    def get(self, agent_id: str) -> Optional[ParkedAgent]:
        """Return the parked entry for an agent, or ``None``."""
        return self._parked.get(agent_id)

    def __len__(self) -> int:
        return len(self._parked)

    # ── internals ──────────────────────────────────────────────────────

    def _make_handler(self, agent_id: str) -> Callable[[Event], Any]:
        """Build a stable async bus handler that wakes a single agent."""

        async def _on_event(event: Event) -> None:
            self.wake(agent_id, event)

        return _on_event

    def _schedule_state_change(self, agent_id: str, to_state: str) -> None:
        """Schedule a best-effort ``agent_state_changed`` emission.

        Mutators are sync; emissions only happen when called from inside a
        running loop (the orchestrator's async lifecycle). The emission runs
        as a detached task — this is telemetry, deliberately distinct from
        the no-fire-and-forget rule that applies to *wake handlers* (user
        code). Without a running loop the emission is skipped silently, so
        sync-context callers (and the existing test suite) are unaffected.
        """
        if self._bus is None:
            return
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            return
        task = loop.create_task(self._emit_state_change(agent_id, to_state))
        self._pending.add(task)
        task.add_done_callback(self._pending.discard)

    async def _emit_state_change(self, agent_id: str, to_state: str) -> None:
        """Publish agent_state_changed (from parked-state model) on the bus.

        Payload follows the canonical mapping (EVENT_UI_MAPPING.md §3/§5):
        ``data = {"agent_id", "from", "to"}``. Failures are logged and
        swallowed — emission must never break parking/waking.
        """
        if self._bus is None:
            return
        try:
            await self._bus.publish_typed(
                EventType.AGENT_STATE_CHANGED,
                source="parked_agents",
                data={"agent_id": agent_id, "from": "active" if to_state == "parked" else "parked", "to": to_state},
            )
        except Exception as e:
            logger.warning(
                "parked_agents_emit_error", agent_id=agent_id, error=str(e)
            )

    def _detach(self, entry: ParkedAgent) -> None:
        """Remove an entry's bus subscriptions (safe without a bus)."""
        if self._bus is None:
            return
        for et, handler in entry.handlers.items():
            self._bus.unsubscribe(et, handler)
        entry.handlers.clear()
