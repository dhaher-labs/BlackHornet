"""Tests for src/integration/rtd_launcher.py (Phase 2: BH_RUNTIME wiring).

No network, no cargo: the "rust daemon" is a tiny executable Python script
that speaks BH bridge protocol v1 (one request per connection, JSON lines)
and answers ``ok: true`` to every method — enough to exercise the launcher's
spawn / health-check / shutdown contract without the real binary.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import textwrap
import time
from pathlib import Path
from typing import Any, Dict

import pytest

import src.integration.rtd_launcher as rtd_module
from src.integration.rtd_launcher import RtdLauncher, read_runtime_flavor

# ── Fake daemon ────────────────────────────────────────────────────────

FAKE_DAEMON_BODY = textwrap.dedent(
    """
    # Minimal bh-rtd stand-in: protocol v1, ok:true for every method.
    import json
    import os
    import signal
    import socket
    import sys

    sock_path = os.environ["BH_RTD_SOCKET"]
    state_dir = os.environ["BH_RTD_STATE_DIR"]

    # Journal what the launcher passed us (asserted by tests).
    os.makedirs(state_dir, exist_ok=True)
    env_snapshot = {
        k: v for k, v in os.environ.items() if k.startswith("BH_RTD_")
    }
    with open(os.path.join(state_dir, "env.json"), "w") as fh:
        json.dump(env_snapshot, fh)

    running = True

    def _on_term(signum, frame):
        global running
        running = False

    signal.signal(signal.SIGTERM, _on_term)

    if os.path.exists(sock_path):
        os.unlink(sock_path)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(sock_path)
    server.listen(8)
    server.settimeout(0.2)

    journal_path = os.path.join(state_dir, "methods.jsonl")
    while running:
        try:
            conn, _ = server.accept()
        except socket.timeout:
            continue
        with conn:
            conn.settimeout(5)
            buf = b""
            while not buf.endswith(b"\\n"):
                chunk = conn.recv(4096)
                if not chunk:
                    break
                buf += chunk
            if not buf:
                continue
            request = json.loads(buf.decode("utf-8"))
            with open(journal_path, "a") as fh:
                fh.write(request.get("method", "?") + "\\n")
            response = {
                "id": request.get("id"),
                "ok": True,
                "result": {"status": "ok", "agents": 0, "timers": 0},
            }
            conn.sendall((json.dumps(response) + "\\n").encode("utf-8"))
    server.close()
    os.unlink(sock_path)
    sys.exit(0)
    """
)


def _write_fake_daemon(tmp_path: Path) -> Path:
    script = tmp_path / "fake_bh_rtd.py"
    # Pin the shebang to the running interpreter: the launcher execs the
    # binary directly, and /usr/bin/env python3 must not be assumed.
    script.write_text(f"#!{sys.executable}\n" + FAKE_DAEMON_BODY, encoding="utf-8")
    script.chmod(0o755)
    return script


def _read_env_snapshot(state_dir: Path) -> Dict[str, Any]:
    with open(state_dir / "env.json", encoding="utf-8") as fh:
        return json.load(fh)


def _read_methods(state_dir: Path) -> list:
    with open(state_dir / "methods.jsonl", encoding="utf-8") as fh:
        return [line.strip() for line in fh if line.strip()]


# ── Flavor selection ───────────────────────────────────────────────────

class TestReadRuntimeFlavor:
    def test_unset_defaults_to_python(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("BH_RUNTIME", raising=False)
        assert read_runtime_flavor() == "python"

    def test_explicit_python(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("BH_RUNTIME", "python")
        assert read_runtime_flavor() == "python"

    def test_explicit_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("BH_RUNTIME", "rust")
        assert read_runtime_flavor() == "rust"

    def test_empty_counts_as_unset(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("BH_RUNTIME", "")
        assert read_runtime_flavor() == "python"

    def test_garbage_fails_closed(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("BH_RUNTIME", "hunter2")
        with pytest.raises(ValueError, match="BH_RUNTIME"):
            read_runtime_flavor()

    def test_garbage_fails_closed_at_construction(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("BH_RUNTIME", "Go")
        with pytest.raises(ValueError):
            RtdLauncher()


# ── Python flavor: disabled no-op handle ───────────────────────────────

class TestPythonFlavorNoop:
    def test_start_and_shutdown_have_zero_side_effects(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("BH_RUNTIME", "python")
        monkeypatch.delenv("BH_RTD_BIN", raising=False)

        def _forbidden(*args: Any, **kwargs: Any) -> None:
            raise AssertionError("subprocess.Popen must not run for flavor=python")

        monkeypatch.setattr(rtd_module.subprocess, "Popen", _forbidden)
        launcher = RtdLauncher()
        launcher.start()  # must be a no-op
        launcher.shutdown()  # must be a no-op

        assert launcher.flavor == "python"
        assert launcher.is_active is False
        assert launcher.process is None
        assert launcher.socket_path is None
        assert launcher.daemon_bin is None


# ── Rust flavor: fail-loud binary resolution ───────────────────────────

class TestRustBinaryResolution:
    def test_missing_bin_raises_runtimeerror(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("BH_RUNTIME", "rust")
        monkeypatch.setenv("BH_RTD_BIN", "/nonexistent/bh-rtd")
        launcher = RtdLauncher()
        with pytest.raises(RuntimeError, match="not found or not executable"):
            launcher.start()
        assert launcher.is_active is False

    def test_non_executable_bin_raises(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        not_executable = tmp_path / "bh-rtd"
        not_executable.write_text("#!/bin/sh\n", encoding="utf-8")
        assert not os.access(str(not_executable), os.X_OK)
        monkeypatch.setenv("BH_RUNTIME", "rust")
        monkeypatch.setenv("BH_RTD_BIN", str(not_executable))
        launcher = RtdLauncher()
        with pytest.raises(RuntimeError, match="not found or not executable"):
            launcher.start()


# ── Rust flavor: full lifecycle against the fake daemon ────────────────

class TestRustLifecycle:
    def test_start_health_shutdown(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        fake_bin = _write_fake_daemon(tmp_path)
        run_dir = tmp_path / "run"
        brain_sock = tmp_path / "jeumpa.sock"

        monkeypatch.setenv("BH_RUNTIME", "rust")
        monkeypatch.delenv("BH_RTD_MAX_THREADS", raising=False)
        monkeypatch.setenv("BH_RTD_BIN", str(fake_bin))

        launcher = RtdLauncher(runtime_dir=str(run_dir), brain_socket=str(brain_sock))
        launcher.start()

        try:
            assert launcher.flavor == "rust"
            assert launcher.is_active is True
            assert launcher.daemon_bin == str(fake_bin)
            assert launcher.socket_path == str(run_dir / "bh-rtd.sock")
            assert os.path.exists(str(run_dir / "bh-rtd.sock"))

            state_dir = run_dir / "state"
            env_seen = _read_env_snapshot(state_dir)
            assert env_seen["BH_RTD_SOCKET"] == str(run_dir / "bh-rtd.sock")
            assert env_seen["BH_RTD_STATE_DIR"] == str(state_dir)
            assert env_seen["BH_RTD_MAX_THREADS"] == "8"  # launcher default
            assert env_seen["BH_RTD_BRAIN_SOCKET"] == str(brain_sock)
            assert "BH_RTD_TOOLS_SOCKET" not in env_seen  # not fabricated
        finally:
            launcher.shutdown()

        # Process stopped (clean exit(0) from SIGTERM handler; negative also
        # accepted per contract for daemons without a handler).
        proc = launcher.process
        assert proc is not None
        assert proc.returncode == 0 or proc.returncode < 0

        # Cleanup: socket gone, state journals prove the protocol ran.
        assert not os.path.exists(str(run_dir / "bh-rtd.sock"))
        assert _read_methods(state_dir)[0] == "health"  # health was asked
        assert _read_methods(state_dir)[-1] == "host/shutdown"  # graceful first
        assert launcher.is_active is False

        # Idempotent shutdown.
        launcher.shutdown()
        launcher.shutdown()

    def test_accepts_caller_owned_runtime_dir(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        fake_bin = _write_fake_daemon(tmp_path)
        run_dir = tmp_path / "caller-run"
        run_dir.mkdir()

        monkeypatch.setenv("BH_RUNTIME", "rust")
        monkeypatch.setenv("BH_RTD_BIN", str(fake_bin))

        launcher = RtdLauncher(runtime_dir=str(run_dir))
        launcher.start()
        try:
            assert launcher.socket_path == str(run_dir / "bh-rtd.sock")
        finally:
            launcher.shutdown()
        # Caller-owned dir is not removed; only the socket is.
        assert run_dir.is_dir()
        assert not os.path.exists(str(run_dir / "bh-rtd.sock"))

    def test_double_start_fails_loud(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        fake_bin = _write_fake_daemon(tmp_path)
        monkeypatch.setenv("BH_RUNTIME", "rust")
        monkeypatch.setenv("BH_RTD_BIN", str(fake_bin))

        launcher = RtdLauncher(runtime_dir=str(tmp_path / "run2"))
        launcher.start()
        try:
            with pytest.raises(RuntimeError, match="already started"):
                launcher.start()
        finally:
            launcher.shutdown()
        assert launcher.is_active is False

    def test_binary_override_param_beats_env(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("BH_RUNTIME", "rust")
        monkeypatch.setenv("BH_RTD_BIN", "/nonexistent/from-env")
        launcher = RtdLauncher(daemon_bin="/nonexistent/from-param")
        # Param wins: the failure message must reference the param path.
        with pytest.raises(RuntimeError, match="from-param"):
            launcher.start()


# ── One-shot protocol client sanity (pure, no subprocess) ──────────────

class TestOneShotClient:
    def test_request_round_trip(self, tmp_path: Path) -> None:
        """_request speaks protocol v1 against a hand-rolled mini server."""
        import socket as socket_mod
        import threading

        sock_path = str(tmp_path / "mini.sock")
        server = socket_mod.socket(socket_mod.AF_UNIX, socket_mod.SOCK_STREAM)
        server.bind(sock_path)
        server.listen(1)
        seen: Dict[str, Any] = {}

        def serve() -> None:
            conn, _ = server.accept()
            with conn:
                line = b""
                while not line.endswith(b"\n"):
                    chunk = conn.recv(4096)
                    if not chunk:
                        break
                    line += chunk
                seen["request"] = json.loads(line.decode("utf-8"))
                conn.sendall(
                    (json.dumps(
                        {"id": 1, "ok": True, "result": {"status": "ok"}}
                    ) + "\n").encode("utf-8")
                )
            server.close()

        thread = threading.Thread(target=serve)
        thread.start()
        try:
            response = rtd_module._request(sock_path, "health", {})
        finally:
            thread.join(timeout=5)
        assert response["ok"] is True
        assert seen["request"]["method"] == "health"
        assert seen["request"]["id"] == 1
        assert stat.S_ISSOCK(os.stat(sock_path).st_mode)  # served over real UDS

    def test_shutdown_is_safe_when_nothing_started(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("BH_RUNTIME", "rust")
        launcher = RtdLauncher()  # never start()ed
        launcher.shutdown()  # must not raise
        assert launcher.is_active is False


# ── Guard: the fake daemon must be a well-behaved executable ───────────

def test_fake_daemon_exits_cleanly_on_sigterm(tmp_path: Path) -> None:
    fake_bin = _write_fake_daemon(tmp_path)
    assert stat.S_IMODE(fake_bin.stat().st_mode) & 0o111
    env = {
        "BH_RTD_SOCKET": str(tmp_path / "s.sock"),
        "BH_RTD_STATE_DIR": str(tmp_path / "st"),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    }
    proc = subprocess.Popen([str(fake_bin)], env=env)
    try:
        # Wait until it is serving (handler installed) before signalling,
        # otherwise SIGTERM lands during interpreter startup (default action).
        sock_file = tmp_path / "s.sock"
        deadline = 5.0
        while deadline > 0 and not sock_file.exists():
            deadline -= 0.05
            time.sleep(0.05)
        assert sock_file.exists(), "fake daemon never bound its socket"
        proc.terminate()
        assert proc.wait(timeout=5) == 0
    finally:
        if proc.poll() is None:  # pragma: no cover — only on failure
            proc.kill()
            proc.wait(timeout=5)
