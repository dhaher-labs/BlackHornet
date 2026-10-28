"""UDS JSON-lines bridge server exposing the BH agent_tools (Phase 1).

Exposes :class:`src.agent_tools.registry.ToolRegistry` to the Rust
``bh-mcp-bridge`` client (``rust/crates/bh-mcp-bridge`` — the Phase 1
``infinity-mcp-bridge`` deliverable) over a Unix domain socket using **BH
bridge protocol v1** — see ``rust/README.md`` ("BH bridge protocol") for the
full wire contract.

This file is *additive*: it imports and reflects the existing
``src/agent_tools`` package and changes nothing in the production tool
files. Tool logic is **not** ported — execution always goes through the
existing :meth:`ToolRegistry.call` dispatch (permission check, rate limit,
circuit breaker, audit log).

Safe introspection contract
---------------------------
``tools/list`` builds the manifest from ``MCPTool`` **metadata only**
(``name`` / ``category`` / ``autonomy_level`` / input & output schemas) and
never executes a tool: no ``execute()``, no side effects, and no audit-log
entries (the registry only logs inside ``call()``). Tool classes come from
the package's ``__all__``; only concrete :class:`MCPTool` subclasses are
instantiated, each guarded so one broken tool can never take down the
inventory (fail-loud logging, never a fabricated entry).

Wire contract (v1)
------------------
Identical envelope to ``src/colony/jeumpa_uds_server.py`` (the two servers
duplicate this small plumbing on purpose — Phase 1 constraint: exactly one
new file per Python side):

* Request:  ``{"id": <str>, "method": "<name>", "params": {...}}``
* Response: ``{"id": <echoed>, "ok": true, "result": {...}}`` or
  ``{"id": <echoed-or-null>, "ok": false, "error": {"code", "message"}}``
* Codes: ``-32700`` parse, ``-32602`` invalid request/params, ``-32601``
  unknown method, ``-32603`` internal.

Methods
-------
``health``
    ``{"status": "ok", "service": "agent-tools-uds", "protocol": "v1",
    "tool_count": n, "categories": [...]}``.
``tools/list``
    ``{"tools": [{name, category, autonomy_level, input_schema,
    output_schema}, ...]}`` — metadata only, no execution.
``tools/call``
    ``params: {name, args?, context?}`` → whatever the registry returns
    (always ``ok: true`` at the protocol level; the *business* outcome is
    ``result.success`` — including registry rejections like "Tool not
    found" and "Rate limited", which arrive as ``success: false``). The
    Rust client maps ``success: false`` onto a typed error.

Trust model
-----------
The socket is local-only and created ``0600``; anything that can reach it
may invoke tools. ``params.context`` may lower or raise the calling agent's
declared autonomy level — the default is the bridge's own trusted-local
identity (``autonomy_level: 4``) because the UDS boundary itself is the
authentication. Run the socket inside a user-owned runtime directory.
"""

from __future__ import annotations

import asyncio
import json
import os
import stat
from typing import Any

import src.agent_tools as agent_tools
from src.agent_tools.base import MCPTool
from src.agent_tools.registry import ToolRegistry
from src.config.logging_config import get_logger

logger = get_logger(__name__)

# ── Protocol constants (mirror rust/crates/bh-rtd/src/uds.rs) ──────────

PROTOCOL_VERSION = 1

CODE_PARSE_ERROR = -32700
CODE_INVALID_REQUEST = -32602
CODE_UNKNOWN_METHOD = -32601
CODE_INTERNAL_ERROR = -32603

# Manifest keys exposed to the Rust bridge (metadata only — deliberately
# narrower than MCPTool.describe(), which also carries runtime health).
MANIFEST_KEYS = ("name", "category", "autonomy_level", "input_schema", "output_schema")

# Default dispatch identity for tools/call (see trust model above).
BRIDGE_AGENT_ID = "uds-mcp-bridge"
BRIDGE_AUTONOMY_LEVEL = 4


# ── Inventory helpers (safe introspection only) ────────────────────────

def concrete_tool_classes() -> list[type[MCPTool]]:
    """Return the concrete :class:`MCPTool` subclasses exported by the package.

    Pure reflection of ``src.agent_tools.__all__``: no instantiation, no
    execution, no schema work — safe to call at import time.
    """
    classes: list[type[MCPTool]] = []
    for name in agent_tools.__all__:
        obj = getattr(agent_tools, name, None)
        if (
            isinstance(obj, type)
            and issubclass(obj, MCPTool)
            and obj is not MCPTool
            and not getattr(obj, "__abstractmethods__", frozenset())
        ):
            classes.append(obj)
    return classes


