"""K2/K4 wiring gate (Task 18): EcosystemRunner must boot the async colony
core behind the sync launcher and expose a live ecosystem snapshot via the
APIBridge /api/v1/ecosystem route contract.

Parked-agent lifecycle bootstrap (Phase 5 M2+): the runner must wire
agent_registry + parked_registry + park_wake into the orchestrator so the
sync tick parks idle agents and wakes them on their subscriptions."""

from __future__ import annotations

import asyncio
import unittest


class TestEcosystemRunner(unittest.TestCase):
    def test_start_stop_lifecycle(self):
        from src.integration.ecosystem_runner import EcosystemRunner

        runner = EcosystemRunner()
        runner.start()
        try:
            snap = runner.get_status_snapshot()
            self.assertIn("ecosystem", snap)
            self.assertTrue(runner.orchestrator._running)
        finally:
            runner.stop()
        self.assertFalse(runner.orchestrator._running)

    def test_api_bridge_serves_live_ecosystem(self):
        from src.integration.ecosystem_runner import EcosystemRunner
        from src.ui.api_bridge import APIBridge

        runner = EcosystemRunner()
        runner.start()
        try:
            bridge = APIBridge(orchestrator=runner.orchestrator)
            resp = bridge.get_ecosystem_status()
            self.assertTrue(resp.success, resp.error)
            self.assertIsInstance(resp.data, dict)
            self.assertIn("ecosystem", resp.data)
        finally:
            runner.stop()

    def test_bridge_returns_503_without_orchestrator(self):
        from src.ui.api_bridge import APIBridge

        bridge = APIBridge()
        resp = bridge.get_ecosystem_status()
        self.assertFalse(resp.success)
        self.assertEqual(resp.status_code, 503)


class TestParkedLifecycleBootstrap(unittest.TestCase):
    """Host bootstrap wire (parked_registry=..., park_wake=...) — the last
    connection of the parked lifecycle: EcosystemRunner must construct and
    wire the registries so orchestrator sync ticks drive parking/waking."""

    class _FakeAgent:
        def __init__(self, state):
            self.state = state

    def _sync(self, orchestrator):
        asyncio.run(orchestrator._sync_parked_agents())

    def test_registries_wired_into_orchestrator(self):
        import inspect

        from src.integration.ecosystem_runner import EcosystemRunner

        runner = EcosystemRunner()
        self.assertIs(runner.orchestrator._agent_registry, runner.agent_registry)
        self.assertIs(runner.orchestrator._parked_registry, runner.parked_registry)
        self.assertIsNotNone(runner.orchestrator._park_wake)
        # Wake contract: sync callable (park() rejects async handlers).
        self.assertFalse(
            inspect.iscoroutinefunction(runner.orchestrator._park_wake)
        )

    def test_sync_tick_parks_idle_agent(self):
        from src.colony_types.models import AgentState
        from src.integration.ecosystem_runner import EcosystemRunner

        runner = EcosystemRunner()
        runner.agent_registry._agents["idle1"] = self._FakeAgent(
            AgentState.SUSPENDED
        )

        self._sync(runner.orchestrator)
        self.assertTrue(runner.parked_registry.is_parked("idle1"))

        # Idempotent: a second tick must not churn or wake.
        self._sync(runner.orchestrator)
        self.assertTrue(runner.parked_registry.is_parked("idle1"))

    def test_parked_agent_wakes_on_state_transition(self):
        from src.colony_types.models import AgentState
        from src.integration.ecosystem_runner import EcosystemRunner

        runner = EcosystemRunner()
        agent = self._FakeAgent(AgentState.SUSPENDED)
        runner.agent_registry._agents["a1"] = agent

        self._sync(runner.orchestrator)
        self.assertTrue(runner.parked_registry.is_parked("a1"))

        agent.state = AgentState.ACTIVE
        self._sync(runner.orchestrator)
        self.assertFalse(runner.parked_registry.is_parked("a1"))

    def test_custom_park_wake_injected(self):
        from src.colony_types.models import AgentState
        from src.integration.ecosystem_runner import EcosystemRunner

        seen = []

        def wake(event):
            seen.append(event)

        runner = EcosystemRunner(park_wake=wake)
        self.assertIs(runner.orchestrator._park_wake, wake)

        agent = self._FakeAgent(AgentState.SUSPENDED)
        runner.agent_registry._agents["a1"] = agent
        self._sync(runner.orchestrator)
        self.assertEqual(seen, [])  # parking never wakes

        agent.state = AgentState.ACTIVE
        self._sync(runner.orchestrator)
        self.assertEqual(len(seen), 1)

    def test_event_bus_shared_with_registries(self):
        from src.core.event_bus import EventBus
        from src.integration.ecosystem_runner import EcosystemRunner

        bus = EventBus()
        runner = EcosystemRunner(event_bus=bus)
        self.assertIs(runner.event_bus, bus)
        self.assertIs(runner.agent_registry._event_bus, bus)
        self.assertIs(runner.parked_registry._bus, bus)
        # Caller-supplied bus reaches SessionRecorder (unchanged wiring).
        self.assertIs(runner.session_recorder._event_bus, bus)

    def test_private_bus_when_none_passed(self):
        from src.integration.ecosystem_runner import EcosystemRunner

        runner = EcosystemRunner()
        # Registries share one private bus; recorder stays bus-less exactly
        # as before the parked-lifecycle bootstrap.
        self.assertIsNotNone(runner.event_bus)
        self.assertIs(runner.agent_registry._event_bus, runner.event_bus)
        self.assertIs(runner.parked_registry._bus, runner.event_bus)
        self.assertIsNone(runner.session_recorder._event_bus)


if __name__ == "__main__":
    unittest.main()
