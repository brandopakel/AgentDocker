import importlib.util
import json
from pathlib import Path
import unittest
import os
import sys
import tempfile
import time
import subprocess
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "container_evidence", Path(__file__).parent / "containers/evidence.py")
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)


class ContainerFailureEvidence(unittest.TestCase):
    def test_missing_record_does_not_mask_the_launch_failure(self):
        response = {"type": "error", "code": "engine_unavailable", "message": "primary failure"}
        def missing():
            raise AssertionError("agent not found")
        with self.assertRaises(AssertionError) as raised:
            EVIDENCE.reject_launch(response, missing, lambda _: self.fail("unexpected record"))
        evidence = json.loads(str(raised.exception))
        self.assertEqual(evidence["launch_response"], response)
        self.assertEqual(evidence["cleanup_lookup_error"], "agent not found")

    def test_partial_container_is_retained_for_owned_cleanup(self):
        record = {"container": {"id": "fixture", "owner": "fixture-owner"}}
        remembered = []
        with self.assertRaises(AssertionError) as raised:
            EVIDENCE.reject_launch({"type": "error", "message": "start failed"}, lambda: record, remembered.append)
        self.assertEqual(remembered, [record])
        self.assertEqual(json.loads(str(raised.exception))["launch_response"]["message"], "start failed")


class ContainerLogRetention(unittest.TestCase):
    def test_noisy_command_keeps_only_tail_and_original_exit(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "result.container.log"
            result = EVIDENCE.capture_tail([sys.executable, "-c",
                "import sys;sys.stdout.buffer.write(b'x'*200000+b'final error');sys.exit(7)"], output, limit=1024)
            self.assertEqual(result["exit"], 7)
            self.assertTrue(result["truncated"])
            self.assertFalse(result["timed_out"])
            self.assertEqual(output.stat().st_size, 1024)
            self.assertTrue(output.read_bytes().endswith(b'final error'))

    def test_hung_diagnostic_is_bounded_and_reaped(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "timed-out.log"
            start = time.monotonic()
            result = EVIDENCE.capture_tail([sys.executable, "-c",
                "import os,time;print(os.getpid(),flush=True);time.sleep(60)"], output, timeout=1)
            self.assertTrue(result["timed_out"])
            self.assertLess(time.monotonic() - start, 5)
            pid = int(output.read_bytes())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_post_kill_reap_is_bounded_and_preserves_diagnostic_tail(self):
        real_wait = subprocess.Popen.wait
        children = []
        def stuck_wait(child, timeout=None):
            children.append(child)
            self.assertIsNotNone(timeout, "reaping after kill must have a deadline")
            raise subprocess.TimeoutExpired(child.args, timeout)
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "stuck-reap.log"
            try:
                with patch.object(subprocess.Popen, "wait", stuck_wait):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        EVIDENCE.capture_tail([sys.executable, "-c",
                            "import time;print('primary failure',flush=True);time.sleep(60)"],
                            output, timeout=0.2)
                self.assertEqual(output.read_text(), "primary failure\n")
            finally:
                for child in children:
                    real_wait(child, timeout=2)

    def test_logs_are_retained_beside_result(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            engine = root / "fixture-engine"
            engine.write_text("#!/bin/sh\nprintf 'retained failure'\nexit 2\n")
            engine.chmod(0o700)
            result = root / "artifacts" / "relay-result.json"
            records, errors = EVIDENCE.retain_container_logs(str(engine), ["fixture-id"], result)
            self.assertEqual(errors, [])
            self.assertEqual(records[0]["exit"], 2)
            self.assertEqual((result.parent / records[0]["file"]).read_text(), "retained failure")
            self.assertEqual(records[0]["container"], "fixture-id")

    def test_diagnostic_io_error_remains_secondary(self):
        with tempfile.TemporaryDirectory() as folder:
            blocked = Path(folder) / "not-a-directory"
            blocked.write_text("unchanged")
            records, errors = EVIDENCE.retain_container_logs("unused", ["fixture"], blocked / "result.json")
            self.assertEqual(records, [])
            self.assertEqual(len(errors), 1)
            self.assertEqual(blocked.read_text(), "unchanged")
