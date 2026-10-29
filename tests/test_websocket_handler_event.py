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


class TopicFilterTests(unittest.TestCase):
    """Per-client topic filtering + subscribe contract (EVENT_UI_MAPPING.md §4)."""

    def _handler_two_clients(self) -> Any:
        handler = WebSocketHandler(event_bus=None)
        handler.start()
        s1 = _RecordingSender()
        s2 = _RecordingSender()
        handler.register_client("agents", s1)
        handler.register_client("tasks", s2)
        return handler, s1, s2

    def test_unsubscribed_client_receives_everything(self) -> None:
        handler, s1, s2 = self._handler_two_clients()
        handler._on_event(UIEvent(type="system.startup", data={}))
        self.assertEqual(len(s1.messages), 1)
        self.assertEqual(len(s2.messages), 1)

    def test_topic_glob_filters_per_client(self) -> None:
        handler, s1, s2 = self._handler_two_clients()
        handler.handle_client_message(
            "agents", {"type": "subscribe", "topics": ["agent.*"]}
        )
        handler.handle_client_message(
            "tasks", {"type": "subscribe", "topics": ["task.*"]}
        )
        handler._on_event(CoreEvent(event_type=EventType.AGENT_SPAWNED))
        handler._on_event(CoreEvent(event_type=EventType.TASK_COMPLETED))
        self.assertEqual(len(s1.messages), 1)
        self.assertEqual(s1.messages[0]["data"]["ui_type"], "agent.spawned")
        self.assertEqual(len(s2.messages), 1)
        self.assertEqual(s2.messages[0]["data"]["ui_type"], "task.completed")

    def test_agent_state_changed_maps_to_agent_state(self) -> None:
        handler, s1, s2 = self._handler_two_clients()
        handler.handle_client_message(
            "agents", {"type": "subscribe", "topics": ["agent.*"]}
        )
        handler.handle_client_message(
            "tasks", {"type": "subscribe", "topics": ["task.*"]}
        )
        handler._on_event(
            CoreEvent(
                event_type=EventType.AGENT_STATE_CHANGED,
                data={"agent_id": "a1", "to": "parked"},
            )
        )
        self.assertEqual(s1.messages[0]["data"]["ui_type"], "agent.state")
        self.assertEqual(s2.messages, [])

    def test_exact_topic_match(self) -> None:
        handler, s1, _ = self._handler_two_clients()
        handler.handle_client_message(
            "agents", {"type": "subscribe", "topics": ["task.completed"]}
        )
        handler._on_event(CoreEvent(event_type=EventType.TASK_COMPLETED))
        handler._on_event(CoreEvent(event_type=EventType.TASK_FAILED))
        self.assertEqual(len(s1.messages), 1)
        self.assertEqual(s1.messages[0]["data"]["ui_type"], "task.completed")

    def test_star_wildcard_receives_all(self) -> None:
        handler, s1, _ = self._handler_two_clients()
        handler.handle_client_message("agents", {"type": "subscribe", "topics": ["*"]})
        handler._on_event(CoreEvent(event_type=EventType.TASK_FAILED))
        handler._on_event(CoreEvent(event_type=EventType.HEARTBEAT))
        self.assertEqual(len(s1.messages), 2)
        self.assertEqual(s1.messages[1]["data"]["ui_type"], "agent.heartbeat")

    def test_empty_topics_resubscribe_receives_all(self) -> None:
        handler, s1, _ = self._handler_two_clients()
        handler.handle_client_message(
            "agents", {"type": "subscribe", "topics": ["agent.*"]}
        )
        handler.handle_client_message("agents", {"type": "subscribe", "topics": []})
        handler._on_event(CoreEvent(event_type=EventType.TASK_COMPLETED))
        self.assertEqual(len(s1.messages), 1)

    def test_direct_broadcast_ignores_subscriptions(self) -> None:
        handler, s1, _ = self._handler_two_clients()
        handler.handle_client_message(
            "agents", {"type": "subscribe", "topics": ["agent.*"]}
        )
        handler.broadcast("manual.push", {"note": "admin"})
        self.assertEqual(len(s1.messages), 1)
        self.assertEqual(s1.messages[0]["type"], "manual.push")

    def test_handle_client_message_rejects_malformed(self) -> None:
        handler, _, _ = self._handler_two_clients()
        self.assertFalse(handler.handle_client_message("agents", "not-a-dict"))
        self.assertFalse(handler.handle_client_message("agents", {"type": "ping"}))
        self.assertFalse(
            handler.handle_client_message(
                "agents", {"type": "subscribe", "topics": "agent.*"}
            )
        )
        self.assertFalse(
            handler.handle_client_message(
                "agents", {"type": "subscribe", "topics": [123]}
            )
        )
        self.assertFalse(
            handler.handle_client_message(
                "ghost", {"type": "subscribe", "topics": ["agent.*"]}
            )
        )

    def test_ui_event_type_normalization_table(self) -> None:
        self.assertEqual(WebSocketHandler._ui_event_type("agent_spawned"), "agent.spawned")
        self.assertEqual(WebSocketHandler._ui_event_type("agent_state_changed"), "agent.state")
        self.assertEqual(WebSocketHandler._ui_event_type("heartbeat"), "agent.heartbeat")
        self.assertEqual(WebSocketHandler._ui_event_type("custom"), "ecosystem.event")
        self.assertEqual(WebSocketHandler._ui_event_type("colony_created"), "colony.created")
        self.assertEqual(WebSocketHandler._ui_event_type("agent.spawned"), "agent.spawned")
        self.assertEqual(WebSocketHandler._ui_event_type("unknown"), "unknown")


if __name__ == "__main__":
    unittest.main()
