#!/usr/bin/env python3
"""Package Windows release binaries, then exercise only the extracted archive."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_package", ROOT / "packaging/desktop/package.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


def extract_checked(archive, destination, manifest):
    """Check the exact portable layout and all executable bytes before running."""
    names = PACKAGE.binary_names("x86_64-pc-windows-msvc")
    expected = {"AgentDocker/" + name for name in names}
    expected.update({"AgentDocker/build.json", "AgentDocker/README.txt",
                     "AgentDocker/licenses/LICENSE-Inter.txt",
                     "AgentDocker/licenses/LICENSE-AgentDocker.txt"})
    if PACKAGE.sha256(archive) != manifest["artifacts"][archive.name]:
        raise ValueError("Windows archive checksum differs from its manifest")
    with zipfile.ZipFile(archive) as bundle:
        if len(bundle.infolist()) != len(expected) or set(bundle.namelist()) != expected:
            raise ValueError("Windows archive does not have the exact portable layout")
        if sum(item.file_size for item in bundle.infolist()) > 100 * 1024 ** 2:
            raise ValueError("Windows extracted payload exceeds the size budget")
        # Copy regular file bytes explicitly; never honor symlinks or traversal.
        for name in sorted(expected):
            path = destination / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with bundle.open(name) as source, path.open("xb") as target:
                shutil.copyfileobj(source, target)
    app = destination / "AgentDocker"
    inside = json.loads((app / "build.json").read_text(encoding="utf-8"))
    for key in ("source_commit", "source_tree", "source_input_sha256", "source_dirty", "target",
                "version", "state_schema", "installation_lock", "launcher_redirect", "signing",
                "distribution", "binary_sha256"):
        if inside.get(key) != manifest.get(key):
            raise ValueError(f"Windows archive build metadata differs: {key}")
    for name in names:
        if PACKAGE.sha256(app / name) != manifest["binary_sha256"][name]:
            raise ValueError(f"Windows archived binary checksum differs: {name}")
    return app


def validate_secret_report(observed, info, provider_sha256):
    """Require the exact archive, masked review, terminal boundaries and retirement."""
    def require(ok):
        if not ok:
            raise ValueError("Windows secret acceptance lacks exact UI, receipts, retention or cleanup")

    require(observed.get("result") == "passed"
            and observed.get("source_commit") == info["source_commit"]
            and observed.get("source_tree") == info["source_tree"]
            and observed.get("source_dirty") is False
            and observed.get("binary_sha256") == info["binary_sha256"]
            and observed.get("provider_sha256") == provider_sha256
            and observed.get("cleanup_errors") == [] and observed.get("reader_errors") == []
            and observed.get("remaining_processes") == []
            and not observed.get("model_errors") and not observed.get("unexpected_get")
            and observed.get("window_exit") == 0 and observed.get("terminal_canary_present") is False)
    for flag in ("scratch_removed", "config_unchanged", "binaries_unchanged", "provider_unchanged",
                 "draft_typed_before_question", "suspended_terminal_input_not_echoed",
                 "queued_ordinary_message_held", "notice_required", "masked_app_submission",
                 "secret_fence_closed", "draft_restored_and_delivered", "oversized_line_discarded",
                 "suspended_and_late_input_never_delivered", "final_fence_closed", "stale_answer_refused", "attachment_detached"):
        require(observed.get(flag) is True)
    detach = observed.get("detach", {})
    require(detach.get("sent") is True and detach.get("farewell_observed") is True
            and detach.get("native_process_alive") is False and detach.get("conpty_alive") is False)
    window = observed.get("window_report", {})
    require(window.get("result") == "passed" and window.get("error") is None
            and window.get("connected") is True
            and window.get("scenario_steps_completed") == window.get("scenario_steps_total") == 10)
    captures = observed.get("captures", {})
    require(set(captures) == {"masked-before-consent.png", "masked-after-consent.png", "closed.png", "window.png"}
            and all(isinstance(v, str) and len(v) == 64 and all(c in "0123456789abcdef" for c in v)
                    for v in captures.values()))
    processes = observed.get("watched_processes", [])
    require(len(processes) >= 5 and all(isinstance(p.get("pid"), int) and p["pid"] > 0
                                      and isinstance(p.get("birth"), (int, float)) and p["birth"] > 0 for p in processes))
    require(len({(p["pid"], p["birth"]) for p in processes}) == len(processes))
    require(observed.get("initial_generation", {}).get("pid") in {p["pid"] for p in processes})
    rows = observed.get("completed_receipts", [])
    require(len(rows) == 4 and all(isinstance(r.get("message"), str) and r["message"]
                                  and isinstance(r.get("receipt"), dict) for r in rows))
    require(len({r["message"] for r in rows}) == 4
            and len(observed.get("ordinary_message_ids", [])) == 2
            and {r["message"] for r in rows[:2]} == set(observed["ordinary_message_ids"]))
    request = observed.get("review_metadata", {}).get("request", {})
    require(isinstance(request.get("thread"), str) and bool(request["thread"])
            and all(r["receipt"].get("thread") == request["thread"]
                    and bool(r["receipt"].get("turn")) and bool(r["receipt"].get("item")) for r in rows))
    require(len({(r["receipt"]["turn"], r["receipt"]["item"]) for r in rows}) == 4)
    initial = {r["message"]: r["receipt"] for r in rows}[observed["ordinary_message_ids"][0]]
    require(initial == observed.get("initial_receipt_before_answer"))
    fence = observed.get("fence_before_answer", {})
    require(fence.get("response_attempted") is False and fence.get("thread") == request["thread"]
            and fence.get("turn") == request.get("turn") == initial["turn"])
    requests = observed.get("model_requests", [])
    require(4 <= len(requests) <= 10 and all(r.get("path") == "/v1/responses"
                                           and r.get("discarded_input_present") is False for r in requests))
    ordinary = [r for r in requests if r.get("auxiliary") is False]
    require(len(ordinary) >= 4 and ordinary[0].get("canary_present") is False
            and ordinary[0].get("function_output_count") == 0)
    require(all(r.get("canary_present") is True and r.get("canary_in_function_output") is True
                and r.get("function_output_count") == 1 for r in ordinary[1:]))
    require(any(r.get("saved_draft_present") is True for r in ordinary)
            and ordinary[-1].get("after_bound_present") is True)
    require(ordinary[0].get("original_input_present") is True
            and ordinary[0].get("queued_input_present") is False
            and any(r.get("queued_input_present") is True for r in ordinary)
            and all(r.get("queued_input_present") is False
                    or (r.get("queued_input_present") is True and r.get("canary_in_function_output") is True)
                    for r in ordinary))
    # Completion-list order can differ from submission order: a same-turn
    # steering receipt retires before the original turn's completion.
    # Repeated function output is conversation history, not another submission.
    # A supplied flag is recorded even when absent from the provider's schema.
    require(observed.get("supplied_secret_flag") in ("is_secret", "isSecret")
            and isinstance(observed.get("secret_flag_advertised"), bool))
    scans = observed.get("scans", {})
    require(set(scans) == {"agentdocker", "provider", "internal", "gui"})
    for kind, scan in scans.items():
        require(isinstance(scan.get("files"), int) and 0 < scan["files"] <= 10000
                and isinstance(scan.get("bytes"), int) and 0 < scan["bytes"] <= 1024**3
                and isinstance(scan.get("matches"), list))
        if kind != "provider":
            require(scan["matches"] == [])
        else:
            require(bool(scan["matches"]) and all(isinstance(m.get("occurrences"), int)
                                                 and m["occurrences"] > 0 for m in scan["matches"]))


def validate_mcp_review_report(observed, info, mode="form", idle=False):
    """Require exact archive identity, four decisions, receipts and clean retirement."""
    def require(ok):
        if not ok:
            raise ValueError("Windows MCP review acceptance lacks exact decisions, receipts or cleanup")

    require(mode in ("form", "url") and observed.get("review_mode") == mode)
    require(observed.get("idle_review", False) is idle)
    accept = "Submit" if mode == "form" else "Accept"
    require(observed.get("result") == "passed"
            and observed.get("source_commit") == info["source_commit"]
            and observed.get("source_tree") == info["source_tree"]
            and observed.get("source_dirty") is False
            and observed.get("scratch_removed") is True
            and observed.get("config_unchanged") is True
            and observed.get("cleanup_errors") == []
            and observed.get("remaining_processes") == []
            and not observed.get("model_errors") and not observed.get("unexpected_get")
            and all(observed.get("binary_sha256", {}).get(n) == info["binary_sha256"][n]
                    for n in ("agentdocker.exe", "agentd.exe")))
    processes = observed.get("watched_processes", [])
    require(len(processes) >= 4 and all(isinstance(p.get("pid"), int) and p["pid"] > 0
                                      and isinstance(p.get("birth"), (int, float)) and p["birth"] > 0
                                      for p in processes))
    require(len({(p["pid"], p["birth"]) for p in processes}) == len(processes))
    initial, final = observed.get("initial_agent", {}), observed.get("final_agent", {})
    require(bool(initial.get("process_started_at")) and initial.get("pid") == final.get("pid")
            and isinstance(initial.get("pid"), int) and initial["pid"] > 0
            and initial["process_started_at"] == final.get("process_started_at")
            and initial.get("id") == final.get("id") == observed.get("agent")
            and observed.get("initial_pid") == initial.get("pid"))
    cases = observed.get("cases", [])
    require([c.get("decision") for c in cases] == [accept, "Decline", "Cancel", "cancel-route"])
    ledger = observed.get("final_ledger", {})
    rows = ledger.get("completed", [])
    count = 5 if idle else 8
    require(len(rows) == count and ledger.get("attempt") is None and bool(ledger.get("thread")))
    receipts = {row["message"]: row["receipt"] for row in rows}
    require(len(receipts) == count)
    all_ids, mcp_ids = [], []
    if idle:
        initial_input = observed.get("initial_input")
        initial_ledger = observed.get("initial_completed_ledger", {})
        require(isinstance(initial_input, str) and initial_input in receipts
                and initial_ledger.get("attempt") is None and initial_ledger.get("reviews") == []
                and [r["message"] for r in initial_ledger.get("completed", [])] == [initial_input]
                and initial_ledger["completed"][0]["receipt"] == receipts[initial_input])
        all_ids.append(initial_input)
    for number, case in enumerate(cases, 1):
        require(case.get("result") == "passed")
        ids = [case.get("peer_input")] if idle else [case.get("input"), case.get("peer_input")]
        require(all(isinstance(i, str) and i for i in ids) and len(set(ids)) == len(ids))
        prior_ids = all_ids.copy()
        all_ids.extend(ids)
        require(case.get("receipts") == {i: receipts.get(i) for i in ids}
                and all(isinstance(receipts.get(i), dict) and receipts[i].get("thread") == ledger["thread"]
                        and bool(receipts[i].get("turn")) and bool(receipts[i].get("item")) for i in ids))
        if not idle:
            require(receipts[ids[0]]["item"] != receipts[ids[1]]["item"])
        held = case.get("held_ledger", {})
        require(not held.get("steering")
                and ids[-1] not in [r["message"] for r in held.get("completed", [])]
                and len(held.get("reviews", [])) == 1
                and held["reviews"][0].get("response") is None)
        if idle:
            before = case.get("before_trigger", {})
            require(before.get("attempt") is None and before.get("reviews") == []
                    and held.get("attempt") is None and held["reviews"][0].get("turn") is None
                    and held.get("completed") == before.get("completed")
                    and [r["message"] for r in before.get("completed", [])] == prior_ids
                    and all(r["receipt"] == receipts[r["message"]] for r in before["completed"]))
            wire = case.get("wire_reply", {})
            require(wire.get("value") == case.get("mcp_reply")
                    and isinstance(wire.get("at"), (int, float))
                    and isinstance(case.get("decision_at"), (int, float))
                    and wire["at"] >= case["decision_at"])
            requests = observed.get("model_requests", [])
            offset = case.get("model_requests_before")
            require(offset == number + 1 and len(requests) == 6
                    and all(not r.get("auxiliary") for r in requests)
                    and requests[offset]["at"] >= case["decision_at"]
                    and requests[offset]["case"] == case["decision"])
        else:
            require(held.get("attempt", {}).get("message") == ids[0]
                    and held["attempt"].get("acknowledged") is True)
        require(len(case.get("invalid_answers", [])) == 2
                and all(a.get("code") == "invalid" for a in case["invalid_answers"]))
        action = "accept" if case["decision"] == accept else "decline" if case["decision"] == "Decline" else "cancel"
        content = case.get("submitted_content")
        require(isinstance(content, dict) if mode == "form" and action == "accept" else content is None)
        response = {"action": action, "content": content}
        closed = case.get("closed_review", {})
        request = closed.get("request", {})
        question = case.get("question", {})
        presentation = question.get("presentation", {})
        require(bool(question.get("id")) and presentation.get("kind") == "mcp_" + mode
                and presentation.get("server") == "review_fixture"
                and len(request.get("questions", [])) == 1
                and request["questions"][0].get("message") == question["id"]
                and request["questions"][0].get("presentation") == question["presentation"])
        if mode == "url":
            require(presentation.get("url") == ("https://example.com/private-fixture" if idle else "https://example.com/agentdocker-fixture?case=" + str(number))
                    and presentation.get("elicitation_id") == "fixture-elicitation-" + str(number))
        require(closed.get("outcome") == "resolved" and closed.get("acknowledged") is True
                and request.get("id") == case.get("provider_request") == held["reviews"][0].get("id")
                and request.get("thread") == ledger["thread"]
                and request.get("turn") == (None if idle else receipts[ids[0]]["turn"])
                and request.get("response") == {"id": case["provider_request"], "result": response})
        reply = case.get("mcp_reply", {})
        # The app-server response includes content:null. Codex forwards URL
        # Accept with an empty object and omits content on decline/cancel.
        wire_response = ({"action": action, "content": content if mode == "form" else {}}
                         if action == "accept" else {"action": action})
        require(reply.get("result") == wire_response and bool(reply.get("id")))
        mcp_ids.append(reply["id"])
    require(len(set(all_ids)) == count and set(all_ids) == set(receipts) and len(set(mcp_ids)) == 4)
    if idle:
        require([r["message"] for r in rows] == all_ids
                and len({(r["receipt"]["turn"], r["receipt"]["item"]) for r in rows}) == count)


def validate_native_report(native, info, scenario):
    """A passing process must still identify the exact extracted payload."""
    automatic = scenario == "automatic"
    hashes = native.get("receiver_binary_sha256" if automatic else "binary_sha256", {})
    if native.get("result") != "passed" or any(
        hashes.get(name) != info["binary_sha256"][name]
        for name in ("agentdocker.exe", "agentd.exe")
    ):
        raise ValueError("native Codex acceptance did not pass on the exact archive binaries")
    if automatic and (
        native.get("source_commit") != info["source_commit"]
        or native.get("scratch_removed") is not True
        or any(native.get(key) != [] for key in ("cleanup_errors", "reader_errors", "forced_processes"))
        or len(native.get("steps", [])) != 22
        or not all(step.get("passed") is True for step in native["steps"])
    ):
        raise ValueError("automatic native Codex acceptance has incomplete source, lifecycle or cleanup evidence")
    for probe in ("front_end_exit", "owner_exit") if automatic else ():
        exited = native.get(probe, {})
        if (not isinstance(exited, dict) or len(exited.get("watched", [])) < 3
                or exited.get("remaining") != []
                or exited.get("capability_revoked") is not True
                or exited.get("receipts_preserved") is not True
                or exited.get("additional_model_requests") != 0):
            raise ValueError(f"automatic native Codex acceptance lacks {probe} evidence")
        target = exited.get("target")
        watched = exited.get("watched", [])
        if (not isinstance(target, dict) or not isinstance(target.get("pid"), int)
                or target["pid"] <= 0 or not isinstance(target.get("birth"), (int, float))
                or not isinstance(exited.get("frontend_exit_code"), int)
                or exited["frontend_exit_code"] == 0
                or (probe == "front_end_exit" and
                    (target["pid"] != exited.get("pid") or target["birth"] != exited.get("birth")))
                or (probe == "owner_exit" and
                    (target["pid"] == exited.get("pid") or target not in watched
                     or {"pid": exited.get("pid"), "birth": exited.get("birth")} not in watched))):
            raise ValueError("automatic native Codex acceptance lacks distinct pinned exit targets")
        host = exited.get("console_host")
        if (not isinstance(host, dict) or not isinstance(host.get("pid"), int)
                or host["pid"] <= 0 or host["pid"] in (exited.get("pid"), target["pid"])
                or host.get("alive_after_cleanup") is not True
                or host.get("resize_after_cleanup") is not True):
            raise ValueError("automatic native Codex exit acceptance did not keep its console open")
    if automatic:
        closed = native.get("console_close", {})
        host = closed.get("console_host", {}) if isinstance(closed, dict) else {}
        watched = closed.get("watched") if isinstance(closed, dict) else None
        if (not isinstance(closed, dict)
                or closed.get("trigger") != "drop_final_conpty_owner"
                or closed.get("pty_references_before_release") != 2
                or closed.get("participants_alive_before_release") is not True
                or closed.get("conpty_owner_released") is not True
                or closed.get("remaining") != []
                or closed.get("capability_revoked") is not True
                or closed.get("receipts_preserved") is not True
                or closed.get("additional_model_requests") != 0
                or not isinstance(host, dict)
                or not isinstance(watched, list) or len(watched) < 5
                or host.get("alive_after_cleanup") is not False
                or not isinstance(closed.get("pid"), int)
                or not isinstance(host.get("pid"), int)
                or closed["pid"] <= 0 or host["pid"] <= 0 or closed["pid"] == host["pid"]
                or {"pid": closed["pid"], "birth": closed.get("birth")} not in watched
                or {"pid": host["pid"], "birth": host.get("birth")} not in watched):
            raise ValueError("automatic native Codex acceptance lacks whole-console closure evidence")


def run_native_trial(command, cwd, report, label):
    """Bound one owned fixture and retain timeout cleanup as a failure."""
    import psutil
    trial = subprocess.Popen(command, cwd=cwd)
    owner = psutil.Process(trial.pid)
    try:
        if trial.wait(timeout=300) != 0:
            raise ValueError(f"{label} acceptance failed; see its retained report")
    except subprocess.TimeoutExpired:
        children = owner.children(recursive=True)
        for process in reversed(children):
            try:
                process.kill()
            except psutil.NoSuchProcess:
                pass
        owner.kill()
        _, alive = psutil.wait_procs([owner, *children], timeout=5)
        report[label + "_timeout_survivors"] = [p.pid for p in alive]
        trial.wait(timeout=5)
        raise


def validate_queue_recovery_report(native, info, mode):
    def require(ok):
        if not ok:
            raise ValueError("queued recovery acceptance lacks exact source, receipts, refusals or cleanup")

    require(mode in ("normal", "holds", "client-reply-loss") and native.get("result") == "passed"
            and native.get("source_commit") == info["source_commit"]
            and native.get("scratch_removed") is True
            and all(native.get(key) == [] for key in ("cleanup_errors", "reader_errors", "forced_processes"))
            and not native.get("fixture_errors")
            and len(native.get("recovery_processes", [])) >= 4
            and native.get("recovery_remaining") == []
            and len(native.get("steps", [])) == 9
            and all(s.get("passed") is True for s in native["steps"])
            and all(native.get("receiver_binary_sha256", {}).get(n) == info["binary_sha256"][n]
                    for n in ("agentdocker.exe", "agentd.exe")))
    proof = native.get("queued_recovery", {})
    require(proof.get("mode") == mode and proof.get("hold_seconds", 0) >= 30)
    expected = (["provider_rate_hold", "project_pause"] if mode == "holds" else
                ["wrong_digest", "wrong_message", "manual_read_is_not_start", "foreign_head"])
    require([r.get("case") for r in proof.get("refusals", [])] == expected)
    before = proof.get("ledger_before", {})
    require(len(before.get("completed", [])) == 2 and isinstance(before.get("attempt"), dict)
            and before["attempt"].get("message") == proof.get("message")
            and bool(before["attempt"].get("queued"))
            and before["attempt"].get("start") is None and before["attempt"].get("receipt") is None)
    for refusal in proof["refusals"]:
        require(refusal.get("reply", {}).get("exit_code") not in (None, 0)
                and refusal.get("ledger_after", {}).get("attempt") == before["attempt"]
                and refusal.get("ledger_after", {}).get("completed") == before["completed"])
    rows = proof.get("completed_after", [])
    require(len(rows) == (2 if mode == "holds" else 3) and rows[:2] == before["completed"])
    if mode != "holds":
        if mode == "normal":
            require(proof.get("response", {}).get("exit_code") == 0)
            first = json.loads(proof["response"]["stderr"])
            require(first.get("already_attempted") is False
                    and proof["response"]["stdout"].strip() == first.get("queued_start"))
        else:
            loss = proof.get("local_client_reply_loss", {})
            intent = loss.get("durable_intent_observed", {})
            request = loss.get("command", {})
            require("response" not in proof and loss.get("response_read_calls") == 0
                    and loss.get("client_closed_after_send") is True
                    and loss.get("closed_after_durable_intent") is True
                    and loss.get("endpoint_kind") == "private resolve named pipe"
                    and loss.get("request_bytes") == len((json.dumps(request) + '\n').encode('utf-8'))
                    and request.get("action") == "start_queued"
                    and bool(request.get("agent"))
                    and request.get("agent") == before.get("binding", {}).get("agent")
                    and request.get("message") == proof["message"]
                    and request.get("provider") == native["descriptor"]["provider"]
                    and intent.get("provider") == request["provider"]
                    and bool(intent.get("id")) and bool(intent.get("note"))
                    and intent.get("note") == request.get("note")
                    and intent.get("queued") == before["attempt"].get("queued")
                    and intent.get("transmission") in ("prepared", "started")
                    and intent.get("confirmation") == request.get("confirmation")
                    and intent.get("confirmation") == json.loads(proof["preview"]["stdout"])["pending"]["start_confirmation"])
            # Correlate the saved intent with the final ordinary receipt. The
            # lost response is deliberately absent from the evidence.
            first = {"queued_start": intent["id"], "message": proof["message"],
                     "turn": rows[-1]["receipt"]["turn"]}
            require(intent.get("turn") in (None, first["turn"]))
        require(first.get("message") == proof.get("message") and bool(first.get("queued_start"))
                and bool(first.get("turn"))
                and rows[-1]["message"] == first["message"] and rows[-1]["receipt"]["turn"] == first["turn"]
                and sum(bool(r.get("recovery")) and not r.get("title") for r in native.get("requests", [])) == 1)
        reply = proof.get("repeat", {})
        if reply.get("exit_code") == 0:
            repeat = json.loads(reply["stderr"])
            require(repeat.get("already_attempted") is True
                    and reply["stdout"].strip() == repeat.get("queued_start")
                    and all(first.get(k) == repeat.get(k) for k in ("message", "queued_start", "turn")))
        else:
            require(proof.get("repeat_disposition") == "already_delivered"
                    and reply.get("exit_code") == 1 and reply.get("stdout") == ""
                    and reply.get("stderr", "").strip() == "Error: no retained native input to start"
                    and proof.get("ledger_after_repeat", {}).get("completed") == rows
                    and "attempt" in proof.get("ledger_after_repeat", {})
                    and proof["ledger_after_repeat"]["attempt"] is None
                    and proof.get("retired_preview", {}).get("exit_code") == 0)
            preview = json.loads(proof["retired_preview"]["stdout"])
            require("pending" in preview and preview["pending"] is None)
    else:
        require("daemon is holding this input" in proof["refusals"][0]["reply"].get("stderr", "")
                and "project is paused" in proof["refusals"][1]["reply"].get("stderr", "")
                and proof.get("paused_preview", {}).get("exit_code") == 0
                and not any(r.get("recovery") for r in native.get("requests", [])))
        require(any(p.get("project") == proof.get("pause", {}).get("pause", {}).get("project")
                    for p in proof.get("pauses_after", {}).get("pauses", [])))
        pending = json.loads(proof["paused_preview"]["stdout"])["pending"]
        original = json.loads(proof["preview"]["stdout"])["pending"]
        require(pending["message"] == proof["message"] and pending["start_intent"] is None
                and pending["start_confirmation"] == original["start_confirmation"])
    users = [v for v in proof.get("final_history", {}).get("data", []) if v.get("item", {}).get("type") == "userMessage"]
    require(len(users) == len(rows))
    for row in rows:
        matching = [v for v in users if v["item"].get("clientId") == row["message"]]
        require(len(matching) == 1)
        item = matching[0]
        require(row["receipt"] == {"thread": native["descriptor"]["provider"]["session"],
                                   "turn": item["turnId"], "item": item["item"]["id"]})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--codex", type=Path,
                        help="actual native Codex executable for private ConPTY/loopback receiver acceptance")
    parser.add_argument("--codex-scenario", choices=("startup", "established", "automatic"), default="startup",
                        help="automatic tests the product-owned launcher; startup retains the strict direct-Codex diagnostic")
    parser.add_argument("--startup-samples", type=int, default=0,
                        help="additional fresh-home samples per Windows ancestry type (0..20)")
    parser.add_argument("--service", action="store_true",
                        help="also test the private home's owned Task Scheduler lifecycle")
    parser.add_argument("--queue-recovery", action="store_true",
                        help="with automatic Codex acceptance, test explicit queued start and daemon holds")
    parser.add_argument("--mcp-forms", action="store_true",
                        help="also exercise managed Codex form decisions and exact receipts")
    parser.add_argument("--mcp-urls", action="store_true",
                        help="also exercise managed Codex website decisions without opening a browser")
    parser.add_argument("--mcp-idle", action="store_true",
                        help="also exercise form and website decisions after the model turn ends")
    parser.add_argument("--managed-secret", action="store_true",
                        help="also test experimental masked input and synthetic ConPTY boundaries")
    args = parser.parse_args()
    if not 0 <= args.startup_samples <= 20:
        parser.error("--startup-samples must be between 0 and 20")
    if args.queue_recovery and (not args.codex or args.codex_scenario != "automatic"):
        parser.error("--queue-recovery requires --codex with --codex-scenario automatic")
    if (args.mcp_forms or args.mcp_urls or args.mcp_idle or args.managed_secret) and not args.codex:
        parser.error("--mcp-forms/--mcp-urls/--mcp-idle/--managed-secret require --codex")
    if os.name != "nt":
        parser.error("the archive acceptance trial requires native Windows")
    build = json.loads(args.native_manifest.read_text(encoding="utf-8"))
    if build.get("target") != "x86_64-pc-windows-msvc":
        parser.error("the Windows preview currently targets x64 MSVC")
    output = args.output.resolve()
    info = PACKAGE.package(PACKAGE.parser().parse_args([
        "--binary-dir", build["binary_directory"], "--output", str(output),
        "--source", build["source_commit"], "--version", build["version"], "--target", build["target"]]),
        expected_build=build)
    report = {"result": "failed", "source_commit": info["source_commit"],
              "source_input_sha256": info["source_input_sha256"],
              "artifacts": info["artifacts"], "binary_sha256": info["binary_sha256"],
              "scope": "Extracted unsigned x64 portable archive; native private-home daemon/CLI/terminal/desktop smoke, not a clean-machine or actual-provider trial"}
    try:
        archive = output / next(iter(info["artifacts"]))
        # Both a space and Unicode in a location outside the source/build tree.
        with tempfile.TemporaryDirectory(prefix="AgentDocker portable ü ") as scratch:
            app = extract_checked(archive, Path(scratch), info)
            # The original staging payload cannot accidentally satisfy sibling lookup.
            shutil.rmtree(output / "AgentDocker")
            subprocess.run([sys.executable, str(ROOT / "scripts/windows_daemon_smoke.py"),
                            "--binary-dir", str(app), "--output", str(output / "smoke"), "--desktop",
                            "--startup-samples", str(args.startup_samples)],
                           cwd=scratch, check=True)
            observed = json.loads((output / "smoke/windows-daemon-smoke.json").read_text(encoding="utf-8"))
            if observed.get("result") != "passed" or observed.get("binary_sha256") != info["binary_sha256"]:
                raise ValueError("native smoke did not pass on the exact archive binaries")
            if args.service:
                subprocess.run([sys.executable, str(ROOT / "scripts/windows_service_smoke.py"),
                                "--binary-dir", str(app), "--output", str(output / "service")],
                               cwd=scratch, check=True, timeout=600)
                service = json.loads((output / "service/result.json").read_text(encoding="utf-8"))
                if service.get("result") != "passed" or any(
                    service.get("binary_sha256", {}).get(name) != info["binary_sha256"][name]
                    for name in ("agentdocker.exe", "agentd.exe")
                ):
                    raise ValueError("service lifecycle did not pass on the exact archive binaries")
                report["service"] = {"result": "passed", "steps": len(service["steps"])}
                subprocess.run([sys.executable, str(ROOT / "scripts/windows_connector_service_smoke.py"),
                                "--binary-dir", str(app), "--output", str(output / "connector-service")],
                               cwd=scratch, check=True, timeout=600)
                connector = json.loads((output / "connector-service/result.json").read_text(encoding="utf-8"))
                if connector.get("result") != "passed" or connector.get("cleanup_errors") or any(
                    connector.get("binary_sha256", {}).get(name) != info["binary_sha256"][name]
                    for name in ("agentdocker.exe", "agentd.exe")
                ):
                    raise ValueError("connector service did not pass on the exact archive binaries")
                report["connector_service"] = {"result": "passed", "steps": len(connector["steps"])}
            subprocess.run([sys.executable, str(ROOT / "scripts/windows_install_smoke.py"),
                            "--binary-dir", str(app), "--output", str(output / "installation"),
                            *(["--service"] if args.service else [])],
                           cwd=scratch, check=True, timeout=900)
            installed = json.loads((output / "installation/result.json").read_text(encoding="utf-8"))
            if installed.get("result") != "passed" or installed.get("binary_sha256") != info["binary_sha256"]:
                raise ValueError("installation trial did not pass on the exact archive binaries")
            report["installation"] = {"result": "passed", "steps": len(installed["steps"])}
            report.update(result="passed", steps=len(observed["steps"]), desktop=observed.get("desktop"))
            if args.codex:
                report["result"] = "failed"
                for mode, enabled, idle in [("form", args.mcp_forms, False), ("url", args.mcp_urls, False),
                                            ("form", args.mcp_idle, True), ("url", args.mcp_idle, True)]:
                    if not enabled:
                        continue
                    destination = output / ("mcp-" + ("idle-" if idle else "") + mode + "s")
                    key = "mcp_" + ("idle_" if idle else "") + mode + "s"
                    run_native_trial([sys.executable, str(ROOT / "scripts/windows_mcp_form_smoke.py"),
                                      "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                      "--mode", mode, *(["--idle"] if idle else []),
                                      "--output", str(destination)], scratch, report, key)
                    reviewed = json.loads((destination / "result.json").read_text(encoding="utf-8"))
                    validate_mcp_review_report(reviewed, info, mode, idle)
                    report[key] = reviewed
                if args.managed_secret:
                    destination = output / "managed-secret"
                    run_native_trial([sys.executable, str(ROOT / "scripts/windows_managed_secret_smoke.py"),
                                      "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                      "--output", str(destination)], scratch, report, "managed_secret")
                    secret = json.loads((destination / "result.json").read_text(encoding="utf-8"))
                    validate_secret_report(secret, info, PACKAGE.sha256(args.codex))
                    report["managed_secret"] = secret
                automatic = args.codex_scenario == "automatic"
                driver = "windows_native_launcher_smoke.py" if automatic else "windows_native_codex_smoke.py"
                run_native_trial([sys.executable, str(ROOT / "scripts" / driver),
                                          "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                          *([] if automatic else ["--scenario", args.codex_scenario]),
                                          "--output", str(output / "native-codex")], scratch, report, "native_codex")
                native = json.loads((output / "native-codex/result.json").read_text(encoding="utf-8"))
                validate_native_report(native, info, args.codex_scenario)
                if args.queue_recovery:
                    report["queued_recovery"] = {}
                    for mode in ("normal", "holds", "client-reply-loss"):
                        destination = output / ("queued-recovery-" + mode)
                        run_native_trial([sys.executable, str(ROOT / "scripts/windows_native_launcher_smoke.py"),
                                          "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                          "--queued-recovery", mode, "--output", str(destination)],
                                         scratch, report, "queued_recovery_" + mode)
                        observed_recovery = json.loads((destination / "result.json").read_text(encoding="utf-8"))
                        validate_queue_recovery_report(observed_recovery, info, mode)
                        report["queued_recovery"][mode] = observed_recovery
                report.update(result="passed", native_codex=native, native_codex_scenario=args.codex_scenario)
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (output / "package-acceptance.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
