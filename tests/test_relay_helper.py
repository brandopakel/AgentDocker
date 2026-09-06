"""Exercise accept failures without starting an engine or binding the helper socket."""
import errno
import importlib.util
from pathlib import Path
import socket
import threading
import unittest
from unittest.mock import Mock, patch


class RelayAcceptTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location(
            'relay', Path(__file__).resolve().parents[1] / 'crates/host/src/relay.py')
        self.relay = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.relay)

    def stopped(self):
        self.relay.STOP.set()
        raise socket.timeout()

    def test_transient_accept_failure_backs_off_and_recovers(self):
        listener, stream = Mock(), Mock()
        calls = iter([OSError(errno.EMFILE, 'full'), (stream, None)])
        def accept():
            item = next(calls, None)
            if item is None:
                return self.stopped()
            if isinstance(item, Exception):
                raise item
            return item
        listener.accept.side_effect = accept
        with patch.object(self.relay.STOP, 'wait') as wait, patch.object(self.relay.threading, 'Thread') as worker:
            self.relay.accept(listener)
        wait.assert_called_once_with(0.01)
        worker.return_value.start.assert_called_once()
        stream.close.assert_not_called()

    def test_failed_thread_start_releases_slot_and_closes_stream(self):
        self.relay.SLOTS = threading.BoundedSemaphore(1)
        listener, stream = Mock(), Mock()
        count = 0
        def accept():
            nonlocal count
            count += 1
            return (stream, None) if count == 1 else self.stopped()
        listener.accept.side_effect = accept
        with patch.object(self.relay.threading, 'Thread') as worker, patch.object(self.relay.STOP, 'wait'):
            worker.return_value.start.side_effect = RuntimeError('cannot start thread')
            self.relay.accept(listener)
        stream.close.assert_called_once()
        self.assertTrue(self.relay.SLOTS.acquire(blocking=False))
        self.assertFalse(self.relay.SLOTS.acquire(blocking=False))

    def test_fatal_accept_failure_terminates_helper(self):
        listener = Mock()
        listener.accept.side_effect = OSError(errno.EBADF, 'bad listener')
        with patch.object(self.relay.os, '_exit', side_effect=SystemExit) as exit_process:
            with self.assertRaises(SystemExit):
                self.relay.accept(listener)
        exit_process.assert_called_once_with(1)
