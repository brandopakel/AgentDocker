"""verify.sh records a gate run (check, quick or test) with the daemon it
negotiated the build slot with: the steps as they ran, the counts only the
run's own artifacts establish, the head and tree at both ends, who it was
for, once and bounded, and never changing the run's own result."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import tomllib
import unittest

REPO = Path(__file__).parents[1]

FAKE_CLI = r'''#!/usr/bin/env python3
"""A stand-in agentdocker: the slot, and the runs it is asked to record."""
import json, os, sys, time
args = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f: f.write(json.dumps(args) + "\n")
cmd = args[0] if args else ""
mode = os.environ.get("FAKE_MODE", "known")
if cmd == "ping": sys.exit(0)
if cmd == "leases": print("LEASE ID  RESOURCE  HOLDER  MODE  EXPIRES  NOTE"); sys.exit(0)
if cmd == "claim" and "--help" in args: print("Usage: agentdocker claim"); sys.exit(0)
if cmd == "claim":
    if mode == "anonymous" and "--as" not in args:
        sys.stderr.write("Error: cannot tell which agent this is; give --as\n"); sys.exit(2)
    print("lease-known"); sys.exit(0)
if cmd == "register": print("transient-id"); sys.exit(0)
if cmd == "renew":
    if mode == "renew-fails": sys.stderr.write("Error: no such lease (not_found)\n"); sys.exit(4)
    sys.exit(0)
if cmd in ("release", "deregister"): sys.exit(0)
if cmd == "gate-run":
    if os.environ.get("FAKE_RECORD") == "hang": time.sleep(30)
    path = args[args.index("--file") + 1]
    with open(os.environ["FAKE_RECORDS"], "a") as f:
        f.write(json.dumps({"args": args, "run": json.load(open(path))}) + "\n")
    print("run-id-1"); sys.exit(0)
sys.stderr.write("unexpected " + " ".join(args) + "\n"); sys.exit(9)
'''

# A stand-in cargo: notes what it ran; nextest writes a JUnit report into
# the store its --config-file names, and somebody else's into the shared
# folder, which a recorded run must never read.
FAKE_CARGO = r'''#!/usr/bin/env python3
import json, os, sys, time, tomllib
args = sys.argv[1:]
with open(os.environ["CARGO_LOG"], "a") as f: f.write(json.dumps(args) + "\n")
sub = args[0] if args else ""
if sub == "nextest":
    shared = os.path.join("target", "nextest", "ci"); os.makedirs(shared, exist_ok=True)
    with open(os.path.join(shared, "junit.xml"), "w") as f:
        f.write('<testsuites>' + '<testcase name="foreign"/>' * 99 + '</testsuites>')
    junit = os.environ.get("FAKE_JUNIT", "3,0")
    if "--config-file" in args and junit != "none":
        store = tomllib.load(open(args[args.index("--config-file") + 1], "rb"))["store"]["dir"]
        os.makedirs(os.path.join(store, "ci"), exist_ok=True)
        passed, failed = (int(n) for n in junit.split(","))
        cases = '<testcase name="ok"/>' * passed + '<testcase name="bad"><failure/></testcase>' * failed
        with open(os.path.join(store, "ci", "junit.xml"), "w") as f:
            f.write('<testsuites><testsuite>' + cases + '</testsuite></testsuites>')
time.sleep(float(os.environ.get("CARGO_SLEEP", "0")))
fail = os.environ.get("CARGO_FAIL", "")
if fail and fail.split(":")[0] == sub: sys.exit(int(fail.split(":")[1]))
sys.exit(0)
'''

FIXTURE_TESTS = '''import unittest
class Fixture(unittest.TestCase):
    def test_one(self): pass
    @unittest.skip("not here")
    def test_two(self): pass
'''


def git(root, *args):
    subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                   env=dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_AUTHOR_NAME="t",
                            GIT_AUTHOR_EMAIL="t@example.invalid", GIT_COMMITTER_NAME="t",
                            GIT_COMMITTER_EMAIL="t@example.invalid"))


class GateRecord(unittest.TestCase):
    def run_verify(self, mode="test", environment=None, prepare=None):
        environment = dict(environment or {})
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            for script in ("verify.sh", "gate_record.py"):
                shutil.copy2(REPO / "scripts" / script, root / "scripts" / script)
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            (root / "scripts/docs_check.py").write_text("print('docs ok')\n")
            # The selection writes what FAKE_PACKAGES names.
            (root / "scripts/touched_crates.py").write_text(
                "import os, sys\n"
                "args = sys.argv\n"
                "names = os.environ.get('FAKE_PACKAGES', '').split()\n"
                "open(args[args.index('--output') + 1], 'w').write(''.join(n + '\\n' for n in names))\n"
                "open(args[args.index('--doc-output') + 1], 'w').write('')\n")
            (root / ".config").mkdir()
            shutil.copy2(REPO / ".config/nextest.toml", root / ".config/nextest.toml")
            (root / "tests").mkdir()
            (root / "tests/test_fixture.py").write_text(FIXTURE_TESTS)
            (root / ".gitignore").write_text("target/\n")
            git(root, "init", "-q")
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "root")
            git(root, "update-ref", "refs/remotes/origin/main", "HEAD")
            if prepare:
                prepare(root)
            binary = root.parent / (root.name + "-bin")
            binary.mkdir()
            for name, text in (("agentdocker", FAKE_CLI), ("cargo", FAKE_CARGO), ("cargo-nextest", "#!/bin/sh\n")):
                (binary / name).write_text(text)
                (binary / name).chmod(0o700)
            logs = root.parent / (root.name + "-logs")
            logs.mkdir()
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_") and k != "CI"}
            env.update(PATH=str(binary) + os.pathsep + os.environ["PATH"], HOME=str(root),
                       FAKE_LOG=str(logs / "calls"), FAKE_RECORDS=str(logs / "records"),
                       CARGO_LOG=str(logs / "cargo"))
            env.update(environment)
            started = time.monotonic()
            result = subprocess.run(["bash", "scripts/verify.sh", mode], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=60)
            self.elapsed = time.monotonic() - started
            read = lambda name: [json.loads(line) for line in (logs / name).read_text().splitlines()] \
                if (logs / name).exists() else []
            self.calls, self.cargo = read("calls"), read("cargo")
            self.records = read("records")
            self.head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True,
                                       text=True).stdout.strip()
            self.branch = subprocess.run(["git", "symbolic-ref", "--short", "HEAD"], cwd=root,
                                         capture_output=True, text=True).stdout.strip()
            shutil.rmtree(binary)
            shutil.rmtree(logs)
            return result

    def only_record(self):
        self.assertEqual(len(self.records), 1, self.records)
        return self.records[0]

    def test_a_passing_test_run_is_recorded_with_its_own_counts(self):
        result = self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_JUNIT": "3,0"})
        self.assertEqual(result.returncode, 0, result.stderr)
        record = self.only_record()
        self.assertEqual(record["args"][record["args"].index("--as") + 1], "abc")
        self.assertNotIn("--runner", record["args"])
        run = record["run"]
        self.assertEqual(run["mode"], "test")
        self.assertEqual((run["exit"], run["completed"]), (0, True))
        self.assertEqual([s["name"] for s in run["steps"]], ["nextest", "doctests"])
        self.assertEqual([s["status"] for s in run["steps"]], [0, 0])
        # Its own report's counts, not the 99 in the shared folder; no
        # skipped count, since the report does not list skipped cases.
        self.assertEqual(run["steps"][0]["counts"], {"run": 3, "passed": 3, "failed": 0})
        self.assertNotIn("counts", run["steps"][1])
        self.assertEqual(run["start"], {"head": self.head, "status": "clean"})
        self.assertEqual(run["end"], {"head": self.head, "status": "clean"})
        self.assertEqual(run["branch"], self.branch)
        self.assertIn("recorded this run as gate run run-id-1", result.stderr)
        # The relocated policy is the checked-in one with a store of its own.
        nextest = next(call for call in self.cargo if call[0] == "nextest")
        self.assertIn("--config-file", nextest)
        # The record comes before the slot is released.
        order = [call[0] for call in self.calls]
        self.assertLess(order.index("gate-run"), order.index("release"))

    def test_the_relocated_policy_reads_as_the_checked_in_one(self):
        seen = {}

        def keep(root):
            seen["root"] = root

        self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc"}, prepare=keep)
        nextest = next(call for call in self.cargo if call[0] == "nextest")
        config = Path(nextest[nextest.index("--config-file") + 1])
        # The campaign folder is gone by now; rebuild the copy the same way.
        folder = Path(tempfile.mkdtemp())
        try:
            subprocess.run(["python3", str(REPO / "scripts/gate_record.py"), "nextest-config",
                            "--dir", str(folder), "--source", str(REPO / ".config/nextest.toml")], check=True)
            merged = tomllib.loads((folder / "nextest.toml").read_text())
            store = merged.pop("store")
            self.assertEqual(store, {"dir": str(folder / "nextest")})
            self.assertEqual(merged, tomllib.loads((REPO / ".config/nextest.toml").read_text()))
        finally:
            shutil.rmtree(folder)
        self.assertTrue(str(config).endswith("nextest.toml"))

    def test_a_failing_step_is_recorded_with_its_status_and_nothing_after_it(self):
        result = self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "CARGO_FAIL": "nextest:100",
                                          "FAKE_JUNIT": "2,1"})
        self.assertEqual(result.returncode, 100, result.stderr)
        run = self.only_record()["run"]
        self.assertEqual((run["exit"], run["completed"]), (100, False))
        self.assertEqual([(s["name"], s["status"]) for s in run["steps"]], [("nextest", 100)])
        self.assertEqual(run["steps"][0]["counts"], {"run": 3, "passed": 2, "failed": 1})

    def test_nextest_failing_before_its_report_leaves_counts_missing(self):
        self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "CARGO_FAIL": "nextest:101",
                                 "FAKE_JUNIT": "none"})
        run = self.only_record()["run"]
        self.assertNotIn("counts", run["steps"][0])

    def test_two_runs_keep_their_own_counts(self):
        self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_JUNIT": "5,0"})
        first = self.only_record()["run"]
        self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_JUNIT": "7,0"})
        second = self.only_record()["run"]
        self.assertEqual(first["steps"][0]["counts"]["run"], 5)
        self.assertEqual(second["steps"][0]["counts"]["run"], 7)
        self.assertNotEqual(first["run_key"], second["run_key"])

    def test_an_empty_quick_records_its_scope_and_the_suite_counts(self):
        result = self.run_verify("quick", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_PACKAGES": ""})
        self.assertEqual(result.returncode, 0, result.stderr)
        run = self.only_record()["run"]
        self.assertEqual([s["name"] for s in run["steps"]],
                         ["docs", "fmt", "fuzz_lock", "selection", "installer"])
        self.assertEqual(run["scope"], {"base": self.head, "packages": [], "doc_packages": []})
        self.assertTrue(run["completed"])
        # unittest said OK with one skipped: no passed count is made up.
        self.assertEqual(run["steps"][-1]["counts"], {"run": 2, "failed": 0, "skipped": 1})
        # The suite's own output still reached the terminal.
        self.assertIn("Ran 2 tests", result.stderr)

    def test_a_quick_with_packages_names_them(self):
        result = self.run_verify("quick", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_PACKAGES": "agentd"})
        self.assertEqual(result.returncode, 0, result.stderr)
        run = self.only_record()["run"]
        self.assertEqual(run["scope"]["packages"], ["agentd"])
        self.assertIn("nextest", [s["name"] for s in run["steps"]])

    def test_a_quick_without_a_merge_base_is_recorded_failing_before_any_step(self):
        def unrelated(root):
            # origin/main names a commit that shares no history with HEAD.
            env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_AUTHOR_NAME="t",
                       GIT_AUTHOR_EMAIL="t@example.invalid", GIT_COMMITTER_NAME="t",
                       GIT_COMMITTER_EMAIL="t@example.invalid")
            empty = subprocess.run(["git", "hash-object", "-t", "tree", "-w", "--stdin"], cwd=root,
                                   input="", capture_output=True, text=True, check=True, env=env).stdout.strip()
            other = subprocess.run(["git", "commit-tree", empty, "-m", "unrelated"], cwd=root,
                                   capture_output=True, text=True, check=True, env=env).stdout.strip()
            git(root, "update-ref", "refs/remotes/origin/main", other)

        result = self.run_verify("quick", {"AGENTDOCKER_AGENT_ID": "abc"}, prepare=unrelated)
        self.assertEqual(result.returncode, 1)
        run = self.only_record()["run"]
        self.assertEqual((run["exit"], run["completed"], run["steps"]), (1, False, []))
        self.assertNotIn("scope", run)

    def test_a_lost_slot_is_recorded_as_stopped(self):
        result = self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_MODE": "renew-fails",
                                          "AGENTDOCKER_CAMPAIGN_RENEW_SECS": "1", "CARGO_SLEEP": "4"})
        self.assertEqual(result.returncode, 75, result.stderr)
        run = self.only_record()["run"]
        self.assertEqual(run["exit"], 75)
        self.assertEqual(run["steps"][0]["status"], 75)

    def test_a_dirty_tree_is_said(self):
        self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc"},
                        prepare=lambda root: (root / "scratch.txt").write_text("x"))
        run = self.only_record()["run"]
        self.assertEqual(run["start"]["status"], "dirty")

    def test_an_unknown_caller_is_unstated_and_its_runner_kept_apart(self):
        result = self.run_verify("test", {"FAKE_MODE": "anonymous"})
        self.assertEqual(result.returncode, 0, result.stderr)
        args = self.only_record()["args"]
        self.assertNotIn("--as", args)
        self.assertEqual(args[args.index("--runner") + 1], "transient-id")

    def test_a_daemon_that_never_answers_neither_holds_the_run_nor_changes_it(self):
        result = self.run_verify("test", {"AGENTDOCKER_AGENT_ID": "abc", "FAKE_RECORD": "hang"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLess(self.elapsed, 25)
        self.assertIn("recording this run was not confirmed", result.stderr)
        self.assertEqual(sum(1 for call in self.calls if call[0] == "gate-run"), 1)
        self.assertIn("release", [call[0] for call in self.calls])

    def test_off_and_ci_runs_record_nothing_and_keep_the_shared_report(self):
        for environment in ({"AGENTDOCKER_CAMPAIGN_LEASE": "off"}, {"CI": "1"}):
            environment["AGENTDOCKER_AGENT_ID"] = "abc"
            result = self.run_verify("test", environment)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.records, [], environment)
            self.assertNotIn("gate-run", [call[0] for call in self.calls], environment)
            nextest = next(call for call in self.cargo if call[0] == "nextest")
            self.assertNotIn("--config-file", nextest, environment)

    def test_other_modes_are_not_recorded(self):
        result = self.run_verify("package", {"AGENTDOCKER_AGENT_ID": "abc"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.records, [])


if __name__ == "__main__":
    unittest.main()
