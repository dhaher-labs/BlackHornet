"""UDS JSON-lines bridge server for the Jeumpa Brain (Phase 1).

Exposes :class:`src.colony.jeumpa_brain.JeumpaBrain` to the Rust
``JeumpaProvider`` (``rust/crates/bh-rtd/src/jeumpa.rs``) over a Unix domain
socket using **BH bridge protocol v1** — see ``rust/README.md`` ("BH bridge
protocol") for the full wire contract.

This file is *additive* Phase 1 "bridges before ports" work: it imports and
delegates to the real ``JeumpaBrain`` and deliberately changes nothing in
``jeumpa_brain.py``. If the brain's method names ever drift, the adaptation
happens here and only here.

Wire contract (v1)
------------------
* UTF-8 JSON, one value per ``\\n``-terminated line, **one request per
  connection**: the client connects, sends one request line, reads one
  response line, closes.
* Request:  ``{"id": <str>, "method": "<name>", "params": {...}}``
* Response: ``{"id": <echoed>, "ok": true, "result": {...}}`` or
  ``{"id": <echoed-or-null>, "ok": false,
  "error": {"code": <int>, "message": "<str>"}}``
* Error codes: ``-32700`` parse error, ``-32602`` invalid request/params,
  ``-32601`` unknown method, ``-32603`` internal error.

Methods
-------
``health``
    Liveness probe. Returns ``{"status": "ok", "service": "jeumpa-uds",
    "protocol": "v1", "stats": <brain.get_stats()>}``.
``models``
    Model catalog for Infinity's ``ModelProvider::list_models``: one entry
    per task profile in ``TASK_PROFILES`` (the brain routes by task profile;
    a profile *is* the model as far as the bridge is concerned).
``complete``
    ``params: {messages: [{role, content}...], profile: <task profile>,
    temperature?, max_tokens?, ...}`` → the ``JeumpaResult`` payload as a
    dict. A *business* failure (brain returns ``success: false``) is a
    successful protocol call: ``ok: true`` with ``result.success == false``
    and a server-declared ``result.error_class`` (``throttled`` /
    ``transient`` / ``fatal``) derived from the brain's own error strings so
    the Rust side can map them onto Infinity's ``ErrorClass`` without
    parsing messages. ``ok: false`` is reserved for bridge-level failures.

Trust model
-----------
The socket is local-only and created with ``0600`` permissions; anything
that can reach it may call the brain, so run the socket inside a
user-owned runtime directory.
"""

from __future__ import annotations

import asyncio
import dataclasses
import json
import os
import stat
from typing import Any

from src.colony.jeumpa_brain import JeumpaBrain, TASK_PROFILES
from src.config.logging_config import get_logger

logger = get_logger(__name__)

# ── Protocol constants (mirror rust/crates/bh-rtd/src/uds.rs) ──────────

PROTOCOL_VERSION = 1

CODE_PARSE_ERROR = -32700
CODE_INVALID_REQUEST = -32602
CODE_UNKNOWN_METHOD = -32601
CODE_INTERNAL_ERROR = -32603

DEFAULT_TEMPERATURE = 0.7
DEFAULT_MAX_TOKENS = 2048
# Conservative default: the keyless upstream providers accept at least 16k
# context; the real limit is provider-dependent and enforced upstream.
DEFAULT_CONTEXT_WINDOW = 16_384

# Field set surfaced for any result object (works for the real JeumpaResult
# dataclass and for duck-typed stubs in tests).
_RESULT_FIELDS = (
    "success", "content", "provider", "model",
    "latency_ms", "fallbacks_used", "error", "cached",
)


# ── Server ─────────────────────────────────────────────────────────────

