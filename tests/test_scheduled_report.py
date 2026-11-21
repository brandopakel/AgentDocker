"""A failed scheduled run keeps one issue open, owned by the automation,
until a later scheduled run passes; a repeat, a stale rerun or a person's
issue changes nothing."""
from pathlib import Path
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
    """GitHub as the script sees it: issues and completed scheduled runs,
    changed only through the effects the script applies."""

    def __init__(self):
        self.issues = []
        self.runs = []

    def run(self, number, attempt, result):
        effects = report.decide(
            WORKFLOW,
            {"number": number, "attempt": attempt, "result": result, "url": f"https://example.invalid/{number}"},
            [run for run in self.runs if run[0] != number],
            self.issues,
        )
        for effect in effects:
            self.apply(effect)
        # The run completes after its report.
        self.runs = [run for run in self.runs if run[0] != number] + [(number, attempt, result)]
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
                issue["comments"].append(effect[2])
            elif kind == "close":
                issue["open"] = False

    def open_owned(self):
        return [issue for issue in self.issues if issue["open"] and issue["author"] == report.BOT]


class ScheduledReport(unittest.TestCase):
    def test_one_issue_per_failing_episode(self):
        world = World()
        self.assertEqual(world.run(40, 1, "success"), [])
        created = world.run(41, 1, "failure")
        self.assertEqual([effect[0] for effect in created], ["create"])
        issue = world.open_owned()[0]
        self.assertIn(report.marker(WORKFLOW), issue["body"])
        self.assertIn("https://example.invalid/41", issue["body"])
        # A later failure is added to the same issue.
        self.assertEqual([effect[0] for effect in world.run(42, 1, "failure")], ["comment"])
        self.assertEqual(len(world.issues), 1)
        # Recovery closes it.
        self.assertEqual([effect[0] for effect in world.run(43, 1, "success")], ["comment", "close"])
        self.assertEqual(world.open_owned(), [])
        # A recurrence opens a new one.
        self.assertEqual([effect[0] for effect in world.run(44, 1, "failure")], ["create"])
        self.assertEqual(len(world.issues), 2)

    def test_a_repeated_attempt_has_one_effect(self):
        world = World()
        world.run(40, 1, "failure")
        # The same run and attempt reported again (the report job retried).
        self.assertEqual(report.decide(WORKFLOW, {"number": 40, "attempt": 1, "result": "failure", "url": "u"},
                                       [], world.issues), [])
        self.assertEqual(report.decide(WORKFLOW, {"number": 40, "attempt": 1, "result": "success", "url": "u"},
                                       [], world.issues), [])

    def test_ordering_holds_both_ways(self):
        # 40 fails, 41 passes and closes, then a late failure of 40 leaves
        # it closed and opens nothing.
        world = World()
        world.run(40, 1, "failure")
        world.run(41, 1, "success")
        self.assertEqual(world.open_owned(), [])
        self.assertEqual(world.run(40, 2, "failure"), [])
        self.assertEqual(world.open_owned(), [])
        # A stale success leaves a newer failure open.
        world = World()
        world.run(40, 1, "success")
        world.run(41, 1, "failure")
        self.assertEqual(world.run(40, 2, "success"), [])
        self.assertEqual(len(world.open_owned()), 1)
        # A later attempt of the same run may settle its own failure.
        world = World()
        world.run(40, 1, "failure")
        self.assertEqual([effect[0] for effect in world.run(40, 2, "success")], ["comment", "close"])

    def test_a_persons_issue_of_the_same_title_is_never_touched(self):
        world = World()
        person = {"number": 7, "open": True, "author": "somebody", "title": "Scheduled fuzz.yml fails on main",
                  "body": report.marker(WORKFLOW), "comments": []}
        world.issues.append(person)
        self.assertEqual([effect[0] for effect in world.run(40, 1, "failure")], ["create"])
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

            def completed(self, workflow, current):
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
