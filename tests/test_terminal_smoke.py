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
