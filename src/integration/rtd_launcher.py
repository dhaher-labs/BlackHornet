"""RtdLauncher — start/stop the Phase 2 ``bh-rtd`` Rust daemon subprocess.

Phase 2 ("bh-rtd") embeds infinity-agent-core inside a Rust daemon. The
daemon is NOT linked into the Python process: it runs as a child subprocess
and talks BH bridge protocol v1 over a Unix domain socket (one request per
connection, JSON lines — same envelope as ``src/colony/jeumpa_uds_server.py``
and ``src/agent_tools/uds_server.py``). This module is the only Python-side
plumbing: flavor selection + subprocess lifecycle + health check + graceful
shutdown.

Runtime flavor
--------------
``BH_RUNTIME`` selects the agent runtime:

* ``"python"`` (default; unset or empty counts as unset) — the Rust daemon
  is never started. :class:`RtdLauncher` is a disabled no-op handle with
  zero side effects: no directories, no sockets, no subprocesses.
* ``"rust"`` — opt-in. The daemon binary is resolved from (in order) the
  ``daemon_bin`` constructor argument, the ``BH_RTD_BIN`` environment
  variable, then the default ``<repo-root>/rust/target/release/bh-rtd``.
  Anything else is fail-closed: :func:`read_runtime_flavor` raises
  ``ValueError`` — the repo bans silent fallback ("no fake success").

Environment passed to the daemon subprocess
-------------------------------------------
* ``BH_RTD_SOCKET``      — control UDS path (chosen by this launcher).
* ``BH_RTD_STATE_DIR``   — directory for the timer journal.
* ``BH_RTD_MAX_THREADS`` — worker thread budget (default ``"8"``).
* ``BH_RTD_BRAIN_SOCKET`` / ``BH_RTD_TOOLS_SOCKET`` — optional paths of the
  Python bridge servers (see "Brain/tools sockets" below).

Brain/tools sockets — design decision
-------------------------------------
:class:`~src.colony.jeumpa_uds_server.JeumpaUdsServer` and
:class:`~src.agent_tools.uds_server.AgentToolsUdsServer` are asyncio
servers (``await server.start(path)`` + ``serve_forever()``) whose default
constructors eagerly build a real :class:`~src.colony.jeumpa_brain.JeumpaBrain`
or the full agent-tools registry — i.e. starting them here would force the
launcher to own extra event-loop threads and take on heavy boot-time side
effects beyond the single daemon subprocess. This launcher therefore does
NOT construct or run them itself. Instead it accepts **already-running**
server socket paths (``brain_socket=`` / ``tools_socket=`` constructor
arguments) and passes them through as ``BH_RTD_BRAIN_SOCKET`` /
``BH_RTD_TOOLS_SOCKET``. When omitted the env vars are simply not set and
the daemon runs headless — never a fabricated path.

Fail-loud semantics
-------------------
Startup failures (missing binary, dead child, socket never appears, health
check not ``ok: true``) raise ``RuntimeError`` with an actionable message;
the failed child is reaped first so no orphans are left behind. Shutdown is
best-effort and idempotent and never raises: graceful ``shutdown``
request, then ``terminate()`` → bounded wait → ``kill()``, then socket
cleanup.

Health check contract
---------------------
The health check requires exactly one thing: a protocol v1 response whose
``ok`` is ``true`` for method ``"health"``. The result body is NOT parsed
(deeper schema is the daemon's business — over-parsing here would couple
Python to Rust internals).
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import socket
import stat
import subprocess
import tempfile
import time
from collections.abc import Mapping
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# ── Contract constants ─────────────────────────────────────────────────

RUNTIME_ENV = "BH_RUNTIME"
BIN_ENV = "BH_RTD_BIN"

FLAVOR_PYTHON = "python"
FLAVOR_RUST = "rust"

#: Default daemon binary, relative to the repository root
#: (``src/integration/rtd_launcher.py`` → parents[2] is the repo root).
DEFAULT_BIN_RELATIVE = Path("rust") / "target" / "release" / "bh-rtd"

DEFAULT_MAX_THREADS = "8"

#: Bounded wait for BH_RTD_SOCKET to appear and answer "health".
HEALTH_TIMEOUT_S = 15.0

#: Graceful shutdown method on the daemon control socket. This is the real
#: daemon's method name (``bh_rtd::daemon::dispatch``); sending an unknown
#: method would be answered ``-32601`` and silently ignored here.
SHUTDOWN_METHOD = "shutdown"

_POLL_INTERVAL_S = 0.05
_REQUEST_TIMEOUT_S = 2.0


# ── Flavor selection ───────────────────────────────────────────────────

def read_runtime_flavor() -> str:
    """Read ``BH_RUNTIME`` and return ``"python"`` or ``"rust"``.

    Missing or empty/whitespace-only values count as unset and yield the
    default ``"python"``. Unknown values raise ``ValueError`` (fail-closed —
    never guess, never silently fall back).
    """
    raw = os.environ.get(RUNTIME_ENV)
    if raw is None or not raw.strip():
        return FLAVOR_PYTHON
    flavor = raw.strip()
    if flavor not in (FLAVOR_PYTHON, FLAVOR_RUST):
        raise ValueError(
            f"unsupported {RUNTIME_ENV}={raw!r}: "
            f"expected {FLAVOR_PYTHON!r} (default) or {FLAVOR_RUST!r}"
        )
    return flavor


# ── Protocol v1 one-shot client ────────────────────────────────────────

class _ProtocolError(RuntimeError):
    """Raised when the daemon's response is not a parseable v1 envelope."""


