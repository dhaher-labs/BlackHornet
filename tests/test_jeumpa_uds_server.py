"""Phase 1 tests — JeumpaBrain UDS bridge server (BH bridge protocol v1).

Everything is stubbed: the brain is a duck-typed stub (or the real
``JeumpaResult`` dataclass used as pure data), the transport is a local UDS,
and no external LLM provider is ever contacted.
"""

from __future__ import annotations

import asyncio
import json
import os
import stat
from typing import Any

import pytest

from src.colony.jeumpa_brain import JeumpaResult
from src.colony.jeumpa_uds_server import (
    CODE_INTERNAL_ERROR,
    CODE_INVALID_REQUEST,
    CODE_PARSE_ERROR,
    CODE_UNKNOWN_METHOD,
    JeumpaUdsServer,
    _error_class_for,
)


# ── Stub brain (no network, real JeumpaResult as pure data) ────────────

class StubBrain:
    """Duck-typed JeumpaBrain: records calls, returns canned results."""

    def __init__(self, result: JeumpaResult | None = None, raise_on_call: Exception | None = None):
        self.calls: list[dict[str, Any]] = []
        self._result = result or JeumpaResult(
            success=True,
            content="stub says hi",
            provider="stub",
            model="stub-model",
            latency_ms=1.5,
        )
        self._raise = raise_on_call

    async def chat_messages(self, messages=None, task="default", temperature=0.7, max_tokens=2048):
        self.calls.append(
            {
                "messages": messages,
                "task": task,
                "temperature": temperature,
                "max_tokens": max_tokens,
            }
        )
        if self._raise:
            raise self._raise
        return self._result

    def get_stats(self):
        return {"total_calls": len(self.calls), "cache_size": 0, "providers": []}


# ── Transport helpers (real UDS round-trips, localhost filesystem) ─────

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
    """Run ``coro_fn(path)`` against a started server inside one event loop."""

    async def runner():
        path = await server.start(str(tmp_path / "jeumpa.sock"))
        try:
            return await coro_fn(path)
        finally:
            await server.stop()

    return asyncio.run(runner())


# ── Tests ──────────────────────────────────────────────────────────────

def test_server_start_creates_0600_socket_and_stop_cleans_up(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain())
    path = str(tmp_path / "jeumpa.sock")

    async def scenario():
        bound = await server.start(path)
        assert bound == path
        assert os.path.exists(path)
        assert stat.S_ISSOCK(os.stat(path).st_mode)
        assert (os.stat(path).st_mode & 0o777) == 0o600
        await server.stop()

    asyncio.run(scenario())
    assert not os.path.exists(path), "stop() must remove the socket file"


def test_server_start_refuses_to_clobber_non_socket_path(tmp_path):
    blocker = tmp_path / "not-a-socket"
    blocker.write_text("precious data")
    server = JeumpaUdsServer(brain=StubBrain())
    with pytest.raises(RuntimeError, match="non-socket path"):
        asyncio.run(server.start(str(blocker)))


def test_health_method_roundtrip(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain())

    async def scenario(path):
        return await _roundtrip(path, {"id": "h-1", "method": "health", "params": {}})

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "h-1"
    assert response["ok"] is True
    assert response["result"]["status"] == "ok"
    assert response["result"]["service"] == "jeumpa-uds"
    assert response["result"]["protocol"] == "v1"
    assert response["result"]["stats"]["total_calls"] == 0


def test_models_roundtrip_lists_task_profiles(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain())

    async def scenario(path):
        return await _roundtrip(path, {"id": "m-1", "method": "models", "params": {}})

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is True
    model_ids = [m["model_id"] for m in response["result"]["models"]]
    assert "default" in model_ids and "coding" in model_ids and "reasoning" in model_ids
    for entry in response["result"]["models"]:
        assert set(entry) == {"model_id", "display_name", "context_window", "max_output_tokens"}
        assert entry["context_window"] > 0


def test_complete_roundtrip_with_stubbed_brain(tmp_path):
    brain = StubBrain()
    server = JeumpaUdsServer(brain=brain)

    async def scenario(path):
        return await _roundtrip(
            path,
            {
                "id": "c-1",
                "method": "complete",
                "params": {
                    "messages": [
                        {"role": "system", "content": "be brief"},
                        {"role": "user", "content": "hello"},
                    ],
                    "profile": "coding",
                    "temperature": 0.2,
                    "max_tokens": 64,
                },
            },
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "c-1"
    assert response["ok"] is True
    result = response["result"]
    assert result["success"] is True
    assert result["content"] == "stub says hi"
    assert result["provider"] == "stub"
    assert "error_class" not in result, "success results carry no error class"

    # The server delegated to the brain with the documented adaptation.
    assert len(brain.calls) == 1
    call = brain.calls[0]
    assert call["task"] == "coding"
    assert call["temperature"] == 0.2
    assert call["max_tokens"] == 64
    assert call["messages"] == [
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "hello"},
    ]


