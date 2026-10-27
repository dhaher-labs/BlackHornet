"""CI grep gate (v7 F8, PLAN §2.3): ban fake-success returns from src/.

The repo's worst observability pattern was plausible-looking returned
strings from mock code (orca "Simulated execution of ...", babyagi
"Executed: ..."). Mock-mode code must return explicit degraded/failed
states instead. This gate fails CI if any banned pattern reappears.
"""

from __future__ import annotations

import pathlib
import unittest

SRC_DIR = pathlib.Path(__file__).resolve().parent.parent / "src"

# Banned substrings — each one was a fake-success return at some point.
BANNED_PATTERNS = [
    "Simulated execution of:",       # src/agents/orca.py (worktree no-op)
    'task.result = f"Executed:',     # src/agents/babyagi.py (hardcoded result)
]

# Modules that were fixed in v7 must keep their explicit degraded markers.
REQUIRED_DEGRADED_MARKERS = {
    "agents/orca.py": "DEGRADED-RESULT: no executor wired",
    "agents/babyagi.py": "DEGRADED-RESULT: no executor wired",
    "omega/council.py": "[DEGRADED: rule-based vote",
}


class TestNoFakeSuccess(unittest.TestCase):
    def _py_files(self):
        return [p for p in SRC_DIR.rglob("*.py") if "tests" not in p.name]

    def test_no_banned_fake_success_strings(self):
        offenders = []
        for path in self._py_files():
            text = path.read_text(encoding="utf-8", errors="ignore")
            for pattern in BANNED_PATTERNS:
                if pattern in text:
                    offenders.append(f"{path}: contains {pattern!r}")
        self.assertEqual(
            offenders,
            [],
            "fake-success returns found (PLAN F8 ban):\n" + "\n".join(offenders),
        )

    def test_degraded_markers_still_in_place(self):
        missing = []
        for rel, marker in REQUIRED_DEGRADED_MARKERS.items():
            path = SRC_DIR / rel
            if not path.exists():
                missing.append(f"{path}: file gone (gate needs update)")
            elif marker not in path.read_text(encoding="utf-8", errors="ignore"):
                missing.append(f"{path}: degraded marker {marker!r} missing")
        self.assertEqual(
            missing,
            [],
            "explicit-degraded rule (PLAN F8) violated:\n" + "\n".join(missing),
        )


if __name__ == "__main__":
    unittest.main()