def build_default_registry() -> ToolRegistry:
    """Build a registry with every concrete tool that instantiates cleanly.

    One broken tool is skipped with a warning (visible, never silent) —
    the inventory must reflect reality, not a fabricated full house.
    """
    registry = ToolRegistry()
    for tool_cls in concrete_tool_classes():
        try:
            registry.register(tool_cls())
        except Exception as exc:
            logger.warning(
                "uds_server_tool_skipped class=%s err=%s", tool_cls.__name__, exc
            )
    return registry


# ── Server ─────────────────────────────────────────────────────────────

class AgentToolsUdsServer:
    """Asyncio UDS JSON-lines server wrapping a :class:`ToolRegistry`.

    Usage::

        server = AgentToolsUdsServer()  # default registry = all concrete tools
        await server.start("/run/blackhornet/agent_tools.sock")
        await server.serve_forever()    # or: await server.stop()
    """

    def __init__(self, registry: ToolRegistry | None = None, socket_path: str | None = None) -> None:
        self._registry = registry if registry is not None else build_default_registry()
        self._socket_path = socket_path
        self._server: asyncio.AbstractServer | None = None

    # ── Lifecycle ──────────────────────────────────────────────────

    async def start(self, socket_path: str | None = None) -> str:
        """Bind the UDS server at *socket_path* (or the constructor path).

        Refuses to clobber an existing non-socket file (fail-loud). Returns
        the bound path.
        """
        path = socket_path or self._socket_path
        if not path:
            raise ValueError("socket_path required (constructor or start())")
        _remove_stale_socket(path)
        self._server = await asyncio.start_unix_server(self._handle_conn, path=path)
        os.chmod(path, 0o600)
        self._socket_path = path
        logger.info(
            "agent_tools_uds_server_started path=%s protocol=v%d tool_count=%d",
            path, PROTOCOL_VERSION, self._registry.tool_count,
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
            logger.info("agent_tools_uds_server_stopped path=%s", self._socket_path)
        if self._socket_path:
            try:
                os.unlink(self._socket_path)
            except FileNotFoundError:
                pass
            except OSError as exc:  # non-fatal; server already closed
                logger.debug("agent_tools_uds_server_cleanup path=%s err=%s", self._socket_path, exc)

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
            logger.exception("agent_tools_uds_server_connection_error err=%s", exc)
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
            elif method == "tools/list":
                result = self._tools_list()
            elif method == "tools/call":
                result = await self._tools_call(params)
            else:
                return _error_response(
                    request_id,
                    CODE_UNKNOWN_METHOD,
                    f"unknown method {method!r}; expected health|tools/list|tools/call",
                )
        except ValueError as exc:
            return _error_response(request_id, CODE_INVALID_REQUEST, str(exc))
        except Exception as exc:
            logger.exception("agent_tools_uds_server_internal_error method=%s", method)
            return _error_response(request_id, CODE_INTERNAL_ERROR, f"internal error: {exc}")

        return {"id": request_id, "ok": True, "result": result}

    # ── Methods ────────────────────────────────────────────────────

    def _health(self) -> dict[str, Any]:
        return {
            "status": "ok",
            "service": "agent-tools-uds",
            "protocol": f"v{PROTOCOL_VERSION}",
            "tool_count": self._registry.tool_count,
            "categories": self._registry.list_categories(),
        }

    def _tools_list(self) -> dict[str, Any]:
        # Safe inventory: describe() reads metadata only; nothing here can
        # execute a tool (ToolRegistry only executes inside call()).
        tools = [
            {key: described[key] for key in MANIFEST_KEYS}
            for described in self._registry.list_tools()
        ]
        return {"tools": tools}

    async def _tools_call(self, params: dict[str, Any]) -> Any:
        name = params.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError("'name' must be a non-empty string")
        args = params.get("args", {})
        if not isinstance(args, dict):
            raise ValueError("'args' must be an object")
        context = params.get("context") or {
            "agent_id": BRIDGE_AGENT_ID,
            "autonomy_level": BRIDGE_AUTONOMY_LEVEL,
        }
        if not isinstance(context, dict):
            raise ValueError("'context' must be an object")
        context.setdefault("agent_id", BRIDGE_AGENT_ID)
        context.setdefault("autonomy_level", BRIDGE_AUTONOMY_LEVEL)

        # The sanctioned dispatch path: permission check → rate limit →
        # circuit breaker → execute, all audit-logged. Business failures
        # (unknown tool, denied, breaker open, tool raised) come back as
        # {"success": False, ...} — a successful protocol call with a
        # failed outcome, which the Rust client maps onto its error type.
        return await self._registry.call(name, args, context)


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
