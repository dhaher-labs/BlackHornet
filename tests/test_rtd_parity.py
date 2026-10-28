"""Phase 5 Python/Rust parity checks (PLAN.md §4 Phase 5 exit criterion).

Unlike ``tests/test_rtd_launcher.py`` — which pins the launcher contract
against a FAKE daemon — this module drives the **real** ``bh-rtd`` release
binary through the **real** :class:`~src.integration.rtd_launcher.RtdLauncher`
and asserts the semantic contract the colony relies on under
``BH_RUNTIME=rust``: the same operations the Python flavor serves in-process
(spawn ≈ ``SubagentPool.submit``, events, listing, detach, capacity policy).

What is asserted (parity table, daemon semantics):

* ``health`` — identity (``status``/``service``/``runtime``/``protocol``),
  honest headless brain (``"none"``), the configured pool limit, and the
  Phase 5 M1 additive ``watchdog`` object;
* spawn — ``accepted: true`` with both ids (colony name + runtime UUID);
  duplicates are loud business refusals, never silent overwrites;
* capacity — ``BH_RTD_MAX_THREADS`` overflows refuse loudly
  (``accepted: false``), mirroring the Python pool's fail-loud posture;
* events — ``send_event`` reaches the thread (``events_seen`` grows) and
  events to unknown/detached agents are business refusals;
* detach — ``shutdown_thread`` reports the runtime thread id and later
  events to it are refused (hibernation semantics);
* graceful stop — the ``shutdown`` method the launcher sends actually
  exists on the real daemon, answers ``{"status": "shutting_down"}``, and
  the process exits cleanly (this caught a real contract drift: the
  launcher used to send ``host/shutdown``, which the real daemon answers
  with ``-32601`` — cleanup then relied on SIGTERM alone).

Skips honestly when the binary has not been built — run
``cd rust && cargo build --release`` (or point ``BH_RTD_BIN`` at a built
binary) to include this module. A skip names its reason; it is never a
fake pass.
"""

from __future__ import annotations

import json
import os
import socket
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import pytest

from src.integration.rtd_launcher import RtdLauncher

# ── Real binary resolution ─────────────────────────────────────────────

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BIN = REPO_ROOT / "rust" / "target" / "release" / "bh-rtd"

#: Bounded wait for async daemon state to settle (event delivery, exits).
SETTLE_TIMEOUT_S = 15.0


def _real_binary() -> str | None:
    """Resolve the real daemon binary (``BH_RTD_BIN`` > repo default)."""
    candidate = os.environ.get("BH_RTD_BIN") or str(DEFAULT_BIN)
    path = Path(candidate)
    if path.is_file() and os.access(str(path), os.X_OK):
        return str(path)
    return None


