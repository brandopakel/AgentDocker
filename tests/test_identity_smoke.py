"""Fixture cleanup must never signal a PID after its child was reaped."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("identity_smoke", ROOT / "scripts/identity_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class FixtureCleanup(unittest.TestCase):
    def test_reaped_child_does_not_authorize_signalling_its_former_group(self):
        with subprocess.Popen([sys.executable, "-c", "pass"], start_new_session=True) as child:
            self.assertEqual(child.wait(timeout=5), 0)
            # Its numeric group ID can now belong to someone else.
            with patch.object(SMOKE.os, "killpg", side_effect=AssertionError("reused group")):
                SMOKE.stop(child)


if __name__ == "__main__":
    unittest.main()
