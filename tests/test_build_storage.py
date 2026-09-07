"""A failed resource preflight must stop the build before Cargo runs."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("build_storage", ROOT / "scripts/build_storage.py")
STORAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STORAGE)


class BuildStorage(unittest.TestCase):
    def test_fallback_counts_directory_blocks_and_deduplicates_hardlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for number in range(50):
                (root / str(number)).mkdir()
            payload = root / "0/payload"
            payload.write_bytes(b"data")
            os.link(payload, root / "1/payload")
            original_lstat = Path.lstat

            def allocated_metadata(path):
                metadata = original_lstat(path)
                # Model a filesystem allocating 4 KiB per inode, including
                # empty directories. APFS can report zero for tiny directories.
                return SimpleNamespace(st_dev=metadata.st_dev, st_ino=metadata.st_ino,
                                       st_size=metadata.st_size, st_blocks=8)

            with patch.object(STORAGE.shutil, "which", return_value=None), \
                    patch.object(Path, "lstat", allocated_metadata):
                self.assertEqual(STORAGE.allocated(root), 52 * 4096)

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
                                         "BUILD_TEST_MARKER": str(marker), "AGENTDOCKER_BUILD_MIN_FREE_GIB": str(1 << 63)})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("GiB free", result.stderr)
            self.assertFalse(marker.exists(), "resource refusal must happen before Cargo")
