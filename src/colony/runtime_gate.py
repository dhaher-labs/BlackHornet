"""Runtime gate — the colony's ``BH_RUNTIME`` seam (PLAN.md §4, Phase 2).

The colony's ``spawn_thread`` seam (``SubagentPool.submit``, the
``spawn_thread``≈sub_agent pool mapping of PLAN.md §4) consults this module
to decide where a member-agent thread is executed:

* ``BH_RUNTIME`` unset/``python`` (default): the colony keeps running on
  Python exactly as before — this module is a no-op and never touches the
  network or the filesystem.
* ``BH_RUNTIME=rust``: member-agent threads are spawned on the ``bh-rtd``
  Rust runtime daemon (``infinity-agent-core`` embedded) over the BH bridge
  protocol v1 Unix domain socket. The daemon is expected to be **already
  running** — normally started by ``UnifiedLauncher`` via
  :class:`~src.integration.rtd_launcher.RtdLauncher`. If it is unreachable,
  every call fails loud with :class:`RustRuntimeError` — there is **no**
  silent fallback to the Python runtime (repo F8 rule: no fake success).

Flavor parsing is delegated to
:func:`src.integration.rtd_launcher.read_runtime_flavor` so both seams share
one fail-closed definition (unknown values raise ``ValueError``).
"""

from __future__ import annotations

import json
import logging
import os
import socket
from typing import Any, Dict, Optional

from src.integration.rtd_launcher import read_runtime_flavor

logger = logging.getLogger(__name__)

#: Environment variable holding the daemon control socket path. Required
#: when ``BH_RUNTIME=rust``; ignored otherwise.
SOCKET_ENV = "BH_RTD_SOCKET"

_DEFAULT_TIMEOUT_S = 10.0


class RustRuntimeError(RuntimeError):
    """A rust-runtime request failed (transport, protocol, or refusal).

    Raised instead of falling back: a colony that asked for the Rust
    runtime must never quietly continue on Python.
    """


def runtime_flavor() -> str:
    """``"python"`` (default) or ``"rust"``; fail-closed on unknown values."""
    return read_runtime_flavor()


def is_rust() -> bool:
    """True when the colony should route member-agent spawns to the daemon."""
    return runtime_flavor() == "rust"


def _request(socket_path: str, method: str, params: Dict[str, Any], timeout: float) -> Dict[str, Any]:
    """One BH bridge protocol v1 round trip (one request per connection)."""
    try:
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
    except OSError as exc:
        raise RustRuntimeError(
            f"bh-rtd daemon unreachable on {socket_path} ({exc}); "
            f"start it (UnifiedLauncher / RtdLauncher) or unset BH_RUNTIME=rust"
        ) from exc
    raw = bytes(chunks).split(b"\n", 1)[0].strip()
    if not raw:
        raise RustRuntimeError(
            f"bh-rtd daemon on {socket_path} closed the connection without a reply"
        )
    try:
        envelope = json.loads(raw.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise RustRuntimeError(
            f"bh-rtd daemon on {socket_path} replied with a non-JSON line: {exc}"
        ) from exc
    if envelope.get("ok") is True:
        return envelope.get("result") or {}
    error = envelope.get("error") or {}
    raise RustRuntimeError(
        f"bh-rtd daemon on {socket_path} rejected {method}: "
        f"[{error.get('code', '?')}] {error.get('message', 'unspecified')}"
    )


class RustRuntimeClient:
    """Blocking protocol v1 client for the already-running ``bh-rtd`` daemon.

    Thin on purpose: the daemon owns all thread semantics; this client only
    carries the envelope and turns every failure into a loud exception.
    """

    def __init__(self, socket_path: Optional[str] = None, timeout: float = _DEFAULT_TIMEOUT_S) -> None:
        self.socket_path = socket_path or os.environ.get(SOCKET_ENV, "").strip()
        if not self.socket_path:
            raise RustRuntimeError(
                f"{SOCKET_ENV} is required when BH_RUNTIME=rust "
                "(the bh-rtd daemon control socket path)"
            )
        self.timeout = timeout

    # ── daemon methods (BH bridge protocol v1) ────────────────────────

    def health(self) -> Dict[str, Any]:
        return self._call("health", {})

    def spawn_thread(self, name: str, role: str = "worker", instructions: str = "") -> Dict[str, Any]:
        return self._call(
            "spawn_thread", {"name": name, "role": role, "instructions": instructions}
        )

    def send_event(self, agent_id: str, payload: Any, kind: str = "custom") -> Dict[str, Any]:
        return self._call(
            "send_event", {"agent_id": agent_id, "kind": kind, "payload": payload}
        )

    def shutdown_thread(self, agent_id: str) -> Dict[str, Any]:
        return self._call("shutdown_thread", {"agent_id": agent_id})

    def list_threads(self) -> Dict[str, Any]:
        return self._call("list_threads", {})

    def _call(self, method: str, params: Dict[str, Any]) -> Dict[str, Any]:
        result = _request(self.socket_path, method, params, self.timeout)
        # Business refusals travel as ok:true + accepted:false — loud, too.
        if result.get("accepted") is False:
            raise RustRuntimeError(
                f"bh-rtd daemon refused {method}: {result.get('error', 'unspecified')}"
            )
        return result


def spawn_subagent_on_rust(subagent: Any) -> Optional[Dict[str, Any]]:
    """Spawn ``subagent`` on the Rust daemon when ``BH_RUNTIME=rust``.

    Mapping (documented Phase 2 seam, PLAN.md §4): the colony sub-agent id
    becomes the daemon thread's colony-visible name, the sub-agent task
    becomes the thread's instructions, and the role is the generic
    ``"worker"`` (colony role vocabulary lives Python-side).

    Returns ``None`` when the flavor is the default ``python`` (no-op).
    Returns the daemon's ``spawn_thread`` result otherwise. Any failure —
    unreachable daemon, protocol error, refusal — raises
    :class:`RustRuntimeError`; callers must not fall back silently.
    """
    if runtime_flavor() != "rust":
        return None
    client = RustRuntimeClient()
    result = client.spawn_thread(
        name=str(subagent.id), role="worker", instructions=str(subagent.task)
    )
    logger.info(
        "runtime_gate_spawned_on_rust agent=%s thread_id=%s",
        subagent.id,
        result.get("thread_id"),
    )
    return result
