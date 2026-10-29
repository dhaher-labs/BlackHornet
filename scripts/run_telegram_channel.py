#!/usr/bin/env python3
"""Standalone runner that connects the BlackHornet colony to Telegram.

Reads the bot token from MULTICOLONY_CHANNEL_TELEGRAM_BOT_TOKEN (environment
or repo-root .env -- never hardcoded, never logged) and starts the
TelegramChannel with its aiohttp long-polling fallback (no SDK required).

Commands understood by the MVP bridge:

    /start, /help -> help text
    /ping         -> pong
    /status       -> channel counters
    anything else -> acknowledgement (inbound event recorded)

Optional allowlist: set MULTICOLONY_CHANNEL_TELEGRAM_ALLOWED_CHAT_IDS as a
comma-separated list of chat ids to restrict who the bot responds to.

Usage:
    python scripts/run_telegram_channel.py

The process runs until SIGTERM/SIGINT.
"""

from __future__ import annotations

import asyncio
import os
import signal
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

ENV_TOKEN_KEY = "MULTICOLONY_CHANNEL_TELEGRAM_BOT_TOKEN"
ENV_ALLOWED_KEY = "MULTICOLONY_CHANNEL_TELEGRAM_ALLOWED_CHAT_IDS"

HELP_TEXT = (
    "BlackHornet Colony - Telegram bridge aktif.\n"
    "Perintah: /help, /ping, /status.\n"
    "Pesan lain dicatat sebagai inbound event koloni."
)


def _load_env_file() -> None:
    """Load repo-root .env when python-dotenv is available (best effort)."""
    try:
        from dotenv import load_dotenv
    except ImportError:
        return
    load_dotenv(REPO_ROOT / ".env")


def _parse_allowed_chat_ids(raw: str) -> list[str]:
    """Parse a comma-separated chat-id allowlist from the environment."""
    return [part.strip() for part in raw.split(",") if part.strip()]


def build_channel(token: str, allowed_chat_ids: list[str]) -> Any:
    """Build the TelegramChannel instance (import kept local on purpose)."""
    from src.core.channel import TelegramChannel

    config: dict[str, Any] = {}
    if allowed_chat_ids:
        config["allowed_chat_ids"] = allowed_chat_ids
    return TelegramChannel(channel_id="telegram-main", bot_token=token, config=config)


async def _handle_inbound(channel: Any, message: Any) -> None:
    """Reply to a single inbound message via the channel."""
    from src.colony_types.messages import OutboundMessage

    text = message.content.strip()
    if text.startswith("/ping"):
        reply = "pong"
    elif text.startswith("/status"):
        info = channel.get_info()
        reply = (
            "BlackHornet channel status: "
            f"running={info.get('is_running')} "
            f"sent={info.get('sent_count')} "
            f"received={info.get('received_count')}"
        )
    elif text.startswith("/start") or text.startswith("/help"):
        reply = HELP_TEXT
    else:
        reply = f"Pesan diterima dari {message.sender_name}. (bridge MVP: reasoning engine belum terhubung)"
    outbound = OutboundMessage(
        channel_type="telegram",
        channel_id="telegram-main",
        recipient_id=str(message.sender_id),
        content=reply,
    )
    sent = await channel.send(outbound)
    if not sent:
        print("[telegram] warning: outbound send failed", file=sys.stderr, flush=True)


async def run() -> int:
    """Run the bridge until SIGTERM/SIGINT."""
    _load_env_file()
    token = os.environ.get(ENV_TOKEN_KEY, "").strip()
    if not token:
        print(f"error: {ENV_TOKEN_KEY} is not set", file=sys.stderr)
        return 2
    allowed = _parse_allowed_chat_ids(os.environ.get(ENV_ALLOWED_KEY, ""))

    channel = build_channel(token, allowed)
    await channel.start()
    if not channel.is_running:
        print("error: channel failed to start", file=sys.stderr)
        return 1
    print("[telegram] bridge started (long-polling); SIGTERM/SIGINT to stop", flush=True)

    stop_event = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, stop_event.set)
        except NotImplementedError:  # pragma: no cover - Windows only
            pass

    try:
        while not stop_event.is_set():
            message = await channel.receive()
            if message is None:
                try:
                    await asyncio.wait_for(stop_event.wait(), timeout=0.5)
                except asyncio.TimeoutError:
                    pass
                continue
            print(
                f"[telegram] inbound from {message.sender_name} (chat {message.channel_id})",
                flush=True,
            )
            await _handle_inbound(channel, message)
    finally:
        await channel.stop()
        print("[telegram] bridge stopped", flush=True)
    return 0


def main() -> int:
    """Entry point."""
    try:
        return asyncio.run(run())
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
