"""The package contract comes from the executable, including cross-build runners."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("native_build", ROOT / "scripts/build_native.py")
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class NativeBuildMetadata(unittest.TestCase):
    def test_windows_executable_metadata_is_checked_without_starting_state(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "agentd.exe"
            metadata = {"format": 1, "version": "0.1.0", "os": "windows", "arch": "x86_64", "state_schema": 23}
            executable.write_text("import sys\nassert sys.argv[1:] == ['--build-info']\nprint(" + repr(json.dumps(metadata)) + ")\n")
            self.assertEqual(BUILD.daemon_metadata(executable, "x86_64-pc-windows-msvc", "0.1.0", [sys.executable]), metadata)
            with self.assertRaises(ValueError):
                BUILD.daemon_metadata(executable, "aarch64-pc-windows-msvc", "0.1.0", [sys.executable])

    def test_reported_contract_and_incompatible_builds(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "target daemon"
            metadata = {"format": 1, "version": "0.1.0", "os": "linux", "arch": "aarch64", "state_schema": 27}

            def report(value):
                executable.write_text("import sys\nassert sys.argv[1:] == ['--build-info']\nprint(" + repr(json.dumps(value)) + ")\n")
                return BUILD.daemon_metadata(executable, "aarch64-unknown-linux-gnu", "0.1.0", [sys.executable])

            self.assertEqual(report(metadata)["state_schema"], 27)
            for field, value in [("version", "0.2.0"), ("os", "macos"), ("arch", "x86_64"),
                                 ("state_schema", True), ("state_schema", 0), ("state_schema", 0x1_0000_0000)]:
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    report({**metadata, field: value})


if __name__ == "__main__":
    unittest.main()
