"""Native reopen checks must use the canonical binding, not interleaved TUI text."""
import copy
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from windows_native_launcher_smoke import canonical_reopen, process_snapshot


class ProcessError(Exception):
    pass


class MissingProcess(ProcessError):
    pass


class DeniedProcess(ProcessError):
    pass


class ObservedProcess:
    def __init__(self, pid, live, metadata_error=None):
        self.pid = pid
        self.live = iter(live)
        self.metadata_error = metadata_error

    def create_time(self):
        return 123.0

    def is_running(self):
        return next(self.live)

    def ppid(self):
        if self.metadata_error:
            raise self.metadata_error()
        return 10

    def exe(self):
        return 'private-fixture.exe'

    def status(self):
        return 'running'


class ProcessSnapshots(unittest.TestCase):
    api = SimpleNamespace(NoSuchProcess=MissingProcess, Error=ProcessError)

    def test_a_dead_helper_does_not_hide_later_participant_states(self):
        rows = process_snapshot([ObservedProcess(1, [False]), ObservedProcess(2, [True, True])], self.api)
        self.assertEqual([row['running'] for row in rows], [False, True])
        self.assertEqual(rows[1]['executable'], 'private-fixture.exe')
        self.assertEqual([row['birth'] for row in rows], [123.0, 123.0])

    def test_exit_or_reuse_during_metadata_read_is_not_reported_live(self):
        rows = process_snapshot([ObservedProcess(1, [True, False]),
                                 ObservedProcess(2, [True], MissingProcess)], self.api)
        self.assertTrue(all(row['running'] is False for row in rows))
        self.assertTrue(all(row['ended_during_observation'] for row in rows))

    def test_denied_metadata_remains_unknown_and_keeps_the_other_snapshot(self):
        rows = process_snapshot([ObservedProcess(1, [True], DeniedProcess),
                                 ObservedProcess(2, [True, True])], self.api)
        self.assertIsNone(rows[0]['running'])
        self.assertEqual(rows[0]['observation_error'], 'DeniedProcess')
        self.assertTrue(rows[1]['running'])


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