def _rpc(
    socket_path: str,
    method: str,
    params: dict[str, Any],
    timeout: float = 5.0,
) -> dict[str, Any]:
    """One protocol v1 request: ``{id, method, params}`` → ``{id, ok, ...}``."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(socket_path)
        line = json.dumps({"id": 1, "method": method, "params": params}) + "\n"
        sock.sendall(line.encode("utf-8"))
        chunks = bytearray()
        while b"\n" not in chunks:
            chunk = sock.recv(4096)
            if not chunk:
                break
            chunks.extend(chunk)
    raw = bytes(chunks).split(b"\n", 1)[0].strip()
    assert raw, "daemon closed the connection without a response line"
    response = json.loads(raw.decode("utf-8"))
    assert isinstance(response, dict), "response is not a JSON object"
    return response


def _wait_until(
    probe: Callable[[], Any | None],
    timeout: float = SETTLE_TIMEOUT_S,
) -> Any:
    """Poll *probe* until it returns a truthy value (bounded, honest)."""
    deadline = time.monotonic() + timeout
    while True:
        value = probe()
        if value:
            return value
        assert time.monotonic() < deadline, "condition not reached in time"
        time.sleep(0.05)


# ── Fixture: the real daemon via the real launcher ─────────────────────


@pytest.fixture()
def daemon(
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> Iterator[RtdLauncher]:
    """Start the real bh-rtd binary via RtdLauncher; skip if not built."""
    binary = _real_binary()
    if binary is None:
        pytest.skip(
            "real bh-rtd binary not built — run `cd rust && cargo build "
            "--release` (or set BH_RTD_BIN) to include the parity suite"
        )
    monkeypatch.setenv("BH_RUNTIME", "rust")
    max_threads = getattr(request, "param", "2")
    launcher = RtdLauncher(
        daemon_bin=binary,
        runtime_dir=str(tmp_path / "run"),
        max_threads=str(max_threads),
    )
    launcher.start()
    try:
        yield launcher
    finally:
        launcher.shutdown()


def _call(launcher: RtdLauncher, method: str, params: dict[str, Any]) -> dict[str, Any]:
    """One protocol v1 round-trip against the running daemon."""
    assert launcher.socket_path is not None, "daemon is not active"
    return _rpc(launcher.socket_path, method, params)


def _result(launcher: RtdLauncher, method: str, params: dict[str, Any]) -> dict[str, Any]:
    """One round-trip requiring ``ok: true`` and returning the result body."""
    response = _call(launcher, method, params)
    assert response.get("ok") is True, f"{method} failed: {response}"
    return response["result"]


def _threads(launcher: RtdLauncher) -> dict[str, dict[str, Any]]:
    """Current threads keyed by colony name (the addressing key)."""
    listed = _result(launcher, "list_threads", {})
    return {t["agent_id"]: t for t in listed["threads"]}


# ── Parity: health identity + honest headless posture ──────────────────


def test_health_semantics_parity(daemon: RtdLauncher) -> None:
    health = _result(daemon, "health", {})
    assert health["status"] == "ok"
    assert health["service"] == "bh-rtd"
    assert health["runtime"] == "rust"  # the flavor the gate promises
    assert health["protocol"] == 1  # BH bridge protocol v1
    assert health["brain"] == "none"  # headless is honest, never fabricated
    assert health["max_threads"] == 2  # the launcher's BH_RTD_MAX_THREADS
    assert health["threads"] == 0
    assert health["timers_pending"] == 0
    # Phase 5 M1 additive watchdog object rides along on the same wire.
    watchdog = health["watchdog"]
    assert watchdog["stall_threshold_ms"] > 0
    assert watchdog["active"] == 0
    assert watchdog["parked"] == 0
    assert watchdog["stalled"] == 0
    assert watchdog["stalled_agents"] == []


# ── Parity: spawn / send / list / business refusals ────────────────────


def test_spawn_send_list_parity(daemon: RtdLauncher) -> None:
    spawned = _result(
        daemon,
        "spawn_thread",
        {"name": "parity-worker", "role": "analyst", "instructions": "parity"},
    )
    assert spawned["accepted"] is True
    assert spawned["agent_id"] == "parity-worker"
    assert spawned["thread_id"]  # runtime UUID present
    assert spawned["thread_id"] != "parity-worker"

    # The thread is listed with the colony-visible fields (plus the
    # additive Phase 5 watchdog facts).
    entry = _wait_until(lambda: _threads(daemon).get("parity-worker"))
    assert entry["role"] == "analyst"
    assert entry["events_seen"] == 0
    assert entry["parked"] is False
    assert entry["watch_state"] in {"active", "parked", "stalled"}
    assert isinstance(entry["last_event_age_ms"], int)

    # send_event is accepted and reaches the thread (events_seen grows).
    sent = _result(
        daemon,
        "send_event",
        {
            "agent_id": "parity-worker",
            "kind": "task_assigned",
            "payload": {"task": "parity probe"},
        },
    )
    assert sent["accepted"] is True

    def _seen() -> Any | None:
        current = _threads(daemon).get("parity-worker")
        return current if current and current["events_seen"] >= 1 else None

    delivered = _wait_until(_seen)
    assert delivered["events_seen"] >= 1
    assert delivered["last_event"] != "none"

    # Business refusal: duplicate name — never a silent overwrite.
    dupe = _call(
        daemon, "spawn_thread", {"name": "parity-worker"}
    )
    assert dupe.get("ok") is True, "business refusals travel as ok:true"
    assert dupe["result"]["accepted"] is False
    assert "parity-worker" in dupe["result"]["error"]

    # Business refusal: unknown agent (the event is NOT swallowed).
    miss = _call(daemon, "send_event", {"agent_id": "ghost", "payload": "x"})
    assert miss.get("ok") is True
    assert miss["result"]["accepted"] is False
    assert "ghost" in miss["result"]["error"]


def test_capacity_refusal_is_loud(monkeypatch: pytest.MonkeyPatch) -> None:
    # Dedicated 1-slot daemon: the second spawn must refuse loudly.
    binary = _real_binary()
    if binary is None:
        pytest.skip(
            "real bh-rtd binary not built — run `cd rust && cargo build "
            "--release` (or set BH_RTD_BIN) to include the parity suite"
        )
    import tempfile

    # The launcher resolves the flavor from the PROCESS env at construction
    # (the `environ=` param only feeds the child), so set it for real.
    monkeypatch.setenv("BH_RUNTIME", "rust")

    with tempfile.TemporaryDirectory(prefix="bh-rtd-parity-") as run_dir:
        monkeyenv = dict(os.environ)
        monkeyenv["BH_RUNTIME"] = "rust"  # explicit: no ambient env reliance
        launcher = RtdLauncher(
            daemon_bin=binary,
            runtime_dir=run_dir,
            max_threads="1",
            environ=monkeyenv,
        )
        launcher.start()
        try:
            assert launcher.socket_path is not None
            first = _rpc(launcher.socket_path, "spawn_thread", {"name": "solo"})
            assert first["result"]["accepted"] is True
            second = _rpc(
                launcher.socket_path, "spawn_thread", {"name": "overflow"}
            )
            result = second["result"]
            assert result["accepted"] is False
            assert "capacity" in result["error"].lower()
            assert "1" in result["error"]  # the configured limit is named
        finally:
            launcher.shutdown()


def test_shutdown_thread_detaches_loudly(daemon: RtdLauncher) -> None:
    spawned = _result(daemon, "spawn_thread", {"name": "doomed"})
    thread_id = spawned["thread_id"]

    detached = _result(daemon, "shutdown_thread", {"agent_id": "doomed"})
    assert detached["accepted"] is True
    assert detached["thread_id"] == thread_id  # the runtime id is reported

    # The registry no longer lists it, and events to it are refused.
    assert "doomed" not in _threads(daemon)
    miss = _call(daemon, "send_event", {"agent_id": "doomed", "payload": "x"})
    assert miss["result"]["accepted"] is False
    assert "doomed" in miss["result"]["error"]


# ── Parity: the graceful-stop method the launcher sends really exists ──


def test_graceful_shutdown_method_lands(daemon: RtdLauncher) -> None:
    """``shutdown`` (the launcher's SHUTDOWN_METHOD) answers and stops.

    Guards the launcher⇄daemon contract end-to-end: the method must exist
    on the REAL daemon (unknown methods would be ``-32601``) and take the
    process down cleanly (the binary's graceful stop exits 0).
    """
    assert daemon.socket_path is not None
    assert daemon.process is not None
    response = _rpc(daemon.socket_path, "shutdown", {})
    assert response.get("ok") is True, f"shutdown refused: {response}"
    assert response["result"]["status"] == "shutting_down"

    def _exited() -> Any | None:
        proc = daemon.process
        return proc if proc is not None and proc.poll() is not None else None

    proc = _wait_until(_exited)
    assert proc.returncode == 0, (
        f"graceful stop must exit cleanly, got rc={proc.returncode}"
    )
