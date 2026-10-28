"""EcosystemRunner — bridge between the synchronous UnifiedLauncher and the
async EcosystemOrchestrator (PLAN.md K2/K4 wiring, Task 18).

The colony core (EcosystemOrchestrator) is asyncio-native: ``start()`` spawns
the 60s sync loop plus optional doc-sync/export loops as tasks. The launcher
is synchronous, so this runner owns a private event loop on a daemon thread
and drives the orchestrator across the thread boundary.

Fail-loud per PLAN F8: construction and start errors propagate to the caller;
nothing here fakes success.

Wiring scope (sanctioned by tests/memory/test_federation_ecosystem.py):
graph_store + session_recorder + memory_system. Radar/MeshBus/SelfNest stay
opt-in via ``orchestrator.wire(...)`` for deployments that want UDP peer
discovery and the HTTP mesh — the sync loop guards None components.
"""

from __future__ import annotations

import asyncio
import threading
import uuid
from typing import Any, Dict, Optional

from src.config.logging_config import get_logger

logger = get_logger(__name__)


class EcosystemRunner:
    """Owns an EcosystemOrchestrator and its asyncio loop in a daemon thread."""

    def __init__(
        self,
        graph_store: Any = None,
        memory_system: Any = None,
        event_bus: Any = None,
        colony_name: str = "blackhornet",
    ) -> None:
        from src.colony.orchestrator import EcosystemOrchestrator, OrchestratorConfig
        from src.graph.session_recorder import SessionRecorder

        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(
            target=self._run_loop, name="bh-ecosystem", daemon=True
        )
        self._started = False

        # SessionRecorder records action/observation events into the graph.
        self.session_recorder = SessionRecorder(
            graph_store=graph_store, event_bus=event_bus
        )

        config = OrchestratorConfig(
            colony_id=f"bh-{uuid.uuid4().hex[:8]}",
            colony_name=colony_name,
            # The launcher must not rewrite tracked docs or export sessions
            # as a side effect of booting — those are deliberate ops actions.
            auto_sync_docs=False,
            auto_export_sessions=False,
            data_dir="data",
        )
        self.orchestrator = EcosystemOrchestrator(config)
        self.orchestrator.wire(
            graph_store=graph_store,
            session_recorder=self.session_recorder,
            memory_system=memory_system,
        )

    def _run_loop(self) -> None:
        asyncio.set_event_loop(self._loop)
        self._loop.run_forever()

    def start(self) -> None:
        """Start the recorder, the loop thread, and the orchestrator."""
        if self._started:
            return
        self.session_recorder.start()
        self._thread.start()
        fut = asyncio.run_coroutine_threadsafe(self.orchestrator.start(), self._loop)
        fut.result(timeout=15)  # fail-loud: propagate start errors (F8)
        self._started = True
        logger.info(
            "ecosystem_runner_started colony=%s",
            self.orchestrator._config.colony_name,
        )

    def stop(self) -> None:
        """Stop the orchestrator, then the loop thread."""
        if not self._started:
            return
        try:
            fut = asyncio.run_coroutine_threadsafe(
                self.orchestrator.stop(), self._loop
            )
            fut.result(timeout=15)
        finally:
            self._loop.call_soon_threadsafe(self._loop.stop)
            self._thread.join(timeout=5)
            self._started = False
            logger.info("ecosystem_runner_stopped")

    def get_status_snapshot(self) -> Dict[str, Any]:
        """Delegate so sync consumers can treat the runner as the orchestrator."""
        return self.orchestrator.get_status_snapshot()
