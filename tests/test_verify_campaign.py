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
    head = "LEASE ID  RESOURCE  HOLDER  MODE  EXPIRES  NOTE\n"
    if mode in ("held", "held-then-conflict"):
        return head + "other001  task:local-cargo-campaign  somebody  exclusive  in 20m\n"
    if mode in ("preheld", "own-held"):
        return head + "lease-known  task:local-cargo-campaign  abc  exclusive  in 20m\n"
    if mode == "own-shared":
        return head + "lease-known  task:local-cargo-campaign  abc  shared  in 20m\n"
    return head
if cmd == "ping": sys.exit(0)
if cmd == "leases": sys.stdout.write(held_rows()); sys.exit(0)
if cmd == "register":
    if mode == "register-fails": sys.stderr.write("Error: the project is paused (paused)\n"); sys.exit(4)
    print("transient-id"); sys.exit(0)
if cmd == "renew" and os.environ.get("FAKE_RENEW_REFUSED"):
    sys.stderr.write("Error: lease " + args[1] + " belongs to somebody, not abc (forbidden)\n"); sys.exit(5)
if cmd == "renew":
    # In the scenarios that name a lease, only the one held can be renewed.
    if mode == "own-held" and args[1] != "lease-known":
        sys.stderr.write("Error: lease " + args[1] + " not found (not_found)\n"); sys.exit(4)
    marker = os.environ.get("FAKE_RENEW_AFTER")
    if mode == "renew-fails" and (not marker or os.path.exists(marker)):
        sys.stderr.write("Error: no such lease (not_found)\n"); sys.exit(4)
    sys.exit(0)
if cmd in ("release", "deregister"): sys.exit(0)
if cmd == "claim" and "--help" in args:
    # A CLI that knows a fresh claim says so in its help; an older one does not.
    print("Usage: agentdocker claim [OPTIONS] <RESOURCE>" + ("\n      --fresh  Only a new lease will do" if os.environ.get("FAKE_FRESH") else ""))
    sys.exit(0)
