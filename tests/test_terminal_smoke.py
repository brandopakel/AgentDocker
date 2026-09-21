"""Failed PTY fixture setup must preserve reports without leaking scratch state."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("terminal_smoke", ROOT / "scripts/terminal_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)

WINDOWS_SPEC = importlib.util.spec_from_file_location("windows_smoke", ROOT / "scripts/windows_daemon_smoke.py")
WINDOWS_SMOKE = importlib.util.module_from_spec(WINDOWS_SPEC)
WINDOWS_SPEC.loader.exec_module(WINDOWS_SMOKE)


class ManagedSessionCleanup(unittest.TestCase):
    def test_stop_requested_is_not_exit_observed(self):
        from unittest.mock import Mock
        stopping = {"status": {"state": "stopping"}}
        exited = {"status": {"state": "exited", "code": 0}}
        inspect = Mock(side_effect=[stopping, stopping, exited])
        with patch.object(WINDOWS_SMOKE.time, "sleep"):
            result = WINDOWS_SMOKE.wait_terminal(inspect, "fixture")
        self.assertEqual(result, exited)
        self.assertEqual(inspect.call_count, 3)

    def test_cleanup_deadline_preserves_live_status_for_force_fallback(self):
        from unittest.mock import Mock
        stopping = {"status": {"state": "stopping"}}
        inspect = Mock(return_value=stopping)
        with patch.object(WINDOWS_SMOKE.time, "monotonic", side_effect=[0, 0, 11]), patch.object(WINDOWS_SMOKE.time, "sleep"):
            result = WINDOWS_SMOKE.wait_terminal(inspect, "fixture")
        self.assertEqual(result, stopping)
        self.assertFalse(WINDOWS_SMOKE.terminal_record(result))
        self.assertEqual(inspect.call_count, 2)


class FixtureSetupCleanup(unittest.TestCase):
    def test_missing_binary_removes_fixture_and_records_failure(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            binaries = root / "binaries"
            binaries.mkdir()
            created = []
            mkdtemp = tempfile.mkdtemp

            def track_fixture(**kwargs):
                path = mkdtemp(prefix=kwargs["prefix"], dir=root)
                created.append(Path(path))
                return path

            old_mask = os.umask(0o022)
            try:
                with patch.object(SMOKE.tempfile, "mkdtemp", side_effect=track_fixture):
                    result = SMOKE.smoke(binaries, root / "report", "test-source")
                self.assertEqual(os.umask(0o022), 0o022)
            finally:
                os.umask(old_mask)
            self.assertEqual(result["failure_class"], "FileNotFoundError")
            self.assertEqual(result["result"], "failed")
            self.assertTrue(result["cleanup"]["fixture_removed"])
            self.assertEqual(len(created), 1)
            self.assertFalse(created[0].exists())
            self.assertEqual(json.loads((root / "report/result.json").read_text()), result)

    def test_existing_output_restores_umask_and_preserves_previous_report(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            report = root / "result.json"
            report.write_text("previous evidence")
            old_mask = os.umask(0o022)
            try:
                with self.assertRaises(FileExistsError):
                    SMOKE.smoke(root, root, "test-source")
                self.assertEqual(os.umask(0o022), 0o022)
            finally:
                os.umask(old_mask)
            self.assertEqual(report.read_text(), "previous evidence")


if __name__ == "__main__":
    unittest.main()
