"""The OpenCode plugin's delivery and wake-up paths, run under Node."""
import shutil
import subprocess
import unittest
from pathlib import Path

TEST = Path(__file__).with_name("opencode_plugin.test.cjs")


class OpenCodePlugin(unittest.TestCase):
    @unittest.skipUnless(shutil.which("node"), "Node is not installed")
    def test_delivery_receipts_and_idle_wake(self):
        result = subprocess.run(["node", str(TEST)], capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
