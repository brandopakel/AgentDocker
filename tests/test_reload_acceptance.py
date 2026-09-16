"""The reload acceptance trial's cleanup runs every step whatever the one
before it did, and a failed step or a surviving process fails the trial."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
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
