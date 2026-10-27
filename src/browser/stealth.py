"""Stealth browser patterns from CloakBrowser.

v7 fix (PLAN F4 / architect B3): ``src.agent_tools.browser_tool`` imports
``StealthConfig``, ``apply_stealth`` and ``get_stealth_script`` from this
module.  Previously only ``StealthPatterns.apply_stealth_scripts()`` (a
method) existed, so browser_tool was unimportable.  The module now exposes
the three names browser_tool expects and ``StealthConfig`` accepts the
keyword arguments browser_tool passes.
"""

from __future__ import annotations

import logging
import random
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)


class StealthConfig:
    """Configuration for stealth browser mode.

    Accepts the options passed by ``src.agent_tools.browser_tool``;
    unknown options are ignored so callers can pass superset dicts.
    """

    def __init__(
        self,
        hide_webdriver: bool = True,
        mock_languages: Optional[List[str]] = None,
        mock_plugins: bool = True,
        mock_permissions: bool = True,
        disable_automation_flags: bool = True,
        custom_user_agent: Optional[str] = None,
        disable_images: bool = False,
        disable_css: bool = False,
        **_ignored: Any,
    ) -> None:
        # Options consumed by the stealth scripts
        self.hide_webdriver = hide_webdriver
        self.mock_languages = list(mock_languages) if mock_languages else ["en-US", "en"]
        self.mock_plugins = mock_plugins
        self.mock_permissions = mock_permissions
        self.disable_automation_flags = disable_automation_flags
        self.custom_user_agent = custom_user_agent
        self.disable_images = disable_images
        self.disable_css = disable_css

        # Fingerprint attributes (used by StealthPatterns)
        self.webgl_vendor = "Google Inc."
        self.webgl_renderer = "ANGLE"
        self.navigator_platform = "Win32"
        self.navigator_languages = self.mock_languages
        self.screen_width = 1920
        self.screen_height = 1080
        self.target_recaptcha_score = 0.9
        self.user_agents = [
            custom_user_agent or "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        ]

    def get_random_user_agent(self) -> str:
        return random.choice(self.user_agents)


def get_stealth_script(config: Optional[StealthConfig] = None) -> List[str]:
    """Return the stealth JavaScript patches to inject into a page.

    Standalone function (v7): the name ``src.agent_tools.browser_tool``
    imports at module load.  Delegates to :class:`StealthPatterns`.
    """
    return StealthPatterns(config).apply_stealth_scripts()


async def apply_stealth(page: Any, config: Optional[StealthConfig] = None) -> None:
    """Apply stealth scripts to a Playwright page before navigation.

    Standalone async function (v7): the name
    ``src.agent_tools.browser_tool`` awaits during browser init.
    """
    for script in get_stealth_script(config):
        try:
            await page.add_init_script(script)
        except Exception as e:  # pragma: no cover - depends on browser impl
            logger.debug("apply_stealth_script_error err=%s", e)


class StealthPatterns:
    """Stealth patterns for undetectable browser automation."""

    def __init__(self, config: Optional[StealthConfig] = None):
        self.config = config or StealthConfig()

    def apply_stealth_scripts(self) -> List[str]:
        """Return stealth JavaScript patches."""
        scripts: List[str] = []
        cfg = self.config
        if cfg.hide_webdriver:
            scripts.append(
                "Object.defineProperty(navigator, 'webdriver', {get: () => undefined})"
            )
        if cfg.disable_automation_flags:
            scripts.append(
                "Object.defineProperty(navigator, 'plugins', "
                "{get: () => " + ("[{length: 1}]" if cfg.mock_plugins else "[]") + "})"
            )
            scripts.append(
                "Object.defineProperty(navigator, 'languages', "
                f"{{get: () => {list(cfg.navigator_languages)!r}}})"
            )
        if cfg.mock_permissions:
            scripts.append(
                "const originalQuery = window.navigator.permissions.query; "
                "window.navigator.permissions.query = (parameters) => ("
                "parameters.name === 'notifications' ? "
                "Promise.resolve({state: Notification.permission}) : originalQuery(parameters));"
            )
        scripts.append(
            f"Object.defineProperty(screen, 'width', {{get: () => {cfg.screen_width}}})"
        )
        scripts.append(
            f"Object.defineProperty(screen, 'height', {{get: () => {cfg.screen_height}}})"
        )
        if cfg.disable_images or cfg.disable_css:
            style = []
            if cfg.disable_images:
                style.append("img { display: none !important; }")
            if cfg.disable_css:
                style.append("link[rel=stylesheet] { display: none !important; }")
            scripts.append(
                "document.addEventListener('DOMContentLoaded', () => { "
                "const s = document.createElement('style'); "
                f"s.textContent = {' '.join(style)!r}; "
                "document.head.appendChild(s); });"
            )
        return scripts

    def check_stealth_score(self) -> float:
        """Simulate stealth score check."""
        return self.config.target_recaptcha_score

    def generate_fingerprint(self) -> Dict[str, Any]:
        """Generate a browser fingerprint."""
        return {
            "user_agent": self.config.get_random_user_agent(),
            "screen": f"{self.config.screen_width}x{self.config.screen_height}",
            "platform": self.config.navigator_platform,
            "languages": self.config.navigator_languages,
            "webgl": f"{self.config.webgl_vendor} - {self.config.webgl_renderer}",
        }
