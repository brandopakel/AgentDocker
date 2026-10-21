"""The documents carry no specifics: the patterns find them and leave the contract alone."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("docs_check", ROOT / "scripts/docs_check.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def kinds(line, denied=()):
    return [what for what, _ in CHECK.specifics(line, denied)]


class Specifics(unittest.TestCase):
    def test_finds_moments_numbers_and_people(self):
        self.assertEqual(kinds("Trialled on 2026-09-17 and again on September 18."), ["a date", "a date"])
        for line in ("Trialled on 21 October 2026.", "Trialled on the 3rd of May.", "Since October 2026.",
                     "Seen Sept. 17 and Oct 21st."):
            with self.subTest(line=line):
                self.assertTrue(kinds(line) and set(kinds(line)) == {"a date"}, kinds(line))
        self.assertEqual(kinds("Fixed in #412, see PR 336 and /pull/241."),
                         ["a pull request or issue number"] * 3)
        self.assertEqual(kinds("See #7 and #123456."), ["a pull request or issue number"] * 2)
        self.assertEqual(kinds("Use #000000 for black and #11223344 for translucent colour."), [])
        self.assertEqual(kinds("The colour is fixed, see #123456 and #12345."),
                         ["a pull request or issue number"] * 2)
        self.assertEqual(kinds("The background scan was fixed in #403."), ["a pull request or issue number"])
        for line in ("See #403 and #404 for background changes.", "Fixed background scan in #403, #404 or #405."):
            with self.subTest(line=line):
                self.assertEqual(len(kinds(line)), line.count("#"))
        self.assertEqual(kinds("Use #000000 for black; the fix is in #123456."), ["a pull request or issue number"])
        self.assertEqual(set(kinds(r"An escaped \` is printed, then 21 October 2026, then another \`.")), {"a date"})
        self.assertEqual(kinds("Source `10b57894` passed."), ["a commit"])
        self.assertEqual(kinds("See /actions/runs/39558567833 for the log."), ["a CI run"])
        self.assertEqual(kinds("Logs live in /Users/someone/Library/Logs."), ["a home directory"])
        self.assertEqual(kinds(r"Copied to C:\Users\someone\AppData."), ["a home directory"])
        self.assertEqual(kinds("Kept in /Users/you-team/.agentdocker and /home/me.dev/.config."),
                         ["a home directory"] * 2)
        self.assertEqual(kinds("Sent to claude-code-9529 and codex-51242."), ["a real agent's name"] * 2)

    def test_leaves_the_contract_alone(self):
        for line in (
            "Channels need MCP protocol `2025-06-18` or later.",
            "A revision is named ``2025-06-18`` in a double-backtick span.",
            "The installer writes `/Users/you/.agentdocker` and `/home/$USER/.local/bin`.",
            "On CI the home is /Users/runner; on a laptop it is /Users/me.",
            "A lease lasts 3600 seconds and a card key looks like AD-12.",
            "Version 0.2.0-beta.14 and the `#section` anchor are fine.",
            "Agents are named claude-code-<pid>; colours such as #1e1e1e and #112233ff are not numbers.",
            "The word facade and the hash deadbeef carry no digit.",
            "Agents may 3 times retry, and a march of 10 steps is not a date.",
        ):
            with self.subTest(line=line):
                self.assertEqual(kinds(line), [])

    def test_code_spans_pair_backtick_runs(self):
        line = "a ``x ` y`` b `z`"
        spans = CHECK.code_spans(line)
        self.assertEqual([line[s:e] for s, e in spans], ["``x ` y``", "`z`"])
        self.assertEqual(CHECK.code_spans("an ` unclosed tick"), [])
        line = r"escaped \` then `real \` span`"
        self.assertEqual([line[s:e] for s, e in CHECK.code_spans(line)], [r"`real \`"])

    def test_private_names_come_from_the_deny_list(self):
        self.assertEqual(kinds("Deployed on the keel host.", ("keel",)), ["a private name"])
        self.assertEqual(kinds("A keelson is a beam.", ("keel",)), [])
        with tempfile.TemporaryDirectory() as temporary:
            listing = Path(temporary) / "denied"
            listing.write_text("# private names\nkeel\n\nminio-lab\n")
            with patch.dict(os.environ, {"AGENTDOCKER_SPECIFICS_DENYLIST": str(listing)}):
                self.assertEqual(CHECK.denied_terms(), ("keel", "minio-lab"))
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(CHECK.denied_terms(), ())


class AddedLines(unittest.TestCase):
    """What the gate reads is what the working tree adds since the base,
    whatever the documents are called and however their lines begin."""

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "Test")
        (self.root / "docs").mkdir()
        (self.root / "docs/GUIDE.md").write_text("# Guide\n")
        self.git("add", ".")
        self.git("commit", "-qm", "base")
        self.base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

    def tearDown(self):
        self.temporary.cleanup()

    def added(self):
        with patch.object(CHECK, "ROOT", self.root):
            return CHECK.added_document_lines(self.base)

    def test_names_that_git_would_quote_or_split(self):
        (self.root / "docs/café.md").write_text("Trialled on 2026-10-21.\n")
        (self.root / "docs/My Notes.md").write_text("Seen on 2026-10-22.\n")
        self.git("add", "docs/café.md")
        self.git("commit", "-qm", "quoted")
        self.assertEqual(sorted(self.added()), [
            ("docs/My Notes.md", 1, "Seen on 2026-10-22."),
            ("docs/café.md", 1, "Trialled on 2026-10-21."),
        ])

    def test_an_added_line_that_looks_like_a_header(self):
        (self.root / "docs/GUIDE.md").write_text("# Guide\n++ literal\nTrialled on 2026-10-21.\n")
        self.git("commit", "-qam", "plus")
        self.assertEqual(self.added(), [
            ("docs/GUIDE.md", 2, "++ literal"),
            ("docs/GUIDE.md", 3, "Trialled on 2026-10-21."),
        ])

    def test_lines_split_where_git_splits_them(self):
        (self.root / "docs/GUIDE.md").write_text("# Guide\nSeen here\u2028on 21 October 2026.\r\n", newline="")
        self.git("commit", "-qam", "separator")
        self.assertEqual(self.added(), [("docs/GUIDE.md", 2, "Seen here\u2028on 21 October 2026.\r")])

    def test_a_rename_adds_nothing_it_carried_over(self):
        self.git("mv", "docs/GUIDE.md", "docs/renamed.md")
        self.git("commit", "-qm", "rename")
        self.assertEqual(self.added(), [])
        (self.root / "docs/renamed.md").write_text("# Guide\nTrialled on 2026-10-21.\n")
        # A small file changed this much is no longer a rename to git; either
        # way the new line is read.
        self.assertIn(("docs/renamed.md", 2, "Trialled on 2026-10-21."), self.added())

    def test_an_uncommitted_fix_is_what_counts(self):
        (self.root / "docs/GUIDE.md").write_text("# Guide\nTrialled on 2026-10-21.\n")
        self.git("commit", "-qam", "dated")
        (self.root / "docs/GUIDE.md").write_text("# Guide\nTrialled before a release.\n")
        self.assertEqual(self.added(), [("docs/GUIDE.md", 2, "Trialled before a release.")])


if __name__ == "__main__":
    unittest.main()
