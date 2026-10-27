"""Integration module — wire new modules into existing BH ecosystem.

10.1 GraphTracker — track BH agents/tasks/swarm in graph
10.2 MemoryAdapter — connect MemorySystem to MemoryBridge
10.3 ProviderHub — combine AutoDiscovery + HyperGate
10.4 EventHub — bridge new event bus with BH event bus
10.5 UnifiedLauncher — unified entry point
10.6 KeeperExtension — sandbox + keeper integration
11.1 AMCBridge — wire new modules into AMC core infrastructure
11.2 AMCToolRegistrar — register new modules as AMC tools

v7 fix (PLAN F9): exports are lazy (PEP 562 ``__getattr__``). The previous
eager ``from .unified_launcher import UnifiedLauncher`` made runpy emit a
RuntimeWarning ("found in sys.modules after import of package") on the
primary ``bh-colony`` entrypoint, because ``python -m
src.integration.unified_launcher`` imports the package first, which
imported the module under its canonical name before runpy re-executed it
as ``__main__``. Lazy exports keep ``from src.integration import X`` working
without importing every sibling at package-import time.
"""

from importlib import import_module
from typing import Any

_LAZY_EXPORTS: dict[str, tuple[str, str]] = {
    "GraphTracker": ("src.integration.graph_tracker", "GraphTracker"),
    "MemoryAdapter": ("src.integration.memory_adapter", "MemoryAdapter"),
    "ProviderHub": ("src.integration.provider_hub", "ProviderHub"),
    "EventHub": ("src.integration.event_hub", "EventHub"),
    "UnifiedLauncher": ("src.integration.unified_launcher", "UnifiedLauncher"),
    "KeeperExtension": ("src.integration.keeper_extension", "KeeperExtension"),
    "AMCBridge": ("src.integration.amc_bridge", "AMCBridge"),
    "AMCToolRegistrar": ("src.integration.amc_tools", "AMCToolRegistrar"),
}

__all__ = list(_LAZY_EXPORTS)


def __getattr__(name: str) -> Any:
    if name in _LAZY_EXPORTS:
        module_name, attr = _LAZY_EXPORTS[name]
        value = getattr(import_module(module_name), attr)
        globals()[name] = value  # cache for subsequent lookups
        return value
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def __dir__() -> list[str]:
    return sorted(set(globals()) | set(_LAZY_EXPORTS))