class JeumpaUdsServer:
    """Asyncio UDS JSON-lines server wrapping a :class:`JeumpaBrain`.

    Usage::

        server = JeumpaUdsServer()          # or JeumpaUdsServer(brain=my_stub)
        await server.start("/run/blackhornet/jeumpa.sock")
        await server.serve_forever()        # or: await server.stop()
    """

    def __init__(self, brain: Any | None = None, socket_path: str | None = None) -> None:
        self._brain = brain if brain is not None else JeumpaBrain()
        self._socket_path = socket_path
        self._server: asyncio.AbstractServer | None = None

    # ── Lifecycle ──────────────────────────────────────────────────

    async def start(self, socket_path: str | None = None) -> str:
        """Bind the UDS server at *socket_path* (or the constructor path).

        Refuses to clobber an existing non-socket file (fail-loud — the repo
        bans silent destructive fallbacks). Returns the bound path.
        """
        path = socket_path or self._socket_path
        if not path:
            raise ValueError("socket_path required (constructor or start())")
        _remove_stale_socket(path)
        self._server = await asyncio.start_unix_server(self._handle_conn, path=path)
        os.chmod(path, 0o600)
        self._socket_path = path
        logger.info(
            "jeumpa_uds_server_started path=%s protocol=v%d", path, PROTOCOL_VERSION
        )
        return path

    async def serve_forever(self) -> None:
        """Serve until :meth:`stop` is called."""
        if self._server is None:
            raise RuntimeError("server not started")
        async with self._server:
            await self._server.serve_forever()

    async def stop(self) -> None:
        """Stop the server and remove the socket file. Idempotent."""
        if self._server is not None:
            self._server.close()
            await self._server.wait_closed()
            self._server = None
            logger.info("jeumpa_uds_server_stopped path=%s", self._socket_path)
        if self._socket_path:
            try:
                os.unlink(self._socket_path)
            except FileNotFoundError:
                pass
            except OSError as exc:  # non-fatal; server already closed
                logger.debug("jeumpa_uds_server_cleanup path=%s err=%s", self._socket_path, exc)

    # ── Connection loop ────────────────────────────────────────────

    async def _handle_conn(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            while True:
                line = await reader.readline()
                if not line:  # EOF — client closed (one request per connection)
                    break
                writer.write(await self.handle_line(line))
                await writer.drain()
        except (ConnectionResetError, BrokenPipeError):  # pragma: no cover - client vanished
            pass
        except Exception as exc:  # never let one connection kill the server
            logger.exception("jeumpa_uds_server_connection_error err=%s", exc)
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:  # pragma: no cover
                pass

    # ── Protocol plumbing (pure, unit-testable) ────────────────────

    async def handle_line(self, line: bytes | str) -> bytes:
        """Parse one request line into a response line (bytes, ``\\n``-ended)."""
        try:
            request = json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            response = _error_response(None, CODE_PARSE_ERROR, f"parse error: {exc}")
        else:
            response = await self.handle_request_async(request)
        return (json.dumps(response) + "\n").encode("utf-8")

    async def handle_request_async(self, request: Any) -> dict[str, Any]:
        """Dispatch one parsed request object to a response object."""
        if not isinstance(request, dict):
            return _error_response(None, CODE_INVALID_REQUEST, "request must be a JSON object")

        request_id = request.get("id")
        method = request.get("method")
        params = request.get("params", {})

        if not isinstance(method, str):
            return _error_response(
                request_id, CODE_INVALID_REQUEST, "'method' must be a string"
            )
        if not isinstance(params, dict):
            return _error_response(
                request_id, CODE_INVALID_REQUEST, "'params' must be an object"
            )

        try:
            if method == "health":
                result = self._health()
            elif method == "models":
                result = self._models()
            elif method == "complete":
                result = await self._complete(params)
            else:
                return _error_response(
                    request_id,
                    CODE_UNKNOWN_METHOD,
                    f"unknown method {method!r}; expected health|models|complete",
                )
        except ValueError as exc:
            return _error_response(request_id, CODE_INVALID_REQUEST, str(exc))
        except Exception as exc:
            logger.exception("jeumpa_uds_server_internal_error method=%s", method)
            return _error_response(request_id, CODE_INTERNAL_ERROR, f"internal error: {exc}")

        return {"id": request_id, "ok": True, "result": result}

    # ── Methods ────────────────────────────────────────────────────

    def _health(self) -> dict[str, Any]:
        stats: dict[str, Any] = {}
        get_stats = getattr(self._brain, "get_stats", None)
        if callable(get_stats):
            try:
                stats = get_stats()
            except Exception as exc:  # a broken brain must not hide liveness
                stats = {"error": str(exc)}
        return {
            "status": "ok",
            "service": "jeumpa-uds",
            "protocol": f"v{PROTOCOL_VERSION}",
            "stats": stats,
        }

    def _models(self) -> dict[str, Any]:
        # Task profiles are the bridge's models (routing is profile-based in
        # the brain); reading the TASK_PROFILES constant is introspection of
        # config, not a logic change.
        models = [
            {
                "model_id": name,
                "display_name": f"jeumpa/{name}",
                "context_window": DEFAULT_CONTEXT_WINDOW,
                "max_output_tokens": None,
            }
            for name in sorted(TASK_PROFILES)
        ]
        return {"models": models}

    async def _complete(self, params: dict[str, Any]) -> dict[str, Any]:
        raw_messages = params.get("messages")
        if not isinstance(raw_messages, list) or not raw_messages:
            raise ValueError("'messages' must be a non-empty list of {role, content}")
        messages: list[dict[str, str]] = []
        for entry in raw_messages:
            if (
                not isinstance(entry, dict)
                or "role" not in entry
                or "content" not in entry
            ):
                raise ValueError(
                    "each message must be an object with 'role' and 'content'"
                )
            messages.append({"role": str(entry["role"]), "content": str(entry["content"])})

        profile = params.get("profile", "default")
        if not isinstance(profile, str) or not profile:
            raise ValueError("'profile' must be a non-empty string")
        temperature = float(params.get("temperature", DEFAULT_TEMPERATURE))
        max_tokens = int(params.get("max_tokens", DEFAULT_MAX_TOKENS))

        result = await self._brain.chat_messages(
            messages=messages,
            task=profile,
            temperature=temperature,
            max_tokens=max_tokens,
        )

        payload = _result_to_dict(result)
        if not payload.get("success"):
            payload["error_class"] = _error_class_for(str(payload.get("error", "")))
        return payload


# ── Helpers ────────────────────────────────────────────────────────────

def _remove_stale_socket(path: str) -> None:
    """Remove a leftover socket file at *path*; refuse non-socket paths."""
    if not os.path.exists(path):
        return
    if stat.S_ISSOCK(os.stat(path).st_mode):
        os.unlink(path)
        return
    raise RuntimeError(f"refusing to remove non-socket path {path!r}")


def _error_response(request_id: Any, code: int, message: str) -> dict[str, Any]:
    return {
        "id": request_id,
        "ok": False,
        "error": {"code": code, "message": message},
    }


def _result_to_dict(result: Any) -> dict[str, Any]:
    """Serialise a JeumpaResult (dataclass or duck-typed stub) to a dict."""
    if dataclasses.is_dataclass(result) and not isinstance(result, type):
        return dataclasses.asdict(result)
    return {field: getattr(result, field, _DEFAULTS[field]) for field in _RESULT_FIELDS}


_DEFAULTS: dict[str, Any] = {
    "success": False,
    "content": "",
    "provider": "",
    "model": "",
    "latency_ms": 0.0,
    "fallbacks_used": [],
    "error": "result object missing field",
    "cached": False,
}


def _error_class_for(error: str) -> str:
    """Classify a brain error string into a coarse retry class.

    The brain's cooldown/timeout vocabulary is the source of truth here; the
    mapping is deliberately conservative (unknown → ``fatal``, i.e. do not
    retry) because classification is the provider's job.
    """
    lowered = (error or "").lower()
    if "rate limited" in lowered or "cooldown" in lowered or "429" in lowered:
        return "throttled"
    if "timeout" in lowered:
        return "transient"
    return "fatal"
