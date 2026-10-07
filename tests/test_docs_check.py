#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("docs_check", ROOT / "scripts/docs_check.py")
DOCS_CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DOCS_CHECK)


class VerificationRows(unittest.TestCase):
    def test_only_table_rows_over_the_limit_are_long(self):
        short = "| 2026-10-07 | Trial | `abc1234` | PASS: it held. | [PR #1](https://example.com) |"
        long = "| 2026-10-07 | Trial | `abc1234` | " + "word " * 120 + "| link |"
        header = "| Date | Trial | Source | Result | Record |"
        prose = "A paragraph " * 60
        rows = DOCS_CHECK.long_rows([short, long, header, prose, "| --- | --- |"])
        self.assertEqual(rows, [long])
        self.assertEqual(DOCS_CHECK.long_rows([long], limit=len(long)), [])


if __name__ == "__main__":
    unittest.main()
