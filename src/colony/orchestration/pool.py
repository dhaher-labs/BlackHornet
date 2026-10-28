"""SubagentPool — concurrent execution limiter (DeerFlow pattern)."""

from __future__ import annotations

from typing import List

from src.colony.orchestration.sub_agent import Subagent, SubagentState
from src.colony.runtime_gate import spawn_subagent_on_rust


class SubagentPool:
    def __init__(self, max_concurrent: int = 10) -> None:
        if max_concurrent < 1:
            raise ValueError(
                f"max_concurrent must be >= 1, got {max_concurrent}"
            )
        self.max_concurrent = max_concurrent
        self.agents: List[Subagent] = []

    def submit(self, agent: Subagent) -> None:
        # Phase 2 runtime wedge (PLAN.md §4): with BH_RUNTIME=rust the
        # member-agent thread is spawned on the bh-rtd Rust daemon. This is
        # fail-loud by contract — an unreachable daemon raises
        # RustRuntimeError and the sub-agent is NOT silently dispatched
        # locally. Default flavor returns None and nothing changes.
        if spawn_subagent_on_rust(agent) is not None:
            # The Rust daemon owns execution; the local record just mirrors
            # the RUNNING state (dispatch below then skips it).
            agent.start()
        self.agents.append(agent)
        self._try_dispatch()

    def _try_dispatch(self) -> None:
        running = [a for a in self.agents if a.state == SubagentState.RUNNING]
        pending = [a for a in self.agents if a.state == SubagentState.PENDING]
        available = self.max_concurrent - len(running)
        for agent in pending[:available]:
            agent.start()
