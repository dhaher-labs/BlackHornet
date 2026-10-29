"""Tests for TelegramChannel aiohttp polling fallback (offline, no SDK)."""

from __future__ import annotations

import asyncio
import os
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))


def _update(update_id: int, chat_id: int, text: str, username: str = "tester") -> dict:
    """Build a minimal Telegram update payload."""
    return {
        "update_id": update_id,
        "message": {
            "text": text,
            "chat": {"id": chat_id},
            "from": {"id": 777, "username": username, "first_name": "Test"},
        },
    }


class TestTelegramExtraction(unittest.TestCase):
    """Pure helpers used by the aiohttp polling fallback."""

    def test_extract_inbound_parses_text_message(self):
        from src.core.channel import TelegramChannel

        inbound = TelegramChannel._extract_inbound(_update(1, 42, "halo"))
        self.assertIsNotNone(inbound)
        self.assertEqual(inbound.channel_type, "telegram")
        self.assertEqual(inbound.channel_id, "42")
        self.assertEqual(inbound.sender_id, "777")
        self.assertEqual(inbound.sender_name, "tester")
        self.assertEqual(inbound.content, "halo")

    def test_extract_inbound_ignores_non_text(self):
        from src.core.channel import TelegramChannel

        self.assertIsNone(TelegramChannel._extract_inbound({"update_id": 1}))
        self.assertIsNone(TelegramChannel._extract_inbound({"message": {"chat": {"id": 1}}}))

    def test_extract_inbound_falls_back_to_first_name(self):
        from src.core.channel import TelegramChannel

        update = _update(2, 7, "hi")
        update["message"]["from"] = {"id": 5, "first_name": "Andi"}
        inbound = TelegramChannel._extract_inbound(update)
        self.assertEqual(inbound.sender_name, "Andi")

    def test_chat_allowed(self):
        from src.core.channel import TelegramChannel

        self.assertTrue(TelegramChannel._chat_allowed("1", []))
        self.assertTrue(TelegramChannel._chat_allowed("1", ["1"]))
        self.assertFalse(TelegramChannel._chat_allowed("2", ["1"]))

    def test_next_offset(self):
        from src.core.channel import TelegramChannel

        self.assertEqual(TelegramChannel._next_offset([], 0), 0)
        self.assertEqual(TelegramChannel._next_offset([_update(5, 1, "a")], 0), 6)
        self.assertEqual(TelegramChannel._next_offset([_update(5, 1, "a")], 9), 9)
        self.assertEqual(TelegramChannel._next_offset([{"update_id": "bad"}], 3), 3)


class TestTelegramFallbackLifecycle(unittest.TestCase):
    """start()/stop() behavior when python-telegram-bot is unavailable."""

    def _run(self, coro):
        return asyncio.run(coro)

    def test_start_without_sdk_starts_poll_task_and_stop_cancels(self):
        from src.core.channel import TelegramChannel

        async def scenario():
            release = asyncio.Event()

            async def fake_poll_loop(self) -> None:
                await release.wait()

            with mock.patch.dict(sys.modules, {"telegram": None, "telegram.ext": None}):
                with mock.patch.object(TelegramChannel, "_poll_loop", fake_poll_loop):
                    channel = TelegramChannel(channel_id="t", bot_token="dummy:token")
                    await channel.start()
                    self.assertTrue(channel.is_running)
                    self.assertIsNotNone(channel._poll_task)
                    release.set()
                    await channel.stop()
                    self.assertFalse(channel.is_running)
                    self.assertTrue(channel._poll_task is None or channel._poll_task.done())

        self._run(scenario())

    def test_start_fallback_uses_config_values(self):
        from src.core.channel import TelegramChannel

        async def scenario():
            channel = TelegramChannel(
                channel_id="t",
                bot_token="dummy:token",
                config={
                    "api_url": "https://api.example.org/",
                    "allowed_chat_ids": ["42", "", " 43 "],
                    "poll_timeout_s": "9",
                },
            )
            self.assertEqual(channel._api_url, "https://api.example.org")
            self.assertEqual(channel._allowed_chat_ids, ["42", "43"])
            self.assertEqual(channel._poll_timeout, 9)
            self.assertIsNone(channel._poll_task)

        self._run(scenario())

    def test_stop_without_start_is_safe(self):
        from src.core.channel import TelegramChannel

        async def scenario():
            channel = TelegramChannel(channel_id="t", bot_token="dummy:token")
            await channel.stop()
            self.assertFalse(channel.is_running)

        self._run(scenario())


class TestChannelFactory(unittest.TestCase):
    """create_channel error path."""

    def test_create_channel_rejects_unknown_type(self):
        from src.core.channel import create_channel
        from src.exceptions import ChannelError

        with self.assertRaises(ChannelError) as ctx:
            create_channel("carrier-pigeon")
        self.assertIn("carrier-pigeon", str(ctx.exception))


class TestStdlibKwargsLogger(unittest.TestCase):
    """get_logger stdlib fallback must tolerate structlog-style kwargs."""

    def _capture_logger(self):
        import logging as stdlib_logging

        from src.config.logging_config import _KwargsLoggerAdapter

        records: list = []

        class Capture(stdlib_logging.Handler):
            def emit(self, record):
                records.append(record)

        base = stdlib_logging.getLogger("test.kwargs.adapter")
        base.handlers = [Capture()]
        base.setLevel(stdlib_logging.INFO)
        base.propagate = False
        return _KwargsLoggerAdapter(base, {}), records

    def test_kwargs_rendered_into_message(self):
        adapter, records = self._capture_logger()
        adapter.warning("poll_error", channel_id="tg-1", error="boom")
        rendered = records[0].getMessage()
        self.assertIn("poll_error", rendered)
        self.assertIn("channel_id=tg-1", rendered)
        self.assertIn("error=boom", rendered)

    def test_reserved_kwarg_message_does_not_crash(self):
        adapter, records = self._capture_logger()
        adapter.warning("sdk_missing", message="pip install python-telegram-bot")
        rendered = records[0].getMessage()
        self.assertIn("message=pip install python-telegram-bot", rendered)

    def test_get_logger_call_with_kwargs_is_safe(self):
        from src.config.logging_config import get_logger

        logger = get_logger("test.kwargs.smoke")
        logger.info("smoke_event", key="value")  # must not raise on either path


if __name__ == "__main__":
    unittest.main()
