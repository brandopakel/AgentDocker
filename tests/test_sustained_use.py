"""A stopped soak must keep evidence and never signal an already-reaped PID."""
import importlib.util
import json
import os
import signal
from types import SimpleNamespace
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("sustained_use", ROOT / "scripts/sustained_use.py")
SOAK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOAK)


class Cleanup(unittest.TestCase):
    def test_failed_descendant_query_still_reaps_daemon_and_known_fixture(self):
        errors = [subprocess.TimeoutExpired("ps", 5), subprocess.CalledProcessError(1, "ps")]
        for error in errors:
            with self.subTest(error=type(error).__name__):
                daemon = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                          start_new_session=True)
                fixture = None
                try:
                    fixture = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                               start_new_session=True)
                    fixtures = SOAK.FixtureProcesses()
                    fixtures.remember(fixture.pid)
                    with patch.object(SOAK.subprocess, "check_output", side_effect=error), \
                            patch.object(SOAK, "rpc", side_effect=TimeoutError("shutdown timed out")):
                        result = SOAK.stop_daemon(daemon, Path("absent.sock"),
                                                  fixtures, grace_seconds=0.01)
                    self.assertEqual(result["exit"], -signal.SIGKILL)
                    self.assertTrue(result["forced"])
                    self.assertEqual(result["fixtures"]["remaining"], [])
                    self.assertEqual(result["fixtures"]["errors"], [f"capture: {error}"])
                    self.assertIsNone(SOAK.process_identity(fixture.pid))
                finally:
                    for child in [daemon, fixture]:
                        if child is not None:
                            if child.poll() is None:
                                os.killpg(child.pid, signal.SIGKILL)
                            child.wait(timeout=5)

    def test_forced_daemon_exit_cleans_separate_fixture_group_and_descendant(self):
        with tempfile.TemporaryDirectory() as scratch:
            ready = Path(scratch) / "fixture-pids.json"
            # Both descendants ignore TERM so the bounded escalation is real.
            agent_code = """
import json, os, signal, subprocess, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = subprocess.Popen([sys.executable, '-c',
    'import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)'])
with open(sys.argv[1], 'w') as out:
    json.dump([os.getpid(), child.pid], out)
time.sleep(60)
"""
            daemon_code = """
import subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', sys.argv[1], sys.argv[2]], start_new_session=True)
child.wait()
time.sleep(60)
"""
            daemon = subprocess.Popen([sys.executable, "-c", daemon_code, agent_code, str(ready)],
                                      start_new_session=True)
            fixtures = SOAK.FixtureProcesses()
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() or not ready.read_text():
                    self.assertLess(time.monotonic(), deadline, "fixture did not start")
                    time.sleep(0.01)
                pids = json.loads(ready.read_text())
                fixtures.remember(pids[0])
                self.assertEqual(SOAK.process_identity(pids[1])["group"], pids[0])
                with patch.object(SOAK, "rpc", side_effect=TimeoutError("forced shutdown timeout")):
                    result = SOAK.stop_daemon(daemon, Path(scratch) / "absent.sock",
                                              fixtures, grace_seconds=0.05)
                self.assertTrue(result["forced"])
                self.assertEqual(result["exit"], -signal.SIGKILL)
                self.assertEqual(result["fixtures"]["remaining"], [])
                self.assertEqual(result["fixtures"]["errors"], [])
                self.assertEqual(set(fixtures.members), set(pids))
                self.assertIn({"group": pids[0], "signal": "SIGKILL"}, result["fixtures"]["signals"])
                self.assertTrue(all(SOAK.process_identity(pid) is None for pid in pids))
            finally:
                if daemon.poll() is None:
                    os.killpg(daemon.pid, signal.SIGKILL)
                daemon.wait(timeout=5)
                fixtures.capture()
                fixtures.stop()

    def test_reused_fixture_pid_is_neither_signalled_nor_reaped(self):
        fixtures = SOAK.FixtureProcesses()
        old = {"pid": 123, "group": 123, "birth": "original"}
        fixtures.leaders[123] = old
        fixtures.members[123] = old
        with patch.object(SOAK, "process_identity", return_value={**old, "birth": "reused"}), \
                patch.object(SOAK.os, "killpg") as kill, patch.object(SOAK.os, "waitpid") as reap:
            result = fixtures.stop()
        self.assertEqual(result, {"signals": [], "remaining": [], "errors": []})
        kill.assert_not_called()
        reap.assert_not_called()

    def test_exited_daemon_is_not_signalled_or_sent_another_shutdown(self):
        daemon = Mock(returncode=0)
        daemon.poll.return_value = 0
        with patch.object(SOAK, "rpc") as rpc, patch.object(SOAK.os, "killpg") as kill:
            result = SOAK.stop_daemon(daemon, Path("absent.sock"))
        self.assertEqual(result, {"exit": 0, "forced": False, "errors": []})
        rpc.assert_not_called()
        kill.assert_not_called()
        daemon.wait.assert_not_called()

    def test_disappearing_socket_waits_for_graceful_exit_before_force(self):
        daemon = Mock(returncode=None)
        daemon.poll.return_value = None

        def exited(**kwargs):
            daemon.returncode = 0
            return 0

        daemon.wait.side_effect = exited
        with patch.object(SOAK, "rpc", side_effect=FileNotFoundError("closed socket")), \
                patch.object(SOAK.os, "killpg") as kill:
            result = SOAK.stop_daemon(daemon, Path("absent.sock"))
        self.assertEqual(result["exit"], 0)
        self.assertFalse(result["forced"])
        self.assertIn("FileNotFoundError", result["errors"][0])
        kill.assert_not_called()

    def test_signal_permission_failure_is_reported_without_losing_result(self):
        daemon = Mock(pid=123, returncode=None)
        daemon.poll.return_value = None
        daemon.wait.side_effect = [subprocess.TimeoutExpired("fixture", 30),
                                   subprocess.TimeoutExpired("fixture", 5)]
        with patch.object(SOAK, "rpc"), patch.object(SOAK.os, "killpg", side_effect=PermissionError("denied")):
            result = SOAK.stop_daemon(daemon, Path("absent.sock"))
        self.assertTrue(result["forced"])
        self.assertIsNone(result["exit"])
        self.assertTrue(any("PermissionError" in error for error in result["errors"]))
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "result.json"
            SOAK.save_report(path, {"result": "failed", "cleanup": result})
            self.assertEqual(json.loads(path.read_text())["cleanup"], result)

    def test_failed_report_replacement_keeps_previous_evidence(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "result.json"
            SOAK.save_report(path, {"result": "running"})
            with patch.object(Path, "replace", side_effect=OSError("fixture replace failed")):
                with self.assertRaises(OSError):
                    SOAK.save_report(path, {"result": "interrupted"})
            self.assertEqual(json.loads(path.read_text()), {"result": "running"})


class Finalization(unittest.TestCase):
    def test_signal_after_last_passing_population_is_retained(self):
        for signum in [signal.SIGINT, signal.SIGTERM]:
            with self.subTest(signal=signum), tempfile.TemporaryDirectory() as scratch:
                binary = Path(scratch) / "agentd"
                binary.write_bytes(b"owned test executable")
                args = SimpleNamespace(binary=binary, output=Path(scratch) / "report",
                                       agents=[1], seconds=5, files=1)
                save = SOAK.save_report
                injected = False

                def after_population(path, report):
                    nonlocal injected
                    if report["populations"] and not injected:
                        injected = True
                        os.kill(os.getpid(), signum)
                    save(path, report)

                with patch.object(SOAK, "population", return_value={"result": "passed"}), \
                        patch.object(SOAK, "save_report", side_effect=after_population):
                    self.assertEqual(SOAK.main(args), 130)
                report = json.loads((args.output / "result.json").read_text())
                self.assertEqual(report["result"], "interrupted")
                self.assertEqual(report["signal"], {"number": signum, "name": signal.Signals(signum).name})
                self.assertTrue(report["binary_unchanged"] and report["driver_unchanged"])

    def test_signal_during_final_write_rewrites_success_without_hiding_failure(self):
        for outcome, expected in [("passed", "interrupted"), ("failed", "failed")]:
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as scratch:
                path = Path(scratch) / "result.json"
                interrupted = SOAK.Interruption()
                save = SOAK.save_report
                calls = []

                def interrupt_after_save(path, report):
                    calls.append(report["result"])
                    save(path, report)
                    interrupted.record(signal.SIGTERM, None)

                with patch.object(SOAK, "save_report", side_effect=interrupt_after_save):
                    code = SOAK.finish_report(path, {"result": outcome}, interrupted)
                report = json.loads(path.read_text())
                self.assertEqual(code, 130 if expected == "interrupted" else 1)
                self.assertEqual(report["result"], expected)
                self.assertEqual(report["signal"]["name"], "SIGTERM")
                self.assertEqual(calls, [outcome, expected])


if __name__ == "__main__":
    unittest.main()
