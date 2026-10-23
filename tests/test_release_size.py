"""Release gates enforce exact payload and download boundaries before tagging."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release_size", ROOT / "scripts/check_release_size.py")
SIZE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIZE)
MIB = 1024 ** 2


class ReleaseSize(unittest.TestCase):
    def test_limits_include_all_executables_and_downloads_and_report_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries, archives = root / "bin", root / "dist"
            binaries.mkdir()
            archives.mkdir()

            def resize(path, size):
                with path.open("wb") as file:
                    file.truncate(size)

            resize(binaries / "agentdocker", 16 * MIB)
            # A separate daemon file counts on its own.
            resize(binaries / "agentd", 14 * MIB)
            with self.assertRaisesRegex(ValueError, "no download archives"):
                SIZE.check(binaries, archives)
            resize(archives / "download.tar.gz", 40 * MIB)
            result = SIZE.check(binaries, archives)
            self.assertEqual(result["cli_payload_bytes"], 30 * MIB)
            self.assertEqual(result["executable_bytes"]["agentd"], 14 * MIB)
            self.assertEqual(result["executable_files"], 2)
            self.assertEqual(result["archive_bytes"]["download.tar.gz"], 40 * MIB)
            resize(binaries / "agentd", 14 * MIB + 1)
            with self.assertRaisesRegex(ValueError, f"30 MiB.*{30 * MIB + 1} bytes.*agentd"):
                SIZE.check(binaries, archives)
            resize(binaries / "agentd", 14 * MIB)
            resize(archives / "second.zip", 40 * MIB + 1)
            with self.assertRaisesRegex(ValueError, f"40 MiB.*second.zip.*{40 * MIB + 1}"):
                SIZE.check(binaries, archives)

    def test_the_daemon_link_is_counted_once(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries, archives = root / "bin", root / "dist"
            binaries.mkdir()
            archives.mkdir()
            (archives / "download.tar.gz").write_bytes(b"archive")
            with (binaries / "agentdocker").open("wb") as file:
                file.truncate(20 * MIB)
            os.link(binaries / "agentdocker", binaries / "agentd")
            result = SIZE.check(binaries, archives)
            # Two names of one 20 MiB file: within the 30 MiB budget.
            self.assertEqual(result["cli_payload_bytes"], 20 * MIB)
            self.assertEqual(result["executable_bytes"], {"agentdocker": 20 * MIB, "agentd": 20 * MIB})
            self.assertEqual(result["executable_files"], 1)


if __name__ == "__main__":
    unittest.main()
