"""A TCP socket opened after GUI startup must still fail graphical acceptance."""
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_smoke", ROOT / "scripts/desktop_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


@unittest.skipUnless(shutil.which("lsof"), "native transport acceptance requires lsof")
class DesktopTransport(unittest.TestCase):
    def test_delayed_tcp_listener_is_rejected_and_owned_children_are_stopped(self):
        children = []
        try:
            for script in ["import time; time.sleep(15)",
                           "import socket,time; time.sleep(0.3); s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); time.sleep(15)"]:
                children.append(subprocess.Popen([sys.executable, "-c", script], stdin=subprocess.DEVNULL,
                                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            with self.assertRaisesRegex(RuntimeError, "TCP socket"):
                SMOKE.wait_window(*children, timeout=5)
        finally:
            for child in children:
                SMOKE.stop(child)
        self.assertTrue(all(child.poll() is not None for child in children))


if __name__ == "__main__":
    unittest.main()
