"""A failed scheduled run keeps one issue open, owned by the automation,
until a later scheduled run passes; a repeat, a stale rerun or a person's
issue changes nothing."""
import json
from pathlib import Path
import subprocess
import importlib.util
import io
import re
import sys
import unittest
from unittest import mock

REPO = Path(__file__).parents[1]
spec = importlib.util.spec_from_file_location("scheduled_report", REPO / "scripts/scheduled_report.py")
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)

WORKFLOW = "fuzz.yml"


class World:
    """GitHub as the script sees it: issues and the schedule's decisive
    results, changed only through the effects the script applies."""

    def __init__(self):
        self.issues = []
        self.results = []  # (number, attempt, result) the Actions API reports as decisive
        self.fail_close = False

    def run(self, number, attempt, result, recorded=True):
        history = [r for r in self.results if r[0] != number or r[1] < attempt]
        effects = report.decide(
            WORKFLOW,
            {"number": number, "attempt": attempt, "result": result, "url": f"https://example.invalid/{number}"},
            history,
            self.issues,
        )
        applied = []
        for effect in effects:
            if effect[0] == "close" and self.fail_close:
                break
            self.apply(effect)
            applied.append(effect)
        if recorded and result in ("success", "failure"):
            self.results.append((number, attempt, result))
        return effects

    def apply(self, effect):
        kind = effect[0]
        if kind == "create":
            number = 100 + len(self.issues)
            self.issues.append({"number": number, "open": True, "author": report.BOT,
                                "title": effect[1], "body": effect[2], "comments": []})
        else:
            issue = next(issue for issue in self.issues if issue["number"] == effect[1])
            if kind == "comment":
                issue["comments"].append({"author": report.BOT, "body": effect[2]})
            elif kind == "close":
                issue["open"] = False

    def open_owned(self):
        return [issue for issue in self.issues if issue["open"] and issue["author"] == report.BOT]


def kinds(effects):
    return [effect[0] for effect in effects]


