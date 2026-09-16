"""A TCP socket opened after GUI startup must still fail graphical acceptance."""
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_smoke", ROOT / "scripts/desktop_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)
WORKFLOW_SPEC = importlib.util.spec_from_file_location("iced_workflow_smoke", ROOT / "scripts/iced_workflow_smoke.py")
WORKFLOW = importlib.util.module_from_spec(WORKFLOW_SPEC)
with patch.dict(sys.modules, {"desktop_smoke": SMOKE}):
    WORKFLOW_SPEC.loader.exec_module(WORKFLOW)


class TransportFailureEvidence(unittest.TestCase):
    def test_capture_write_failure_preserves_the_transport_refusal(self):
        process = SimpleNamespace(pid=42, poll=lambda: None)
        failed = subprocess.CompletedProcess([], 2, "", "inspection refused")
        with tempfile.TemporaryDirectory() as directory, patch.object(SMOKE.shutil, "which", return_value="lsof"), patch.object(SMOKE.subprocess, "run", return_value=failed), patch.object(Path, "write_text", side_effect=OSError("disk full")):
            with self.assertRaises(SMOKE.TransportCheckFailed) as caught:
                SMOKE.check_no_tcp([process], time.monotonic() + 5, Path(directory))
        self.assertEqual(caught.exception.observation["stderr"], "inspection refused")
        self.assertEqual(caught.exception.observation["capture_error"], "disk full")

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


class WorkflowFailureEvidence(unittest.TestCase):
    def test_a_closed_or_unencodable_diagnostic_stream_keeps_the_original_exception(self):
        for error in (ValueError("closed stream"), UnicodeEncodeError("ascii", "\u2603", 0, 1, "not representable")):
            with self.subTest(error=type(error).__name__):
                primary = RuntimeError("original workflow refusal")
                stream = Mock()
                stream.write.side_effect = error
                report = {"result": "failed", "error": str(primary)}
                with tempfile.TemporaryDirectory() as directory, patch.object(WORKFLOW, "stop", side_effect=OSError("stop failed")), patch.object(sys, "stderr", stream):
                    with self.assertRaises(RuntimeError) as caught:
                        try:
                            raise primary
                        finally:
                            WORKFLOW.finish_smoke(Mock(), None, "socket", report, Path(directory), time.monotonic(), sys.exc_info()[1])
                self.assertIs(caught.exception, primary)
                self.assertTrue(stream.write.called)

    def test_secondary_cleanup_and_report_failures_keep_the_original_exception(self):
        primary = RuntimeError("original workflow refusal")
        window, daemon = Mock(), Mock()
        daemon.poll.return_value = None
        report = {"result": "failed", "error": str(primary)}
        with tempfile.TemporaryDirectory() as directory, patch.object(WORKFLOW, "stop", side_effect=OSError("stop failed")) as stop, patch.object(WORKFLOW, "rpc", side_effect=OSError("shutdown refused")), patch.object(Path, "write_text", side_effect=OSError("disk full")), patch.object(sys, "stderr", new=io.StringIO()):
            with self.assertRaises(RuntimeError) as caught:
                try:
                    raise primary
                finally:
                    WORKFLOW.finish_smoke(window, daemon, "socket", report, Path(directory), time.monotonic(), sys.exc_info()[1])
        self.assertIs(caught.exception, primary)
        self.assertEqual([call.args[0] for call in stop.call_args_list], [window, daemon])
        self.assertEqual([error["stage"] for error in report["cleanup_errors"]], ["window cleanup", "daemon shutdown", "daemon cleanup", "result report"])
        self.assertEqual(report["error"], str(primary))

    def test_cleanup_failure_invalidates_an_otherwise_passing_result(self):
        report = {"result": "passed"}
        with tempfile.TemporaryDirectory() as directory, patch.object(WORKFLOW, "stop", side_effect=OSError("stop failed")):
            with self.assertRaisesRegex(RuntimeError, "cleanup failed"):
                WORKFLOW.finish_smoke(Mock(), None, "socket", report, Path(directory), time.monotonic(), None)
            recorded = json.loads((Path(directory) / "result.json").read_text())
        self.assertEqual(recorded["result"], "failed")
        self.assertEqual(recorded["cleanup_errors"][0]["stage"], "window cleanup")


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
