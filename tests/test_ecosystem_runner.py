"""K2/K4 wiring gate (Task 18): EcosystemRunner must boot the async colony
core behind the sync launcher and expose a live ecosystem snapshot via the
APIBridge /api/v1/ecosystem route contract."""

from __future__ import annotations

import unittest


class TestEcosystemRunner(unittest.TestCase):
    def test_start_stop_lifecycle(self):
        from src.integration.ecosystem_runner import EcosystemRunner

        runner = EcosystemRunner()
        runner.start()
        try:
            snap = runner.get_status_snapshot()
            self.assertIn("ecosystem", snap)
            self.assertTrue(runner.orchestrator._running)
        finally:
            runner.stop()
        self.assertFalse(runner.orchestrator._running)

    def test_api_bridge_serves_live_ecosystem(self):
        from src.integration.ecosystem_runner import EcosystemRunner
        from src.ui.api_bridge import APIBridge

        runner = EcosystemRunner()
        runner.start()
        try:
            bridge = APIBridge(orchestrator=runner.orchestrator)
            resp = bridge.get_ecosystem_status()
            self.assertTrue(resp.success, resp.error)
            self.assertIsInstance(resp.data, dict)
            self.assertIn("ecosystem", resp.data)
        finally:
            runner.stop()

    def test_bridge_returns_503_without_orchestrator(self):
        from src.ui.api_bridge import APIBridge

        bridge = APIBridge()
        resp = bridge.get_ecosystem_status()
        self.assertFalse(resp.success)
        self.assertEqual(resp.status_code, 503)


if __name__ == "__main__":
    unittest.main()
