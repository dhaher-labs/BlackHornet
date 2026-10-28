"""Phase 3 tests — BH memory UDS peer (``src/memory/rtd_memory_server.py``).

The peer wraps the *existing* ``src/memory`` stack; these tests drive it
over a real local UDS with real SQLite files under ``tmp_path`` — no
mocking of the memory layer itself, no network. The wire contract checked
here is the one pinned by ``rust/crates/bh-rtd/tests/memory.rs``.
"""

from __future__ import annotations

import asyncio
import json
import os
import stat
from typing import Any

import pytest

from src.memory.rtd_memory_server import (
    CODE_INTERNAL_ERROR,
    CODE_INVALID_REQUEST,
    CODE_PARSE_ERROR,
    CODE_UNKNOWN_METHOD,
    RtdMemoryUdsServer,
)


# ── Fixtures / transport helpers ───────────────────────────────────────

@pytest.fixture()
def server(tmp_path):
    """A peer over real SQLite files inside ``tmp_path``."""
    return RtdMemoryUdsServer(
        data_dir=str(tmp_path / "memory"),
        sessions_db=str(tmp_path / "sessions" / "sessions.db"),
        colony_id="bh-colony",
    )


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


def _run_with_server(tmp_path, srv, coro_fn):
    """Run ``coro_fn(path)`` against a started server inside one event loop."""

    async def runner():
        path = await srv.start(str(tmp_path / "memory.sock"))
        try:
            return await coro_fn(path)
        finally:
            await srv.stop()

    return asyncio.run(runner())


def _request(method: str, params: dict[str, Any] | None = None, req_id: str = "t-1") -> dict:
    return {"id": req_id, "method": method, "params": params or {}}


async def _create_session(path: str, agent_id: str = "agent-7") -> str:
    response = await _roundtrip(
        path, _request("conversation.create", {"agent_id": agent_id}, "mk-1")
    )
    assert response["ok"] is True, response
    return response["result"]["session"]["session_id"]


# ── Lifecycle / protocol plumbing ──────────────────────────────────────

def test_server_start_creates_0600_socket_and_stop_cleans_up(tmp_path, server):
    path = str(tmp_path / "memory.sock")

    async def scenario():
        bound = await server.start(path)
        assert bound == path
        assert stat.S_ISSOCK(os.stat(path).st_mode)
        assert (os.stat(path).st_mode & 0o777) == 0o600
        await server.stop()

    asyncio.run(scenario())
    assert not os.path.exists(path), "stop() must remove the socket file"


def test_server_start_refuses_to_clobber_non_socket_path(tmp_path, server):
    blocker = tmp_path / "not-a-socket"
    blocker.write_text("precious data")
    with pytest.raises(RuntimeError, match="non-socket path"):
        asyncio.run(server.start(str(blocker)))


def test_health_roundtrip_reports_service_and_stats(tmp_path, server):
    async def scenario(path):
        return await _roundtrip(path, _request("health"))

    response = _run_with_server(tmp_path, server, scenario)
    assert response["id"] == "t-1"
    assert response["ok"] is True
    result = response["result"]
    assert result["status"] == "ok"
    assert result["service"] == "memory-uds"
    assert result["protocol"] == "v1"
    assert set(result["memory"]) >= {"stm", "ltm", "semantic", "episodic"}
    assert result["federation"]["colony_id"] == "bh-colony"


def test_unknown_method_is_a_protocol_error(tmp_path, server):
    async def scenario(path):
        return await _roundtrip(path, _request("memory.warp"))

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is False
    assert response["error"]["code"] == CODE_UNKNOWN_METHOD
    assert "unknown method" in response["error"]["message"]


def test_malformed_json_line_answers_null_id_and_connection_survives(tmp_path, server):
    async def scenario(path):
        first = await _roundtrip(path, b"{not json}\n")
        second = await _roundtrip(path, _request("health"))
        return first, second

    first, second = _run_with_server(tmp_path, server, scenario)
    assert first["ok"] is False
    assert first["id"] is None
    assert first["error"]["code"] == CODE_PARSE_ERROR
    assert second["ok"] is True, "the server must survive one bad line"


