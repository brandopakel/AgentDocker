"""A failed scheduled-task stop must not skip owned-process retirement."""
import copy
import importlib.util
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location('selective_retention',
    Path(__file__).resolve().parents[1] / 'scripts/windows_selective_retention_smoke.py')
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class ProcessError(Exception):
    pass


class TimeoutExpired(ProcessError):
    pass


class NoSuchProcess(ProcessError):
    pass


class CleanupFailures(unittest.TestCase):
    def setUp(self):
        substitute = patch.dict(sys.modules, {'psutil': SimpleNamespace(
            Error=ProcessError, TimeoutExpired=TimeoutExpired, NoSuchProcess=NoSuchProcess)})
        substitute.start()
        self.addCleanup(substitute.stop)
        self.detail = {'result': 'passed', 'cleanup_errors': []}
        self.saved = []

    def cleanup(self, stop, *processes):
        identities = {(p.pid, 'birth'): p for p in processes}
        SMOKE.cleanup_owned_processes(stop, Mock(), identities, self.detail,
            lambda: self.saved.append(copy.deepcopy(self.detail)))

    def process(self, pid, waits=(None,), kill_error=None):
        return SimpleNamespace(pid=pid, wait=Mock(side_effect=list(waits)),
                               kill=Mock(side_effect=kill_error))

    def test_stop_failure_still_retires_all_owned_processes_and_saves_original_error(self):
        original = AssertionError('scheduler did not stop')
        first = self.process(10, [TimeoutExpired(), None])
        second = self.process(20)
        with self.assertRaises(AssertionError) as caught:
            self.cleanup(Mock(side_effect=original), first, second)
        self.assertIs(caught.exception, original)
        first.kill.assert_called_once_with()
        second.wait.assert_called_once_with(timeout=5)
        self.assertEqual(self.saved[-1]['result'], 'failed')
        self.assertTrue(any('scheduler did not stop' in e for e in self.saved[-1]['cleanup_errors']))

    def test_kill_failure_does_not_skip_later_processes(self):
        first = self.process(10, [TimeoutExpired()], ProcessError('access denied'))
        second = self.process(20)
        self.cleanup(Mock(), first, second)
        second.wait.assert_called_once_with(timeout=5)
        self.assertEqual(self.saved[-1]['result'], 'failed')
        self.assertTrue(any('access denied' in e for e in self.detail['cleanup_errors']))

    def test_followup_wait_timeout_does_not_skip_later_processes(self):
        first = self.process(10, [TimeoutExpired(), TimeoutExpired('still running')])
        second = self.process(20)
        self.cleanup(Mock(), first, second)
        second.wait.assert_called_once_with(timeout=5)
        self.assertTrue(any('still running' in e for e in self.detail['cleanup_errors']))
        self.assertEqual(self.saved[-1]['result'], 'failed')

    def test_inspection_failure_does_not_skip_later_processes(self):
        first = self.process(10, [ProcessError('wait refused')])
        second = self.process(20)
        self.cleanup(Mock(), first, second)
        first.kill.assert_not_called()
        second.wait.assert_called_once_with(timeout=5)
        self.assertTrue(any('wait refused' in e for e in self.detail['cleanup_errors']))
        self.assertEqual(self.saved[-1]['result'], 'failed')

    def test_retired_and_already_absent_processes_need_no_signal(self):
        first = self.process(10)
        second = self.process(20, [NoSuchProcess()])
        self.cleanup(Mock(), first, second)
        first.kill.assert_not_called()
        second.kill.assert_not_called()
        self.assertEqual(self.saved[-1], {'result': 'passed', 'cleanup_errors': []})


if __name__ == '__main__':
    unittest.main()
