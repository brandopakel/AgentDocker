"""Failed PTY fixture setup must preserve reports without leaking scratch state."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("terminal_smoke", ROOT / "scripts/terminal_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)

WINDOWS_SPEC = importlib.util.spec_from_file_location("windows_smoke", ROOT / "scripts/windows_daemon_smoke.py")
WINDOWS_SMOKE = importlib.util.module_from_spec(WINDOWS_SPEC)
WINDOWS_SPEC.loader.exec_module(WINDOWS_SMOKE)


class ManagedSessionCleanup(unittest.TestCase):
    def test_stop_requested_is_not_exit_observed(self):
        from unittest.mock import Mock
        stopping = {"status": {"state": "stopping"}}
        exited = {"status": {"state": "exited", "code": 0}}
        inspect = Mock(side_effect=[stopping, stopping, exited])
        with patch.object(WINDOWS_SMOKE.time, "sleep"):
            result = WINDOWS_SMOKE.wait_terminal(inspect, "fixture")
        self.assertEqual(result, exited)
        self.assertEqual(inspect.call_count, 3)

    def test_cleanup_deadline_preserves_live_status_for_force_fallback(self):
        from unittest.mock import Mock
        stopping = {"status": {"state": "stopping"}}
        inspect = Mock(return_value=stopping)
        with patch.object(WINDOWS_SMOKE.time, "monotonic", side_effect=[0, 0, 11]), patch.object(WINDOWS_SMOKE.time, "sleep"):
            result = WINDOWS_SMOKE.wait_terminal(inspect, "fixture")
        self.assertEqual(result, stopping)
        self.assertFalse(WINDOWS_SMOKE.terminal_record(result))
        self.assertEqual(inspect.call_count, 2)


class StartupSampling(unittest.TestCase):
    def step(self, label, ok, detail):
        if not ok:
            raise AssertionError(label + ": " + detail)

    def test_failure_is_retained_and_home_registered_without_retry(self):
        homes, samples = [], []
        cli = Mock(return_value=SimpleNamespace(returncode=1, stdout="", stderr="startup deadline"))
        home = Path("private-failed-sample")
        with self.assertRaisesRegex(AssertionError, "startup deadline"):
            WINDOWS_SMOKE.startup_sample(cli, self.step, homes, samples, home, "ordinary", 1)
        self.assertEqual(homes, [home])
        self.assertEqual(cli.call_count, 1)
        self.assertEqual(samples[0]["result"], "failed")
        self.assertIn("startup deadline", samples[0]["error"])
        self.assertGreaterEqual(samples[0]["elapsed_seconds"], 0)

    def test_stop_without_observed_exit_keeps_failed_result_and_cleanup_home(self):
        homes, samples = [], []
        cli = Mock(side_effect=[SimpleNamespace(returncode=0, stdout="pong", stderr=""),
                               SimpleNamespace(returncode=0, stdout="stopped", stderr=""),
                               SimpleNamespace(returncode=0, stdout="still running", stderr="")])
        home = Path("private-live-sample")
        with self.assertRaisesRegex(AssertionError, "exits"):
            WINDOWS_SMOKE.startup_sample(cli, self.step, homes, samples, home, "owner-rights", 1)
        self.assertEqual(homes, [home])
        self.assertEqual(samples[0]["result"], "failed")
        self.assertEqual(cli.call_args.kwargs["extra_env"]["AGENTDOCKER_NO_AUTOSTART"], "1")


class FixtureSetupCleanup(unittest.TestCase):
    def test_missing_binary_removes_fixture_and_records_failure(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            binaries = root / "binaries"
            binaries.mkdir()
            created = []
            mkdtemp = tempfile.mkdtemp

            def track_fixture(**kwargs):
                path = mkdtemp(prefix=kwargs["prefix"], dir=root)
                created.append(Path(path))
                return path

            old_mask = os.umask(0o022)
            try:
                with patch.object(SMOKE.tempfile, "mkdtemp", side_effect=track_fixture):
                    result = SMOKE.smoke(binaries, root / "report", "test-source")
                self.assertEqual(os.umask(0o022), 0o022)
            finally:
                os.umask(old_mask)
            self.assertEqual(result["failure_class"], "FileNotFoundError")
            self.assertEqual(result["result"], "failed")
            self.assertTrue(result["cleanup"]["fixture_removed"])
            self.assertEqual(len(created), 1)
            self.assertFalse(created[0].exists())
            self.assertEqual(json.loads((root / "report/result.json").read_text()), result)

    def test_existing_output_restores_umask_and_preserves_previous_report(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            report = root / "result.json"
            report.write_text("previous evidence")
            old_mask = os.umask(0o022)
            try:
                with self.assertRaises(FileExistsError):
                    SMOKE.smoke(root, root, "test-source")
                self.assertEqual(os.umask(0o022), 0o022)
            finally:
                os.umask(old_mask)
            self.assertEqual(report.read_text(), "previous evidence")



class FirstStartTiming(unittest.TestCase):
    """The smoke's first daemon start is timed and bounded by the clock,
    and every way it can end leaves a result with its timing rather than
    an error: this is the path that instruments a cold start."""

    def clock(self, *readings):
        readings = list(readings)

        def read():
            return readings.pop(0) if len(readings) > 1 else readings[0]

        return read

    def test_a_process_creation_that_takes_the_whole_budget_still_reports(self):
        # Requested at 0, created at 91: the budget is gone before any probe.
        probe = Mock()
        result = WINDOWS_SMOKE.wait_first_start(
            lambda: None, probe, lambda: True, budget=90.0, clock=self.clock(0, 91, 91, 91), sleep=lambda _: None)
        probe.assert_not_called()
        self.assertIsNone(result.probe)
        self.assertIsNone(result.answered)
        self.assertAlmostEqual(result.created, 91)
        self.assertIn("no probe was possible within 91 s", result.describe())

    def test_a_probe_never_exceeds_the_remaining_budget(self):
        # Created at once; 89.4 s have passed by the first probe: 0.6 s left.
        timeouts = []

        def probe(timeout):
            timeouts.append(timeout)
            return False

        result = WINDOWS_SMOKE.wait_first_start(
            lambda: None, probe, lambda: True, budget=90.0,
            clock=self.clock(0, 0, 89.4, 90.5, 90.5), sleep=lambda _: None)
        self.assertEqual(len(timeouts), 1)
        self.assertLessEqual(timeouts[0], 0.6)
        self.assertGreater(timeouts[0], 0)
        self.assertIsNone(result.answered)
        self.assertIn("no answer within", result.describe())

    def test_a_probe_that_raises_counts_as_no_answer_and_keeps_the_timing(self):
        def probe(timeout):
            raise AssertionError("ping did not exit within its bound")

        result = WINDOWS_SMOKE.wait_first_start(
            lambda: None, probe, lambda: True, budget=90.0,
            clock=self.clock(0, 0.5, 1, 95, 95), sleep=lambda _: None)
        self.assertIsInstance(result.probe, AssertionError)
        self.assertIsNone(result.answered)
        self.assertAlmostEqual(result.created, 0.5)

    def test_an_answer_records_creation_then_readiness(self):
        answers = iter([False, True])
        result = WINDOWS_SMOKE.wait_first_start(
            lambda: None, lambda timeout: next(answers), lambda: True, budget=90.0,
            clock=self.clock(0, 2, 3, 12, 12, 12), sleep=lambda _: None)
        self.assertAlmostEqual(result.created, 2)
        self.assertAlmostEqual(result.answered, 10)
        self.assertIsNone(result.exited)
        self.assertEqual(result.describe(), "process created 2.00 s after the request, answered 10.0 s after creation")

    def test_a_raised_probe_after_a_failed_one_is_what_the_report_says(self):
        # A failed ping fills the last returned result; the next probe raises:
        # the report carries the exception, not the stale stderr.
        class Result:
            stderr = "cannot reach agentd (stale)\n"

        said = WINDOWS_SMOKE.describe_probe(AssertionError("ping did not exit within 1 s"), Result())
        self.assertEqual(said, "AssertionError: ping did not exit within 1 s")
        self.assertEqual(WINDOWS_SMOKE.describe_probe(False, Result()), "cannot reach agentd (stale)")
        self.assertEqual(WINDOWS_SMOKE.describe_probe(None, None), "no probe")

    def test_a_daemon_that_exits_first_is_said_so(self):
        result = WINDOWS_SMOKE.wait_first_start(
            lambda: None, lambda timeout: False, lambda: False, budget=90.0,
            clock=self.clock(0, 0, 1, 1, 1), sleep=lambda _: None)
        self.assertTrue(result.exited)
        self.assertIsNone(result.answered)


if __name__ == "__main__":
    unittest.main()
