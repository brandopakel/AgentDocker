"""verify.sh takes the machine's build-slot lease for the run: as the session
when the daemon knows it, as a transient agent of its own otherwise, never
unheld while somebody else holds it, renewed while the run lasts, and with
the managed session's variables kept out of the suites' environment."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

FAKE_CLI = r'''#!/usr/bin/env python3
"""A stand-in agentdocker: records every call, answers by scenario."""
import os, sys
log = os.environ["FAKE_LOG"]; mode = os.environ["FAKE_MODE"]
args = sys.argv[1:]
with open(log, "a") as f: f.write(" ".join(args) + "\n")
cmd = args[0] if args else ""
def held_rows():
    if mode in ("held", "held-then-conflict"):
        return "LEASE ID  RESOURCE  HOLDER\nother001  task:local-cargo-campaign  somebody\n"
    if mode == "preheld":
        return "LEASE ID  RESOURCE  HOLDER\nlease-known  task:local-cargo-campaign  abc\n"
    return "LEASE ID  RESOURCE  HOLDER\n"
if cmd == "ping": sys.exit(0)
if cmd == "leases": sys.stdout.write(held_rows()); sys.exit(0)
if cmd == "register":
    if mode == "register-fails": sys.stderr.write("Error: the project is paused (paused)\n"); sys.exit(4)
    print("transient-id"); sys.exit(0)
if cmd == "renew":
    if mode == "renew-fails": sys.stderr.write("Error: no such lease (not_found)\n"); sys.exit(4)
    sys.exit(0)
if cmd in ("release", "deregister"): sys.exit(0)
if cmd == "claim":
    has_as = "--as" in args
    if mode in ("known", "preheld", "renew-fails"): print("lease-known"); sys.exit(0)
    if mode == "register-fails":
        sys.stderr.write("Error: give --as\n"); sys.exit(2)
    if mode == "anonymous":
        if has_as: print("lease-transient"); sys.exit(0)
        sys.stderr.write("Error: cannot tell which agent this is; give --as\n"); sys.exit(2)
    if mode == "held":
        sys.stderr.write("Error: task:local-cargo-campaign is held by somebody (conflict)\n"); sys.exit(3)
    if mode == "held-then-conflict":
        if has_as:
            sys.stderr.write("Error: held by somebody (conflict)\n"); sys.exit(3)
        sys.stderr.write("Error: give --as\n"); sys.exit(2)
sys.stderr.write("unexpected " + " ".join(args) + "\n"); sys.exit(9)
'''


class VerifyCampaign(unittest.TestCase):
    def run_verify(self, mode, environment, cargo_sleep=0.0):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(Path(__file__).parents[1] / "scripts/verify.sh", root / "scripts/verify.sh")
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            binary = root / "bin"; binary.mkdir()
            cli = binary / "agentdocker"; cli.write_text(FAKE_CLI); cli.chmod(0o700)
            log = root / "calls.log"; seen = root / "cargo-env.log"
            cargo = binary / "cargo"
            # The stand-in cargo notes its environment, then works for a
            # while with a helper of its own, as cargo does with rustc,
            # so a stopped step must take its helper down too.
            cargo.write_text(
                "#!/bin/sh\n"
                "env | grep '^AGENTDOCKER_' >> \"$CARGO_ENV_LOG\" || true\n"
                "echo step >> \"$CARGO_ENV_LOG.steps\"\n"
                f"sleep {cargo_sleep + 30} & echo $! >> \"$CARGO_ENV_LOG.helpers\"\n"
                f"sleep {cargo_sleep}\n"
                "kill $! 2>/dev/null; exit 0\n")
            cargo.chmod(0o700)
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_") and k != "CI"}
            env.update(environment)
            env.update(PATH=str(binary) + os.pathsep + os.environ["PATH"], FAKE_LOG=str(log),
                       FAKE_MODE=mode, CARGO_ENV_LOG=str(seen), HOME=str(root))
            result = subprocess.run(["bash", "scripts/verify.sh", "test"], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=20)
            calls = log.read_text().splitlines() if log.exists() else []
            cargo_env = seen.read_text() if seen.exists() else ""
            steps = Path(str(seen) + ".steps")
            helpers = Path(str(seen) + ".helpers")
            self.steps = len(steps.read_text().splitlines()) if steps.exists() else 0
            self.helpers = [int(pid) for pid in helpers.read_text().split()] if helpers.exists() else []
            return result, calls, cargo_env

    def test_a_known_session_claims_as_itself_and_the_suites_see_none_of_its_variables(self):
        result, calls, cargo_env = self.run_verify("known", {
            "AGENTDOCKER_AGENT_ID": "abc", "AGENTDOCKER_SOCKET": "/tmp/s.sock",
            "AGENTDOCKER_AGENT_NAME": "me", "AGENTDOCKER_CLAUDE_CHANNEL_INPUT": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        claim = next(c for c in calls if c.startswith("claim"))
        self.assertIn("--as abc", claim)
        self.assertIn("--socket /tmp/s.sock", claim)
        self.assertIn("release lease-known --as abc", " ".join(calls))
        self.assertNotIn("register", " ".join(calls))
        self.assertNotIn("AGENTDOCKER_AGENT_ID", cargo_env)
        self.assertNotIn("AGENTDOCKER_SOCKET", cargo_env)
        self.assertNotIn("AGENTDOCKER_CLAUDE_CHANNEL_INPUT", cargo_env)
        self.assertIn("AGENTDOCKER_CAMPAIGN_LEASE=off", cargo_env, "nested runs inherit the decision")

    def test_an_unnamed_run_registers_itself_holds_and_ends_its_agent(self):
        result, calls, _ = self.run_verify("anonymous", {})
        self.assertEqual(result.returncode, 0, result.stderr)
        joined = "\n".join(calls)
        self.assertIn("register --name verify-", joined)
        self.assertIn("claim task:local-cargo-campaign --as transient-id", joined)
        self.assertIn("release lease-transient --as transient-id", joined)
        self.assertIn("deregister --as transient-id", joined)
        self.assertLess(joined.index("release lease-transient"), joined.index("deregister"))

    def test_a_held_slot_stops_the_run_before_cargo(self):
        for mode in ("held", "held-then-conflict"):
            result, calls, cargo_env = self.run_verify(mode, {"AGENTDOCKER_AGENT_ID": "abc"} if mode == "held" else {})
            self.assertEqual(result.returncode, 75, f"{mode}: {result.stderr}")
            self.assertIn("held by another campaign", result.stderr)
            self.assertEqual(cargo_env, "", f"{mode}: cargo ran")
            if mode == "held-then-conflict":
                self.assertIn("deregister --as transient-id", "\n".join(calls), "the transient agent is ended")

    def test_a_run_the_daemon_will_not_have_does_not_run(self):
        result, calls, cargo_env = self.run_verify("register-fails", {})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("could not register as one", result.stderr)
        self.assertEqual(cargo_env, "", "cargo ran unheld")
        self.assertNotIn("release", "\n".join(calls))

    def test_the_off_switch_still_keeps_the_session_out_of_the_suites(self):
        home = os.path.join(os.environ.get("HOME", "/nonexistent"), ".agentdocker")
        result, calls, cargo_env = self.run_verify("known", {
            "AGENTDOCKER_CAMPAIGN_LEASE": "off", "AGENTDOCKER_HOME": home,
            "AGENTDOCKER_AGENT_ID": "abc", "AGENTDOCKER_SOCKET": "/tmp/s.sock"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [])
        for name in ("AGENTDOCKER_HOME", "AGENTDOCKER_AGENT_ID", "AGENTDOCKER_SOCKET"):
            self.assertNotIn(name, cargo_env)

    def test_a_lease_the_caller_already_held_is_renewed_and_never_released(self):
        result, calls, _ = self.run_verify("preheld", {
            "AGENTDOCKER_AGENT_ID": "abc", "AGENTDOCKER_CAMPAIGN_RENEW_SECS": "1"}, cargo_sleep=1.5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already yours", result.stderr)
        joined = "\n".join(calls)
        self.assertIn("renew lease-known --as abc", joined)
        self.assertNotIn("release", joined)
        self.assertEqual(self.steps, 2)

    def test_a_lost_slot_stops_the_run_and_its_whole_tree(self):
        # The first renewal fails at once: the step in flight is ended with
        # the helper it started, no further step starts, the run reads as
        # the loss, and the lease is still released on the way out.
        result, calls, _ = self.run_verify("renew-fails", {
            "AGENTDOCKER_AGENT_ID": "abc", "AGENTDOCKER_CAMPAIGN_RENEW_SECS": "0"}, cargo_sleep=6)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("the build slot was lost", result.stderr)
        self.assertLessEqual(self.steps, 1, "no step starts on another's campaign")
        self.assertIn("release lease-known", "\n".join(calls), "released on the way out")
        import time
        deadline = time.time() + 5
        while time.time() < deadline and any(self.alive(pid) for pid in self.helpers):
            time.sleep(0.1)
        self.assertFalse(any(self.alive(pid) for pid in self.helpers), "the step's helper was left running")

    @staticmethod
    def alive(pid):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True
        return True

    def test_ci_a_private_daemon_or_off_skips_the_slot(self):
        for environment in ({"CI": "1"}, {"AGENTDOCKER_HOME": "/tmp/private-home"}, {"AGENTDOCKER_CAMPAIGN_LEASE": "off"}):
            result, calls, _ = self.run_verify("known", environment)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls, [], f"{environment}: the daemon was asked")


if __name__ == "__main__":
    unittest.main()
