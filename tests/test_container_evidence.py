import importlib.util
import json
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "container_evidence", Path(__file__).parent / "containers/evidence.py")
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)


class ContainerFailureEvidence(unittest.TestCase):
    def test_missing_record_does_not_mask_the_launch_failure(self):
        response = {"type": "error", "code": "engine_unavailable", "message": "primary failure"}
        def missing():
            raise AssertionError("agent not found")
        with self.assertRaises(AssertionError) as raised:
            EVIDENCE.reject_launch(response, missing, lambda _: self.fail("unexpected record"))
        evidence = json.loads(str(raised.exception))
        self.assertEqual(evidence["launch_response"], response)
        self.assertEqual(evidence["cleanup_lookup_error"], "agent not found")

    def test_partial_container_is_retained_for_owned_cleanup(self):
        record = {"container": {"id": "fixture", "owner": "fixture-owner"}}
        remembered = []
        with self.assertRaises(AssertionError) as raised:
            EVIDENCE.reject_launch({"type": "error", "message": "start failed"}, lambda: record, remembered.append)
        self.assertEqual(remembered, [record])
        self.assertEqual(json.loads(str(raised.exception))["launch_response"]["message"], "start failed")
