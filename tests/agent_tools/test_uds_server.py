"""Phase 1 tests — agent_tools UDS bridge server (BH bridge protocol v1).

Real tool logic is never exercised: tests run against a registry containing
only a trivial ``EchoTool`` (and a raising tool for failure mapping) defined
right here. No network, no shell, no filesystem side effects. A separate
test proves the *default* registry build is safe introspection: listing
produces no execution (the registry's audit log stays empty).
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from src.agent_tools.base import MCPTool
from src.agent_tools.registry import ToolRegistry
from src.agent_tools.uds_server import (
    CODE_INVALID_REQUEST,
    CODE_PARSE_ERROR,
    CODE_UNKNOWN_METHOD,
    AgentToolsUdsServer,
    build_default_registry,
    concrete_tool_classes,
)


# ── Trivial test tools (the only tools executed by this module) ────────

class EchoTool(MCPTool):
    """Returns its input; the test suite's stand-in for a real tool."""

    def name(self) -> str:
        return "test.echo"

    def category(self) -> str:
        return "test"

    def autonomy_level(self) -> int:
        return 0

    def input_schema(self):
        return {
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
        }

    def output_schema(self):
        return {"type": "object", "properties": {"echoed": {"type": "string"}}}

    async def execute(self, params, context):
        return {"success": True, "echoed": params.get("message", "")}


class ExplodingTool(MCPTool):
    """Always raises — exercises the registry's exception mapping."""

    def name(self) -> str:
        return "test.explode"

    def category(self) -> str:
        return "test"

    def autonomy_level(self) -> int:
        return 0

    def input_schema(self):
        return {"type": "object"}

    def output_schema(self):
        return {"type": "object"}

    async def execute(self, params, context):
        raise RuntimeError("tool exploded")


def _make_server() -> AgentToolsUdsServer:
    registry = ToolRegistry()
    registry.register(EchoTool())
    registry.register(ExplodingTool())
    return AgentToolsUdsServer(registry=registry)


# ── Transport helpers ──────────────────────────────────────────────────

async def _roundtrip(path: str, payload: Any) -> dict[str, Any]:
    reader, writer = await asyncio.open_unix_connection(path)
    if isinstance(payload, bytes):
        writer.write(payload)
    else:
        writer.write((json.dumps(payload) + "\n").encode("utf-8"))
    await writer.drain()
    line = await reader.readline()
    writer.close()
    try:
        await writer.wait_closed()
    except Exception:
        pass
    return json.loads(line)


def _run_with_server(tmp_path, server, coro_fn):
    async def runner():
        path = await server.start(str(tmp_path / "agent_tools.sock"))
        try:
            return await coro_fn(path)
        finally:
            await server.stop()

    return asyncio.run(runner())


# ── Tests ──────────────────────────────────────────────────────────────

def test_health_roundtrip(tmp_path):
    server = _make_server()

    async def scenario(path):
        return await _roundtrip(path, {"id": "h-1", "method": "health", "params": {}})

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "h-1"
    assert response["ok"] is True
    result = response["result"]
    assert result["status"] == "ok"
    assert result["service"] == "agent-tools-uds"
    assert result["protocol"] == "v1"
    assert result["tool_count"] == 2
    assert result["categories"] == ["test"]


def test_tools_list_returns_metadata_only_manifest(tmp_path):
    server = _make_server()

    async def scenario(path):
        return await _roundtrip(path, {"id": "l-1", "method": "tools/list", "params": {}})

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is True
    tools = response["result"]["tools"]
    names = {t["name"] for t in tools}
    assert names == {"test.echo", "test.explode"}
    for tool in tools:
        assert set(tool) == {
            "name",
            "category",
            "autonomy_level",
            "input_schema",
            "output_schema",
        }, "manifest is metadata-only"
    echo = next(t for t in tools if t["name"] == "test.echo")
    assert echo["category"] == "test"
    assert echo["autonomy_level"] == 0
    assert echo["input_schema"]["required"] == ["message"]


