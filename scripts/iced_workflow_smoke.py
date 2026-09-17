#!/usr/bin/env python3
"""Exercise production Iced controls in owned fixture state, then reopen the app.

Scenarios invoke callbacks collected from the actual rendered controls. This checks
native rendering and action wiring; it is not physical keyboard or screen-reader testing.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import socket
import subprocess
import sys
import tempfile
import time
from desktop_smoke import bounded_output, stop, wait_window


def finish_smoke(window, daemon, endpoint, report, output, started, primary_error):
    """Attempt all cleanup and reporting without hiding a workflow failure."""
    errors = []

    def retain(stage, error):
        errors.append({"stage": stage, "error": bounded_output(str(error))})
        report["cleanup_errors"] = errors
        report["result"] = "failed"

    try:
        stop(window)
    except Exception as error:
        retain("window cleanup", error)
    try:
        if daemon is not None and daemon.poll() is None:
            try:
                rpc(endpoint, {"op": "shutdown"})
                daemon.wait(timeout=10)
            except Exception as error:
                retain("daemon shutdown", error)
                stop(daemon)
    except Exception as error:
        retain("daemon cleanup", error)
    report["elapsed_seconds"] = time.monotonic() - started
    try:
        (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    except Exception as error:
        retain("result report", error)
    if errors:
        detail = "native workflow cleanup failed: " + json.dumps(errors)
        if primary_error is None:
            raise RuntimeError(detail)
        try:
            print(detail, file=sys.stderr)
        except Exception:
            # A broken diagnostic stream must not replace the workflow error.
            pass


def rpc(endpoint, request, timeout=5):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(timeout)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(request).encode() + b"\n")
        with stream.makefile("rb") as response:
            result = json.loads(response.readline(1024 * 1024))
        if result.get("type") == "error":
            raise RuntimeError(result)
        return result


def until(fn, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = fn()
        if result:
            return result
        time.sleep(0.1)
    raise TimeoutError("fixture condition did not become true")


def step(op, **fields):
    return {"op": op, **fields}


def measure_idle(binary_dir, env, cwd, daemon, output):
    """Sample an ordinary window after warmup; fixture capture polling is disabled."""
    window = None
    samples = []
    with (output / "idle-window.log").open("w") as log:
        try:
            window = subprocess.Popen([str(binary_dir / "agentdocker-ui")], cwd=cwd, env=env,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            time.sleep(3)
            for _ in range(12):
                window_exit, daemon_exit = window.poll(), daemon.poll()
                if window_exit is not None or daemon_exit is not None:
                    raise RuntimeError(f"idle measurement process exited: window={window_exit}, daemon={daemon_exit}, completed_samples={len(samples)}")
                sample = {}
                for name, process in [("window", window), ("daemon", daemon)]:
                    row = subprocess.check_output(["ps", "-p", str(process.pid), "-o", "rss=,pcpu="], text=True, timeout=5).split()
                    sample[name] = {"rss_bytes": int(row[0]) * 1024, "ps_cpu_percent": float(row[1])}
                samples.append(sample)
                time.sleep(1)
        finally:
            stop(window)
    return {"method": "12 ps RSS/CPU samples one second apart after 3s warmup; ordinary window without smoke polling; CPU is ps-reported, not a battery or power measurement",
            "samples": samples,
            "summary": {name: {"median_rss_bytes": statistics.median(s[name]["rss_bytes"] for s in samples),
                               "maximum_rss_bytes": max(s[name]["rss_bytes"] for s in samples),
                               "median_ps_cpu_percent": statistics.median(s[name]["ps_cpu_percent"] for s in samples)}
                        for name in ("window", "daemon")}}


def smoke(binary_dir, output, *, skip_idle_measurement=False):
    binary_dir = binary_dir.resolve(strict=True)
    output = output.absolute()
    output.mkdir(mode=0o700)
    started = time.monotonic()
    daemon = window = None
    checks = []
    report = {"result": "failed", "checks": checks, "scope": __doc__,
              "os": platform.platform(), "architecture": platform.machine(),
              "binary_sha256": {name: hashlib.sha256((binary_dir / name).read_bytes()).hexdigest()
                                for name in ("agentd", "agentdocker", "agentdocker-ui")}}
    with tempfile.TemporaryDirectory(prefix="ad-iced-", dir="/tmp") as scratch:
        root = Path(scratch).resolve()
        project, pinned, home = root / "discovered", root / "pinned-api", root / "home"
        state, endpoint = root / "state", root / "d.sock"
        tools = root / "bin"
        for path in [project, pinned, home, tools]:
            path.mkdir(mode=0o700)
        fixture = root / "terminal.py"
        fixture.write_text('import sys, time\nprint("\\033[34mICED TERMINAL READY λ 日本語\\033[0m", flush=True)\nfor line in sys.stdin:\n print("ECHO:" + line, flush=True)\n')
        runtime = tools / "codex"
        runtime.write_text('#!/bin/sh\nif [ "$1" = "--version" ]; then echo "codex fixture 1"; exit 0; fi\nexec ' + shutil.which("python3") + ' -u ' + str(fixture) + '\n')
        runtime.chmod(0o700)
        claude_runtime = tools / "claude"
        claude_runtime.write_text('#!/bin/sh\necho "Claude Code fixture 1"\n')
        claude_runtime.chmod(0o700)
        env = {**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
               "CODEX_HOME": str(home / ".codex"), "CLAUDE_CONFIG_DIR": str(home / ".claude"),
               "PATH": f"{tools}:/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
               "AGENTDOCKER_HOME": str(state), "AGENTDOCKER_SOCKET": str(endpoint),
               "AGENTDOCKER_NO_AUTOSTART": "1", "AGENTDOCKER_NO_NOTIFICATIONS": "1", "RUST_LOG": "warn"}
        try:
            with (output / "daemon.log").open("w") as log:
                daemon = subprocess.Popen([str(binary_dir / "agentd")], cwd=project, env=env,
                                          stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            def ready():
                if daemon.poll() is not None:
                    raise RuntimeError("fixture daemon exited")
                try:
                    return rpc(endpoint, {"op": "ping"}).get("type") == "pong"
                except OSError:
                    return False
            until(ready)
            previous = rpc(endpoint, {"op": "register", "spec": {"name": "terminal-fixture", "runtime": "fixture",
                           "workdir": str(project)}})["agent"]
            rpc(endpoint, {"op": "deregister", "agent": previous["id"]})
            agent = rpc(endpoint, {"op": "run", "spec": {"name": "terminal-fixture", "runtime": "fixture",
                         "command": [sys.executable, "-u", str(fixture)], "workdir": str(project),
                         "tty": True, "restore": False}})["agent"]
            human = rpc(endpoint, {"op": "me", "workdir": str(project)})["agent"]
            dismissible = [rpc(endpoint, {"op": "send", "from": agent["id"], "to": human["id"],
                            "kind": "chat", "payload": {"text": text}})["message"]
                           for text in [f"Received message {index}" for index in range(35)]]
            room = rpc(endpoint, {"op": "channel_open", "agent": agent["id"], "task": "Fixture coordination", "members": [human["id"]]})["channel"]
            with ThreadPoolExecutor(max_workers=1) as pool:
                answer = pool.submit(rpc, endpoint, {"op": "ask", "from": agent["id"], "to": human["id"],
                                     "question": "Use the fixture API?", "timeout_secs": 150}, 160)
                question = until(lambda: rpc(endpoint, {"op": "questions", "agent": human["id"]}).get("questions"))[0]
                reviews = []
                for decision in ["Allow", "Deny"]:
                    presentation = {"kind": "codex_command", "command": "printf fixture", "cwd": str(project),
                                    "reason": (f"Exercise the {decision} control\n\n"
                                               "Requested connection: example.com (https)\n\n"
                                               "Additional access for this command:\n"
                                               "Read: /fixture/input\nWrite: /fixture/output\nExclude: /fixture/private\n\n"
                                               "Deny cancels this Codex request.")}
                    fallback = ("Allow Codex to run this command once?\n\nDirectory: " + presentation["cwd"] +
                                "\nCommand:\n" + presentation["command"] + "\n\nReason: " + presentation["reason"] +
                                "\n\nReply Allow or Deny.")
                    created = rpc(endpoint, {"op": "post_question", "from": agent["id"], "to": human["id"],
                                             "question": fallback, "presentation": presentation, "timeout_secs": 150})
                    reviews.append((created["message"], decision))
                presentation = {"kind": "choices", "question": "Which fixture route?",
                                "options": [{"label": "Blue", "description": "Use the blue route"},
                                            {"label": "Green", "description": "Use the green route"}]}
                fallback = presentation["question"] + "".join(f"\n- {o['label']}: {o['description']}" for o in presentation["options"])
                created = rpc(endpoint, {"op": "post_question", "from": agent["id"], "to": human["id"],
                                         "question": fallback, "presentation": presentation, "timeout_secs": 150})
                reviews.append((created["message"], "Blue"))
                allow, deny, choice = (item[0] for item in reviews)
                review_steps = [step("fill", id=f"answer-{choice}", text="Keep this draft until I choose"),
                                step("wait_text", text="Requested connection: example.com (https)"),
                                step("wait_text", text="Read: /fixture/input"),
                                step("wait_text", text="Write: /fixture/output"),
                                step("wait_text", text="Exclude: /fixture/private"),
                                step("wait_text", text="Deny cancels this Codex request."),
                                step("wait_control", id=f"answer-{allow}", present=False),
                                step("focus", id=f"answer-allow-{allow}"), step("wait_focus", id=f"answer-allow-{allow}"),
                                step("capture", name="command-approval"), step("click", id=f"answer-allow-{allow}"),
                                step("wait_control", id=f"answer-allow-{allow}", present=False),
                                step("wait_text", text="Keep this draft until I choose"), step("click", id=f"answer-deny-{deny}"),
                                step("wait_control", id=f"answer-deny-{deny}", present=False),
                                step("wait_text", text="Keep this draft until I choose"), step("capture", name="question-choices"),
                                step("click", id=f"answer-choice-{choice}-0"), step("wait_control", id=f"answer-choice-{choice}-0", present=False)]
                steps = [step("click", id=f"project-{project}"), step("wait_text", text="terminal-fixture"), step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("wait_control", id=f"session-{previous['id']}", present=False), step("capture", name="projects-live"),
                         step("click", id="pause-project"),
                         step("fill", id="pause-reason", text="Fixture pause · preserve this reason 日本語"),
                         step("capture", name="pause-draft"),
                         step("click", id="pause-submit"),
                         step("wait_control", id="resume-project", present=True),
                         step("wait_text", text="Paused · Fixture pause"),
                         step("capture", name="project-paused"),
                         step("click", id="resume-project"),
                         step("wait_control", id="pause-project", present=True),
                         # The uppercase eyebrow is a separate rendered heading:
                         # seeing the mixed-case sidebar label alone cannot pass.
                         step("click", id=f"project-menu-{project}"), step("click", id=f"project-rename-start-{project}"),
                         step("fill", id=f"project-rename-{project}", text="Renamed Project"), step("click", id=f"project-rename-save-{project}"),
                         step("click", id="projects"), step("wait_text", text="RENAMED PROJECT"), step("capture", name="renamed-all-projects"),
                         step("click", id=f"project-{project}"),
                         step("click", id=f"project-menu-{project}"), step("click", id=f"project-rename-start-{project}"),
                         step("fill", id=f"project-rename-{project}", text=""), step("click", id=f"project-rename-save-{project}"),
                         # Ended sessions are one collapsed group under the current ones,
                         # not a tab: opening it shows the previous run beside the live one.
                         step("wait_control", id=f"session-{previous['id']}", present=False),
                         step("click", id="sessions-earlier"), step("wait_control", id=f"session-{previous['id']}", present=True),
                         step("wait_control", id=f"session-{agent['id']}", present=True), step("capture", name="session-earlier"),
                         step("click", id="sessions-earlier"), step("wait_control", id=f"session-{previous['id']}", present=False),
                         # A search matching only an ended session opens Earlier without
                         # also claiming that no sessions match. A missing term still does.
                         step("fill", id="session-search", text=previous["id"]),
                         step("wait_control", id=f"session-{previous['id']}", present=True),
                         step("wait_control", id=f"session-{agent['id']}", present=False),
                         step("wait_text_absent", text="No matching sessions"),
                         step("capture", name="earlier-search-match"),
                         step("fill", id="session-search", text="fixture-no-session-matches"),
                         step("wait_control", id=f"session-{previous['id']}", present=False),
                         step("wait_text", text="No matching sessions"),
                         step("fill", id="session-search", text=""),
                         step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("click", id="sessions-attention"), step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("click", id="sessions-current"),
                         # Messages: the direct conversation with the fixture holds its
                         # question cards and every message it sent; opening it reads
                         # them, so nothing is cleared by hand.
                         step("click", id="inbox"), step("click", id=f"thread-{agent['id']}"),
                         step("wait_text", text="Received message 34"),
                         step("fill", id=f"answer-{question['id']}", text="Use API v2"),
                         step("wait_text", text="Use API v2"),
                         step("capture", name="inbox-draft"), step("click", id="connections"), step("click", id="inbox"),
                         step("wait_text", text="Use API v2"), step("click", id=f"send-answer-{question['id']}"),
                         step("wait_text", text="answered"), *review_steps, step("click", id="projects"),
                         step("click", id=f"session-{agent['id']}"), step("resize", width=720, height=540), step("wait_control", id="attach-session", present=True),
                         step("capture", name="compact-session"), step("click", id="close-session"),
                         step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("click", id=f"session-{agent['id']}"), step("resize", width=1180, height=760),
                         step("click", id="session-message"), step("fill", id="session-message-text", text="Direct user queue input"),
                         step("click", id="close-session"), step("click", id=f"session-{agent['id']}"),
                         step("click", id="session-message"), step("wait_text", text="Direct user queue input"),
                         step("click", id="send-session-message"), step("wait_text", text="Message saved to queue"),
                         step("capture", name="direct-message-queued"),
                         step("click", id="attach-session"),
                         step("wait_text", text="ICED TERMINAL READY λ 日本語"), step("capture", name="terminal"),
                         step("focus", id="detach-terminal"), step("wait_focus", id="detach-terminal"), step("click", id="detach-terminal"),
                         step("click", id="project-more"), step("click", id="project-tab-Channels"), step("click", id=f"reply-channel-{room['id']}"),
                         step("fill", id="channel-message", text="Fixture channel message"), step("click", id="send-channel"),
                         step("wait_text", text="Message sent"), step("wait_text", text="Fixture channel message"), step("capture", name="channels"),
                         step("click", id="project-tab-Journal"), step("capture", name="activity"),
                         step("click", id="project-more"), step("click", id="project-tab-Channels"), step("wait_text", text="Fixture channel message"),
                         step("click", id="project-more"), step("click", id="project-tab-Leases"), step("capture", name="coordination"),
                         step("click", id="connections"), step("capture", name="connections"),
                         step("click", id="connection-details-codex"), step("wait_text", text="Tools (MCP)"),
                         step("click", id="setup-review-codex"),
                         step("wait_text", text="Connect Codex"), step("capture", name="setup-review"),
                         step("click", id="apply-setup"), step("wait_text", text="Codex setup saved"),
                         step("click", id="undo-setup"), step("wait_text", text="Codex setup undone"), step("click", id="close-setup"),
                         step("click", id="add-project"), step("fill", id="project-path", text=str(pinned)),
                         step("click", id="pin-folder"), step("wait_text", text="pinned-api"), step("capture", name="pinned-empty-project"),
                         # The row's own menu renames the entry here (nothing on disk) and
                         # an empty name goes back to the folder's.
                         step("click", id=f"project-menu-{pinned}"), step("wait_control", id=f"project-remove-{pinned}", present=True),
                         step("click", id=f"project-rename-start-{pinned}"), step("wait_control", id=f"project-rename-{pinned}", present=True),
                         step("fill", id=f"project-rename-{pinned}", text="Pinned API"),
                         step("click", id=f"project-rename-save-{pinned}"), step("wait_text", text="Pinned API"),
                         step("wait_control", id=f"project-remove-{pinned}", present=False), step("capture", name="project-renamed"),
                         step("click", id=f"project-menu-{pinned}"), step("wait_control", id=f"project-rename-start-{pinned}", present=True),
                         step("click", id=f"project-rename-start-{pinned}"), step("wait_control", id=f"project-rename-{pinned}", present=True),
                         step("fill", id=f"project-rename-{pinned}", text=""), step("click", id=f"project-rename-save-{pinned}"),
                         step("wait_control", id=f"project-rename-save-{pinned}", present=False), step("wait_text", text="pinned-api"),
                         step("click", id="launch-agent"), step("click", id="launch-tool-codex"),
                         step("wait_text", text="Idle messages: On"), step("click", id="launch-idle-input"),
                         step("wait_text", text="Idle messages: Off"),
                         # This fake CLI is a terminal fixture, not a provider input server.
                         step("fill", id="launch-name", text="launched-from-iced"), step("click", id="confirm-launch"),
                         step("wait_text", text="Agent launched"), step("click", id="attach-session"),
                         step("wait_text", text="ICED TERMINAL READY λ 日本語"), step("capture", name="launched-terminal"), step("click", id="detach-terminal"),
                         step("click", id="stop-session"), step("wait_text", text="Confirm stop"), step("click", id="stop-session"),
                         step("click", id="project-more"), step("click", id="project-tab-Console"), step("fill", id="console-command", text="ps --all"),
                         step("click", id="run-command"), step("wait_text", text="launched-from-iced"), step("capture", name="commands"),
                         step("click", id="settings"), step("click", id="dark-theme"), step("capture", name="settings-dark"),
                         step("resize", width=720, height=540), step("click", id="larger-ui"), step("click", id="larger-ui"),
                         step("focus", id="installation"), step("wait_focus", id="installation"), step("capture", name="compact-zoom-focus"),
                         step("click", id="installation"), step("capture", name="installation"), step("click", id="projects"),
                         step("wait_text", text="All projects"), step("click", id=f"project-{pinned}"),
                         step("pause", millis=400)]
                def launch(name, scenario):
                    nonlocal window
                    script = root / f"{name}.json"
                    script.write_text(json.dumps(scenario))
                    capture = output / name
                    with (output / f"{name}.log").open("w") as log:
                        window = subprocess.Popen([str(binary_dir / "agentdocker-ui"), "--smoke-test", str(capture),
                                                   "--smoke-scenario", str(script), "--smoke-deadline", "150"],
                                                  cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
                        observation = wait_window(daemon, window, capture, timeout=180)
                    result = json.loads((capture / "result.json").read_text())
                    assert result["scenario_steps_completed"] == len(scenario), result
                    return observation
                if platform.system() == "Darwin":
                    steps.insert(2, step("native_accessibility"))
                try:
                    report["first_window"] = launch("workflows", steps)
                    checks.append("ended_session_search_shows_earlier_match_without_false_empty_state_and_keeps_genuine_no_match_state")
                    assert answer.result(timeout=5).get("text") == "Use API v2", "answer did not reach asking agent"
                    agent_inbox = rpc(endpoint, {"op": "inbox", "agent": agent["id"], "drain": False})["messages"]
                    for question_id, decision in reviews:
                        answers = [m for m in agent_inbox if m.get("reply_to") == question_id]
                        assert len(answers) == 1 and answers[0]["from"] == human["id"], answers
                        assert answers[0]["payload"] == {"text": decision}, answers
                    checks.append("structured_command_and_choice_controls_use_exact_shared_answer_routes_and_preserve_other_drafts")
                    direct = [message for message in agent_inbox if message["payload"] == "Direct user queue input"]
                    assert len(direct) == 1 and direct[0]["from"] == human["id"], direct
                    assert direct[0]["to"] == {"kind": "agent", "value": agent["id"]}, direct
                    checks.append("direct_human_input_uses_peer_inbox_queue_and_preserves_draft_across_navigation")
                    remaining = rpc(endpoint, {"op": "inbox", "agent": human["id"], "drain": False})["messages"]
                    retained = {message["id"] for message in remaining}
                    # Opening the conversation read it: every row of it left the
                    # person's queue, and nothing else did.
                    assert not set(dismissible) & retained, "reading the conversation left rows queued"
                    conversations = rpc(endpoint, {"op": "conversations", "project": str(project)})["conversations"]
                    with_agent = next(c for c in conversations if c["kind"] == "dm" and agent["id"] in c["conversation"])
                    assert with_agent["unread"] == 0, with_agent
                    checks.append("reading_a_conversation_acknowledges_its_rows_and_clears_its_unread_count")
                except BaseException:
                    stop(window)
                    stop(daemon)
                    raise
            checks.extend(["rendered_actions", "answer_and_draft_navigation", "native_vt_rendering", "terminal_attach_detach", "channels", "setup_review_apply_undo", "launch_and_confirmed_stop", "focused_control_reveal", "compact_zoom", "current_history_separation", "project_attention", "compact_session_navigation"])
            def narrow_inbox():
                nonlocal window
                # Narrow Inbox: below the two-column breakpoint the window shows the
                # conversation list or one conversation, never both. Choosing one,
                # the back control, a native notification route, a draft kept across
                # switching and resizing, and wide-to-narrow-and-back are exercised
                # against the rendered controls; the route is forwarded to the
                # running window mid-scenario exactly as a click on a notification
                # would forward it.
                card = rpc(endpoint, {"op": "task_create", "from": "user", "project": str(project), "title": "Fix the fixture login",
                                      "acceptance": "Login works with SSO and a password", "column": "ready"})["task"]
                narrow = rpc(endpoint, {"op": "run", "spec": {"name": "narrow-fixture", "runtime": "fixture",
                              "command": ["/bin/sleep", "150"], "workdir": str(project), "restore": False}})["agent"]
                routed = rpc(endpoint, {"op": "send", "from": narrow["id"], "to": human["id"],
                                        "kind": "chat", "payload": {"text": "NARROW ROUTE TARGET"}})["message"]
                route = {"home": str(state), "socket": str(endpoint),
                         "target": {"message": routed, "agent": narrow["id"], "project": room["project"], "channel": None}}
                narrow_steps = [step("resize", width=720, height=540),
                                step("click", id=f"project-{project}"), step("click", id="pause-project"),
                                step("fill", id="pause-reason", text="Narrow pause draft 日本語"),
                                step("capture", name="narrow-pause-draft"), step("click", id="pause-cancel"),
                                step("click", id="inbox"),
                                step("wait_control", id=f"thread-{agent['id']}", present=True),
                                step("wait_control", id="thread-back", present=False), step("capture", name="narrow-inbox-list"),
                                step("click", id=f"thread-{agent['id']}"), step("wait_control", id="thread-back", present=True),
                                step("wait_control", id=f"thread-{agent['id']}", present=False),
                                step("wait_control", id=f"reply-{agent['id']}", present=True), step("capture", name="narrow-inbox-thread"),
                                step("fill", id=f"reply-{agent['id']}", text="Keep this narrow draft"),
                                step("click", id="thread-back"), step("wait_control", id="thread-back", present=False),
                                step("wait_control", id=f"thread-{agent['id']}", present=True),
                                step("click", id=f"thread-{narrow['id']}"), step("wait_control", id=f"reply-{narrow['id']}", present=True),
                                step("wait_control", id=f"reply-{agent['id']}", present=False),
                                step("click", id="thread-back"), step("click", id=f"thread-{agent['id']}"),
                                step("wait_text", text="Keep this narrow draft"),
                                step("resize", width=1180, height=760), step("wait_control", id="thread-back", present=False),
                                step("wait_control", id=f"thread-{agent['id']}", present=True),
                                step("wait_control", id=f"reply-{agent['id']}", present=True), step("wait_text", text="Keep this narrow draft"),
                                step("capture", name="wide-inbox-both-columns"),
                                # Enter sends: the composer's own action is the send, driven here
                                # as a click on the input, and the words arrive in the pane.
                                step("click", id=f"thread-{narrow['id']}"), step("wait_control", id=f"reply-{narrow['id']}", present=True),
                                step("fill", id=f"reply-{narrow['id']}", text="Sent with Enter"), step("click", id=f"reply-{narrow['id']}"),
                                step("wait_text", text="Sent with Enter"),
                                # `@` offers who is here; a pick finishes the name.
                                step("fill", id=f"reply-{narrow['id']}", text="ask @term"),
                                step("wait_control", id=f"mention-{agent['id']}", present=False),
                                step("fill", id=f"reply-{narrow['id']}", text="ask @narr"),
                                step("wait_control", id=f"mention-{narrow['id']}", present=True), step("click", id=f"mention-{narrow['id']}"),
                                step("wait_text", text="ask @narrow-fixture "), step("wait_control", id=f"mention-{narrow['id']}", present=False),
                                step("fill", id=f"reply-{narrow['id']}", text=""),
                                # A new channel from the sidebar: name, purpose, members, and it opens.
                                step("click", id="new-conversation"), step("wait_control", id="new-kind-channel", present=True),
                                step("click", id="new-kind-channel"), step("wait_control", id="new-channel-name", present=True),
                                step("fill", id="new-channel-name", text="Planning Room"), step("fill", id="new-channel-purpose", text="Plan the fixture"),
                                step("click", id=f"new-member-{narrow['id']}"), step("capture", name="new-channel-form"),
                                step("click", id="new-channel-create"), step("wait_text", text="#planning-room"),
                                step("wait_control", id="new-channel-create", present=False),
                                step("wait_control", id="invite-channel", present=True), step("click", id="invite-channel"),
                                step("wait_control", id=f"invite-member-{agent['id']}", present=True),
                                step("wait_control", id=f"invite-member-{narrow['id']}", present=False),
                                step("click", id=f"invite-member-{agent['id']}"),
                                step("wait_control", id=f"invite-member-{agent['id']}", present=False),
                                step("capture", name="channel-member-added"), step("click", id="new-conversation"),
                                # A new direct message is one pick.
                                step("click", id="new-conversation"), step("click", id="new-kind-direct"),
                                step("wait_control", id=f"new-direct-{agent['id']}", present=True), step("capture", name="new-direct-form"),
                                step("click", id=f"new-direct-{agent['id']}"), step("wait_control", id=f"reply-{agent['id']}", present=True),
                                step("wait_control", id=f"new-direct-{agent['id']}", present=False),
                                step("wait_text", text="Keep this narrow draft"),
                                step("resize", width=720, height=540), step("wait_control", id="thread-back", present=True),
                                step("wait_text", text="Keep this narrow draft"), step("click", id="thread-back"),
                                step("wait_control", id="thread-back", present=False), step("click", id="projects")]
                narrow_gate = len(narrow_steps)
                narrow_steps += [step("wait_control", id=f"reply-{narrow['id']}", present=True), step("wait_control", id="thread-back", present=True),
                                 step("wait_control", id=f"thread-{agent['id']}", present=False), step("wait_text", text="NARROW ROUTE TARGET"),
                                 step("capture", name="narrow-notification-route"),
                                 # A thread takes the narrow window with its own composer and
                                 # its own draft; closing it returns to the conversation.
                                 step("click", id=f"thread-{routed}"), step("wait_control", id="close-thread", present=True),
                                 step("wait_control", id=f"reply-thread-{routed}", present=True),
                                 step("wait_control", id=f"reply-{narrow['id']}", present=False),
                                 step("fill", id=f"reply-thread-{routed}", text="Only in the thread"),
                                 step("capture", name="narrow-thread"),
                                 step("click", id="close-thread"), step("wait_control", id=f"reply-{narrow['id']}", present=True),
                                 step("wait_control", id=f"reply-thread-{routed}", present=False),
                                 # The thread's draft is its own and survives closing and reopening it.
                                 step("click", id=f"thread-{routed}"), step("wait_control", id=f"reply-thread-{routed}", present=True),
                                 step("wait_text", text="Only in the thread"), step("click", id="close-thread"),
                                 step("wait_control", id=f"reply-thread-{routed}", present=False),
                                 step("click", id="thread-back"), step("click", id=f"thread-{agent['id']}"),
                                 step("wait_text", text="Keep this narrow draft"), step("capture", name="narrow-draft-kept"),
                                 # The board: the person files a card as Ready, it is for the
                                 # taking; the fixture agent pulls it on the daemon (the gated
                                 # action below) and the card shows its holder; the person
                                 # opens it, reads what done means, moves it on and archives it.
                                 step("resize", width=1180, height=760), step("click", id="projects"),
                                 step("click", id=f"project-{project}"), step("click", id="project-tab-Board"),
                                 step("wait_control", id="task-title", present=True),
                                 step("wait_control", id=f"task-{card['id']}", present=True),
                                 step("fill", id="task-title", text="Write the fixture notes"),
                                 step("fill", id="task-acceptance", text="Notes cover the fixture routes"),
                                 step("click", id="task-file-backlog"), step("wait_text", text="Write the fixture notes"),
                                 step("click", id=f"task-{card['id']}"), step("wait_text", text="Login works with SSO and a password"),
                                 step("wait_text", text="for the taking"), step("capture", name="board-ready")]
                board_gate = len(narrow_steps)
                narrow_steps += [step("wait_text_absent", text="for the taking"), step("wait_text", text="narrow-fixture"),
                                 step("capture", name="board-pulled"),
                                 # Back is offered in every column but Backlog, so the move to
                                 # Review is awaited by the next step's label changing to Done.
                                 step("click", id=f"task-next-{card['id']}"), step("wait_text", text="Done \u203a"),
                                 step("click", id=f"task-next-{card['id']}"), step("wait_control", id=f"task-next-{card['id']}", present=False),
                                 step("capture", name="board-done"),
                                 step("click", id=f"task-archive-{card['id']}"), step("wait_control", id=f"task-{card['id']}", present=False)]
                def pull_card():
                    cards = {c["id"]: c for c in rpc(endpoint, {"op": "tasks", "project": str(project)})["tasks"]}
                    filed = [c for c in cards.values() if c["title"] == "Write the fixture notes"]
                    assert len(filed) == 1 and filed[0]["column"] == "backlog" and filed[0]["acceptance"] == "Notes cover the fixture routes", cards
                    assert cards[card["id"]]["column"] == "ready", cards
                    pulled = rpc(endpoint, {"op": "task_pull", "agent": narrow["id"], "task": card["id"]})
                    assert pulled["type"] == "task" and pulled["task"]["assignee"] == narrow["id"] and pulled["task"]["column"] == "in_progress", pulled
                    # The pull holds the card as a task:<id> lease with its title as the note.
                    held = rpc(endpoint, {"op": "leases", "resource": f"task:{card['id']}"})["leases"]
                    assert [(l["holder"], l["note"]) for l in held] == [(narrow["id"], card["title"])], held
                    try:
                        rpc(endpoint, {"op": "task_pull", "agent": agent["id"], "task": card["id"]})
                    except RuntimeError as refused:
                        again = refused.args[0]
                        assert again["code"] == "conflict" and again["details"]["assignee"] == narrow["id"], again
                    else:
                        raise AssertionError("a second pull of a held card was accepted")
                    return card["id"]
                def forward_route(name, gate):
                    def progressed():
                        if window is not None and window.poll() is not None:
                            raise RuntimeError("narrow window exited before the notification route")
                        try:
                            return json.loads((output / name / "progress.json").read_text())["completed"] >= gate
                        except (FileNotFoundError, json.JSONDecodeError):
                            return False
                    until(progressed, timeout=120)
                    primary = window.pid
                    result = subprocess.run([str(binary_dir / "agentdocker-ui"), "--open-notification", json.dumps(route)],
                                            cwd=project, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=8)
                    assert result.returncode == 0, result.stderr
                    assert window.poll() is None and window.pid == primary, "narrow window was replaced by the route"
                def when_reached(name, gate, act):
                    def progressed():
                        if window is not None and window.poll() is not None:
                            raise RuntimeError(f"narrow window exited before step {gate}")
                        try:
                            return json.loads((output / name / "progress.json").read_text())["completed"] >= gate
                        except (FileNotFoundError, json.JSONDecodeError):
                            return False
                    until(progressed, timeout=150)
                    return act()
                def launch_routed(name, scenario, gate):
                    nonlocal window
                    script = root / f"{name}.json"
                    script.write_text(json.dumps(scenario))
                    capture = output / name
                    with (output / f"{name}.log").open("w") as log:
                        window = subprocess.Popen([str(binary_dir / "agentdocker-ui"), "--smoke-test", str(capture),
                                                   "--smoke-scenario", str(script), "--smoke-deadline", "150"],
                                                  cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
                        with ThreadPoolExecutor(max_workers=1) as pool:
                            forwarded = pool.submit(lambda: (forward_route(name, gate), when_reached(name, board_gate, pull_card))[1])
                            observation = wait_window(daemon, window, capture, timeout=200)
                            report["board_card"] = forwarded.result(timeout=5)
                    result = json.loads((capture / "result.json").read_text())
                    assert result["scenario_steps_completed"] == len(scenario), result
                    return observation
                report["narrow_inbox_window"] = launch_routed("narrow-inbox", narrow_steps, narrow_gate)
                checks.append("narrow_inbox_shows_list_or_one_conversation_and_routes_notifications_and_keeps_drafts_across_switch_and_resize")
                board = {c["id"]: c for c in rpc(endpoint, {"op": "tasks", "project": str(project), "archived": True})["tasks"]}
                assert board[card["id"]]["column"] == "done" and board[card["id"]]["assignee"] == narrow["id"] and board[card["id"]].get("archived_at"), board
                assert any(c["title"] == "Write the fixture notes" and c["column"] == "backlog" and not c.get("archived_at") for c in board.values()), board
                checks.append("a_card_filed_in_the_window_was_pulled_once_by_an_agent_shown_with_its_holder_moved_on_by_the_person_and_archived")
                # What the window did reached the daemon: the words sent with
                # Enter are archived, and the room opened from the sidebar has
                # the person, the picked member and the later invited agent.
                sent = rpc(endpoint, {"op": "history", "conversation": f"dm:{min(human['id'], narrow['id'])}:{max(human['id'], narrow['id'])}",
                                      "limit": 50})["messages"]
                assert any("Sent with Enter" in json.dumps(m) for m in sent), sent
                opened = [c for c in rpc(endpoint, {"op": "channels", "project": str(project)})["channels"] if c.get("name") == "planning-room"]
                assert len(opened) == 1 and set(opened[0]["members"]) == {human["id"], narrow["id"], agent["id"]}, opened
                invited = rpc(endpoint, {"op": "peek_input", "agent": agent["id"]})["messages"]
                notices = [m for m in invited if m.get("to") == {"kind": "channel", "value": opened[0]["id"]}
                           and "added terminal-fixture to this channel" in json.dumps(m.get("payload"))]
                assert len(notices) == 1, notices
                checks.append("enter_sends_and_sidebar_channel_creation_and_invitation_reach_the_exact_members")
                # The largest saved columns must not crush the conversation.
                # Change only this private profile while its window is closed.
                catalog_path = state / "workspace.json"
                saved_catalog = catalog_path.read_text()
                pane_catalog = json.loads(saved_catalog)
                pane_catalog["panes"] = {"rail": 440.0, "sidebar": 560.0, "thread": 640.0}
                pane_catalog.setdefault("appearance", {})["text_size"] = 14.0
                catalog_path.write_text(json.dumps(pane_catalog))
                try:
                    pane_steps = [
                        step("resize", width=1200, height=760),
                        step("click", id=f"project-{project}"), step("click", id="inbox"),
                        step("click", id=f"thread-{narrow['id']}"),
                        step("fill", id=f"reply-{narrow['id']}", text="Keep this conversation draft"),
                        step("click", id=f"thread-{routed}"),
                        step("wait_control", id=f"reply-{narrow['id']}", present=True),
                        step("wait_control", id=f"reply-thread-{routed}", present=True),
                        step("fill", id=f"reply-thread-{routed}", text="Keep this thread draft"),
                        step("capture", name="maximum-columns-1200"),
                        step("resize", width=1000, height=760),
                        step("wait_control", id=f"reply-{narrow['id']}", present=False),
                        step("wait_control", id=f"reply-thread-{routed}", present=True),
                        step("capture", name="thread-fallback-1000"),
                        step("click", id="close-thread"),
                        step("wait_control", id=f"reply-{narrow['id']}", present=True),
                        step("wait_text", text="Keep this conversation draft"),
                        step("click", id=f"thread-{routed}"),
                        step("wait_text", text="Keep this thread draft"),
                        step("resize", width=1200, height=760),
                        step("wait_control", id=f"reply-{narrow['id']}", present=True),
                        step("click", id="settings"), step("click", id="larger-ui"),
                        step("click", id="larger-ui"), step("click", id="larger-ui"),
                        step("click", id="larger-ui"), step("click", id="inbox"),
                        step("wait_control", id=f"reply-{narrow['id']}", present=False),
                        step("wait_control", id=f"reply-thread-{routed}", present=True),
                        step("wait_text", text="Keep this thread draft"),
                        step("capture", name="zoom-without-resize"),
                        step("resize", width=2000, height=900),
                        step("wait_control", id=f"reply-{narrow['id']}", present=True),
                        step("wait_control", id=f"reply-thread-{routed}", present=True),
                        step("wait_text", text="Keep this conversation draft"),
                        step("wait_text", text="Keep this thread draft"),
                        step("capture", name="expanded-columns"),
                        step("click", id="projects"), step("click", id=f"project-{project}"),
                        step("click", id="project-more"), step("click", id="project-tab-Channels"),
                        step("click", id=f"reply-channel-{room['id']}"),
                        step("fill", id="channel-message", text="Keep this channel across reopen"),
                        step("click", id="projects"), step("click", id=f"project-{project}"),
                        step("click", id=f"session-{narrow['id']}"), step("click", id="session-message"),
                        step("fill", id="session-message-text", text="Keep this session across reopen"),
                    ]
                    report["constrained_panes_window"] = launch("constrained-panes", pane_steps)
                    kept = json.loads(catalog_path.read_text())["panes"]
                    assert kept == pane_catalog["panes"], kept
                    checks.append("maximum_saved_columns_shrink_or_use_one_pane_and_zoom_reflows_without_losing_drafts_or_preferences")
                finally:
                    catalog_path.write_text(saved_catalog)
                # The prior window closes normally immediately after its last edit.
                # Its close must flush all three kinds, including a hidden thread.
                draft_paths = list((state / "drafts").glob("*/drafts.json"))
                assert len(draft_paths) == 1, draft_paths
                saved_drafts = json.loads(draft_paths[0].read_text())
                assert saved_drafts["sessions"][narrow["id"]] == "Keep this session across reopen", saved_drafts
                assert "Keep this conversation draft" in saved_drafts["conversations"].values(), saved_drafts
                assert "Keep this thread draft" in saved_drafts["conversations"].values(), saved_drafts
                assert saved_drafts["channels"][room["id"]] == "Keep this channel across reopen", saved_drafts
                restored_steps = [
                    step("resize", width=1800, height=900),
                    step("click", id=f"project-{project}"), step("click", id="inbox"),
                    step("click", id=f"thread-{narrow['id']}"),
                    step("wait_text", text="Keep this conversation draft"),
                    step("click", id=f"thread-{routed}"),
                    step("wait_text", text="Keep this thread draft"),
                    step("capture", name="restored-conversation-and-thread"),
                    step("click", id="projects"), step("click", id=f"project-{project}"),
                    step("click", id=f"session-{narrow['id']}"), step("click", id="session-message"),
                    step("wait_text", text="Keep this session across reopen"),
                    step("capture", name="restored-session"),
                    step("click", id="projects"), step("click", id=f"project-{project}"),
                    step("click", id="project-more"), step("click", id="project-tab-Channels"),
                    step("click", id=f"reply-channel-{room['id']}"),
                    step("wait_text", text="Keep this channel across reopen"),
                    step("capture", name="restored-channel"),
                ]
                report["restored_drafts_window"] = launch("restored-drafts", restored_steps)
                retained_input = rpc(endpoint, {"op": "peek_input", "agent": narrow["id"]})["messages"]
                for marker in ("Keep this conversation draft", "Keep this thread draft", "Keep this session across reopen"):
                    assert not any(marker in json.dumps(m.get("payload")) for m in retained_input), marker
                channel_inputs = rpc(endpoint, {"op": "peek_input", "agent": agent["id"]})["messages"]
                assert not any("Keep this channel across reopen" in json.dumps(m.get("payload")) for m in channel_inputs)
                channel_history = rpc(endpoint, {"op": "history", "conversation": f"channel:{room['id']}", "limit": 100})["messages"]
                assert not any("Keep this channel across reopen" in json.dumps(m) for m in channel_history)
                checks.append("normal_close_flushes_hidden_conversation_thread_session_and_channel_drafts_and_reopen_never_sends_them")
                rpc(endpoint, {"op": "stop", "agent": narrow["id"], "force": False})
                until(lambda: rpc(endpoint, {"op": "inspect", "agent": narrow["id"]})["agent"]["status"]["state"] == "exited")

            catalog = json.loads((state / "workspace.json").read_text())
            assert catalog["selected"] == str(pinned), catalog
            entries = [entry for entry in catalog["projects"] if entry["project"]["root"] == str(pinned)]
            assert len(entries) == 1 and entries[0]["pinned"], entries
            assert not list(pinned.iterdir()), "Adding or launching unexpectedly wrote into the folder"
            assert not (home / ".codex/config.toml").exists(), "Undo did not restore fixture configuration"
            messages = rpc(endpoint, {"op": "inbox", "agent": agent["id"], "drain": False})["messages"]
            assert any(m.get("payload") == "Fixture channel message" for m in messages), messages
            launched = [a for a in rpc(endpoint, {"op": "list", "all": True})["agents"] if a["spec"]["name"] == "launched-from-iced"]
            assert len(launched) == 1 and launched[0]["status"]["state"] == "exited", launched
            report["restored_window"] = launch("restored", [step("wait_text", text="pinned-api"), step("wait_text", text="No agents in this project"), step("capture", name="restored-last-project"), step("click", id="sessions-earlier"), step("wait_text", text="launched-from-iced"), step("capture", name="restored-earlier")])
            checks.extend(["folder_pin_has_no_project_files", "same_project_after_launch", "last_project_restore", "quiet_project_retained", "saved_appearance"])
            # Private metadata fixture, not a model or idle-wake assertion.
            receiver = rpc(endpoint, {"op": "register", "spec": {
                "name": "readiness-fixture", "runtime": "claude-code", "workdir": str(project)},
                "pid": os.getpid()})["agent"]
            def now():
                return datetime.now(timezone.utc).isoformat()
            def readiness_window(name, expected):
                conversation = "dm:" + ":".join(sorted([human["id"], receiver["id"]]))
                input_status = "Idle delivery not verified" if name == "readiness-contact" else expected
                return launch(name, [step("click", id="connections"),
                                     step("click", id="connection-details-claude-code"),
                                     step("wait_text", text="readiness-fixture"),
                                     step("wait_text", text=expected), step("capture", name=name),
                                     step("click", id="projects"), step("click", id=f"project-{project}"),
                                     step("click", id="inbox"), step("click", id=f"thread-{receiver['id']}"),
                                     step("wait_text", text=input_status),
                                     step("fill", id=f"reply-{receiver['id']}", text="Keep the connection draft"),
                                     step("click", id=f"input-connection-{conversation}"),
                                     step("wait_text", text="Tools (MCP)"), step("click", id="inbox"),
                                     step("wait_text", text="Keep the connection draft"),
                                     step("wait_text", text=input_status), step("capture", name=name+"-composer")])
            rpc(endpoint, {"op": "report_activity", "agent": receiver["id"],
                           "observation": {"activity": "working", "observed_at": now()}})
            report["activity_only_window"] = readiness_window("readiness-activity", "Idle delivery not verified")
            rpc(endpoint, {"op": "report_adapter", "agent": receiver["id"], "adapter": "mcp",
                           "contact": {"process_started_at": receiver["process_started_at"], "observed_at": now()}})
            report["contact_window"] = readiness_window("readiness-contact", "Connected · messages wait for its next prompt")
            def input_report(value):
                rpc(endpoint, {"op": "report_input", "agent": receiver["id"],
                               "process_started_at": receiver["process_started_at"],
                               "observed_at": now(), "report": value})
            input_report({"state": "ready"})
            report["ready_window"] = readiness_window("readiness-ready", "Receiver active, awaiting first receipt")
            message = rpc(endpoint, {"op": "send", "from": human["id"], "to": receiver["id"],
                                     "kind": "chat", "payload": {"text": "readiness fixture"}})["message"]
            input_report({"state": "received", "input": {"messages": [message], "receipt": {"provider": "claude_channel"}}})
            report["received_window"] = readiness_window("readiness-received", "Delivery verified")
            input_report({"state": "paused", "reason": "Fixture transport is disconnected"})
            report["paused_window"] = readiness_window("readiness-paused", "Delivery paused")
            checks.append("rendered_session_readiness_separates_activity_contact_receiver_receipt_and_pause")
            input_report({"state": "ready"})
            rpc(endpoint, {"op": "report_provider", "agent": receiver["id"],
                           "process_started_at": receiver["process_started_at"], "observed_at": now(),
                           "report": {"state": "blocked", "issue": {"kind": "usage"}}})
            retained = rpc(endpoint, {"op": "inbox", "agent": receiver["id"], "drain": False})["messages"]
            assert rpc(endpoint, {"op": "delivery_queue", "agent": receiver["id"]})["type"] == "input_waiting"
            report["provider_limit_window"] = launch("provider-limit", [
                step("click", id="projects"), step("click", id=f"project-{project}"),
                step("wait_text", text="readiness-fixture: Usage limit"),
                step("wait_control", id=f"needs-you-review-{receiver['id']}", present=False),
                step("click", id=f"needs-you-provider-{receiver['id']}"),
                step("wait_control", id="review-delivery", present=False),
                step("wait_text", text="Usage limit"),
                step("click", id="session-message"), step("fill", id="session-message-text", text="Keep this draft during recovery"),
                step("capture", name="provider-limit"), step("click", id="resume-provider"),
                step("wait_control", id="resume-provider", present=False),
                step("wait_text", text="Keep this draft during recovery"), step("capture", name="provider-resumed")])
            recovered = rpc(endpoint, {"op": "inspect", "agent": receiver["id"]})["agent"]
            assert recovered["provider_availability"]["issue"] is None
            assert rpc(endpoint, {"op": "delivery_queue", "agent": receiver["id"]})["messages"] == retained
            checks.append("provider_limit_resume_preserves_draft_receipts_and_retained_queue")
            rpc(endpoint, {"op": "deregister", "agent": receiver["id"]})
            narrow_inbox()
            report["idle_resources"] = (
                {"result": "not_run", "reason": "Explicit --skip-idle-measurement: foreground CPU/RSS sample omitted; no idle performance claim."}
                if skip_idle_measurement else measure_idle(binary_dir, env, project, daemon, output)
            )
            report["result"] = "passed"
        except Exception as error:
            report["error"] = str(error)
            raise
        finally:
            finish_smoke(window, daemon, endpoint, report, output, started, sys.exc_info()[1])
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--skip-idle-measurement", action="store_true",
                        help="omit the foreground idle CPU/RSS sample while retaining graphical workflow checks")
    args = parser.parse_args()
    print(json.dumps(smoke(args.binary_dir, args.output,
                           skip_idle_measurement=args.skip_idle_measurement), indent=2))
