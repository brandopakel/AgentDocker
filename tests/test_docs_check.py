"""The documents carry no specifics: the patterns find them and leave the contract alone."""
import importlib.util
import os
from pathlib import Path
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
        self.assertEqual(kinds("Fixed in #412, see PR 336 and /pull/241."),
                         ["a pull request or issue number"] * 3)
        self.assertEqual(kinds("Source `10b57894` passed."), ["a commit"])
        self.assertEqual(kinds("See /actions/runs/39558567833 for the log."), ["a CI run"])
        self.assertEqual(kinds("Logs live in /Users/someone/Library/Logs."), ["a home directory"])
        self.assertEqual(kinds(r"Copied to C:\Users\someone\AppData."), ["a home directory"])
        self.assertEqual(kinds("Sent to claude-code-9529 and codex-51242."), ["a real agent's name"] * 2)

    def test_leaves_the_contract_alone(self):
        for line in (
            "Channels need MCP protocol `2025-06-18` or later.",
            "The installer writes `/Users/you/.agentdocker` and `/home/$USER/.local/bin`.",
            "On CI the home is /Users/runner; on a laptop it is /Users/me.",
            "A lease lasts 3600 seconds and a card key looks like AD-12.",
            "Version 0.2.0-beta.14 and the `#section` anchor are fine.",
            "Agents are named claude-code-<pid>; a hex colour such as #1e1e1e is not a number.",
            "The word facade and the hash deadbeef carry no digit.",
        ):
            with self.subTest(line=line):
                self.assertEqual(kinds(line), [])

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


if __name__ == "__main__":
    unittest.main()
