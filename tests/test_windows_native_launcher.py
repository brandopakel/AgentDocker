"""Native reopen checks must use the canonical binding, not interleaved TUI text."""
import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from windows_native_launcher_smoke import canonical_reopen


class NativeReopen(unittest.TestCase):
    def test_reopen_requires_exact_generation_identity_and_unchanged_receipts(self):
        provider = {"session": "thread", "profile": "private", "process": {"pid": 42, "started_at": "birth"}}
        descriptor = {"version": 1, "provider": provider}
        generation = {"provider": copy.deepcopy(provider)}
        receipts = [{"message": "first"}, {"message": "second"}]
        ledger = {"binding": {"agent": "canonical", "provider": copy.deepcopy(provider)},
                  "attempt": None, "completed": copy.deepcopy(receipts)}
        self.assertTrue(canonical_reopen(descriptor, generation, ledger, "canonical", "thread", receipts))
        mutations = [
            lambda d, g, l: d.update(version=2),
            lambda d, g, l: d.update(birth={}),
            lambda d, g, l: d["provider"].update(session="other"),
            lambda d, g, l: g["provider"]["process"].update(started_at="stale"),
            lambda d, g, l: l["binding"].update(agent="helper"),
            lambda d, g, l: l["binding"]["provider"].update(profile="other"),
            lambda d, g, l: l.update(attempt={"message": "unexpected"}),
            lambda d, g, l: l["completed"].append({"message": "replayed"}),
            lambda d, g, l: l["completed"].reverse(),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                d, g, l = copy.deepcopy((descriptor, generation, ledger))
                mutate(d, g, l)
                self.assertFalse(canonical_reopen(d, g, l, "canonical", "thread", receipts))


if __name__ == "__main__":
    unittest.main()
