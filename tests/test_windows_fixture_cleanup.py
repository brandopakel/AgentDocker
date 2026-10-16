"""Native Windows fixture cleanup across the host's legacy path limit."""
import importlib.util
import os
from pathlib import Path
import shutil
import stat
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "windows_fixture_cleanup", ROOT / "scripts/windows_native_codex_smoke.py")
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


@unittest.skipUnless(os.name == "nt", "requires native Windows path semantics")
class WindowsFixtureCleanup(unittest.TestCase):
    def test_long_readonly_provider_tree_is_removed_without_host_policy_change(self):
        root = Path(tempfile.mkdtemp(prefix="AgentDocker cleanup ü ")).resolve()
        value = str(root)
        extended = Path(value if value.startswith("\\\\?\\") else
                        "\\\\?\\UNC\\" + value[2:] if value.startswith("\\\\") else
                        "\\\\?\\" + value)
        leaf = extended
        while len(str(leaf)) < 300:
            leaf /= "provider-plugin-reference-directory"
        leaf.mkdir(parents=True)
        packed = leaf / "readonly-provider.pack"
        packed.write_bytes(b"private cleanup regression")
        packed.chmod(stat.S_IREAD)
        try:
            FIXTURE.remove_fixture(root)
            self.assertFalse(extended.exists())
        finally:
            # Retire only this test's tree if the old implementation fails.
            # Do not mask the original assertion or deletion error.
            if extended.exists():
                if packed.exists():
                    packed.chmod(stat.S_IWRITE | stat.S_IREAD)
                shutil.rmtree(extended)


if __name__ == "__main__":
    unittest.main()