if cmd == "claim":
    has_as = "--as" in args
    if "--fresh" in args and os.environ.get("FAKE_OLD_DAEMON"):
        # The CLI asks the daemon before a fresh claim; an older one is refused there.
        sys.stderr.write("Error: the running daemon does not know a fresh claim: it is older than this command and would renew a lease you already hold instead. Nothing was claimed.\n"); sys.exit(1)
    if mode == "own-held":
        if "--fresh" in args:
            sys.stderr.write("Error: task:local-cargo-campaign is already held by this agent (lease lease-known): a fresh claim waits for it to be released (conflict)\n"); sys.exit(3)
        print("lease-known"); sys.exit(0)
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
                f"[ -n \"$CARGO_FAIL\" ] && exit \"$CARGO_FAIL\"\n"
                f"sleep {cargo_sleep + 30} >/dev/null 2>&1 </dev/null & a=$!\n"
                # One helper that ignores TERM, as a stuck test binary might.
                f"sh -c 'trap \"\" TERM; sleep {cargo_sleep + 30}' >/dev/null 2>&1 </dev/null & b=$!\n"
                "echo $a $b >> \"$CARGO_ENV_LOG.helpers\"\n"
                f"sleep {cargo_sleep}\n"
                "kill -9 $a $b 2>/dev/null; exit 0\n")
            cargo.chmod(0o700)
            # The run looks for the tools its mode needs before anything else.
            nextest = binary / "cargo-nextest"; nextest.write_text("#!/bin/sh\n"); nextest.chmod(0o700)
            (root / "scripts/benchmark_manifest.py").write_text('print(\'{"source":"fixture"}\')\n')
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_") and k != "CI"}
            env.update(environment)
            env.update(PATH=str(binary) + os.pathsep + os.environ["PATH"], FAKE_LOG=str(log),
                       FAKE_MODE=mode, CARGO_ENV_LOG=str(seen), HOME=str(root),
                       CARGO_FAIL=environment.pop("CARGO_FAIL", ""))
            result = subprocess.run(["bash", "scripts/verify.sh", environment.pop("VERIFY_MODE", "test")],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=30)
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
        # The first `claim` call only asks the CLI whether it knows `--fresh`.
        claim = next(c for c in calls if c.startswith("claim task:"))
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

    def test_the_slot_is_claimed_fresh_where_the_cli_knows_how(self):
        # One build at a time, whoever the builders claim as: the claim
        # says a lease this agent already holds does not count.
        result, calls, _ = self.run_verify("known", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        claim = next(c for c in calls if c.startswith("claim task:"))
        self.assertIn("--fresh", claim)
        self.assertIn("release lease-known --as abc", "\n".join(calls))
        # An older CLI is asked as before.
        result, calls, _ = self.run_verify("known", {"AGENTDOCKER_AGENT_ID": "abc"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--fresh", next(c for c in calls if c.startswith("claim task:")))

    def test_another_run_of_the_same_agent_holds_the_slot_and_this_one_does_not_build(self):
        result, calls, cargo_env = self.run_verify("own-held", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("held by another run of this same agent", result.stderr)
        self.assertIn("AGENTDOCKER_CAMPAIGN_LEASE=<its lease id>", result.stderr)
        self.assertEqual(cargo_env, "", "cargo ran beside the other run")
        self.assertNotIn("release", "\n".join(calls), "the other run's lease is not this one's to release")

    def test_a_lease_the_caller_names_is_worked_inside_and_never_released(self):
        result, calls, cargo_env = self.run_verify("own-held", {
            "AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1", "AGENTDOCKER_CAMPAIGN_LEASE": "lease-known"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("inside the caller's own hold", result.stderr)
        joined = "\n".join(calls)
        # Renewing the named lease is the proof that it is this agent's:
        # nothing is claimed, so nothing of another worker's can be taken
        # for it, and nothing is released.
        self.assertIn("renew lease-known --as abc", joined)
        self.assertNotIn("claim", joined)
        self.assertNotIn("release", joined)
        self.assertNotEqual(cargo_env, "", "the run went on to cargo")

    def test_a_named_lease_that_is_not_held_stops_the_run_whoever_holds_the_slot_now(self):
        # The lease the caller names lapsed, and another of the agent's
        # workers has the slot (as lease-known): no claim is made that it
        # could be answered with, no cargo runs, nothing is released.
        result, calls, cargo_env = self.run_verify("own-held", {
            "AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1", "AGENTDOCKER_CAMPAIGN_LEASE": "lease-of-mine"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("names lease-of-mine, which is not an exclusive lease on task:local-cargo-campaign", result.stderr)
        self.assertEqual(cargo_env, "")
        joined = "\n".join(calls)
        self.assertNotIn("claim", joined)
        self.assertNotIn("release", joined)
        # The slot's own exclusive lease, but another agent's: only its
        # holder can renew it, and the refusal stops the run.
        result, calls, cargo_env = self.run_verify("held", {
            "AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1", "FAKE_RENEW_REFUSED": "1",
            "AGENTDOCKER_CAMPAIGN_LEASE": "other001"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("names other001, which this agent does not hold", result.stderr)
        self.assertEqual(cargo_env, "")
        joined = "\n".join(calls)
        self.assertIn("renew other001 --as abc", joined)
        self.assertNotIn("claim", joined)
        self.assertNotIn("release", joined)

    def test_a_named_lease_must_be_the_build_slots_own_and_exclusive(self):
        # Any lease this agent holds would renew: one on an unrelated path
        # while somebody else has the slot, or a shared hold of the slot,
        # says nothing about holding it. Neither is renewed, claimed for,
        # or built under.
        for mode, named in (("held", "lease-on-an-unrelated-path"), ("own-shared", "lease-known")):
            result, calls, cargo_env = self.run_verify(mode, {
                "AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1", "AGENTDOCKER_CAMPAIGN_LEASE": named})
            self.assertEqual(result.returncode, 75, f"{mode}: {result.stderr}")
            self.assertIn("not an exclusive lease on task:local-cargo-campaign", result.stderr)
            self.assertEqual(cargo_env, "", f"{mode}: cargo ran without the slot")
            joined = "\n".join(calls)
            for never in ("renew", "claim", "release"):
                self.assertNotIn(never, joined, f"{mode}: {never}")

    def test_a_daemon_older_than_the_cli_is_claimed_plainly_and_a_held_lease_is_kept(self):
        # The CLI knows `--fresh` and the running daemon does not: the CLI
        # refuses the fresh claim with nothing claimed, and the run falls
        # back to a plain one. The lease this agent already held is then
        # the caller's own, as before the word existed: never released.
        result, calls, cargo_env = self.run_verify("preheld", {
            "AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1", "FAKE_OLD_DAEMON": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already yours", result.stderr)
        claims = [c for c in calls if c.startswith("claim task:")]
        self.assertEqual(len(claims), 2, claims)
        self.assertIn("--fresh", claims[0])
        self.assertNotIn("--fresh", claims[1])
        self.assertNotIn("release", "\n".join(calls))
        self.assertNotEqual(cargo_env, "")
        # With nothing held before, the plain claim's lease is this run's.
        result, calls, _ = self.run_verify("known", {
            "AGENTDOCKER_AGENT_ID": "abc", "FAKE_FRESH": "1", "FAKE_OLD_DAEMON": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("release lease-known --as abc", "\n".join(calls))

    def test_a_lost_slot_stops_the_run_and_its_whole_tree(self):
        # The first renewal fails at once: the step in flight is ended with
        # the helper it started, no further step starts, the run reads as
        # the loss, and the lease is still released on the way out.
        result, calls, _ = self.run_verify("renew-fails", {
            "AGENTDOCKER_AGENT_ID": "abc", "AGENTDOCKER_CAMPAIGN_RENEW_SECS": "1"}, cargo_sleep=8)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("the build slot was lost", result.stderr)
        self.assertEqual(self.steps, 1, "the first step was in flight; no second step starts")
        self.assertIn("release lease-known", "\n".join(calls), "released on the way out")
        # Both helpers are gone by the time the run has exited — the one
        # that ignored TERM was killed — and the lease went after them.
        self.assertEqual(len(self.helpers), 2)
        self.assertFalse(any(self.alive(pid) for pid in self.helpers), "a helper of the step outlived the run")

    def test_the_bench_finisher_ends_the_campaign_too(self):
        # bench and fuzz take over the EXIT trap with a finisher of their
        # own: a first-step failure there must still release the lease and
        # stop the renew helper.
        result, calls, _ = self.run_verify("known", {
            "AGENTDOCKER_AGENT_ID": "abc", "VERIFY_MODE": "bench", "CARGO_FAIL": "7",
            "AGENTDOCKER_CAMPAIGN_RENEW_SECS": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("release lease-known --as abc", "\n".join(calls), "the finisher ended the campaign")

    def test_a_lost_slot_during_a_bench_workload_ends_the_campaign(self):
        # The six socket workloads are steps too: the one in flight is
        # ended, no further one starts, the status file says which ran and
        # how it ended, the finisher still runs and the lease is released.
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(Path(__file__).parents[1] / "scripts/verify.sh", root / "scripts/verify.sh")
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            (root / "scripts/benchmark_manifest.py").write_text('print(\'{"source":"fixture"}\')\n')
            binary = root / "bin"; binary.mkdir()
            cli = binary / "agentdocker"; cli.write_text(FAKE_CLI); cli.chmod(0o700)
            log = root / "calls.log"
            cargo = binary / "cargo"
            cargo.write_text("#!/usr/bin/env python3\nimport json, os, pathlib, sys, time\n"
                # Force setup to outlast the first renewal, reproducing the race.
                "if 'leases' in sys.argv: time.sleep(2)\n"
                "root = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'release'\n"
                "for flag, name, path in [('--bin', 'agentdocker', root / 'agentdocker'), "
                "('--example', 'socket_load', root / 'examples/socket_load')]:\n"
                "    if flag in sys.argv:\n"
                "        print(json.dumps({'reason': 'compiler-artifact', 'target': {'name': name}, 'executable': str(path)}))\n")
            cargo.chmod(0o700)
            build = root / "build"
            target = build / "release/examples"; target.mkdir(parents=True)
            (target.parent / "agentdocker").write_text("fixture daemon")
            workload = target / "socket_load"
            workload.write_text("#!/bin/sh\nprintf '%s %s\\n' \"$2\" \"$4\" >> calls\nsleep 8\necho '{}'\n")
            workload.chmod(0o700)
            (root / "artifacts").mkdir()
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_") and k != "CI"}
            env.update(PATH=str(binary) + os.pathsep + os.environ["PATH"], FAKE_LOG=str(log),
                       # Fail renewal only after the workload actually enters;
                       # compile/setup may legitimately take longer than one second.
                       FAKE_MODE="renew-fails", FAKE_RENEW_AFTER=str(root / "calls"),
                       CARGO_TARGET_DIR=str(build), HOME=str(root),
                       AGENTDOCKER_AGENT_ID="abc", AGENTDOCKER_CAMPAIGN_RENEW_SECS="1")
            result = subprocess.run(["bash", "scripts/verify.sh", "bench"], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=40)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("the remaining workloads do not run", result.stderr)
            self.assertEqual((root / "calls").read_text().splitlines(), ["1 shared"], "only the first workload started")
            status = (root / "artifacts/benchmark-status.tsv").read_text()
            self.assertIn("shared\t1\t75", status)
            self.assertEqual(len(status.splitlines()), 1)
            self.assertTrue((root / "artifacts/benchmark-manifest-after.json").exists(), "the finisher ran")
            self.assertIn("release lease-known --as abc", log.read_text())

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
