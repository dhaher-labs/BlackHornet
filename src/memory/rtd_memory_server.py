"""BH memory UDS peer — Phase 3 "memory seam" server side (Task 19-b).

Exposes the **existing** :mod:`src.memory` stack to the Rust
``bh-rtd`` memory seam client (``rust/crates/bh-rtd/src/memory.rs``) over a
Unix domain socket using **BH bridge protocol v1** — the same wire contract
as ``src/colony/jeumpa_uds_server.py`` and ``src/agent_tools/uds_server.py``
(the small plumbing duplication across the three peers is deliberate:
Phase 1 constraint, exactly one new file per Python side).

Nothing is ported and no storage is re-implemented here. Every method is a
thin adapter over an API that already ships:

=======================  ==================================================
Wire method              Wraps (existing API)
=======================  ==================================================
``conversation.create``  :meth:`SessionMemory.create_session`
``conversation.exists``  :meth:`SessionMemory.load_session`
``conversation.append``  :meth:`SessionMemory.add_message` (batch)
``conversation.get``     :meth:`SessionMemory.get_messages` + Infinity
                         leading-row-count window slicing (peer-side)
``conversation.is_closed``  session ``status == "closed"``
``conversation.close``   :meth:`SessionMemory.close_session` **+**
                         :meth:`MemoryFederation.share_memory` hibernation
                         record **+** :meth:`MemorySystem.record_episode`
``conversation.list``    :meth:`SessionMemory.list_sessions`
``state.set/get/delete`` :class:`MemorySystem` LTM (:class:`LongTermMemory`,
                         durable SQLite + FTS5)
``federation.share``     :meth:`MemoryFederation.share_memory`
``federation.recall``    :meth:`MemoryFederation.recall`
``memory.episodes``      :meth:`MemorySystem.get_episodes` (episodic SQLite)
``health``               :meth:`MemorySystem.stats` +
                         :meth:`MemoryFederation.get_stats`
=======================  ==================================================

Wire contract (v1)
------------------
* Request:  ``{"id": <str>, "method": "<name>", "params": {...}}``
* Response: ``{"id": <echo>, "ok": true, "result": {...}}`` or
  ``{"id": <echo-or-null>, "ok": false, "error": {"code", "message"}}``
* Codes: ``-32700`` parse, ``-32602`` invalid request/params (including
  *unknown session* / *closed session* / *malformed entry* — the Rust seam
  maps these onto typed errors and never invents empty data), ``-32601``
  unknown method, ``-32603`` internal.

Persistence semantics (honest by construction)
----------------------------------------------
* Conversations and state are **durable**: ``SessionMemory`` rows and the
  LTM live in SQLite under ``--sessions-db`` / ``--data-dir`` and survive a
  peer restart (proven end-to-end by ``rust/crates/bh-rtd/tests/memory_e2e.rs``).
* ``MemoryFederation`` records are the *live cross-agent layer* — they live
  in the peer process (that is the shipped design of
  ``memory_federation.py``) and do **not** survive a restart. The durable
  hibernation trail is the closed session + LTM state entries + the
  ``hibernate`` episode, all SQLite.

Post-MVP (documented, NOT faked)
--------------------------------
**ChromaDB read-through** (PLAN.md §4 Phase 3) is not wired here: the peer
never touches :mod:`src.memory.vector` / Chroma, and the Rust client claims
no vector recall. Wiring a fake success would violate the repo's no-fake
rule; it lands when a real Chroma deployment exists.

Trust model
-----------
Local-only UDS created ``0600``; anything that can reach the socket may
read/write BH memory. Run it inside a user-owned runtime directory.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import stat
from typing import Any

from src.config.logging_config import get_logger
from src.memory.memory_federation import MemoryFederation
from src.memory.session import SessionMemory
from src.memory.system import MemorySystem, MemorySystemConfig

logger = get_logger(__name__)

# ── Protocol constants (mirror rust/crates/bh-bridge-uds) ──────────────

PROTOCOL_VERSION = 1

CODE_PARSE_ERROR = -32700
CODE_INVALID_REQUEST = -32602
CODE_UNKNOWN_METHOD = -32601
CODE_INTERNAL_ERROR = -32603

SERVICE_NAME = "memory-uds"

# Session statuses that accept appended messages (SessionMemory rule).
_APPENDABLE_STATUSES = ("active", "compacted")


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


# ── Param validation helpers (fail-loud, -32602) ───────────────────────

def _require_str(params: dict[str, Any], key: str, *, allow_empty: bool = False) -> str:
    value = params.get(key)
    if not isinstance(value, str) or (not allow_empty and not value):
        raise ValueError(f"'{key}' must be a non-empty string")
    return value


def _opt_str(params: dict[str, Any], key: str) -> str:
    """Optional string that normalises missing/null to ``''`` (the Rust
    client sends ``""`` for absent filters)."""
    value = params.get(key)
    if value is None:
        return ""
    if not isinstance(value, str):
        raise ValueError(f"'{key}' must be a string or null")
    return value


def _opt_int(params: dict[str, Any], key: str) -> int | None:
    value = params.get(key)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"'{key}' must be an integer or null")
    return value


def _opt_float(params: dict[str, Any], key: str) -> float | None:
    value = params.get(key)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"'{key}' must be a number or null")
    return float(value)


def _opt_tags(params: dict[str, Any], key: str = "tags") -> list[str]:
    value = params.get(key)
    if value is None:
        return []
    if not isinstance(value, list) or not all(isinstance(t, str) for t in value):
        raise ValueError(f"'{key}' must be an array of strings or null")
    return value


def _federated_memory_dump(memory: Any) -> dict[str, Any]:
    """Serialise a ``FederatedMemory`` dataclass for the wire (the Rust
    client decodes a strict subset; extra keys are ignored there)."""
    return {
        "memory_id": memory.memory_id,
        "content": memory.content,
        "source_agent": memory.source_agent,
        "memory_type": memory.memory_type,
        "importance": memory.importance,
        "confidence": memory.confidence,
        "tags": list(memory.tags),
        "agreement_count": memory.agreement_count,
        "access_count": memory.access_count,
        "created_at": memory.created_at,
        "last_accessed": memory.last_accessed,
    }


# ── Server ─────────────────────────────────────────────────────────────

class RtdMemoryUdsServer:
    """Asyncio UDS JSON-lines server over the BH memory stack.

    Usage::

        server = RtdMemoryUdsServer(data_dir="~/.bh/memory")
        await server.start("/run/blackhornet/memory.sock")
        await server.serve_forever()   # or: await server.stop()
    """

    def __init__(
        self,
        memory: MemorySystem | None = None,
        sessions: SessionMemory | None = None,
        federation: MemoryFederation | None = None,
        data_dir: str = "~/.bh/memory",
        sessions_db: str = "~/.bh/sessions/sessions.db",
        colony_id: str = "bh-colony",
    ) -> None:
        self._memory = memory if memory is not None else MemorySystem(
            config=MemorySystemConfig(data_dir=data_dir)
        )
        self._sessions = sessions if sessions is not None else SessionMemory(
            db_path=sessions_db
        )
        self._federation = federation if federation is not None else MemoryFederation(
            colony_id=colony_id
        )
        self._socket_path: str | None = None
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
            "rtd_memory_uds_server_started path=%s protocol=v%d service=%s",
            path, PROTOCOL_VERSION, SERVICE_NAME,
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
            logger.info("rtd_memory_uds_server_stopped path=%s", self._socket_path)
        if self._socket_path:
            try:
                os.unlink(self._socket_path)
            except FileNotFoundError:
                pass
            except OSError as exc:
                logger.debug("rtd_memory_uds_server_cleanup path=%s err=%s",
                             self._socket_path, exc)

    def close_stores(self) -> None:
        """Flush/close the underlying SQLite handles (call at shutdown)."""
        self._sessions.close()
        self._memory.close()

    # ── Connection loop ────────────────────────────────────────────

    async def _handle_conn(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        try:
            while True:
                line = await reader.readline()
                if not line:  # EOF — client closed (one request per connection)
                    break
                writer.write(await self.handle_line(line))
                await writer.drain()
        except (ConnectionResetError, BrokenPipeError):  # pragma: no cover
            pass
        except Exception as exc:  # never let one connection kill the server
            logger.exception("rtd_memory_uds_server_connection_error err=%s", exc)
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
            response = await self.handle_request(request)
        return (json.dumps(response) + "\n").encode("utf-8")

    async def handle_request(self, request: Any) -> dict[str, Any]:
        """Dispatch one parsed request object to a response object."""
        if not isinstance(request, dict):
            return _error_response(None, CODE_INVALID_REQUEST, "request must be a JSON object")

        request_id = request.get("id")
        method = request.get("method")
        params = request.get("params", {})

        if not isinstance(method, str):
            return _error_response(request_id, CODE_INVALID_REQUEST, "'method' must be a string")
        if not isinstance(params, dict):
            return _error_response(request_id, CODE_INVALID_REQUEST, "'params' must be an object")

        try:
            if method == "health":
                result = self._health()
            elif method == "conversation.create":
                result = await self._conversation_create(params)
            elif method == "conversation.exists":
                result = await self._conversation_exists(params)
            elif method == "conversation.append":
                result = await self._conversation_append(params)
            elif method == "conversation.get":
                result = await self._conversation_get(params)
            elif method == "conversation.is_closed":
                result = await self._conversation_is_closed(params)
            elif method == "conversation.close":
                result = await self._conversation_close(params)
            elif method == "conversation.list":
                result = self._conversation_list(params)
            elif method == "state.set":
                result = self._state_set(params)
            elif method == "state.get":
                result = self._state_get(params)
            elif method == "state.delete":
                result = self._state_delete(params)
            elif method == "federation.share":
                result = self._federation_share(params)
            elif method == "federation.recall":
                result = self._federation_recall(params)
            elif method == "memory.episodes":
                result = self._memory_episodes(params)
            else:
                return _error_response(
                    request_id,
                    CODE_UNKNOWN_METHOD,
                    f"unknown method {method!r}; expected health|conversation.*|state.*|"
                    "federation.*|memory.episodes",
                )
        except ValueError as exc:
            return _error_response(request_id, CODE_INVALID_REQUEST, str(exc))
        except Exception as exc:
            logger.exception("rtd_memory_uds_server_internal_error method=%s", method)
            return _error_response(request_id, CODE_INTERNAL_ERROR, f"internal error: {exc}")
        return {"id": request_id, "ok": True, "result": result}

    # ── health ─────────────────────────────────────────────────────

    def _health(self) -> dict[str, Any]:
        return {
            "status": "ok",
            "service": SERVICE_NAME,
            "protocol": f"v{PROTOCOL_VERSION}",
            "memory": self._memory.stats(),
            "federation": self._federation.get_stats(),
        }

    # ── conversation.* (SessionMemory — durable SQLite) ────────────

    async def _load_session_or_error(self, session_id: str) -> Any:
        session = await self._sessions.load_session(session_id)
        if session is None:
            raise ValueError(f"unknown session {session_id!r}")
        return session

    async def _conversation_create(self, params: dict[str, Any]) -> dict[str, Any]:
        agent_id = _opt_str(params, "agent_id")
        colony_id = _opt_str(params, "colony_id") or self._federation.colony_id
        metadata = params.get("metadata") or {}
        if not isinstance(metadata, dict):
            raise ValueError("'metadata' must be an object or null")
        session = await self._sessions.create_session(
            agent_id=agent_id, colony_id=colony_id, metadata=metadata
        )
        return {"session": session.model_dump()}

    async def _conversation_exists(self, params: dict[str, Any]) -> dict[str, Any]:
        session_id = _require_str(params, "session_id")
        session = await self._sessions.load_session(session_id)
        return {"exists": session is not None}

    async def _conversation_append(self, params: dict[str, Any]) -> dict[str, Any]:
        session_id = _require_str(params, "session_id")
        messages = params.get("messages")
        if not isinstance(messages, list) or not messages:
            raise ValueError("'messages' must be a non-empty array")
        session = await self._load_session_or_error(session_id)
        if session.status not in _APPENDABLE_STATUSES:
            raise ValueError(
                f"session {session_id!r} is {session.status!r}; append refused"
            )

        appended = 0
        for entry in messages:
            if not isinstance(entry, dict):
                raise ValueError("each message must be an object")
            role = entry.get("role")
            content = entry.get("content")
            metadata = entry.get("metadata") or {}
            token_count = entry.get("token_count")
            if not isinstance(role, str) or not role:
                raise ValueError("message 'role' must be a non-empty string")
            if not isinstance(content, str):
                raise ValueError("message 'content' must be a string")
            if not isinstance(metadata, dict):
                raise ValueError("message 'metadata' must be an object or null")
            if token_count is not None and (
                isinstance(token_count, bool) or not isinstance(token_count, int)
            ):
                raise ValueError("message 'token_count' must be an integer or null")
            saved = await self._sessions.add_message(
                session_id,
                role,
                content,
                metadata=metadata,
                token_count=token_count,
            )
            if saved is None:
                # SessionMemory refused ( vanished / closed mid-batch ).
                raise ValueError(
                    f"session {session_id!r} refused message {appended + 1}"
                )
            appended += 1
        return {"appended": appended}

    async def _conversation_get(self, params: dict[str, Any]) -> dict[str, Any]:
        session_id = _require_str(params, "session_id")
        start_from = _opt_int(params, "start_from")
        up_to = _opt_int(params, "up_to")
        if start_from is not None and start_from < 0:
            raise ValueError("'start_from' must be >= 0")
        if up_to is not None and up_to < 0:
            raise ValueError("'up_to' must be >= 0")
        await self._load_session_or_error(session_id)

        messages = await self._sessions.get_messages(session_id)
        # Infinity leading-row-count convention (see
        # infinity-agent-core 0.2.0 traits::ConversationStore::load_history_up_to):
        # start_from skips that many leading messages, up_to truncates to
        # that many leading messages; both None = unbounded.
        start = start_from if start_from is not None else 0
        end = up_to if up_to is not None else len(messages)
        window = messages[start:end] if end > start else []
        return {"messages": [m.model_dump() for m in window], "count": len(window)}

    async def _conversation_is_closed(self, params: dict[str, Any]) -> dict[str, Any]:
        session_id = _require_str(params, "session_id")
        session = await self._load_session_or_error(session_id)
        return {"closed": session.status == "closed"}

    async def _conversation_close(self, params: dict[str, Any]) -> dict[str, Any]:
        session_id = _require_str(params, "session_id")
        note = _opt_str(params, "note")
        session = await self._load_session_or_error(session_id)
        agent_id = session.agent_id or "unknown-agent"

        messages = await self._sessions.get_messages(session_id)
        if note:
            summary = note
        elif messages:
            summary = (
                f"agent {agent_id} hibernated: session {session_id} closed with "
                f"{len(messages)} messages; last: {messages[-1].content[:160]!r}"
            )
        else:
            summary = f"agent {agent_id} hibernated: session {session_id} closed (no messages)"

        already_closed = session.status == "closed"
        if not already_closed:
            await self._sessions.close_session(session_id)

        # Exit-criterion trail: federation record (live layer) + durable
        # episodic record (SQLite).
        federated = self._federation.share_memory(
            content=summary,
            source_agent=agent_id,
            memory_type="fact",
            importance=0.8,
            confidence=0.5,
            tags=["hibernation", agent_id, session_id],
        )
        self._memory.record_episode(agent_id, "hibernate", summary)

        refreshed = await self._sessions.load_session(session_id)
        return {
            "session": refreshed.model_dump() if refreshed else session.model_dump(),
            "hibernation": _federated_memory_dump(federated),
            "already_closed": already_closed,
        }

    def _conversation_list(self, params: dict[str, Any]) -> dict[str, Any]:
        agent_id = _opt_str(params, "agent_id") or None
        status = _opt_str(params, "status") or None
        if status is not None and status not in ("active", "compacted", "archived", "closed"):
            raise ValueError(
                "'status' must be one of active|compacted|archived|closed or null"
            )
        return {"sessions": self._sessions.list_sessions(agent_id=agent_id, status=status)}

    # ── state.* (MemorySystem LTM — durable SQLite + FTS5) ─────────

    def _state_set(self, params: dict[str, Any]) -> dict[str, Any]:
        key = _require_str(params, "key")
        if "value" not in params:
            raise ValueError("'value' is required")
        value = params["value"]
        tags = _opt_tags(params)
        importance = _opt_float(params, "importance")
        if importance is not None and not 0.0 <= importance <= 1.0:
            raise ValueError("'importance' must be within [0.0, 1.0]")
        content = json.dumps(value, sort_keys=True, ensure_ascii=False)
        entry_id = self._memory.store(
            key, content, tags=tags, importance=0.5 if importance is None else importance
        )
        return {"stored": True, "key": key, "id": entry_id}

    def _state_get(self, params: dict[str, Any]) -> dict[str, Any]:
        key = _require_str(params, "key")
        entry = self._memory.ltm.get(key)
        if entry is None:
            return {"found": False}
        try:
            value = json.loads(entry.content)
        except (json.JSONDecodeError, TypeError):
            # LTM is a generic text store; non-JSON content is returned
            # verbatim rather than faked into a shape it does not have.
            value = entry.content
        return {
            "found": True,
            "entry": {
                "id": entry.id,
                "key": entry.key,
                "value": value,
                "tags": list(entry.tags),
                "importance": entry.importance,
            },
        }

    def _state_delete(self, params: dict[str, Any]) -> dict[str, Any]:
        key = _require_str(params, "key")
        return {"deleted": self._memory.ltm.remove(key)}

    # ── federation.* (MemoryFederation — live cross-agent layer) ───

    def _federation_share(self, params: dict[str, Any]) -> dict[str, Any]:
        content = _require_str(params, "content")
        source_agent = _require_str(params, "source_agent")
        memory_type = _opt_str(params, "memory_type") or "insight"
        importance = _opt_float(params, "importance")
        confidence = _opt_float(params, "confidence")
        tags = _opt_tags(params)
        if importance is not None and not 0.0 <= importance <= 1.0:
            raise ValueError("'importance' must be within [0.0, 1.0]")
        if confidence is not None and not 0.0 <= confidence <= 1.0:
            raise ValueError("'confidence' must be within [0.0, 1.0]")
        federated = self._federation.share_memory(
            content=content,
            source_agent=source_agent,
            memory_type=memory_type,
            importance=0.5 if importance is None else importance,
            confidence=0.5 if confidence is None else confidence,
            tags=tags,
        )
        return {"memory": _federated_memory_dump(federated)}

    def _federation_recall(self, params: dict[str, Any]) -> dict[str, Any]:
        query = _require_str(params, "query")
        agent_id = _opt_str(params, "agent_id") or None
        memory_type = _opt_str(params, "memory_type") or None
        min_importance = _opt_float(params, "min_importance")
        limit = _opt_int(params, "limit")
        if limit is not None and limit < 0:
            raise ValueError("'limit' must be >= 0")
        if min_importance is not None and not 0.0 <= min_importance <= 1.0:
            raise ValueError("'min_importance' must be within [0.0, 1.0]")
        memories = self._federation.recall(
            query,
            agent_id=agent_id,
            min_importance=0.0 if min_importance is None else min_importance,
            memory_type=memory_type,
            limit=10 if limit is None else limit,
        )
        return {"memories": [_federated_memory_dump(m) for m in memories]}

    # ── memory.episodes (episodic SQLite — durable trail) ──────────

    def _memory_episodes(self, params: dict[str, Any]) -> dict[str, Any]:
        agent_id = _require_str(params, "agent_id")
        limit = _opt_int(params, "limit")
        if limit is not None and limit < 0:
            raise ValueError("'limit' must be >= 0")
        return {"episodes": self._memory.get_episodes(agent_id, limit=20 if limit is None else limit)}


# ── CLI (long-lived process; used by the Rust e2e test) ────────────────

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m src.memory.rtd_memory_server",
        description="BH memory UDS peer (BH bridge protocol v1) — Phase 3 memory seam.",
    )
    parser.add_argument("--socket", required=True, help="UDS path to bind (0600)")
    parser.add_argument("--data-dir", default="~/.bh/memory",
                        help="MemorySystem data dir (LTM/semantic/episodic SQLite)")
    parser.add_argument("--sessions-db", default="~/.bh/sessions/sessions.db",
                        help="SessionMemory SQLite path")
    parser.add_argument("--colony-id", default="bh-colony")
    args = parser.parse_args(argv)

    server = RtdMemoryUdsServer(
        data_dir=args.data_dir,
        sessions_db=args.sessions_db,
        colony_id=args.colony_id,
    )

    async def run() -> None:
        loop = asyncio.get_running_loop()
        stop = asyncio.Event()
        for sig in (signal.SIGTERM, signal.SIGINT):
            try:
                loop.add_signal_handler(sig, stop.set)
            except NotImplementedError:  # pragma: no cover - non-unix
                pass
        await server.start(args.socket)
        serve = asyncio.ensure_future(server.serve_forever())
        await stop.wait()
        await server.stop()
        serve.cancel()

    try:
        asyncio.run(run())
    finally:
        server.close_stores()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
