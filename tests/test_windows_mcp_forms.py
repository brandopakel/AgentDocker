"""Report validation must reject altered Windows form consent and delivery evidence."""
import copy
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("windows_package_smoke", ROOT / "scripts/windows_package_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class WindowsMcpForms(unittest.TestCase):
    def fixture(self):
        info = {"source_commit": "a" * 40, "source_tree": "b" * 40,
                "binary_sha256": {"agentdocker.exe": "c" * 64, "agentd.exe": "d" * 64}}
        report = dict(info, result="passed", source_dirty=False, scratch_removed=True,
                      config_unchanged=True, cleanup_errors=[], remaining_processes=[],
                      watched_processes=[{"pid": p, "birth": float(p)} for p in range(10, 14)],
                      agent="agent", initial_pid=11, cases=[],
                      initial_agent={"id": "agent", "pid": 11, "process_started_at": "birth"},
                      final_agent={"id": "agent", "pid": 11, "process_started_at": "birth"},
                      final_ledger={"thread": "thread", "attempt": None, "completed": []})
        for n, decision in enumerate(["Submit", "Decline", "Cancel", "cancel-route"]):
            first, peer = f"input-{n}", f"peer-{n}"
            receipts = {i: {"thread": "thread", "turn": f"turn-{n}", "item": i} for i in [first, peer]}
            report["final_ledger"]["completed"].extend({"message": i, "receipt": r} for i, r in receipts.items())
            action = "accept" if decision == "Submit" else "decline" if decision == "Decline" else "cancel"
            content = {"name": "Ada"} if action == "accept" else None
            question = {"id": f"question-{n}", "presentation": {"kind": "mcp_form", "schema": {
                "type": "object", "properties": {"name": {"type": "string"}}}}}
            response = {"action": action, "content": content}
            report["cases"].append({"decision": decision, "result": "passed", "input": first,
                "peer_input": peer, "receipts": receipts, "submitted_content": content,
                "provider_request": n, "question": question,
                "held_ledger": {"attempt": {"message": first, "acknowledged": True},
                                "reviews": [{"id": n, "response": None}], "completed": []},
                "invalid_answers": [{"code": "invalid"}, {"code": "invalid"}],
                "closed_review": {"outcome": "resolved", "acknowledged": True, "request": {
                    "id": n, "thread": "thread", "turn": f"turn-{n}",
                    "questions": [{"message": question["id"], "presentation": copy.deepcopy(question["presentation"])}],
                    "response": {"id": n, "result": response}}},
                "mcp_reply": {"id": f"mcp-{n}", "result": copy.deepcopy(response)}})
        return report, info

    def test_four_decisions_accept_only_complete_correlated_report(self):
        report, info = self.fixture()
        SMOKE.validate_mcp_form_report(report, info)

    def test_changed_identity_consent_receipts_or_cleanup_refuse_acceptance(self):
        mutations = [
            lambda r: r.update(source_commit="other"),
            lambda r: r.update(source_tree="other"),
            lambda r: r.update(source_dirty=True),
            lambda r: r["binary_sha256"].update({"agentdocker.exe": "other"}),
            lambda r: r.update(scratch_removed=False),
            lambda r: r.update(config_unchanged=False),
            lambda r: r.update(cleanup_errors=["forced cleanup"]),
            lambda r: r.update(remaining_processes=[{"pid": 12, "birth": 12}]),
            lambda r: r.update(watched_processes=[]),
            lambda r: r["watched_processes"].append(r["watched_processes"][0]),
            lambda r: r["final_agent"].update(process_started_at="replaced"),
            lambda r: r["cases"].pop(),
            lambda r: r["cases"][0]["invalid_answers"].pop(),
            lambda r: r["cases"][0]["held_ledger"].update(steering={"message": "peer-0"}),
            lambda r: r["cases"][0]["held_ledger"]["completed"].append({"message": "peer-0"}),
            lambda r: r["cases"][0]["held_ledger"]["reviews"][0].update(response={"sent": True}),
            lambda r: r["cases"][0]["closed_review"].update(acknowledged=False),
            lambda r: r["cases"][0]["closed_review"]["request"].update(turn="other"),
            lambda r: r["cases"][0]["closed_review"]["request"]["questions"][0].update(message="other"),
            lambda r: r["cases"][0]["closed_review"]["request"]["questions"][0]["presentation"].update(schema={}),
            lambda r: r["cases"][0]["mcp_reply"]["result"].update(content={"name": "changed"}),
            lambda r: r["cases"][1].update(submitted_content={"secret": "should not be sent"}),
            lambda r: r["cases"][1]["mcp_reply"].update(id="mcp-0"),
            lambda r: r["final_ledger"]["completed"].pop(),
            lambda r: r["cases"][0]["receipts"].pop("peer-0"),
        ]
        for number, mutate in enumerate(mutations):
            with self.subTest(case=number):
                report, info = self.fixture()
                # Keep expected provenance independent of the altered report.
                report = copy.deepcopy(report)
                mutate(report)
                with self.assertRaises(ValueError):
                    SMOKE.validate_mcp_form_report(report, info)


if __name__ == "__main__":
    unittest.main()
