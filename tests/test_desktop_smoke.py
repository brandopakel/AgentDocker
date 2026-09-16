"""A TCP socket opened after GUI startup must still fail graphical acceptance."""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_smoke", ROOT / "scripts/desktop_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class TransportFailureEvidence(unittest.TestCase):
    def test_failed_inspector_records_bounded_output_and_process_exit(self):
        process = SimpleNamespace(pid=42, poll=lambda: 0)
        failed = subprocess.CompletedProcess([], 2, "", "permission denied\n" + "x" * 4096)
        with tempfile.TemporaryDirectory() as directory, patch.object(SMOKE.shutil, "which", return_value="lsof"), patch.object(SMOKE.subprocess, "run", return_value=failed):
            capture = Path(directory) / "capture"
            with self.assertRaises(SMOKE.TransportCheckFailed) as caught:
                SMOKE.check_no_tcp([process], time.monotonic() + 5, capture)
            evidence = json.loads((capture / "transport-failure.json").read_text())
            self.assertEqual(evidence, caught.exception.observation)
            self.assertEqual(evidence["reason"], "transport could not be checked")
            self.assertEqual(evidence["pid"], 42)
            self.assertEqual(evidence["returncode"], 2)
            self.assertEqual(evidence["process_status"], 0)
            self.assertIn("permission denied", evidence["stderr"])
            self.assertLess(len(evidence["stderr"]), 2100)
            self.assertTrue(evidence["stderr"].endswith("[truncated]"))

    def test_timeout_retains_partial_byte_output_and_fails(self):
        process = SimpleNamespace(pid=43, poll=lambda: None)
        timeout = subprocess.TimeoutExpired("lsof", 5, output=b"partial\xff", stderr=b"inspection stalled")
        with patch.object(SMOKE.shutil, "which", return_value="lsof"), patch.object(SMOKE.subprocess, "run", side_effect=timeout):
            with self.assertRaises(SMOKE.TransportCheckFailed) as caught:
                SMOKE.check_no_tcp([process], time.monotonic() + 5)
        self.assertEqual(caught.exception.observation["reason"], "lsof timed out")
        self.assertIn("partial", caught.exception.observation["stdout"])
        self.assertEqual(caught.exception.observation["stderr"], "inspection stalled")
        self.assertIsNone(caught.exception.observation["process_status"])


@unittest.skipUnless(shutil.which("lsof"), "native transport acceptance requires lsof")
class DesktopTransport(unittest.TestCase):
    def test_delayed_tcp_listener_is_rejected_and_owned_children_are_stopped(self):
        children = []
        try:
            for script in ["import time; time.sleep(15)",
                           "import socket,time; time.sleep(0.3); s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); time.sleep(15)"]:
                children.append(subprocess.Popen([sys.executable, "-c", script], stdin=subprocess.DEVNULL,
                                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            with tempfile.TemporaryDirectory() as directory:
                capture = Path(directory)
                with self.assertRaisesRegex(SMOKE.TransportCheckFailed, "TCP socket"):
                    SMOKE.wait_window(*children, capture, timeout=5)
                evidence = json.loads((capture / "transport-failure.json").read_text())
                self.assertEqual(evidence["reason"], "TCP socket reported")
                self.assertEqual(evidence["pid"], children[1].pid)
                self.assertEqual(evidence["returncode"], 0)
                self.assertIn("TCP", evidence["stdout"])
        finally:
            for child in children:
                SMOKE.stop(child)
        self.assertTrue(all(child.poll() is not None for child in children))


if __name__ == "__main__":
    unittest.main()
