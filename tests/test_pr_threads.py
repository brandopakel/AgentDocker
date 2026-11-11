"""A merge waits only on review threads that are unresolved and still on a
line the head has; outdated ones no longer block."""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch
from contextlib import redirect_stderr, redirect_stdout

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("pr_threads", ROOT / "scripts/pr_threads.py")
THREADS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(THREADS)


def thread(resolved, outdated, path="crates/core/src/lib.rs", line=10, author="reviewer-1", body="Check this\nmore"):
    return {
        "isResolved": resolved,
        "isOutdated": outdated,
        "path": path,
        "line": line,
        "comments": {"nodes": [{"author": {"login": author}, "body": body}]},
    }


class PullRequestThreads(unittest.TestCase):
    def test_only_unresolved_current_threads_are_open(self):
        threads = [
            thread(resolved=False, outdated=False, line=1),
            thread(resolved=True, outdated=False, line=2),
            thread(resolved=False, outdated=True, line=3),
            thread(resolved=True, outdated=True, line=4),
        ]
        self.assertEqual([t["line"] for t in THREADS.open_threads(threads)], [1])

    def test_outdated_threads_count_when_asked(self):
        threads = [thread(resolved=False, outdated=True, line=3), thread(resolved=True, outdated=True, line=4)]
        self.assertEqual([t["line"] for t in THREADS.open_threads(threads, include_outdated=True)], [3])

    def test_a_thread_is_described_by_place_author_and_first_line(self):
        self.assertEqual(
            THREADS.describe(thread(False, False)),
            "crates/core/src/lib.rs:10  reviewer-1: Check this",
        )

    def test_a_bot_marker_is_skipped_for_the_first_real_line(self):
        marked = thread(False, False, body="<!-- bot-marker:v1 -->\n\n_Nit_: say it shorter\nmore")
        self.assertEqual(THREADS.describe(marked), "crates/core/src/lib.rs:10  reviewer-1: _Nit_: say it shorter")

    def test_a_thread_without_a_line_or_comment_still_describes(self):
        bare = {"isResolved": False, "isOutdated": False, "path": "docs/GUIDE.md", "line": None, "comments": {"nodes": []}}
        self.assertEqual(THREADS.describe(bare), "docs/GUIDE.md  unknown: ")

    def test_the_first_prose_line_skips_rules_and_may_be_absent(self):
        self.assertEqual(THREADS.first_prose_line("---\n\n***\nSay it\n"), "Say it")
        self.assertEqual(THREADS.first_prose_line("<!-- only -->\n---\n"), "")


def page(nodes, next_cursor=None):
    return json.dumps({"data": {"repository": {"pullRequest": {"reviewThreads": {
        "pageInfo": {"hasNextPage": next_cursor is not None, "endCursor": next_cursor},
        "nodes": nodes,
    }}}}})


class PullRequestThreadsOverTheCli(unittest.TestCase):
    """Pages are joined, and the exit status tells open threads from a read
    that could not be made."""

    def test_every_page_is_fetched_with_its_cursor(self):
        calls = []

        def fake_run(command, **kwargs):
            calls.append(command)
            if "after=c1" in command:
                return subprocess.CompletedProcess(command, 0, stdout=page([thread(True, False, line=2)]), stderr="")
            return subprocess.CompletedProcess(command, 0, stdout=page([thread(False, False, line=1)], "c1"), stderr="")

        with patch.object(THREADS.subprocess, "run", fake_run):
            threads = THREADS.fetch_threads("owner", "name", 7)
        self.assertEqual([t["line"] for t in threads], [1, 2])
        self.assertEqual(len(calls), 2)
        self.assertNotIn("after=c1", calls[0])
        self.assertIn("after=c1", calls[1])

    def run_main(self, argv, run):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(THREADS.sys, "argv", ["pr_threads.py", *argv]), \
                patch.object(THREADS.subprocess, "run", run), \
                redirect_stdout(out), redirect_stderr(err):
            status = THREADS.main()
        return status, out.getvalue(), err.getvalue()

    def test_open_threads_exit_one_and_none_exit_zero(self):
        def with_open(command, **kwargs):
            return subprocess.CompletedProcess(command, 0, stdout=page([thread(False, False)]), stderr="")

        def all_done(command, **kwargs):
            return subprocess.CompletedProcess(command, 0, stdout=page([thread(True, False)]), stderr="")

        status, out, err = self.run_main(["7", "--repo", "owner/name"], with_open)
        self.assertEqual((status, out.strip()), (1, "crates/core/src/lib.rs:10  reviewer-1: Check this"))
        self.assertIn("1 open review thread", err)
        status, out, err = self.run_main(["7", "--repo", "owner/name"], all_done)
        self.assertEqual((status, out), (0, ""))
        self.assertIn("no open review threads", err)

    def test_a_read_that_cannot_be_made_exits_two(self):
        def missing_cli(command, **kwargs):
            raise FileNotFoundError("gh")

        def refused(command, **kwargs):
            raise subprocess.CalledProcessError(1, command, stderr="not signed in")

        for run in (missing_cli, refused):
            with self.subTest(run=run.__name__):
                status, out, err = self.run_main(["7", "--repo", "owner/name"], run)
                self.assertEqual((status, out), (2, ""))
                self.assertIn("could not read the review threads", err)


if __name__ == "__main__":
    unittest.main()
