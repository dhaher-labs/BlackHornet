"""Tests for the Phase 2 ``BH_RUNTIME`` gate (``src/colony/runtime_gate.py``)
and its wiring into the colony spawn seam (``SubagentPool.submit``).

Contract under test (PLAN.md §4, repo F8 rule):

* default (unset / ``python``) — byte-for-byte no-op: nothing dials the
  daemon, the pool behaves exactly as before;
* ``BH_RUNTIME=rust`` + live daemon — the sub-agent thread is really
  spawned through the daemon's UDS protocol (end-to-end against the real
  Rust binary when it has been built; skipped honestly otherwise);
* ``BH_RUNTIME=rust`` + unreachable daemon — **fail loud** with
  ``RustRuntimeError``, no silent fallback to local dispatch;
* unknown ``BH_RUNTIME`` values — fail closed (``ValueError``).
"""

from __future__ import annotations

import os
import unittest
from pathlib import Path
from unittest import mock

import pytest

from src.colony.orchestration.pool import SubagentPool
from src.colony.orchestration.sub_agent import Subagent, SubagentState
from src.colony.runtime_gate import (
    RustRuntimeClient,
    RustRuntimeError,
    is_rust,
    runtime_flavor,
    spawn_subagent_on_rust,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


def _find_daemon_binary() -> str | None:
    """Prefer the release binary, fall back to the debug one (cargo artifacts)."""
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "rust" / "target" / profile / "bh-rtd"
        if candidate.is_file() and os.access(str(candidate), os.X_OK):
            return str(candidate)
    return None


class TestFlavorSelection(unittest.TestCase):
    def test_default_flavor_is_python(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BH_RUNTIME", None)
            self.assertEqual(runtime_flavor(), "python")
            self.assertFalse(is_rust())

    def test_explicit_python_and_case(self):
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "python"}):
            self.assertEqual(runtime_flavor(), "python")
        # The Rust-side gate trims/accepts case-insensitively; the Python
        # side is deliberately strict lowercase (documented contract).
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "rust"}):
            self.assertTrue(is_rust())

    def test_unknown_flavor_fails_closed(self):
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "simulated"}):
            with self.assertRaises(ValueError):
                runtime_flavor()
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "PYTHON"}):
            with self.assertRaises(ValueError):
                runtime_flavor()


class TestPythonFlavorIsANoop(unittest.TestCase):
    def test_spawn_subagent_on_rust_returns_none_without_daemon(self):
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "python"}):
            agent = Subagent(id="a1", task="task1", max_retries=0, timeout=30)
            self.assertIsNone(spawn_subagent_on_rust(agent))
            self.assertEqual(agent.state, SubagentState.PENDING)

    def test_pool_submit_is_unchanged_under_default_flavor(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BH_RUNTIME", None)
            os.environ.pop("BH_RTD_SOCKET", None)
            pool = SubagentPool(max_concurrent=3)
            agent = Subagent(id="a1", task="task1", max_retries=0, timeout=30)
            pool.submit(agent)  # must not touch any socket
            self.assertIn(agent, pool.agents)
            self.assertEqual(agent.state, SubagentState.RUNNING)


class TestRustFlavorFailsLoud(unittest.TestCase):
    def test_rust_without_socket_env_fails_loud(self):
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "rust"}):
            os.environ.pop("BH_RTD_SOCKET", None)
            with self.assertRaises(RustRuntimeError) as ctx:
                RustRuntimeClient()
            self.assertIn("BH_RTD_SOCKET", str(ctx.exception))

    def test_pool_submit_with_dead_daemon_fails_loud_not_silent(self):
        dead_socket = "/nonexistent/bh-rtd-test-dead.sock"
        with mock.patch.dict(
            os.environ, {"BH_RUNTIME": "rust", "BH_RTD_SOCKET": dead_socket}
        ):
            pool = SubagentPool(max_concurrent=3)
            agent = Subagent(id="a1", task="task1", max_retries=0, timeout=30)
            with self.assertRaises(RustRuntimeError):
                pool.submit(agent)
            # Fail-loud means fail-loud: nothing was dispatched or recorded.
            self.assertEqual(pool.agents, [])
            self.assertEqual(agent.state, SubagentState.PENDING)


@pytest.mark.skipif(
    _find_daemon_binary() is None,
    reason="bh-rtd binary not built (cd rust && cargo build); "
    "the rust-flavor end-to-end path needs the real daemon",
)
class TestRustFlavorEndToEnd(unittest.TestCase):
    """Real daemon, real UDS, real pool wiring — the full Phase 2 wedge."""

    def test_pool_spawn_goes_through_daemon_end_to_end(self):
        from src.integration.rtd_launcher import RtdLauncher

        binary = _find_daemon_binary()
        assert binary is not None
        with mock.patch.dict(os.environ, {"BH_RUNTIME": "rust"}):
            launcher = RtdLauncher(daemon_bin=binary)
            try:
                launcher.start()
                assert launcher.socket_path is not None
                with mock.patch.dict(
                    os.environ,
                    {"BH_RUNTIME": "rust", "BH_RTD_SOCKET": launcher.socket_path},
                ):
                    client = RustRuntimeClient()
                    health = client.health()
                    self.assertEqual(health.get("runtime"), "rust")

                    pool = SubagentPool(max_concurrent=3)
                    agent = Subagent(id="e2e-worker", task="probe the wedge",
                                     max_retries=0, timeout=30)
                    pool.submit(agent)

                    # The daemon really owns the thread now.
                    threads = client.list_threads().get("threads", [])
                    names = {t.get("agent_id") for t in threads}
                    self.assertIn("e2e-worker", names)

                    # Local record mirrors RUNNING (daemon owns execution).
                    self.assertEqual(agent.state, SubagentState.RUNNING)

                    # Duplicate colony name is a loud refusal, not a fake.
                    with self.assertRaises(RustRuntimeError):
                        pool.submit(
                            Subagent(id="e2e-worker", task="dupe",
                                     max_retries=0, timeout=30)
                        )

                    # shutdown_thread detaches cleanly on the daemon side.
                    result = client.shutdown_thread("e2e-worker")
                    self.assertTrue(result.get("accepted"))
            finally:
                launcher.shutdown()

        # The launcher stopped the daemon; the socket is gone.
        self.assertFalse(launcher.is_active)


if __name__ == "__main__":
    unittest.main()
