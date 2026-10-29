"""Tests for WebSocketHandler._on_event event-type resolution (slice 10b).

Locks the contract documented in docs/02-arch/EVENT_UI_MAPPING.md:
- UI-bus events (src/ui/event_bus.Event, plain ``type`` str) surface their
  type string verbatim — zero behavior change from the historical path.
- Core-bus events (src/colony_types.events.Event, ``event_type`` enum)
  surface the enum's ``.value`` instead of the historical "unknown"
  placeholder, so bridged ecosystem events remain readable.
"""

import json
import unittest
from typing import Any, Dict, List

from src.colony_types.events import Event as CoreEvent
from src.colony_types.events import EventType
from src.ui.event_bus import Event as UIEvent
from src.ui.websocket_handler import WebSocketHandler


class _RecordingSender:
    """send_fn stub that decodes each pushed JSON message."""

    def __init__(self) -> None:
        self.messages: List[Dict[str, Any]] = []

    def __call__(self, payload: str) -> bool:
        self.messages.append(json.loads(payload))
        return True


class ResolveEventTypeTests(unittest.TestCase):
    def _running_handler(self) -> Any:
        handler = WebSocketHandler(event_bus=None)
        handler.start()
        sender = _RecordingSender()
        handler.register_client("c1", sender)
        return handler, sender

    def test_ui_bus_event_type_surfaces_verbatim(self) -> None:
        handler, sender = self._running_handler()
        handler._on_event(UIEvent(type="agent.spawned", data={"id": "a1"}))
        self.assertEqual(len(sender.messages), 1)
        self.assertEqual(sender.messages[0]["type"], "ecosystem.event")
        self.assertEqual(sender.messages[0]["data"]["event_type"], "agent.spawned")
        self.assertEqual(sender.messages[0]["data"]["event_data"], {"id": "a1"})

    def test_core_bus_enum_event_surfaces_value(self) -> None:
        handler, sender = self._running_handler()
        event = CoreEvent(event_type=EventType.AGENT_SPAWNED, data={"id": "a1"})
        handler._on_event(event)
        self.assertEqual(len(sender.messages), 1)
        self.assertEqual(sender.messages[0]["data"]["event_type"], "agent_spawned")
        self.assertEqual(sender.messages[0]["data"]["source"], "system")

    def test_unknown_shape_falls_back_to_unknown(self) -> None:
        handler, sender = self._running_handler()
        handler._on_event(object())
        self.assertEqual(sender.messages[0]["data"]["event_type"], "unknown")

    def test_not_running_swallows_event(self) -> None:
        handler = WebSocketHandler(event_bus=None)
        sender = _RecordingSender()
        handler.register_client("c1", sender)
        handler._on_event(UIEvent(type="agent.spawned", data={}))
        self.assertEqual(sender.messages, [])

    def test_resolve_event_type_direct(self) -> None:
        self.assertEqual(
            WebSocketHandler._resolve_event_type(UIEvent(type="task.completed")),
            "task.completed",
        )
        self.assertEqual(
            WebSocketHandler._resolve_event_type(
                CoreEvent(event_type=EventType.TASK_COMPLETED)
            ),
            "task_completed",
        )
        self.assertEqual(WebSocketHandler._resolve_event_type(object()), "unknown")


if __name__ == "__main__":
    unittest.main()
