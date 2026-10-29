"""Tests for the subscription-aware parked agent registry (Phase 5 M2+).

Covers parking/waking semantics, bus-driven wakeup (including wildcard),
re-park detachment, manual dispatch, and error containment.
"""

import asyncio
import unittest

from src.colony_types.events import Event
from src.colony_types.models import EventType
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


if __name__ == "__main__":
    unittest.main()
