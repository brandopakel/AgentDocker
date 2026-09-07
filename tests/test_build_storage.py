"""A failed resource preflight must stop the build before Cargo runs."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class BuildStorage(unittest.TestCase):
    def test_low_space_refuses_verification_before_cargo(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "cargo-called"
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\ntouch "$BUILD_TEST_MARKER"\nexit 99\n')
            cargo.chmod(0o700)
            result = subprocess.run(["bash", str(ROOT / "scripts/verify.sh"), "check"],
                                    cwd=ROOT, capture_output=True, text=True, timeout=30,
                                    env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                                         "BUILD_TEST_MARKER": str(marker), "AGENTDOCKER_BUILD_MIN_FREE_GIB": "4096"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("GiB free", result.stderr)
            self.assertFalse(marker.exists(), "resource refusal must happen before Cargo")
