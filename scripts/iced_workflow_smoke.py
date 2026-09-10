#!/usr/bin/env python3
"""Exercise production Iced controls in owned fixture state, then reopen the app.

Scenarios invoke callbacks collected from the actual rendered controls. This checks
native rendering and action wiring; it is not physical keyboard or screen-reader testing.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
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
from desktop_smoke import stop, wait_window


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
                if window.poll() is not None or daemon.poll() is not None:
                    raise RuntimeError("idle measurement process exited")
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


def smoke(binary_dir, output):
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
                steps = [step("click", id=f"project-{project}"), step("wait_text", text="terminal-fixture"), step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("wait_control", id=f"session-{previous['id']}", present=False), step("capture", name="projects-live"),
                         step("click", id="sessions-history"), step("wait_control", id=f"session-{previous['id']}", present=True),
                         step("wait_control", id=f"session-{agent['id']}", present=False), step("capture", name="session-history"),
                         step("click", id="sessions-attention"), step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("click", id="sessions-current"),
                         step("click", id="inbox"), step("fill", id=f"answer-{question['id']}", text="Use API v2"),
                         step("wait_control", id=f"dismiss-message-{question['id']}", present=False),
                         step("wait_control", id=f"dismiss-message-{dismissible[0]}", present=False),
                         step("click", id=f"dismiss-message-{dismissible[5]}"),
                         step("wait_control", id=f"dismiss-message-{dismissible[5]}", present=False),
                         step("wait_control", id=f"dismiss-message-{dismissible[6]}", present=True),
                         step("click", id=f"dismiss-shown-{dismissible[4]}"),
                         step("wait_control", id=f"dismiss-message-{dismissible[6]}", present=False),
                         step("wait_control", id=f"dismiss-message-{dismissible[0]}", present=True),
                         step("wait_text", text="Use API v2"),
                         step("capture", name="inbox-draft"), step("click", id="connections"), step("click", id="inbox"),
                         step("wait_text", text="Use API v2"), step("click", id=f"send-answer-{question['id']}"),
                         step("wait_text", text="answered"), step("click", id="projects"),
                         step("click", id=f"session-{agent['id']}"), step("resize", width=720, height=540), step("wait_control", id="attach-session", present=True),
                         step("capture", name="compact-session"), step("click", id="close-session"),
                         step("wait_control", id=f"session-{agent['id']}", present=True),
                         step("click", id=f"session-{agent['id']}"), step("resize", width=1180, height=760),
                         step("click", id="attach-session"),
                         step("wait_text", text="ICED TERMINAL READY λ 日本語"), step("capture", name="terminal"),
                         step("focus", id="detach-terminal"), step("wait_focus", id="detach-terminal"), step("click", id="detach-terminal"),
                         step("click", id="project-tab-Channels"), step("click", id=f"reply-channel-{room['id']}"),
                         step("fill", id="channel-message", text="Fixture channel message"), step("click", id="send-channel"),
                         step("wait_text", text="Message sent"), step("wait_text", text="Fixture channel message"), step("capture", name="channels"),
                         step("click", id="project-tab-Journal"), step("capture", name="activity"),
                         step("click", id="project-tab-Channels"), step("wait_text", text="Fixture channel message"),
                         step("click", id="project-more"), step("click", id="project-tab-Leases"), step("capture", name="coordination"),
                         step("click", id="connections"), step("capture", name="connections"),
                         step("click", id="connection-details-codex"), step("wait_text", text="MCP:"),
                         step("click", id="connection-details-codex"), step("click", id="setup-codex"),
                         step("wait_text", text="Review integration changes"), step("capture", name="setup-review"),
                         step("click", id="apply-setup"), step("wait_text", text="Setup applied"),
                         step("click", id="undo-setup"), step("wait_text", text="Setup undone"), step("click", id="close-setup"),
                         step("click", id="add-project"), step("fill", id="project-path", text=str(pinned)),
                         step("click", id="pin-folder"), step("wait_text", text="pinned-api"), step("capture", name="pinned-empty-project"),
                         step("click", id="launch-agent"), step("click", id="launch-tool-codex"),
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
                    assert answer.result(timeout=5).get("text") == "Use API v2", "answer did not reach asking agent"
                    remaining = rpc(endpoint, {"op": "inbox", "agent": human["id"], "drain": False})["messages"]
                    retained = {message["id"] for message in remaining}
                    assert set(dismissible[:4]) <= retained, "unshown messages were dismissed"
                    assert not set(dismissible[4:]) & retained, "shown messages were not dismissed"
                    checks.append("explicit_individual_and_bulk_dismissal_preserves_unshown_messages_and_answer_draft")
                except BaseException:
                    stop(window)
                    stop(daemon)
                    raise
            checks.extend(["rendered_actions", "answer_and_draft_navigation", "native_vt_rendering", "terminal_attach_detach", "channels", "setup_review_apply_undo", "launch_and_confirmed_stop", "focused_control_reveal", "compact_zoom", "current_history_separation", "project_attention", "compact_session_navigation"])
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
            report["restored_window"] = launch("restored", [step("wait_text", text="pinned-api"), step("wait_text", text="No current sessions"), step("capture", name="restored-last-project"), step("click", id="sessions-history"), step("wait_text", text="launched-from-iced"), step("capture", name="restored-history")])
            checks.extend(["folder_pin_has_no_project_files", "same_project_after_launch", "last_project_restore", "quiet_project_retained", "saved_appearance"])
            report["idle_resources"] = measure_idle(binary_dir, env, project, daemon, output)
            report["result"] = "passed"
        finally:
            stop(window)
            if daemon is not None and daemon.poll() is None:
                try:
                    rpc(endpoint, {"op": "shutdown"})
                    daemon.wait(timeout=10)
                except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
                    stop(daemon)
            report["elapsed_seconds"] = time.monotonic() - started
            (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(smoke(args.binary_dir, args.output), indent=2))
