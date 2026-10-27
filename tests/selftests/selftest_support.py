"""Shared isolation helpers for the legacy tests/selftests suite (task 14-b).

This module is deliberately NOT collected anywhere:
  - pytest default patterns are ``test_*.py`` / ``*_test.py``  -> no match
  - unittest discovery pattern for this suite is ``*_tests.py`` -> no match

It only provides state-isolation helpers so the pre-v7 unittest files can
restore global state (event loops, root logging config) that production
modules mutate at import time.
"""

from __future__ import annotations

import asyncio
import logging
from typing import List, Optional, Tuple

LoggingSnapshot = Tuple[int, List[logging.Handler]]


def close_event_loop(loop: Optional[asyncio.AbstractEventLoop]) -> None:
    """Tear down a test event loop without leaking state.

    Cancels leftover tasks, shuts down async generators, detaches the loop
    from the asyncio policy and closes it.  Prevents the two classic
    cross-test asyncio pollution symptoms:
      * "Task was destroyed but it is pending!" warnings
      * a *closed* loop being handed to later tests via the policy
    """
    if loop is None:
        return
    try:
        pending = [t for t in asyncio.all_tasks(loop) if not t.done()]
        for task in pending:
            task.cancel()
        if pending:
            loop.run_until_complete(
                asyncio.gather(*pending, return_exceptions=True)
            )
        loop.run_until_complete(loop.shutdown_asyncgens())
    except Exception:
        pass
    finally:
        asyncio.set_event_loop(None)
        if not loop.is_closed():
            loop.close()


def snapshot_root_logging() -> LoggingSnapshot:
    """Snapshot the root logger level + handler list."""
    root = logging.getLogger()
    return root.level, list(root.handlers)


def restore_root_logging(snapshot: LoggingSnapshot) -> None:
    """Restore the root logger from a :func:`snapshot_root_logging` snapshot.

    Removes (and closes) any handler attached after the snapshot was taken —
    e.g. by modules that call ``logging.basicConfig()`` at import time — and
    restores the previous level.
    """
    level, handlers = snapshot
    root = logging.getLogger()
    for handler in root.handlers[:]:
        if handler not in handlers:
            root.removeHandler(handler)
            try:
                handler.close()
            except Exception:
                pass
    root.setLevel(level)
