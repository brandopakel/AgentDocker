"""Native installer cleanup must preserve failures, including its own failures."""
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(os.name == "nt", "installer uses native Windows APIs")
class InstallerFixtureCleanup(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(ROOT / "scripts"))
        try:
            spec = importlib.util.spec_from_file_location(
                "windows_installer_fixture", ROOT / "scripts/windows_install_smoke.py")
            cls.fixture = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(cls.fixture)
        finally:
            sys.path.pop(0)

    def test_cleanup_error_is_saved_before_it_propagates(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            retained = scratch / "retained.txt"
            retained.write_bytes(b"failed cleanup evidence")
            report, saved = {"result": "passed"}, []
            with patch.object(self.fixture, "remove_fixture", side_effect=OSError("cleanup refused")):
                with self.assertRaisesRegex(OSError, "cleanup refused"):
                    self.fixture.finish_fixture(report, scratch, lambda: saved.append(dict(report)))
            self.assertEqual(saved, [{"result": "failed", "cleanup_error": "OSError: cleanup refused"}])
            self.assertEqual(retained.read_bytes(), b"failed cleanup evidence")

    def test_failed_behavior_preserves_scratch_for_diagnosis(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            report, saved = {"result": "failed", "error": "original failure"}, []
            with patch.object(self.fixture, "remove_fixture") as remove:
                self.fixture.finish_fixture(report, scratch, lambda: saved.append(dict(report)))
                remove.assert_not_called()
            self.assertTrue(scratch.exists())
            self.assertEqual(saved, [report])


if __name__ == "__main__":
    unittest.main()
