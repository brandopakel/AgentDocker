"""A merge waits only on review threads that are unresolved and still on a
line the head has; outdated ones no longer block."""
import importlib.util
from pathlib import Path
import unittest

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


if __name__ == "__main__":
    unittest.main()
