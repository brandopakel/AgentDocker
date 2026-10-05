"""Report validation must reject altered Windows form consent and delivery evidence."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("windows_package_smoke", ROOT / "scripts/windows_package_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class WindowsMcpForms(unittest.TestCase):
    def fixture(self):
        info = {"source_commit": "a" * 40, "source_tree": "b" * 40,
                "binary_sha256": {"agentdocker.exe": "c" * 64, "agentd.exe": "d" * 64}}
        report = dict(info, result="passed", review_mode="form", source_dirty=False, scratch_removed=True,
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
            question = {"id": f"question-{n}", "presentation": {"kind": "mcp_form", "server": "review_fixture", "schema": {
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
                "mcp_reply": {"id": f"mcp-{n}", "result": copy.deepcopy(response) if action == "accept" else {"action": action}}})
        return report, info

    def test_four_decisions_accept_only_complete_correlated_report(self):
        report, info = self.fixture()
        SMOKE.validate_mcp_review_report(report, info)

    def url_fixture(self):
        report, info = self.fixture()
        report["review_mode"] = "url"
        for number, case in enumerate(report["cases"], 1):
            if case["decision"] == "Submit":
                case["decision"] = "Accept"
            action = "accept" if case["decision"] == "Accept" else "decline" if case["decision"] == "Decline" else "cancel"
            presentation = {"kind": "mcp_url", "server": "review_fixture",
                            "url": "https://example.com/agentdocker-fixture?case=" + str(number),
                            "elicitation_id": "fixture-elicitation-" + str(number)}
            case["question"]["presentation"] = presentation
            request = case["closed_review"]["request"]
            request["questions"][0]["presentation"] = copy.deepcopy(presentation)
            case["submitted_content"] = None
            request["response"]["result"] = {"action": action, "content": None}
            case["mcp_reply"]["result"] = {"action": action}
        return report, info

    def test_url_decisions_require_original_destination_and_no_form_content(self):
        report, info = self.url_fixture()
        SMOKE.validate_mcp_review_report(report, info, "url")

        def replace_presentation(report, **fields):
            case = report["cases"][0]
            case["question"]["presentation"].update(fields)
            case["closed_review"]["request"]["questions"][0]["presentation"].update(fields)

        mutations = [
            lambda r: r.update(review_mode="form"),
            lambda r: r["cases"][0].update(decision="Submit"),
            lambda r: r["cases"][0].update(submitted_content={"name": "must not be shared"}),
            lambda r: r["cases"][0]["mcp_reply"]["result"].update(content={"name": "must not be shared"}),
            lambda r: replace_presentation(r, server="another_server"),
            lambda r: replace_presentation(r, url="https://example.com/changed"),
            lambda r: replace_presentation(r, elicitation_id="another_callback"),
            lambda r: r["cases"][0]["mcp_reply"]["result"].update(action="decline"),
        ]
        for number, mutate in enumerate(mutations):
            with self.subTest(case=number):
                report, info = self.url_fixture()
                mutate(report)
                with self.assertRaises(ValueError):
                    SMOKE.validate_mcp_review_report(report, info, "url")

    def test_stdio_fixture_emits_only_the_selected_review_and_correlates_its_reply(self):
        for mode in ("form", "url"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="MCP fixture ü ") as root:
                log = Path(root) / "wire.jsonl"
                reply = {"jsonrpc": "2.0", "id": "fixture-elicitation-1", "result": {"action": "cancel"}}
                messages = [
                    {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}},
                    {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "review_with_" + mode}},
                    reply,
                ]
                run = subprocess.run([sys.executable, str(ROOT / "scripts/mcp_form_fixture_server.py"), str(log), mode],
                                     input="".join(json.dumps(m) + "\n" for m in messages),
                                     capture_output=True, text=True, encoding="utf-8", timeout=5)
                self.assertEqual(run.returncode, 0, run.stderr)
                self.assertEqual(run.stderr, "")
                emitted = [json.loads(line) for line in run.stdout.splitlines()]
                self.assertEqual(len(emitted), 4)
                self.assertEqual([t["name"] for t in emitted[1]["result"]["tools"]], ["review_with_" + mode])
                request = emitted[2]
                self.assertEqual((request["method"], request["id"], request["params"]["mode"]),
                                 ("elicitation/create", reply["id"], mode))
                if mode == "url":
                    self.assertEqual(request["params"]["url"], "https://example.com/agentdocker-fixture?case=1")
                    self.assertEqual(request["params"]["elicitationId"], reply["id"])
                    self.assertNotIn("requestedSchema", request["params"])
                else:
                    self.assertEqual(request["params"]["requestedSchema"]["type"], "object")
                    self.assertNotIn("url", request["params"])
                self.assertEqual(json.loads(emitted[3]["result"]["content"][0]["text"])["reply"], reply)
                wire = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
                self.assertEqual([x["value"] for x in wire if x["direction"] == "out"], emitted)

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
            lambda r: r["cases"][1]["mcp_reply"]["result"].update(content={"name": "not submitted"}),
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
                    SMOKE.validate_mcp_review_report(report, info)


if __name__ == "__main__":
    unittest.main()