def test_tools_list_executes_nothing(tmp_path):
    """Safe introspection: listing must not leave audit-log entries."""
    registry = ToolRegistry()
    echo = EchoTool()
    registry.register(echo)
    server = AgentToolsUdsServer(registry=registry)

    async def scenario(path):
        first = await _roundtrip(path, {"id": "l-2", "method": "tools/list", "params": {}})
        second = await _roundtrip(path, {"id": "l-3", "method": "tools/list", "params": {}})
        return first, second

    first, second = _run_with_server(tmp_path, server, scenario)
    assert first["result"] == second["result"], "listing is pure"
    assert registry.get_audit_log() == [], "no tool execution at listing time"
    assert echo.health_check() is True


def test_tools_call_roundtrip(tmp_path):
    server = _make_server()

    async def scenario(path):
        return await _roundtrip(
            path,
            {
                "id": "x-1",
                "method": "tools/call",
                "params": {"name": "test.echo", "args": {"message": "ping"}},
            },
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "x-1"
    assert response["ok"] is True
    assert response["result"]["success"] is True
    assert response["result"]["echoed"] == "ping"


def test_tools_call_unknown_tool_is_business_failure_not_protocol_error(tmp_path):
    server = _make_server()

    async def scenario(path):
        return await _roundtrip(
            path,
            {"id": "x-2", "method": "tools/call", "params": {"name": "nope.execute", "args": {}}},
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is True, "the call itself succeeded; the outcome failed"
    assert response["result"]["success"] is False
    assert "Tool not found" in response["result"]["error"]


def test_tools_call_raising_tool_maps_to_registry_error_payload(tmp_path):
    server = _make_server()

    async def scenario(path):
        return await _roundtrip(
            path,
            {"id": "x-3", "method": "tools/call", "params": {"name": "test.explode", "args": {}}},
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is True
    assert response["result"]["success"] is False
    assert response["result"]["error_code"] == -32603
    assert "tool exploded" in response["result"]["error"]


def test_tools_call_with_invalid_params_is_invalid_request(tmp_path):
    server = _make_server()

    async def scenario(path):
        missing_name = await _roundtrip(
            path, {"id": "x-4", "method": "tools/call", "params": {"args": {}}}
        )
        bad_args = await _roundtrip(
            path,
            {"id": "x-5", "method": "tools/call", "params": {"name": "test.echo", "args": [1]}},
        )
        return missing_name, bad_args

    missing_name, bad_args = _run_with_server(tmp_path, server, scenario)
    assert missing_name["error"]["code"] == CODE_INVALID_REQUEST
    assert bad_args["error"]["code"] == CODE_INVALID_REQUEST


def test_unknown_method_and_malformed_line(tmp_path):
    server = _make_server()

    async def scenario(path):
        unknown = await _roundtrip(path, {"id": "u-1", "method": "tools/delete", "params": {}})
        bad = await _roundtrip(path, b"{{{not json\n")
        return unknown, bad

    unknown, bad = _run_with_server(tmp_path, server, scenario)
    assert unknown["ok"] is False
    assert unknown["error"]["code"] == CODE_UNKNOWN_METHOD
    assert bad["id"] is None
    assert bad["error"]["code"] == CODE_PARSE_ERROR


def test_concrete_tool_class_reflection_is_pure():
    classes = concrete_tool_classes()
    assert classes, "the package exports concrete MCPTool subclasses"
    for cls in classes:
        assert issubclass(cls, MCPTool) and cls is not MCPTool
        assert not getattr(cls, "__abstractmethods__", frozenset())


def test_build_default_registry_is_safe_and_lists_without_execution():
    registry = build_default_registry()
    assert registry.tool_count >= 1, "at least the always-safe tools register"

    server = AgentToolsUdsServer(registry=registry)
    listing = asyncio.run(
        server.handle_request_async({"id": 1, "method": "tools/list", "params": {}})
    )
    assert listing["ok"] is True
    tools = listing["result"]["tools"]
    assert tools
    for tool in tools:
        assert set(tool) == {
            "name",
            "category",
            "autonomy_level",
            "input_schema",
            "output_schema",
        }
        assert isinstance(tool["autonomy_level"], int)
        assert 0 <= tool["autonomy_level"] <= 4
    # Nothing was executed: the registry only writes audit entries in call().
    assert registry.get_audit_log() == []
