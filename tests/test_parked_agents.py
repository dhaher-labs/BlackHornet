"""Tests for the subscription-aware parked agent registry (Phase 5 M2+).

Covers parking/waking semantics, bus-driven wakeup (including wildcard),
re-park detachment, manual dispatch, and error containment.
"""

import asyncio
import unittest

from src.colony_types.events import Event
from src.colony_types.models import AgentState, EventType
from src.core.event_bus import EventBus
from src.swarm.parked_agents import ParkedAgentRegistry


class _Recorder:
    """Sync wake handler that records events."""

    def __init__(self):
        self.events = []

    def __call__(self, event):
        self.events.append(event)


def _run(coro):
    """Run a coroutine on a fresh event loop (repo pattern: no pytest-asyncio)."""
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


class TestParkedAgentsManual(unittest.TestCase):
    """Registry without a bus: manual wake / on_event dispatch."""

    def test_park_and_manual_wake(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        reg.park("a1", ["task_submitted"], rec)

        self.assertTrue(reg.is_parked("a1"))
        self.assertEqual(reg.parked_ids(), ["a1"])
        self.assertEqual(len(reg), 1)

        self.assertTrue(reg.wake("a1"))
        self.assertEqual(len(rec.events), 1)
        self.assertIsNone(rec.events[0])  # manual wake passes no event
        self.assertFalse(reg.is_parked("a1"))
        self.assertFalse(reg.wake("a1"))  # already woken

    def test_wake_missing_agent(self):
        reg = ParkedAgentRegistry()
        self.assertFalse(reg.wake("ghost"))

    def test_unpark_removes_without_wake(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        reg.park("a1", ["task_submitted"], rec)

        self.assertTrue(reg.unpark("a1"))
        self.assertEqual(len(rec.events), 0)  # no wake callback fired
        self.assertFalse(reg.is_parked("a1"))
        self.assertFalse(reg.unpark("a1"))

    def test_on_event_matches_exact_and_wildcard(self):
        reg = ParkedAgentRegistry()
        exact = _Recorder()
        wild = _Recorder()
        other = _Recorder()
        reg.park("exact", ["task_submitted"], exact)
        reg.park("wild", ["*"], wild)
        reg.park("other", ["task_completed"], other)

        evt = Event(event_type=EventType.TASK_SUBMITTED, data={"k": 1})
        woken = _run(reg.on_event(evt))

        self.assertEqual(woken, ["exact", "wild"])  # registry order
        self.assertEqual(len(exact.events), 1)
        self.assertEqual(exact.events[0], evt)
        self.assertEqual(len(wild.events), 1)
        self.assertEqual(len(other.events), 0)  # not subscribed
        self.assertFalse(reg.is_parked("exact"))
        self.assertTrue(reg.is_parked("other"))

    def test_on_event_no_match_leaves_everything_parked(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        reg.park("a1", ["task_submitted"], rec)

        evt = Event(event_type=EventType.AGENT_TERMINATED)
        woken = _run(reg.on_event(evt))

        self.assertEqual(woken, [])
        self.assertTrue(reg.is_parked("a1"))
        self.assertEqual(len(rec.events), 0)

    def test_park_strips_blank_event_types(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        reg.park("a1", ["", "  ", "task_submitted"], rec)

        entry = reg.get("a1")
        self.assertEqual(entry.event_types, frozenset({"task_submitted"}))


class TestParkedAgentsWithBus(unittest.TestCase):
    """Registry attached to the EventBus: automatic wakeup."""

    def setUp(self):
        self.bus = EventBus()
        _run(self.bus.start())

    def tearDown(self):
        _run(self.bus.stop())

    def test_bus_event_wakes_parked_agent(self):
        bus = self.bus
        reg = ParkedAgentRegistry(bus=bus)
        rec = _Recorder()
        reg.park("a1", ["task_submitted"], rec)

        _run(bus.publish_event(Event(event_type=EventType.TASK_SUBMITTED)))

        self.assertEqual(len(rec.events), 1)
        self.assertFalse(reg.is_parked("a1"))

        # Subscription detached on wake: republishing wakes nobody.
        _run(bus.publish_event(Event(event_type=EventType.TASK_SUBMITTED)))
        self.assertEqual(len(rec.events), 1)

    def test_bus_wildcard_subscription(self):
        bus = self.bus
        reg = ParkedAgentRegistry(bus=bus)
        rec = _Recorder()
        reg.park("a1", ["*"], rec)

        _run(bus.publish_event(Event(event_type=EventType.HEARTBEAT)))
        self.assertEqual(len(rec.events), 1)

    def test_bus_non_matching_event_keeps_agent_parked(self):
        bus = self.bus
        reg = ParkedAgentRegistry(bus=bus)
        rec = _Recorder()
        reg.park("a1", ["task_submitted"], rec)

        _run(bus.publish_event(Event(event_type=EventType.HEARTBEAT)))
        self.assertEqual(len(rec.events), 0)
        self.assertTrue(reg.is_parked("a1"))
        reg.unpark("a1")  # detach subscription (clean singleton bus)

    def test_repark_replaces_subscriptions(self):
        bus = self.bus
        reg = ParkedAgentRegistry(bus=bus)
        rec1 = _Recorder()
        rec2 = _Recorder()
        reg.park("a1", ["task_submitted"], rec1)
        reg.park("a1", ["task_completed"], rec2)  # replaces entry

        _run(bus.publish_event(Event(event_type=EventType.TASK_SUBMITTED)))
        self.assertEqual(len(rec1.events), 0)  # old subscription detached
        self.assertTrue(reg.is_parked("a1"))

        _run(bus.publish_event(Event(event_type=EventType.TASK_COMPLETED)))
        self.assertEqual(len(rec2.events), 1)
        self.assertFalse(reg.is_parked("a1"))

    def test_unpark_detaches_bus_subscription(self):
        bus = self.bus
        reg = ParkedAgentRegistry(bus=bus)
        rec = _Recorder()
        reg.park("a1", ["task_submitted"], rec)
        reg.unpark("a1")

        _run(bus.publish_event(Event(event_type=EventType.TASK_SUBMITTED)))
        self.assertEqual(len(rec.events), 0)


class TestParkedAgentsContracts(unittest.TestCase):
    """Contract enforcement and error containment."""

    def test_async_wake_handler_rejected(self):
        reg = ParkedAgentRegistry()

        async def bad(event):  # pragma: no cover - must never run
            pass

        with self.assertRaises(ValueError):
            reg.park("a1", ["task_submitted"], bad)

    def test_wake_handler_exception_contained(self):
        reg = ParkedAgentRegistry()

        def boom(event):
            raise RuntimeError("wake failed")

        reg.park("a1", ["task_submitted"], boom)
        self.assertTrue(reg.wake("a1"))  # does not raise
        self.assertFalse(reg.is_parked("a1"))  # still removed

    def test_bus_handler_exception_contained(self):
        bus = EventBus()
        _run(bus.start())
        reg = ParkedAgentRegistry(bus=bus)

        def boom(event):
            raise RuntimeError("wake failed")

        reg.park("a1", ["task_submitted"], boom)
        _run(bus.publish_event(Event(event_type=EventType.TASK_SUBMITTED)))
        self.assertFalse(reg.is_parked("a1"))


class TestParkedAgentsEmissions(unittest.TestCase):
    """Mutators emit agent_state_changed bus events (EVENT_UI_MAPPING.md §5)."""

    def setUp(self):
        self.bus = EventBus()
        _run(self.bus.start())

    def tearDown(self):
        _run(self.bus.stop())

    def test_park_emits_parked_event(self):
        async def scenario():
            seen = []

            async def rec(event):
                seen.append(event)

            self.bus.subscribe("agent_state_changed", rec)
            reg = ParkedAgentRegistry(bus=self.bus)
            reg.park("a1", ["task_submitted"], _Recorder())
            await asyncio.gather(*list(reg._pending))
            return seen

        seen = _run(scenario())
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0].event_type, EventType.AGENT_STATE_CHANGED)
        self.assertEqual(seen[0].source, "parked_agents")
        self.assertEqual(
            seen[0].data,
            {"agent_id": "a1", "from": "active", "to": "parked"},
        )

    def test_wake_emits_active_event(self):
        async def scenario():
            seen = []

            async def rec(event):
                seen.append(event)

            self.bus.subscribe("agent_state_changed", rec)
            reg = ParkedAgentRegistry(bus=self.bus)
            reg.park("a1", ["task_submitted"], _Recorder())
            await asyncio.gather(*list(reg._pending))
            self.assertTrue(reg.wake("a1"))
            await asyncio.gather(*list(reg._pending))
            return seen

        seen = _run(scenario())
        self.assertEqual(len(seen), 2)
        self.assertEqual(seen[0].data["to"], "parked")
        self.assertEqual(seen[1].data["to"], "active")
        self.assertEqual(seen[1].data["from"], "parked")

    def test_unpark_emits_active_event(self):
        async def scenario():
            seen = []

            async def rec(event):
                seen.append(event)

            self.bus.subscribe("agent_state_changed", rec)
            reg = ParkedAgentRegistry(bus=self.bus)
            reg.park("a1", ["task_submitted"], _Recorder())
            await asyncio.gather(*list(reg._pending))
            self.assertTrue(reg.unpark("a1"))
            await asyncio.gather(*list(reg._pending))
            return seen

        seen = _run(scenario())
        self.assertEqual(len(seen), 2)
        self.assertEqual(seen[1].data["to"], "active")

    def test_wake_missing_agent_emits_nothing(self):
        async def scenario():
            seen = []

            async def rec(event):
                seen.append(event)

            self.bus.subscribe("agent_state_changed", rec)
            reg = ParkedAgentRegistry(bus=self.bus)
            self.assertFalse(reg.wake("ghost"))
            await asyncio.gather(*list(reg._pending))
            return seen

        self.assertEqual(_run(scenario()), [])

    def test_no_bus_skips_emission_silently(self):
        async def scenario():
            reg = ParkedAgentRegistry()
            rec = _Recorder()
            reg.park("a1", ["task_submitted"], rec)
            self.assertTrue(reg.wake("a1"))
            return reg

        reg = _run(scenario())
        self.assertEqual(reg._pending, set())
        self.assertFalse(reg.is_parked("a1"))

    def test_emission_failure_contained(self):
        class _BoomBus:
            def subscribe(self, event_type, handler):
                pass

            def unsubscribe(self, event_type, handler):
                pass

            async def publish_typed(self, *args, **kwargs):
                raise RuntimeError("boom")

        async def scenario():
            reg = ParkedAgentRegistry(bus=_BoomBus())  # type: ignore[arg-type]
            rec = _Recorder()
            reg.park("a1", ["task_submitted"], rec)
            await asyncio.gather(*list(reg._pending))  # must not raise
            self.assertTrue(reg.wake("a1"))  # registry still functional
            await asyncio.gather(*list(reg._pending))
            return rec

        rec = _run(scenario())
        self.assertEqual(len(rec.events), 1)  # wake handler ran


class TestParkedAgentsSync(unittest.TestCase):
    """sync_from_states: consumer-side park-idle / wake-busy wiring."""

    def setUp(self):
        self.bus = EventBus()
        _run(self.bus.start())

    def tearDown(self):
        _run(self.bus.stop())

    def test_sync_parks_idle_only(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        report = reg.sync_from_states(
            {
                "a": AgentState.SUSPENDED,
                "b": AgentState.ACTIVE,
                "c": AgentState.TERMINATED,
            },
            rec,
        )
        self.assertEqual(report, {"parked": ["a"], "woken": []})
        self.assertTrue(reg.is_parked("a"))
        self.assertEqual(len(reg), 1)
        # Parking never invokes the wake handler.
        self.assertEqual(rec.events, [])

    def test_sync_wakes_parked_agent_back_to_active(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        reg.park("a", ["task_assigned"], rec)

        report = reg.sync_from_states({"a": AgentState.ACTIVE}, rec)

        self.assertEqual(report, {"parked": [], "woken": ["a"]})
        self.assertFalse(reg.is_parked("a"))
        self.assertEqual(len(rec.events), 1)
        self.assertIsNone(rec.events[0])  # manual wake carries no event

    def test_sync_is_idempotent_no_churn(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        states = {"a": AgentState.WAITING, "b": AgentState.SUSPENDED}

        first = reg.sync_from_states(states, rec)
        entry_before = reg.get("a")
        second = reg.sync_from_states(states, rec)

        self.assertEqual(first["parked"], ["a", "b"])
        self.assertEqual(second, {"parked": [], "woken": []})
        self.assertIs(reg.get("a"), entry_before)  # same entry, not re-parked
        self.assertEqual(entry_before.parked_at, reg.get("a").parked_at)

    def test_sync_transitional_states_untouched(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        report = reg.sync_from_states(
            {
                "a": AgentState.REGISTERED,
                "b": AgentState.INITIALIZING,
                "c": AgentState.DRAINING,
                "d": AgentState.COMPACTING,
            },
            rec,
        )
        self.assertEqual(report, {"parked": [], "woken": []})
        self.assertEqual(len(reg), 0)

    def test_sync_accepts_raw_state_strings(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        report = reg.sync_from_states(
            {"a": "suspended", "b": "waiting"}, rec
        )
        self.assertEqual(report["parked"], ["a", "b"])

    def test_sync_rejects_unknown_state(self):
        reg = ParkedAgentRegistry()
        with self.assertRaises(ValueError):
            reg.sync_from_states({"a": "hibernating"}, _Recorder())

    def test_sync_default_wake_topics_and_metadata(self):
        bus = self.bus
        reg = ParkedAgentRegistry(bus=bus)
        rec = _Recorder()
        reg.sync_from_states(
            {"a": AgentState.WAITING}, rec, metadata={"reason": "idle"}
        )

        entry = reg.get("a")
        self.assertEqual(
            entry.event_types, frozenset({"task_submitted", "task_completed"})
        )
        self.assertEqual(entry.metadata, {"reason": "idle"})

        # Bus-driven wake through the default topics (closed loop §5).
        _run(bus.publish_event(Event(event_type=EventType.TASK_SUBMITTED)))
        self.assertEqual(len(rec.events), 1)
        self.assertFalse(reg.is_parked("a"))

    def test_sync_wake_on_ready_state(self):
        reg = ParkedAgentRegistry()
        rec = _Recorder()
        reg.park("a", ["*"], rec)
        report = reg.sync_from_states({"a": AgentState.READY}, rec)
        self.assertEqual(report["woken"], ["a"])


if __name__ == "__main__":
    unittest.main()