class ScheduledReport(unittest.TestCase):
    def test_one_issue_per_failing_episode(self):
        world = World()
        self.assertEqual(world.run(40, 1, "success"), [])
        self.assertEqual(kinds(world.run(41, 1, "failure")), ["create"])
        issue = world.open_owned()[0]
        self.assertIn(report.marker(WORKFLOW), issue["body"])
        self.assertIn("https://example.invalid/41", issue["body"])
        self.assertEqual(kinds(world.run(42, 1, "failure")), ["comment"])
        self.assertEqual(len(world.issues), 1)
        self.assertEqual(kinds(world.run(43, 1, "success")), ["comment", "close"])
        self.assertEqual(world.open_owned(), [])
        self.assertEqual(kinds(world.run(44, 1, "failure")), ["create"])
        self.assertEqual(len(world.issues), 2)

    def test_a_repeated_attempt_has_one_effect(self):
        world = World()
        world.run(40, 1, "failure")
        self.assertEqual(world.run(40, 1, "failure"), [])
        self.assertEqual(len(world.issues), 1)

    def test_a_retry_after_half_a_recovery_makes_only_the_close(self):
        world = World()
        world.run(40, 1, "failure")
        world.fail_close = True
        self.assertEqual(kinds(world.run(41, 1, "success", recorded=False)), ["comment", "close"])
        self.assertEqual(len(world.open_owned()), 1, "the close failed")
        world.fail_close = False
        self.assertEqual(kinds(world.run(41, 1, "success")), ["close"])
        self.assertEqual(world.open_owned(), [])

    def test_ordering_holds_both_ways(self):
        # 40 fails, 41 passes and closes; a late failure of 40 leaves it closed.
        world = World()
        world.run(40, 1, "failure")
        world.run(41, 1, "success")
        self.assertEqual(world.run(40, 2, "failure"), [])
        self.assertEqual(world.open_owned(), [])
        # A later attempt of the same run settles its own failure.
        world = World()
        world.run(40, 1, "failure")
        self.assertEqual(kinds(world.run(40, 2, "success")), ["comment", "close"])
        # An older attempt's success does not close a newer attempt's failure.
        world = World()
        world.run(40, 2, "failure")
        self.assertEqual(world.run(40, 1, "success"), [])
        self.assertEqual(len(world.open_owned()), 1)

    def test_only_a_decisive_newer_result_suppresses(self):
        # A cancelled 41 decides nothing: 40's failure still opens.
        world = World()
        world.run(41, 1, "cancelled")
        self.assertEqual(kinds(world.run(40, 1, "failure")), ["create"])
        # 41's failure is on the issue though the Actions listing lacks it
        # (41 is being rerun): an older success leaves it open.
        world = World()
        world.run(41, 1, "failure", recorded=False)
        self.assertEqual(world.run(40, 2, "success"), [])
        self.assertEqual(len(world.open_owned()), 1)
        # A newer success that found nothing to close still decides: a
        # delayed failure of an older run opens nothing.
        world = World()
        world.run(41, 1, "success")
        self.assertEqual(world.run(40, 1, "failure"), [])
        self.assertEqual(world.issues, [])

    def test_only_the_bots_own_receipts_count(self):
        world = World()
        world.run(40, 1, "failure")
        issue = world.open_owned()[0]
        # Somebody quotes a marker for a newer run in their own comment.
        issue["comments"].append({"author": "somebody", "body": report.run_marker(99, 1, "success")})
        self.assertEqual(kinds(world.run(41, 1, "failure")), ["comment"])
        self.assertEqual(kinds(world.run(42, 1, "success")), ["comment", "close"])

    def test_a_persons_issue_of_the_same_title_is_never_touched(self):
        world = World()
        person = {"number": 7, "open": True, "author": "somebody", "title": "Scheduled fuzz.yml fails on main",
                  "body": report.marker(WORKFLOW), "comments": []}
        world.issues.append(person)
        self.assertEqual(kinds(world.run(40, 1, "failure")), ["create"])
        world.run(41, 1, "success")
        self.assertTrue(person["open"])
        self.assertEqual(person["comments"], [])

    def test_a_cancelled_or_skipped_run_decides_nothing(self):
        for result in ("cancelled", "skipped"):
            self.assertEqual(report.decide(WORKFLOW, {"number": 1, "attempt": 1, "result": result, "url": "u"},
                                           [], []), [])

    def test_main_reads_the_run_and_applies_through_its_client(self):
        class Stub:
            def __init__(self):
                self.applied = []

            def history(self, workflow, number, attempt):
                return []

            def issues(self, workflow):
                return []

            def apply(self, effect):
                self.applied.append(effect)

        stub = Stub()
        environ = {"GITHUB_REPOSITORY": "owner/repo", "GITHUB_RUN_NUMBER": "12", "GITHUB_RUN_ATTEMPT": "2",
                   "GITHUB_RUN_ID": "999", "GITHUB_SERVER_URL": "https://github.example"}
        with mock.patch.object(sys, "argv", ["scheduled_report.py", "--workflow", WORKFLOW, "--result", "failure"]), \
                mock.patch("sys.stdout", io.StringIO()):
            self.assertEqual(report.main(client=stub, environ=environ), 0)
        self.assertEqual(stub.applied[0][0], "create")
        self.assertIn("https://github.example/owner/repo/actions/runs/999", stub.applied[0][2])
        self.assertIn(report.run_marker(12, 2, "failure"), stub.applied[0][2])

    def test_the_client_reads_every_open_issue_and_walks_back_a_running_attempt(self):
        owned_body = report.marker(WORKFLOW) + "\nopen"
        calls = []

        def fake(argv, **_):
            calls.append(argv)
            path = next(arg for arg in argv[2:] if arg.startswith("repos/"))
            out = ""
            if "/issues?" in path and "state=open" in path:
                # Two pages, joined by --paginate: the owned one is on the second.
                out = json.dumps({"number": 1, "state": "open", "title": "x", "body": "other", "author": "somebody"}) + "\n"
                out += json.dumps({"number": 205, "state": "open", "title": "t", "body": owned_body, "author": report.BOT}) + "\n"
            elif path.endswith("/comments"):
                out = json.dumps({"author": "somebody", "body": "hi"}) + "\n"
            elif "/workflows/" in path:
                out = json.dumps({"id": 7, "run_number": 41, "run_attempt": 2, "status": "in_progress", "conclusion": None}) + "\n"
            elif "/attempts/1" in path:
                out = json.dumps({"status": "completed", "conclusion": "failure"}) + "\n"
            return subprocess.CompletedProcess(argv, 0, out, "")

        client = report.Gh("owner/repo")
        with mock.patch.object(report.subprocess, "run", side_effect=fake):
            issues = client.issues(WORKFLOW)
            history = client.history(WORKFLOW, 42, 1)
        self.assertEqual([issue["number"] for issue in issues], [205])
        self.assertEqual(issues[0]["comments"], [{"author": "somebody", "body": "hi"}])
        open_call = next(call for call in calls if any("state=open" in arg for arg in call))
        self.assertIn("--paginate", open_call)
        self.assertEqual(history, [(41, 1, "failure")])

    def test_the_report_job_is_scheduled_only_and_cannot_decide_the_fuzz_result(self):
        text = (REPO / ".github/workflows/fuzz.yml").read_text()
        job = text[text.index("\n  report:"):]
        self.assertIn("needs: fuzz", job)
        self.assertIn("if: always() && github.event_name == 'schedule'", job)
        self.assertIn("continue-on-error: true", job)
        self.assertIn("FUZZ_RESULT: ${{ needs.fuzz.result }}", job)
        self.assertIn("cancel-in-progress: false", job)
        self.assertIn("issues: write", job)
        # Only this job may write issues; the workflow and the fuzz job read.
        before = text[: text.index("\n  report:")]
        self.assertNotIn("issues: write", before)
        self.assertRegex(before, re.compile(r"^permissions:\n  contents: read\n", re.M))


if __name__ == "__main__":
    unittest.main()