def test_non_object_params_is_an_invalid_request(tmp_path, server):
    async def scenario(path):
        return await _roundtrip(
            path, {"id": "t-2", "method": "state.get", "params": ["nope"]}
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["error"]["code"] == CODE_INVALID_REQUEST


# ── conversation.* ─────────────────────────────────────────────────────

def test_conversation_create_mints_session_id(tmp_path, server):
    async def scenario(path):
        response = await _roundtrip(
            path,
            _request("conversation.create", {"agent_id": "agent-7", "colony_id": "c1"}),
        )
        return response

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is True
    session = response["result"]["session"]
    assert session["agent_id"] == "agent-7"
    assert session["colony_id"] == "c1"
    assert session["status"] == "active"
    assert session["session_id"]


def test_conversation_append_get_roundtrip_with_infinity_window(tmp_path, server):
    async def scenario(path):
        session_id = await _create_session(path)
        append = await _roundtrip(
            path,
            _request(
                "conversation.append",
                {
                    "session_id": session_id,
                    "messages": [
                        {"role": "user", "content": "ping"},
                        {"role": "assistant", "content": "pong"},
                        {"role": "user", "content": "again"},
                    ],
                },
                "ap-1",
            ),
        )
        # Infinity leading-row-count convention: skip 1 leading, keep first 3.
        window = await _roundtrip(
            path,
            _request(
                "conversation.get",
                {"session_id": session_id, "start_from": 1, "up_to": 3},
                "get-1",
            ),
        )
        unbounded = await _roundtrip(
            path, _request("conversation.get", {"session_id": session_id}, "get-2")
        )
        return append, window, unbounded

    append, window, unbounded = _run_with_server(tmp_path, server, scenario)
    assert append["result"]["appended"] == 3
    assert [m["content"] for m in window["result"]["messages"]] == ["pong", "again"]
    assert window["result"]["count"] == 2
    assert len(unbounded["result"]["messages"]) == 3
    for message in unbounded["result"]["messages"]:
        assert {"message_id", "role", "content", "timestamp"} <= set(message)


def test_conversation_append_unknown_session_fails_loud(tmp_path, server):
    async def scenario(path):
        return await _roundtrip(
            path,
            _request(
                "conversation.append",
                {"session_id": "missing", "messages": [{"role": "user", "content": "x"}]},
            ),
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is False
    assert response["error"]["code"] == CODE_INVALID_REQUEST
    assert "unknown session" in response["error"]["message"]


def test_conversation_get_unknown_session_fails_loud_not_empty(tmp_path, server):
    async def scenario(path):
        return await _roundtrip(
            path, _request("conversation.get", {"session_id": "ghost"}, "g-1")
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["ok"] is False
    assert "unknown session" in response["error"]["message"]


def test_conversation_close_hibernates_into_federation_and_episodes(tmp_path, server):
    async def scenario(path):
        session_id = await _create_session(path, agent_id="hiber-1")
        await _roundtrip(
            path,
            _request(
                "conversation.append",
                {
                    "session_id": session_id,
                    "messages": [{"role": "user", "content": "holding 2 positions"}],
                },
            ),
        )
        closed = await _roundtrip(
            path,
            _request("conversation.close", {"session_id": session_id}, "cl-1"),
        )
        is_closed = await _roundtrip(
            path, _request("conversation.is_closed", {"session_id": session_id}, "ic-1")
        )
        recalled = await _roundtrip(
            path,
            _request(
                "federation.recall",
                {"query": "hibernated", "agent_id": "hiber-1"},
                "rc-1",
            ),
        )
        episodes = await _roundtrip(
            path, _request("memory.episodes", {"agent_id": "hiber-1"}, "ep-1")
        )
        return closed, is_closed, recalled, episodes

    closed, is_closed, recalled, episodes = _run_with_server(
        tmp_path, server, scenario
    )
    assert closed["result"]["session"]["status"] == "closed"
    assert closed["result"]["already_closed"] is False
    hibernation = closed["result"]["hibernation"]
    assert hibernation["memory_type"] == "fact"
    assert "hiber-1" in hibernation["tags"]
    assert "hibernation" in hibernation["tags"]

    assert is_closed["result"]["closed"] is True
    assert len(recalled["result"]["memories"]) == 1
    assert recalled["result"]["memories"][0]["source_agent"] == "hiber-1"
    assert any(ep["type"] == "hibernate" for ep in episodes["result"]["episodes"])


def test_conversation_close_is_idempotent_and_refuses_append_afterwards(tmp_path, server):
    async def scenario(path):
        session_id = await _create_session(path, agent_id="hiber-2")
        first = await _roundtrip(
            path, _request("conversation.close", {"session_id": session_id}, "c-1")
        )
        second = await _roundtrip(
            path, _request("conversation.close", {"session_id": session_id}, "c-2")
        )
        refused = await _roundtrip(
            path,
            _request(
                "conversation.append",
                {"session_id": session_id, "messages": [{"role": "user", "content": "x"}]},
            ),
        )
        return first, second, refused

    first, second, refused = _run_with_server(tmp_path, server, scenario)
    assert first["result"]["already_closed"] is False
    assert second["result"]["already_closed"] is True
    assert refused["ok"] is False
    assert "closed" in refused["error"]["message"]


def test_conversation_list_filters_by_agent_and_status(tmp_path, server):
    async def scenario(path):
        await _create_session(path, agent_id="a-1")
        await _create_session(path, agent_id="a-2")
        listed = await _roundtrip(
            path, _request("conversation.list", {"agent_id": "a-1"}, "l-1")
        )
        closed_status = await _roundtrip(
            path,
            _request("conversation.list", {"status": "closed"}, "l-2"),
        )
        bad_status = await _roundtrip(
            path, _request("conversation.list", {"status": "nope"}, "l-3")
        )
        return listed, closed_status, bad_status

    listed, closed_status, bad_status = _run_with_server(tmp_path, server, scenario)
    assert [s["agent_id"] for s in listed["result"]["sessions"]] == ["a-1"]
    assert closed_status["result"]["sessions"] == []
    assert bad_status["error"]["code"] == CODE_INVALID_REQUEST


# ── state.* ────────────────────────────────────────────────────────────

def test_state_set_get_delete_roundtrip_with_genuine_miss(tmp_path, server):
    async def scenario(path):
        stored = await _roundtrip(
            path,
            _request(
                "state.set",
                {
                    "key": "agent-7:position",
                    "value": {"btc": 0.5, "mode": "hibernated"},
                    "tags": ["hibernation", "agent-7"],
                    "importance": 0.9,
                },
                "s-1",
            ),
        )
        got = await _roundtrip(
            path, _request("state.get", {"key": "agent-7:position"}, "s-2")
        )
        miss = await _roundtrip(
            path, _request("state.get", {"key": "agent-7:absent"}, "s-3")
        )
        deleted = await _roundtrip(
            path, _request("state.delete", {"key": "agent-7:position"}, "s-4")
        )
        deleted_again = await _roundtrip(
            path, _request("state.delete", {"key": "agent-7:position"}, "s-5")
        )
        return stored, got, miss, deleted, deleted_again

    stored, got, miss, deleted, deleted_again = _run_with_server(
        tmp_path, server, scenario
    )
    assert stored["result"]["stored"] is True
    entry = got["result"]["entry"]
    assert got["result"]["found"] is True
    assert entry["value"] == {"btc": 0.5, "mode": "hibernated"}
    assert entry["tags"] == ["hibernation", "agent-7"]
    assert entry["importance"] == 0.9
    assert miss["result"] == {"found": False}
    assert deleted["result"]["deleted"] is True
    assert deleted_again["result"]["deleted"] is False


def test_state_set_validates_importance_and_requires_value(tmp_path, server):
    async def scenario(path):
        bad_importance = await _roundtrip(
            path,
            _request("state.set", {"key": "k", "value": 1, "importance": 1.5}, "v-1"),
        )
        missing_value = await _roundtrip(
            path, _request("state.set", {"key": "k"}, "v-2")
        )
        missing_key = await _roundtrip(
            path, _request("state.get", {}, "v-3")
        )
        return bad_importance, missing_value, missing_key

    bad_importance, missing_value, missing_key = _run_with_server(
        tmp_path, server, scenario
    )
    for response in (bad_importance, missing_value, missing_key):
        assert response["ok"] is False
        assert response["error"]["code"] == CODE_INVALID_REQUEST


# ── federation.* ───────────────────────────────────────────────────────

def test_federation_share_dedupes_identical_content(tmp_path, server):
    async def scenario(path):
        payload = {
            "content": "BTC lead follows the Asia session",
            "source_agent": "agent-3",
            "memory_type": "pattern",
            "importance": 0.7,
            "confidence": 0.6,
            "tags": ["btc"],
        }
        first = await _roundtrip(path, _request("federation.share", payload, "f-1"))
        second = await _roundtrip(path, _request("federation.share", payload, "f-2"))
        return first, second

    first, second = _run_with_server(tmp_path, server, scenario)
    assert first["result"]["memory"]["memory_id"] == second["result"]["memory"]["memory_id"]
    assert second["result"]["memory"]["agreement_count"] == 2


def test_federation_share_validates_ranges(tmp_path, server):
    async def scenario(path):
        return await _roundtrip(
            path,
            _request(
                "federation.share",
                {"content": "x", "source_agent": "a", "importance": 2.0},
                "f-3",
            ),
        )

    response = _run_with_server(tmp_path, server, scenario)
    assert response["error"]["code"] == CODE_INVALID_REQUEST


# ── Durability: a NEW peer instance over the same SQLite files ─────────

def test_persistence_survives_peer_restart_on_same_db_files(tmp_path):
    sessions_db = str(tmp_path / "sessions" / "sessions.db")
    data_dir = str(tmp_path / "memory")

    async def write_phase():
        server = RtdMemoryUdsServer(
            data_dir=data_dir, sessions_db=sessions_db, colony_id="bh-colony"
        )
        path = await server.start(str(tmp_path / "w.sock"))
        try:
            created = await _roundtrip(
                path,
                _request("conversation.create", {"agent_id": "hiber-9"}, "w-1"),
            )
            session_id = created["result"]["session"]["session_id"]
            await _roundtrip(
                path,
                _request(
                    "conversation.append",
                    {
                        "session_id": session_id,
                        "messages": [
                            {"role": "user", "content": "pre-hibernation orders"},
                            {"role": "assistant", "content": "state saved"},
                        ],
                    },
                    "w-2",
                ),
            )
            await _roundtrip(
                path,
                _request(
                    "state.set",
                    {"key": "hiber-9:portfolio", "value": {"eth": 12}},
                    "w-3",
                ),
            )
            await _roundtrip(
                path, _request("conversation.close", {"session_id": session_id}, "w-4")
            )
            return session_id
        finally:
            await server.stop()
            server.close_stores()

    session_id = asyncio.run(write_phase())

    async def read_phase():
        # Brand-new process-equivalent: fresh instances reload from SQLite.
        server = RtdMemoryUdsServer(
            data_dir=data_dir, sessions_db=sessions_db, colony_id="bh-colony"
        )
        try:
            exists = await server.handle_request(
                _request("conversation.exists", {"session_id": session_id})
            )
            history = await server.handle_request(
                _request("conversation.get", {"session_id": session_id})
            )
            closed = await server.handle_request(
                _request("conversation.is_closed", {"session_id": session_id})
            )
            state = await server.handle_request(
                _request("state.get", {"key": "hiber-9:portfolio"})
            )
            episodes = await server.handle_request(
                _request("memory.episodes", {"agent_id": "hiber-9"})
            )
            return exists, history, closed, state, episodes
        finally:
            server.close_stores()

    exists, history, closed, state, episodes = asyncio.run(read_phase())
    assert exists["result"]["exists"] is True
    contents = [m["content"] for m in history["result"]["messages"]]
    assert contents == ["pre-hibernation orders", "state saved"]
    assert closed["result"]["closed"] is True
    assert state["result"]["entry"]["value"] == {"eth": 12}
    assert any(ep["type"] == "hibernate" for ep in episodes["result"]["episodes"])
