"""The reload acceptance trial's cleanup runs every step whatever the one
before it did, and a failed step or a surviving process fails the trial."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("reload_acceptance", ROOT / "scripts/reload_acceptance.py")
TRIAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TRIAL)


class FakeDaemon:
    """A daemon handle that has already exited."""
    pid = 1

    def poll(self):
        return 0

    def kill(self):
        raise AssertionError("an exited daemon is not killed")

    def wait(self, timeout=None):
        return 0


class ReloadAcceptanceCleanup(unittest.TestCase):
    def test_cleanup_joins_inflight_worker_subprocess_and_skips_unstarted_thread(self):
        completed = []
        def worker():
            subprocess.run([sys.executable, "-c", "import time; time.sleep(.1)"], check=True)
            completed.append(True)
        thread = threading.Thread(target=worker)
        thread.start()
        result = {"passed": True}
        TRIAL.join_workers(result, [thread, threading.Thread(target=lambda: None)])
        self.assertEqual(completed, [True])
        self.assertFalse(thread.is_alive())
        self.assertTrue(result["passed"])

    def test_worker_survivor_fails_cleanup(self):
        class Stuck:
            ident = 1
            def join(self, timeout):
                pass
            def is_alive(self):
                return True
        result = {"passed": True}
        TRIAL.join_workers(result, [Stuck()])
        self.assertFalse(result["passed"])
        self.assertIn("workload worker did not stop", result["cleanup_errors"])

    def test_a_failed_shutdown_still_ends_the_children_and_fails_the_trial(self):
        children = [subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    for _ in range(2)]
        original = TRIAL.rpc

        def failing_rpc(sock, request, timeout=60):
            raise RuntimeError("shutdown refused")

        TRIAL.rpc = failing_rpc
        try:
            with tempfile.TemporaryDirectory() as temporary:
                sock = Path(temporary) / "never.sock"
                log = open(Path(temporary) / "log", "ab")
                result = {"passed": True, "daemon_pids": [1]}
                TRIAL.cleanup(result, sock, FakeDaemon(), log, children[0], children[1], set())
        finally:
            TRIAL.rpc = original
            for child in children:
                if child.poll() is None:
                    child.kill()
                    child.wait()
        self.assertTrue(all(child.poll() is not None for child in children), "children ended despite the failed shutdown")
        self.assertFalse(result["passed"])
        self.assertTrue(any(error.startswith("shutdown:") for error in result["cleanup_errors"]), result)
        self.assertIn("cleanup:", result["error"])
        self.assertTrue(log.closed)

    def test_failed_shutdown_retires_the_captured_successor_group(self):
        # This successor is a child here (unlike the real reparented one), so
        # reap it concurrently after fallback termination to model init.
        import threading
        successor = subprocess.Popen(["sleep", "30"],
                                     start_new_session=True)
        tracked = {successor.pid: TRIAL.process_identity(successor.pid)}
        reaper = threading.Thread(target=successor.wait, daemon=True)
        reaper.start()
        original = TRIAL.rpc
        TRIAL.rpc = lambda *args, **kwargs: {"type": "error", "code": "unavailable", "message": "shutdown refused"}
        try:
            with tempfile.TemporaryDirectory() as temporary:
                log = open(Path(temporary) / "log", "ab")
                result = {"passed": True, "daemon_pids": [successor.pid]}
                TRIAL.cleanup(result, Path(temporary) / "missing.sock", FakeDaemon(), log, None, None, set(), tracked)
            self.assertFalse(result["passed"], "a refused shutdown remains recorded")
            self.assertEqual(result["survivors"], [])
            self.assertEqual(len(result["cleanup_errors"]), 1, result)
            self.assertIsNotNone(successor.poll())
        finally:
            TRIAL.rpc = original
            if successor.poll() is None:
                successor.kill()
            reaper.join(timeout=5)

    def test_reused_successor_pid_is_not_signalled(self):
        successor = subprocess.Popen(["sleep", "30"],
                                     start_new_session=True)
        original = TRIAL.rpc
        TRIAL.rpc = lambda *args, **kwargs: {"type": "ok"}
        try:
            identity = TRIAL.process_identity(successor.pid)
            identity["description"] = "previous process at this PID"
            with tempfile.TemporaryDirectory() as temporary:
                log = open(Path(temporary) / "log", "ab")
                result = {"passed": True, "daemon_pids": [successor.pid]}
                TRIAL.cleanup(result, Path(temporary) / "missing.sock", FakeDaemon(), log, None, None, set(), {successor.pid: identity})
            self.assertIsNone(successor.poll(), "the different process must remain alive")
            self.assertEqual(result["survivors"], [])
        finally:
            TRIAL.rpc = original
            successor.kill()
            successor.wait()

    def test_a_survivor_the_trial_saw_started_fails_the_trial(self):
        survivor = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        original = TRIAL.rpc
        TRIAL.rpc = lambda sock, request, timeout=60: {"type": "ok"}
        try:
            with tempfile.TemporaryDirectory() as temporary:
                log = open(Path(temporary) / "log", "ab")
                result = {"passed": True}
                TRIAL.cleanup(result, Path(temporary) / "never.sock", FakeDaemon(), log, None, None, {survivor.pid})
        finally:
            TRIAL.rpc = original
            survivor.kill()
            survivor.wait()
        self.assertFalse(result["passed"])
        self.assertEqual(result["survivors"], [f"pid {survivor.pid}"])
        self.assertEqual(result["cleanup_errors"], [])


if __name__ == "__main__":
    unittest.main()
