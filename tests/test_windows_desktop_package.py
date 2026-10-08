"""Windows archives bind PE architecture and extracted bytes to their provenance."""
import copy
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("windows_package_smoke", ROOT / "scripts/windows_package_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)
PACKAGE = SMOKE.PACKAGE
SPEC = importlib.util.spec_from_file_location("desktop_release", ROOT / "packaging/desktop/release.py")
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class WindowsDesktopPackaging(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.binaries = self.root / "compiled"
        self.binaries.mkdir()
        self.target = "x86_64-pc-windows-msvc"
        self.names = PACKAGE.binary_names(self.target)
        # Only PE header validation is exercised by this fixture. Executability
        # is established separately on the native runner's real compiled files.
        header = bytearray(256)
        header[:2] = b"MZ"
        struct.pack_into("<I", header, 60, 64)
        header[64:68] = b"PE\0\0"
        struct.pack_into("<H", header, 68, 0x8664)
        struct.pack_into("<HHH", header, 84, 112, 0x0002, 0x20B)
        for name in self.names:
            (self.binaries / name).write_bytes(header)
        self.manifest = {"format": 1, "source_commit": "a" * 40, "source_tree": "b" * 40,
                         "source_input_sha256": "c" * 64, "source_dirty": False,
                         "version": "0.1.0", "target": self.target, "state_schema": 23,
                         "installation_lock": 1, "launcher_redirect": 1}
        self.save_manifest()
        self.output = self.root / "package"
        self.args = PACKAGE.parser().parse_args([
            "--binary-dir", str(self.binaries), "--output", str(self.output),
            "--source", self.manifest["source_commit"], "--version", "0.1.0", "--target", self.target])

    def save_manifest(self):
        self.manifest["binary_sha256"] = {name: PACKAGE.sha256(self.binaries / name) for name in self.names}
        (self.binaries / "native-build.json").write_text(json.dumps(self.manifest), encoding="utf-8")

    def secret_report(self):
        # Deliberately synthetic validator data; only the native trial proves execution.
        report = {"result": "passed", "source_commit": self.manifest["source_commit"],
                  "source_tree": self.manifest["source_tree"], "source_dirty": False,
                  "binary_sha256": dict(self.manifest["binary_sha256"]), "provider_sha256": "d" * 64,
                  "cleanup_errors": [], "reader_errors": [], "remaining_processes": [],
                  "window_exit": 0, "terminal_canary_present": False,
                  "window_report": {"result": "passed", "error": None, "connected": True,
                                    "scenario_steps_completed": 10, "scenario_steps_total": 10},
                  "captures": {n + ".png": "e" * 64 for n in ["masked-before-consent", "masked-after-consent", "closed", "window"]},
                  "watched_processes": [{"pid": n, "birth": n * 10} for n in range(1, 6)],
                  "initial_generation": {"pid": 1, "birth": "recorded-process-generation"},
                  "completed_receipts": [{"message": str(n), "receipt": {"thread": "thread", "turn": str(n), "item": str(n)}} for n in range(4)],
                  "ordinary_message_ids": ["0", "1"], "review_metadata": {"request": {"thread": "thread", "turn": "0"}},
                  "fence_before_answer": {"thread": "thread", "turn": "0", "response_attempted": False},
                  "supplied_secret_flag": "isSecret", "secret_flag_advertised": False,
                  "model_requests": [{"path": "/v1/responses", "auxiliary": False,
                                      "discarded_input_present": False, "canary_present": n > 0,
                                      "canary_in_function_output": n > 0, "function_output_count": int(n > 0),
                                      "saved_draft_present": n >= 3, "after_bound_present": n == 4} for n in range(5)],
                  "scans": {n: {"files": 2, "bytes": 100, "matches": [{"occurrences": 2}] if n == "provider" else []}
                            for n in ["agentdocker", "provider", "internal", "gui"]}}
        for key in ["scratch_removed", "config_unchanged", "binaries_unchanged", "provider_unchanged",
                    "draft_typed_before_question", "suspended_terminal_input_not_echoed", "queued_ordinary_message_held",
                    "notice_required", "masked_app_submission", "secret_fence_closed", "draft_restored_and_delivered",
                    "oversized_line_discarded", "suspended_and_late_input_never_delivered", "final_fence_closed", "stale_answer_refused"]:
            report[key] = True
        return report

    def test_secret_acceptance_rejects_wrong_bytes_leaks_incomplete_ui_and_duplicate_receipts(self):
        valid = self.secret_report()
        SMOKE.validate_secret_report(valid, self.manifest, "d" * 64)
        corruptions = [lambda r: r.update(source_tree="foreign"),
                       lambda r: r["binary_sha256"].update({"agentdocker-ui.exe": "f" * 64}),
                       lambda r: r.update(provider_sha256="f" * 64),
                       lambda r: r.update(scratch_removed=False),
                       lambda r: r.update(notice_required=False),
                       lambda r: r.update(suspended_terminal_input_not_echoed=False),
                       lambda r: r.update(oversized_line_discarded=False),
                       lambda r: r["cleanup_errors"].append("forced retirement"),
                       lambda r: r["remaining_processes"].append({"pid": 2, "birth": 20}),
                       lambda r: r.update(terminal_canary_present=True),
                       lambda r: r["scans"]["agentdocker"]["matches"].append({"occurrences": 1}),
                       lambda r: r["scans"].pop("provider"),
                       lambda r: r["window_report"].update(scenario_steps_completed=9),
                       lambda r: r["captures"].pop("masked-before-consent.png"),
                       lambda r: r["completed_receipts"][3].update(receipt=r["completed_receipts"][2]["receipt"]),
                       lambda r: r["completed_receipts"][2]["receipt"].update(thread="foreign"),
                       lambda r: r["model_requests"][-1].update(discarded_input_present=True),
                       lambda r: r["model_requests"][-1].update(function_output_count=2)]
        for number, change in enumerate(corruptions):
            with self.subTest(corruption=number):
                invalid = copy.deepcopy(valid); change(invalid)
                with self.assertRaises(ValueError):
                    SMOKE.validate_secret_report(invalid, self.manifest, "d" * 64)

    def automatic_report(self):
        # Synthetic records exercise acceptance of provenance and cleanup;
        # the Windows job separately establishes actual execution.
        return {"result": "passed", "source_commit": self.manifest["source_commit"],
                "receiver_binary_sha256": dict(self.manifest["binary_sha256"]),
                "scratch_removed": True, "cleanup_errors": [], "reader_errors": [],
                "forced_processes": [], "steps": [{"passed": True} for _ in range(22)],
                "front_end_exit": {"pid": 4, "birth": 40, "frontend_exit_code": 15,
                                   "target": {"pid": 4, "birth": 40},
                                   "watched": [{"pid": n, "birth": n * 10} for n in (1, 2, 3)],
                                   "remaining": [],
                                   "capability_revoked": True, "receipts_preserved": True,
                                   "console_host": {"pid": 5, "alive_after_cleanup": True,
                                                    "resize_after_cleanup": True},
                                   "additional_model_requests": 0},
                "owner_exit": {"pid": 7, "birth": 70, "frontend_exit_code": 1,
                               "target": {"pid": 8, "birth": 80},
                               "watched": [{"pid": n, "birth": n * 10} for n in (7, 8, 9)],
                               "remaining": [], "capability_revoked": True,
                               "receipts_preserved": True, "additional_model_requests": 0,
                               "console_host": {"pid": 10, "alive_after_cleanup": True,
                                                "resize_after_cleanup": True}},
                "console_close": {"pid": 11, "birth": 110,
                                  "trigger": "drop_final_conpty_owner",
                                  "pty_references_before_release": 2,
                                  "participants_alive_before_release": True,
                                  "conpty_owner_released": True,
                                  "watched": [{"pid": n, "birth": n * 10} for n in range(11, 17)],
                                  "remaining": [], "capability_revoked": True,
                                  "receipts_preserved": True, "additional_model_requests": 0,
                                  "console_host": {"pid": 12, "birth": 120,
                                                   "alive_after_cleanup": False}}}

    def test_native_console_close_requires_actual_release_and_complete_retirement(self):
        native = self.automatic_report()
        for change in (lambda r: r.pop("console_close"),
                       lambda r: r.update(console_close=None),
                       lambda r: r["console_close"].update(trigger="kill_frontend"),
                       lambda r: r["console_close"].update(pty_references_before_release=3),
                       lambda r: r["console_close"].update(participants_alive_before_release=False),
                       lambda r: r["console_close"].update(conpty_owner_released=False),
                       lambda r: r["console_close"].update(remaining=[13]),
                       lambda r: r["console_close"].update(capability_revoked=False),
                       lambda r: r["console_close"].update(receipts_preserved=False),
                       lambda r: r["console_close"].update(additional_model_requests=1),
                       lambda r: r["console_close"].update(watched=None),
                       lambda r: r["console_close"]["watched"].pop(0),
                       lambda r: r["console_close"]["console_host"].update(birth=121),
                       lambda r: r["console_close"]["console_host"].update(alive_after_cleanup=True)):
            invalid = copy.deepcopy(native)
            change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_native_report(invalid, self.manifest, "automatic")

    def test_native_owner_exit_report_requires_its_own_pinned_target_and_cleanup(self):
        native = self.automatic_report()
        for change in (lambda r: r.pop("owner_exit"),
                       lambda r: r.update(owner_exit=copy.deepcopy(r["front_end_exit"])),
                       lambda r: r["owner_exit"].update(remaining=[8]),
                       lambda r: r["owner_exit"].update(capability_revoked=False),
                       lambda r: r["owner_exit"].update(receipts_preserved=False),
                       lambda r: r["owner_exit"].update(additional_model_requests=1),
                       lambda r: r["owner_exit"].update(frontend_exit_code=0),
                       lambda r: r["owner_exit"]["target"].update(birth=81),
                       lambda r: r["owner_exit"]["watched"].pop(0),
                       lambda r: r["owner_exit"]["console_host"].update(pid=8),
                       lambda r: r["owner_exit"]["console_host"].update(alive_after_cleanup=False)):
            invalid = copy.deepcopy(native)
            change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_native_report(invalid, self.manifest, "automatic")

    def test_automatic_native_report_requires_extracted_binary_and_source_provenance(self):
        native = self.automatic_report()
        SMOKE.validate_native_report(native, self.manifest, "automatic")
        for change in (lambda r: r.update(source_commit="d" * 40),
                       lambda r: r["receiver_binary_sha256"].update({"agentd.exe": "d" * 64}),
                       lambda r: r.update(receiver_binary_sha256={})):
            invalid = copy.deepcopy(native)
            change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_native_report(invalid, self.manifest, "automatic")

    def test_automatic_native_report_refuses_partial_or_forced_success(self):
        native = self.automatic_report()
        changes = [lambda r: r.update(result="failed"),
                   lambda r: r.update(scratch_removed=False),
                   lambda r: r["steps"].pop(),
                   lambda r: r["steps"][0].update(passed=False)]
        for key in ("cleanup_errors", "reader_errors", "forced_processes"):
            changes.extend((lambda r, k=key: r.update({k: ["failure"]}),
                            lambda r, k=key: r.pop(k)))
        for change in changes:
            invalid = copy.deepcopy(native)
            change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_native_report(invalid, self.manifest, "automatic")

    def test_direct_native_report_cannot_borrow_launcher_provenance(self):
        native = self.automatic_report()
        with self.assertRaises(ValueError):
            SMOKE.validate_native_report(native, self.manifest, "startup")
        native["binary_sha256"] = native.pop("receiver_binary_sha256")
        SMOKE.validate_native_report(native, self.manifest, "established")
        native["binary_sha256"]["agentdocker.exe"] = "d" * 64
        with self.assertRaises(ValueError):
            SMOKE.validate_native_report(native, self.manifest, "established")

    def test_native_exit_report_refuses_missing_or_partial_cleanup_evidence(self):
        native = self.automatic_report()
        for change in (lambda r: r.pop("front_end_exit"),
                       lambda r: r.update(front_end_exit=None),
                       lambda r: r["front_end_exit"].update(watched=[]),
                       lambda r: r["front_end_exit"].update(remaining=[1]),
                       lambda r: r["front_end_exit"].update(capability_revoked=False),
                       lambda r: r["front_end_exit"].update(receipts_preserved=False),
                       lambda r: r["front_end_exit"].update(additional_model_requests=1)):
            invalid = copy.deepcopy(native)
            change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_native_report(invalid, self.manifest, "automatic")

    def test_native_exit_report_refuses_a_missing_or_dead_console_host(self):
        native = self.automatic_report()
        for change in (lambda r: r["front_end_exit"].pop("console_host"),
                       lambda r: r["front_end_exit"].update(console_host=None),
                       lambda r: r["front_end_exit"]["console_host"].update(pid=0),
                       lambda r: r["front_end_exit"]["console_host"].update(pid=4),
                       lambda r: r["front_end_exit"]["console_host"].update(alive_after_cleanup=False),
                       lambda r: r["front_end_exit"]["console_host"].pop("resize_after_cleanup")):
            invalid = copy.deepcopy(native)
            change(invalid)
            with self.assertRaisesRegex(ValueError, "keep its console open"):
                SMOKE.validate_native_report(invalid, self.manifest, "automatic")

    def test_native_bootstrap_contract_is_preserved_in_portable_metadata(self):
        self.manifest['launcher_redirect'] = 2
        self.save_manifest()
        info, archive = self.build()
        self.assertEqual(info['launcher_redirect'], 2)
        with zipfile.ZipFile(archive) as bundle:
            metadata = json.loads(bundle.read('AgentDocker/build.json'))
        self.assertEqual(metadata['launcher_redirect'], 2)

    def queued_recovery_report(self, mode):
        native = self.automatic_report()
        native.update(steps=[{"passed": True} for _ in range(9)],
                      descriptor={"provider": {"session": "thread"}},
                      recovery_processes=[{"pid": n, "birth": n * 10} for n in range(1, 5)],
                      recovery_remaining=[], requests=[])
        rows = [{"message": str(n), "receipt": {"thread": "thread", "turn": "turn" + str(n),
                                               "item": "item" + str(n)}} for n in range(3)]
        before = {"completed": rows[:2], "attempt": {"message": "2", "queued": "entry", "receipt": None}}
        preview = {"pending": {"message": "2", "start_confirmation": "digest", "start_intent": None}}
        cases = (["provider_rate_hold", "project_pause"] if mode == "holds" else
                 ["wrong_digest", "wrong_message", "manual_read_is_not_start", "foreign_head"])
        proof = {"mode": mode, "hold_seconds": 30, "ledger_before": before,
                 "message": "2", "preview": {"exit_code": 0, "stdout": json.dumps(preview)},
                 "refusals": [{"case": case, "reply": {"exit_code": 1, "stderr": "daemon is holding this input; project is paused"},
                               "ledger_after": copy.deepcopy(before)} for case in cases],
                 "completed_after": rows[:2] if mode == "holds" else rows}
        if mode == "holds":
            proof.update(paused_preview=copy.deepcopy(proof["preview"]),
                         pause={"pause": {"project": "project"}}, pauses_after={"pauses": [{"project": "project"}]})
        else:
            first = {"message": "2", "turn": "turn2", "queued_start": "intent", "already_attempted": False}
            proof.update(response={"exit_code": 0, "stdout": "intent\n", "stderr": json.dumps(first)},
                         repeat={"exit_code": 0, "stdout": "intent\n", "stderr": json.dumps(dict(first, already_attempted=True))})
            native["requests"] = [{"recovery": True, "title": False}]
        proof["final_history"] = {"data": [{"turnId": row["receipt"]["turn"],
                                           "item": {"type": "userMessage", "clientId": row["message"],
                                                    "id": row["receipt"]["item"]}} for row in proof["completed_after"]]}
        native["queued_recovery"] = proof
        return native

    def test_queued_recovery_rejects_duplicate_input_and_invented_retry_or_cleanup(self):
        native = self.queued_recovery_report("normal")
        SMOKE.validate_queue_recovery_report(native, self.manifest, "normal")
        for change in (lambda r: r["requests"].append(dict(r["requests"][0])),
                       lambda r: r["queued_recovery"]["final_history"]["data"].pop(),
                       lambda r: r["queued_recovery"]["completed_after"][-1]["receipt"].update(turn="wrong"),
                       lambda r: r["queued_recovery"]["repeat"].update(stderr=r["queued_recovery"]["response"]["stderr"]),
                       lambda r: r["queued_recovery"]["response"].update(stdout="other-intent\n"),
                       lambda r: r["queued_recovery"]["repeat"].update(stdout="intent\nextra-output\n"),
                       lambda r: r["queued_recovery"]["refusals"][0]["ledger_after"]["attempt"].update(start={"id": "unexpected"}),
                       lambda r: r["recovery_remaining"].append({"pid": 2, "birth": 20}),
                       lambda r: r.update(source_commit="wrong")):
            invalid = copy.deepcopy(native); change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_queue_recovery_report(invalid, self.manifest, "normal")

    def test_queued_recovery_hold_requires_real_refusal_and_preserved_preview(self):
        native = self.queued_recovery_report("holds")
        SMOKE.validate_queue_recovery_report(native, self.manifest, "holds")
        for change in (lambda r: r["queued_recovery"]["refusals"][-1]["reply"].update(exit_code=0),
                       lambda r: r["queued_recovery"]["refusals"][-1]["reply"].update(stderr="unrelated failure"),
                       lambda r: r["queued_recovery"]["pauses_after"].update(pauses=[]),
                       lambda r: r["queued_recovery"]["paused_preview"].update(stdout='{"pending":{"message":"2","start_confirmation":"changed","start_intent":null}}'),
                       lambda r: r["requests"].append({"recovery": True, "title": False})):
            invalid = copy.deepcopy(native); change(invalid)
            with self.assertRaises(ValueError):
                SMOKE.validate_queue_recovery_report(invalid, self.manifest, "holds")

    def test_completed_recovery_retry_requires_exact_receipt_and_empty_preview(self):
        native = self.queued_recovery_report("normal")
        proof = native["queued_recovery"]
        proof.update(repeat_disposition="already_delivered",
                     repeat={"exit_code": 1, "stdout": "", "stderr": "Error: no retained native input to start\n"},
                     ledger_after_repeat={"completed": copy.deepcopy(proof["completed_after"]), "attempt": None},
                     retired_preview={"exit_code": 0, "stdout": '{"pending":null}'})
        SMOKE.validate_queue_recovery_report(native, self.manifest, "normal")
        for change in (lambda p: p["repeat"].update(stderr="Error: daemon unavailable"),
                       lambda p: p["ledger_after_repeat"]["completed"][-1].update(message="other"),
                       lambda p: p["ledger_after_repeat"].update(attempt={"message": "2"}),
                       lambda p: p["ledger_after_repeat"].pop("attempt"),
                       lambda p: p["retired_preview"].update(stdout='{"pending":{"message":"2"}}'),
                       lambda p: p["retired_preview"].update(stdout='{}'),
                       lambda p: p.update(repeat_disposition="pending_intent")):
            invalid = copy.deepcopy(native); change(invalid["queued_recovery"])
            with self.assertRaises(ValueError):
                SMOKE.validate_queue_recovery_report(invalid, self.manifest, "normal")

    def lost_client_recovery_report(self):
        native = self.queued_recovery_report("client-reply-loss")
        proof = native["queued_recovery"]
        proof.pop("response")
        proof["ledger_before"]["binding"] = {"agent": "agent"}
        request = {"action": "start_queued", "agent": "agent", "message": "2",
                   "provider": native["descriptor"]["provider"],
                   "confirmation": "digest", "note": "Private loss trial"}
        proof["local_client_reply_loss"] = {
            "endpoint_kind": "private resolve named pipe", "command": request,
            "request_bytes": len((json.dumps(request) + '\n').encode('utf-8')),
            "response_read_calls": 0, "client_closed_after_send": True,
            "closed_after_durable_intent": True,
            "durable_intent_observed": {"id": "intent", "queued": "entry",
                                        "confirmation": "digest", "note": request["note"],
                                        "provider": request["provider"],
                                        "transmission": "prepared", "turn": None}}
        return native

    def test_lost_client_recovery_requires_unread_reply_and_exact_original_intent(self):
        native = self.lost_client_recovery_report()
        SMOKE.validate_queue_recovery_report(native, self.manifest, "client-reply-loss")
        for change in (lambda p: p.update(response={}),
                       lambda p: p["local_client_reply_loss"].update(response_read_calls=1),
                       lambda p: p["local_client_reply_loss"].update(client_closed_after_send=False),
                       lambda p: p["local_client_reply_loss"].update(closed_after_durable_intent=False),
                       lambda p: p["local_client_reply_loss"].update(request_bytes=0),
                       lambda p: p["local_client_reply_loss"]["command"].update(agent="wrong"),
                       lambda p: p["local_client_reply_loss"]["durable_intent_observed"].update(id="wrong"),
                       lambda p: p["local_client_reply_loss"]["durable_intent_observed"].update(queued="other"),
                       lambda p: p["local_client_reply_loss"]["durable_intent_observed"].update(confirmation="wrong"),
                       lambda p: p["local_client_reply_loss"]["durable_intent_observed"].update(transmission="unknown"),
                       lambda p: p["local_client_reply_loss"]["durable_intent_observed"].update(turn="other")):
            invalid = copy.deepcopy(native); change(invalid["queued_recovery"])
            with self.assertRaises(ValueError):
                SMOKE.validate_queue_recovery_report(invalid, self.manifest, "client-reply-loss")

    def test_lost_client_retry_after_retirement_requires_completed_receipt(self):
        native = self.lost_client_recovery_report()
        proof = native["queued_recovery"]
        proof.update(repeat_disposition="already_delivered",
                     repeat={"exit_code": 1, "stdout": "", "stderr": "Error: no retained native input to start\n"},
                     ledger_after_repeat={"completed": copy.deepcopy(proof["completed_after"]), "attempt": None},
                     retired_preview={"exit_code": 0, "stdout": '{"pending":null}'})
        SMOKE.validate_queue_recovery_report(native, self.manifest, "client-reply-loss")
        for change in (lambda p: p["repeat"].update(stderr="Error: daemon unavailable"),
                       lambda p: p["ledger_after_repeat"]["completed"][-1].update(message="other"),
                       lambda p: p["ledger_after_repeat"].update(attempt={"message": "2"}),
                       lambda p: p["retired_preview"].update(stdout='{"pending":{"message":"2"}}')):
            invalid = copy.deepcopy(native); change(invalid["queued_recovery"])
            with self.assertRaises(ValueError):
                SMOKE.validate_queue_recovery_report(invalid, self.manifest, "client-reply-loss")

    def build(self):
        info = PACKAGE.package(self.args)
        return info, self.output / next(iter(info["artifacts"]))

    def test_portable_archive_contains_only_exact_executables_metadata_and_licenses(self):
        info, archive = self.build()
        self.assertEqual(info["signing"], "unsigned")
        self.assertEqual(info["distribution"], "portable-preview")
        self.assertEqual(info["binary_sha256"], self.manifest["binary_sha256"])
        app = SMOKE.extract_checked(archive, self.root / "unpacked ü folder", info)
        for name in self.names:
            self.assertEqual((app / name).read_bytes(), (self.binaries / name).read_bytes())
        self.assertEqual((app / "licenses/LICENSE-AgentDocker.txt").read_bytes(), (ROOT / "LICENSE").read_bytes())
        self.assertEqual((app / "licenses/LICENSE-Inter.txt").read_bytes(), (ROOT / "crates/ui/src/fonts/LICENSE-Inter.txt").read_bytes())
        self.assertIn("desktop install --from . --local-preview", (app / "README.txt").read_text(encoding="utf-8"))
        self.assertEqual((self.output / (archive.name + ".sha256")).read_text().strip(), info["artifacts"][archive.name] + "  " + archive.name)

    def accepted_fixture(self):
        # This is synthetic acceptance for promotion-policy tests only;
        # windows_package_smoke supplies real execution evidence in CI.
        self.manifest["version"] = self.args.version = "0.2.0-rc.1"
        self.manifest["launcher_redirect"] = 2
        self.save_manifest()
        info, archive = self.build()
        desktop = self.output / "smoke/desktop"
        desktop.mkdir(parents=True)
        screenshot = desktop / "window.png"
        screenshot.write_bytes(b"fixture screenshot")
        self.observed = {"result": "passed", "binary_sha256": info["binary_sha256"],
                         "steps": [{"step": "fixture only", "ok": True}],
                         "desktop": {"result": "passed", "screenshot_sha256": PACKAGE.sha256(screenshot),
                                     "native_result": {"result": "passed", "connected": True}}}
        self.report = {key: info[key] for key in ("source_commit", "source_input_sha256", "binary_sha256", "artifacts")}
        self.report.update(result="passed", steps=1, desktop=self.observed["desktop"],
                           installation={"result": "passed", "steps": 1})
        self.installed = {"result": "passed", "source_commit": info["source_commit"],
                          "binary_sha256": info["binary_sha256"], "service_requested": True,
                          "scratch_removed": True, "steps": [{"passed": True}],
                          "service": {"result": "passed", "steps": 1}}
        self.service = {"result": "passed", "cleanup_errors": [], "scratch_removed": True, "installed_launchers": True,
                        "binary_sha256": info["binary_sha256"], "steps": [{"ok": True}]}
        (self.output / "installation/service").mkdir(parents=True)
        self.save_acceptance()
        return info, archive

    def save_acceptance(self):
        (self.output / "package-acceptance.json").write_text(json.dumps(self.report))
        (self.output / "installation/result.json").write_text(json.dumps(self.installed))
        (self.output / "installation/service/result.json").write_text(json.dumps(self.service))
        (self.output / "smoke/windows-daemon-smoke.json").write_text(json.dumps(self.observed))

    def promote(self, tag="v0.2.0-rc.1", source="a" * 40):
        return RELEASE.windows_preview(self.binaries / "native-build.json", self.output,
                                       self.root / "release", tag, source)

    def test_preview_promotion_retains_accepted_bytes_and_separate_manifest(self):
        info, archive = self.accepted_fixture()
        self.promote()
        release = self.root / "release"
        self.assertEqual((release / archive.name).read_bytes(), archive.read_bytes())
        self.assertEqual(json.loads((release / "windows-preview-manifest.json").read_text()), info)
        self.assertEqual(json.loads((release / "windows-preview-acceptance.json").read_text()), self.report)
        self.assertFalse(list(release.glob("manifest-*.json")))  # not a Mac/Linux feed input
        self.assertFalse((release / "updates-preview.json").exists())
        feed = json.loads((release / "updates-preview-windows.json").read_text())
        self.assertEqual([entry["target"] for entry in feed["releases"]], [self.target])
        self.assertEqual(feed["releases"][0]["archive"]["sha256"], info["artifacts"][archive.name])
        self.assertEqual(feed["releases"][0]["signing"], "unsigned-preview")
        self.assertEqual(feed["policy"]["activation"], "explicit")
        self.assertIn("desktop install --from . --local-preview", (release / "WINDOWS-PREVIEW.txt").read_text())
        self.assertEqual(len(list(release.iterdir())), 8)
        with self.assertRaises(FileExistsError):
            self.promote()

    def test_windows_preview_rejects_stable_wrong_version_and_dirty_build(self):
        self.accepted_fixture()
        for tag, error in [("v0.2.0", "prerelease"), ("v0.3.0-rc.1", "matching clean")]:
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, error):
                self.promote(tag)
            self.assertFalse((self.root / "release").exists())
        with self.assertRaisesRegex(ValueError, "matching clean"):
            self.promote(source="d" * 40)  # another commit with the same version
        self.manifest["source_dirty"] = True
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "matching clean"):
            self.promote()

    def test_windows_preview_refuses_failed_stale_or_incomplete_acceptance(self):
        self.accepted_fixture()
        original_report, original_observed = json.dumps(self.report), json.dumps(self.observed)
        mutations = [lambda: self.report.update(result="failed"),
                     lambda: self.report.update(source_input_sha256="d" * 64),
                     lambda: self.report.update(steps=2),
                     lambda: self.observed.update(steps=[]),
                     lambda: self.observed["steps"][0].update(ok=False),
                     lambda: self.observed.update(binary_sha256={}),
                     lambda: self.observed["desktop"]["native_result"].update(connected=False)]
        for mutation in mutations:
            self.report, self.observed = json.loads(original_report), json.loads(original_observed)
            mutation()
            self.save_acceptance()
            with self.assertRaises(ValueError):
                self.promote()
            self.assertFalse((self.root / "release").exists())
        self.report, self.observed = json.loads(original_report), json.loads(original_observed)
        self.save_acceptance()
        (self.output / "smoke/desktop/window.png").write_bytes(b"replaced")
        with self.assertRaisesRegex(ValueError, "screenshot"):
            self.promote()

    def test_windows_feed_requires_successful_exact_installed_lifecycle(self):
        self.accepted_fixture()
        original_install, original_service = json.dumps(self.installed), json.dumps(self.service)
        mutations = [lambda: self.installed.update(result="failed"),
                     lambda: self.installed.update(source_commit="f" * 40),
                     lambda: self.installed.update(binary_sha256={}),
                     lambda: self.installed.update(service_requested=False),
                     lambda: self.installed.update(scratch_removed=False),
                     lambda: self.installed["steps"][0].update(passed=False),
                     lambda: self.installed.update(service={}),
                     lambda: self.service.update(result="failed"),
                     lambda: self.service.update(installed_launchers=False),
                     lambda: self.service.update(cleanup_errors=["still running"]),
                     lambda: self.service.update(binary_sha256={}),
                     lambda: self.service["steps"][0].update(ok=False)]
        for mutation in mutations:
            self.installed, self.service = json.loads(original_install), json.loads(original_service)
            mutation()
            self.save_acceptance()
            with self.assertRaisesRegex(ValueError, "installed lifecycle"):
                self.promote()
            self.assertFalse((self.root / "release").exists())

    def test_windows_preview_refuses_archive_replaced_during_promotion(self):
        _, archive = self.accepted_fixture()
        copy = RELEASE.shutil.copyfile

        def replace(source, destination, **kwargs):
            result = copy(source, destination, **kwargs)
            if Path(source) == archive:
                value = bytearray(Path(destination).read_bytes())
                value[-1] ^= 1  # same size, different bytes
                Path(destination).write_bytes(value)
            return result

        with patch.object(RELEASE.shutil, "copyfile", side_effect=replace):
            with self.assertRaisesRegex(ValueError, "checksum"):
                self.promote()
        self.assertFalse((self.root / "release").exists())

    def test_preview_release_notes_preserve_existing_text_and_are_idempotent(self):
        original = "## Changes\n\nA contributor's release notes.\n"
        notes = RELEASE.preview_notes("v0.2.0-rc.1", original)
        self.assertTrue(notes.endswith(original))
        self.assertIn("unsigned portable preview", notes)
        self.assertIn("actual-provider trials remain open", notes)
        self.assertEqual(RELEASE.preview_notes("v0.2.0-rc.1", notes), notes)
        self.assertEqual(RELEASE.preview_notes("v0.2.0", original), original)

    def test_wrong_machine_dll_or_truncated_pe_is_refused_before_publication(self):
        binary = self.binaries / "agentd.exe"
        original = binary.read_bytes()
        for offset, value, fmt in [(68, 0xAA64, "<H"), (86, 0x2002, "<H"),
                                   (86, 0, "<H"), (60, 0xFFFF_FFFF, "<I"),
                                   (84, 0xFFFF, "<H"), (88, 0x10B, "<H"), (64, 0, "<I")]:
            with self.subTest(offset=offset, value=value):
                header = bytearray(original)
                struct.pack_into(fmt, header, offset, value)
                binary.write_bytes(header)
                self.save_manifest()  # Matching hashes must not bypass the PE check.
                with self.assertRaisesRegex(ValueError, "Windows"):
                    PACKAGE.package(self.args)
                self.assertFalse(self.output.exists())
        binary.write_bytes(b"MZ")
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "Windows"):
            PACKAGE.package(self.args)

    def test_substituted_executable_and_wrong_manifest_are_refused(self):
        (self.binaries / "agentdocker.exe").write_bytes(b"replacement")
        with self.assertRaisesRegex(ValueError, "changed after"):
            PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())
        self.manifest["target"] = "x86_64-unknown-linux-gnu"
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "target"):
            PACKAGE.package(self.args)

    def test_replacement_during_copy_cannot_acquire_the_verified_source_provenance(self):
        copy = PACKAGE.shutil.copyfile

        def replace(source, destination, **kwargs):
            result = copy(source, destination, **kwargs)
            if source.name == "agentd.exe":
                # Still a valid x64 executable header, but no longer the build's bytes.
                with destination.open("ab") as stream:
                    stream.write(b"different build")
            return result

        with patch.object(PACKAGE.shutil, "copyfile", side_effect=replace):
            with self.assertRaisesRegex(ValueError, "between build verification and packaging"):
                PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())

    def test_supplied_build_evidence_cannot_be_replaced_by_another_directory_manifest(self):
        for key, value in [("source_input_sha256", "d" * 64),
                           ("binary_sha256", {name: "e" * 64 for name in self.names})]:
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "supplied build evidence"):
                PACKAGE.package(self.args, expected_build={**self.manifest, key: value})
            self.assertFalse(self.output.exists())

    def test_changed_archive_is_refused_before_extraction(self):
        info, archive = self.build()
        with archive.open("ab") as stream:
            stream.write(b"changed")
        destination = self.root / "extract"
        with self.assertRaisesRegex(ValueError, "archive checksum"):
            SMOKE.extract_checked(archive, destination, info)
        self.assertFalse(destination.exists())

    def test_unexpected_archive_member_is_refused_even_with_matching_hash(self):
        info, archive = self.build()
        with zipfile.ZipFile(archive, "a") as bundle:
            bundle.writestr("../outside.exe", "must not be extracted")
        info["artifacts"][archive.name] = PACKAGE.sha256(archive)
        with self.assertRaisesRegex(ValueError, "exact portable layout"):
            SMOKE.extract_checked(archive, self.root / "extract", info)
        self.assertFalse((self.root / "outside.exe").exists())

    def test_archive_metadata_and_executable_hashes_must_match(self):
        info, archive = self.build()
        for key, value in [("state_schema", 99), ("binary_sha256", {name: "d" * 64 for name in self.names})]:
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "metadata differs"):
                SMOKE.extract_checked(archive, self.root / key, {**info, key: value})

    def test_substituted_zip_executable_is_refused_even_with_matching_archive_hash(self):
        info, archive = self.build()
        with zipfile.ZipFile(archive) as bundle:
            members = {name: bundle.read(name) for name in bundle.namelist()}
        members["AgentDocker/agentd.exe"] = b"substituted payload"
        with zipfile.ZipFile(archive, "w") as bundle:
            for name, content in members.items():
                bundle.writestr(name, content)
        info["artifacts"][archive.name] = PACKAGE.sha256(archive)
        with self.assertRaisesRegex(ValueError, "binary checksum differs"):
            SMOKE.extract_checked(archive, self.root / "extract", info)

    def test_mac_only_options_and_existing_outputs_are_refused(self):
        self.args.dmg = True
        with self.assertRaisesRegex(ValueError, "only for macOS"):
            PACKAGE.package(self.args)
        self.args.dmg = False
        self.output.mkdir()
        (self.output / "keep.txt").write_text("previous")
        with self.assertRaises(FileExistsError):
            PACKAGE.package(self.args)
        self.assertEqual((self.output / "keep.txt").read_text(), "previous")


if __name__ == "__main__":
    unittest.main()