def test_complete_business_failure_carries_error_class(tmp_path):
    brain = StubBrain(
        result=JeumpaResult(
            success=False,
            error="All providers failed. Last error: Rate limited",
            fallbacks_used=["pollinations/openai-fast"],
        )
    )
    server = JeumpaUdsServer(brain=brain)

    async def scenario(path):
        return await _roundtrip(
            path,
            {
                "id": "c-2",
                "method": "complete",
                "params": {"messages": [{"role": "user", "content": "hi"}], "profile": "chat"},
            },
        )

    response = _run_with_server(tmp_path, server, scenario)
    # Business failure = successful protocol call with a failed outcome.
    assert response["ok"] is True
    assert response["result"]["success"] is False
    assert response["result"]["error_class"] == "throttled"


def test_complete_with_invalid_params_is_an_invalid_request_error(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain())

    async def scenario(path):
        return await _roundtrip(
            path,
            {"id": "c-3", "method": "complete", "params": {"profile": "chat"}},
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is False
    assert response["error"]["code"] == CODE_INVALID_REQUEST


def test_brain_exception_is_an_internal_error(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain(raise_on_call=RuntimeError("brain exploded")))

    async def scenario(path):
        return await _roundtrip(
            path,
            {
                "id": "c-4",
                "method": "complete",
                "params": {"messages": [{"role": "user", "content": "hi"}]},
            },
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "c-4"
    assert response["ok"] is False
    assert response["error"]["code"] == CODE_INTERNAL_ERROR
    assert "brain exploded" in response["error"]["message"]


def test_unknown_method_is_a_protocol_error(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain())

    async def scenario(path):
        return await _roundtrip(path, {"id": "u-1", "method": "teleport", "params": {}})

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "u-1"
    assert response["ok"] is False
    assert response["error"]["code"] == CODE_UNKNOWN_METHOD


def test_malformed_json_line_answers_null_id_and_connection_survives(tmp_path):
    server = JeumpaUdsServer(brain=StubBrain())

    async def scenario(path):
        reader, writer = await asyncio.open_unix_connection(path)
        writer.write(b"definitely not json\n")
        await writer.drain()
        bad = json.loads(await reader.readline())
        # The same connection still serves well-formed requests.
        writer.write((json.dumps({"id": "h-9", "method": "health", "params": {}}) + "\n").encode())
        await writer.drain()
        good = json.loads(await reader.readline())
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass
        return bad, good

    bad, good = _run_with_server(tmp_path, server, scenario)
    assert bad["id"] is None
    assert bad["ok"] is False
    assert bad["error"]["code"] == CODE_PARSE_ERROR
    assert good["ok"] is True


def test_handle_request_async_unit_without_socket():
    """The dispatch layer is testable without any transport at all."""
    server = JeumpaUdsServer(brain=StubBrain())

    health = asyncio.run(server.handle_request_async({"id": 7, "method": "health", "params": {}}))
    assert health == {
        "id": 7,
        "ok": True,
        "result": {
            "status": "ok",
            "service": "jeumpa-uds",
            "protocol": "v1",
            "stats": {"total_calls": 0, "cache_size": 0, "providers": []},
        },
    }

    missing_method = asyncio.run(server.handle_request_async({"id": 8, "params": {}}))
    assert missing_method["ok"] is False
    assert missing_method["error"]["code"] == CODE_INVALID_REQUEST

    not_an_object = asyncio.run(server.handle_request_async(["nope"]))
    assert not_an_object["ok"] is False
    assert not_an_object["error"]["code"] == CODE_INVALID_REQUEST


def test_error_class_classifier_maps_brain_vocabulary():
    assert _error_class_for("Provider pollinations in cooldown") == "throttled"
    assert _error_class_for("Rate limited") == "throttled"
    assert _error_class_for("HTTP 429 too many requests") == "throttled"
    assert _error_class_for("Timeout") == "transient"
    assert _error_class_for("All providers failed. Last error: HTTP 503") == "fatal"
    assert _error_class_for("") == "fatal"