def _request(
    socket_path: str,
    method: str,
    params: Mapping[str, Any],
    timeout: float = _REQUEST_TIMEOUT_S,
) -> dict[str, Any]:
    """Send one protocol v1 request on *socket_path* and return the response.

    One request per connection, JSON lines: ``{"id", "method", "params"}``
    in, ``{"id", "ok", "result"|"error"}`` out. Deliberately dumb: callers
    decide what the response means (the health check only needs ``ok``).
    """
    request_id: int = 1
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(socket_path)
        line = json.dumps(
            {"id": request_id, "method": method, "params": dict(params)}
        ) + "\n"
        sock.sendall(line.encode("utf-8"))
        chunks = bytearray()
        while b"\n" not in chunks:
            chunk = sock.recv(4096)
            if not chunk:
                break
            chunks.extend(chunk)
    raw = bytes(chunks).split(b"\n", 1)[0].strip()
    if not raw:
        raise _ProtocolError("daemon closed connection without a response line")
    try:
        response = json.loads(raw.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise _ProtocolError(f"daemon response is not valid JSON: {exc}") from exc
    if not isinstance(response, dict):
        raise _ProtocolError("daemon response is not a JSON object")
    return response


# ── Launcher ───────────────────────────────────────────────────────────

class RtdLauncher:
    """Own the ``bh-rtd`` daemon subprocess for ``BH_RUNTIME=rust``.

    Usage::

        launcher = RtdLauncher()   # reads BH_RUNTIME; fail-closed on garbage
        launcher.start()           # no-op under flavor=python
        ...
        launcher.shutdown()        # idempotent, best-effort, never raises
    """

    def __init__(
        self,
        daemon_bin: str | None = None,
        runtime_dir: str | None = None,
        brain_socket: str | None = None,
        tools_socket: str | None = None,
        max_threads: str | None = None,
        environ: Mapping[str, str] | None = None,
    ) -> None:
        """Store configuration; resolve the runtime flavor immediately.

        Args:
            daemon_bin: Explicit daemon binary path (overrides ``BH_RTD_BIN``
                and the repo-relative default).
            runtime_dir: Directory for the control socket and state dir.
                Created if missing. When omitted a private ``tempfile``
                directory is used and removed on shutdown.
            brain_socket: Path of an already-running JeumpaUdsServer socket
                (see module docstring for why the launcher does not start it).
            tools_socket: Path of an already-running AgentToolsUdsServer socket.
            max_threads: Value for ``BH_RTD_MAX_THREADS`` (default ``"8"``,
                overridable via the inherited environment).
            environ: Environment mapping for the child process (defaults to
                ``os.environ``); kept as a parameter for tests.

        Raises:
            ValueError: ``BH_RUNTIME`` is set to an unsupported value.
        """
        self._flavor = read_runtime_flavor()
        self._daemon_bin_arg = daemon_bin
        self._runtime_dir_arg = runtime_dir
        self._brain_socket = brain_socket
        self._tools_socket = tools_socket
        self._max_threads = max_threads
        self._environ = dict(environ) if environ is not None else None

        self._proc: subprocess.Popen[bytes] | None = None
        self._active = False
        self._shut_down = False
        self._resolved_bin: str | None = None
        self._socket_path: str | None = None
        self._state_dir: str | None = None
        self._runtime_dir: str | None = None
        self._owns_runtime_dir = False

    # ── Introspection ──────────────────────────────────────────────

    @property
    def flavor(self) -> str:
        """Resolved runtime flavor: ``"python"`` or ``"rust"``."""
        return self._flavor

    @property
    def is_active(self) -> bool:
        """True while a daemon started by this handle is running."""
        return self._active

    @property
    def socket_path(self) -> str | None:
        """Control UDS path of the running daemon (None when not active)."""
        return self._socket_path if self._active else None

    @property
    def daemon_bin(self) -> str | None:
        """Resolved daemon binary path (None until a rust start resolves it)."""
        return self._resolved_bin

    @property
    def process(self) -> subprocess.Popen[bytes] | None:
        """The daemon subprocess handle, or None (python flavor / not started)."""
        return self._proc

    # ── Lifecycle ──────────────────────────────────────────────────

    def start(self) -> None:
        """Start the daemon (rust flavor) or do nothing (python flavor).

        Python flavor: disabled no-op — zero side effects. Rust flavor:
        resolve the binary (fail-loud), spawn the subprocess with the
        ``BH_RTD_*`` environment, wait up to :data:`HEALTH_TIMEOUT_S` for
        the control socket to answer ``"health"`` with ``ok: true``.

        Raises:
            RuntimeError: binary missing/not executable, child died during
                startup, socket never appeared, or health check failed.
            OSError: (propagated) runtime dir / socket setup failed.
        """
        if self._flavor != FLAVOR_RUST:
            logger.debug("rtd_launcher_disabled flavor=python")
            return
        if self._active:
            raise RuntimeError("RtdLauncher already started (is_active=True)")
        if self._shut_down:
            raise RuntimeError("RtdLauncher was shut down; construct a new one")

        binary = self._resolve_binary()
        runtime_dir = self._prepare_runtime_dir()
        self._resolved_bin = binary
        self._socket_path = os.path.join(runtime_dir, "bh-rtd.sock")
        self._state_dir = os.path.join(runtime_dir, "state")
        os.makedirs(self._state_dir, exist_ok=True)

        child_env = dict(self._environ) if self._environ is not None else dict(os.environ)
        child_env["BH_RTD_SOCKET"] = self._socket_path
        child_env["BH_RTD_STATE_DIR"] = self._state_dir
        child_env["BH_RTD_MAX_THREADS"] = (
            self._max_threads
            or child_env.get("BH_RTD_MAX_THREADS")
            or DEFAULT_MAX_THREADS
        )
        if self._brain_socket:
            child_env["BH_RTD_BRAIN_SOCKET"] = self._brain_socket
        if self._tools_socket:
            child_env["BH_RTD_TOOLS_SOCKET"] = self._tools_socket

        logger.info(
            "rtd_launcher_spawn bin=%s socket=%s state_dir=%s max_threads=%s",
            binary, self._socket_path, self._state_dir, child_env["BH_RTD_MAX_THREADS"],
        )
        self._proc = subprocess.Popen([binary], env=child_env)
        self._wait_healthy()

    def shutdown(self, timeout: float = 5.0) -> None:
        """Stop the daemon. Best-effort and idempotent; never raises.

        Sequence: graceful ``shutdown`` request (protocol v1, ignored
        on failure) → ``terminate()`` → bounded wait (*timeout* seconds) →
        ``kill()`` if still alive → control-socket cleanup.
        """
        if self._shut_down:
            return
        self._shut_down = True  # set first: guarantees idempotency on re-entry
        if self._flavor != FLAVOR_RUST:
            return

        # 1. Graceful request — best effort, the daemon may already be gone.
        if self._active and self._socket_path and os.path.exists(self._socket_path):
            try:
                _request(self._socket_path, SHUTDOWN_METHOD, {})
            except Exception as exc:  # noqa: BLE001 — best-effort by contract
                logger.warning("rtd_shutdown_request_failed err=%s", exc)

        # 2. terminate → wait → kill.
        proc = self._proc
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                logger.warning("rtd_terminate_timeout killing pid=%s", proc.pid)
                proc.kill()
                try:
                    proc.wait(timeout=2.0)
                except subprocess.TimeoutExpired:  # pragma: no cover
                    logger.error("rtd_kill_timeout pid=%s", proc.pid)

        # 3. Filesystem cleanup.
        self._cleanup_files()
        self._active = False
        logger.info("rtd_launcher_stopped")

    # ── Startup helpers ────────────────────────────────────────────

    def _resolve_binary(self) -> str:
        """Resolve the daemon binary path (arg > BH_RTD_BIN > repo default)."""
        if self._daemon_bin_arg:
            candidate = self._daemon_bin_arg
        else:
            env_bin = os.environ.get(BIN_ENV)
            if env_bin:
                candidate = env_bin
            else:
                candidate = str(
                    Path(__file__).resolve().parents[2] / DEFAULT_BIN_RELATIVE
                )
        path = Path(candidate)
        if not path.is_file() or not os.access(str(path), os.X_OK):
            raise RuntimeError(
                f"bh-rtd daemon binary not found or not executable: {path}. "
                f"Build it with 'cd rust && cargo build --release' or point "
                f"{BIN_ENV} at an existing binary."
            )
        return str(path)

    def _prepare_runtime_dir(self) -> str:
        """Create (or adopt) the runtime dir for socket + state files."""
        if self._runtime_dir_arg:
            self._owns_runtime_dir = False
            os.makedirs(self._runtime_dir_arg, exist_ok=True)
            self._runtime_dir = self._runtime_dir_arg
            return self._runtime_dir_arg
        self._owns_runtime_dir = True
        self._runtime_dir = tempfile.mkdtemp(prefix="bh-rtd-")
        return self._runtime_dir

    def _wait_healthy(self) -> None:
        """Poll for the control socket and require ``ok: true`` on "health"."""
        assert self._socket_path is not None and self._proc is not None
        deadline = time.monotonic() + HEALTH_TIMEOUT_S
        while True:
            rc = self._proc.poll()
            if rc is not None:
                self._cleanup_files()
                self._proc = None
                raise RuntimeError(
                    f"bh-rtd exited during startup (rc={rc}) before answering "
                    f"health on {self._socket_path}"
                )
            if os.path.exists(self._socket_path):
                try:
                    response = _request(
                        self._socket_path, "health", {}, timeout=_REQUEST_TIMEOUT_S
                    )
                except (OSError, _ProtocolError, ValueError) as exc:
                    # Socket file may exist a moment before the daemon starts
                    # listening — keep polling until the deadline.
                    if time.monotonic() >= deadline:
                        self._terminate_failed_startup()
                        raise RuntimeError(
                            f"bh-rtd control socket never became healthy within "
                            f"{HEALTH_TIMEOUT_S}s at {self._socket_path}: {exc}"
                        ) from exc
                    time.sleep(_POLL_INTERVAL_S)
                    continue
                # Health contract: require ok:true on "health" — nothing else.
                if response.get("ok") is True:
                    self._active = True
                    logger.info(
                        "rtd_launcher_healthy socket=%s pid=%s",
                        self._socket_path, self._proc.pid,
                    )
                    return
                self._terminate_failed_startup()
                raise RuntimeError(
                    f"bh-rtd health check returned ok=false on "
                    f"{self._socket_path}: {response.get('error')}"
                )
            if time.monotonic() >= deadline:
                self._terminate_failed_startup()
                raise RuntimeError(
                    f"BH_RTD_SOCKET did not appear within {HEALTH_TIMEOUT_S}s "
                    f"at {self._socket_path}"
                )
            time.sleep(_POLL_INTERVAL_S)

    def _terminate_failed_startup(self) -> None:
        """Reap a child whose startup failed so nothing is left behind."""
        self._cleanup_files()
        proc = self._proc
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                try:
                    proc.wait(timeout=2.0)
                except subprocess.TimeoutExpired:  # pragma: no cover
                    logger.error("rtd_kill_timeout pid=%s", proc.pid)
        self._proc = None

    def _cleanup_files(self) -> None:
        """Remove the control socket (and a launcher-owned runtime dir)."""
        if self._socket_path:
            try:
                if os.path.exists(self._socket_path) and _is_socket(
                    self._socket_path
                ):
                    os.unlink(self._socket_path)
            except OSError as exc:  # best-effort cleanup
                logger.warning("rtd_socket_cleanup_failed path=%s err=%s",
                               self._socket_path, exc)
        if self._owns_runtime_dir and self._runtime_dir:
            shutil.rmtree(self._runtime_dir, ignore_errors=True)


def _is_socket(path: str) -> bool:
    """True if *path* exists and is a Unix domain socket."""
    try:
        return stat.S_ISSOCK(os.stat(path).st_mode)
    except OSError:
        return False
