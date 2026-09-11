#!/usr/bin/env python3
"""Exercise notification navigation through real desktop processes and private IPC.

This tests existing-window and cold-start routing with actual daemon state and
rendered controls. It does not simulate or claim a Notification Center click.
Native sender authorization and physical notification clicks are separate trials.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from desktop_smoke import stop
from iced_workflow_smoke import rpc, step, until


def smoke(binary_dir, output):
    binary_dir = binary_dir.resolve(strict=True)
    output = output.absolute()
    output.mkdir(mode=0o700)
    hashes = {name: hashlib.sha256((binary_dir / name).read_bytes()).hexdigest()
              for name in ("agentd", "agentdocker-ui")}
    report = {"result": "failed", "scope": __doc__, "platform": platform.platform(),
              "binary_sha256": hashes, "checks": [], "accepted_activations": [],
              "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    started = time.monotonic()
    daemon = window = None
    children = []
    pool = ThreadPoolExecutor(max_workers=2)
    futures = []
    with tempfile.TemporaryDirectory(prefix="ad-notification-", dir="/tmp") as scratch:
        root = Path(scratch).resolve()
        first, second, home, state, tools = (root / n for n in ("first", "second", "home", "state", "bin"))
        endpoint = root / "daemon.sock"
        for path in (first, second, home, tools):
            path.mkdir(mode=0o700)
        runtime = tools / "codex"
        runtime.write_text('#!/bin/sh\necho "codex notification-fixture"\n')
        runtime.chmod(0o700)
        env = {**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
               "CODEX_HOME": str(home / ".codex"), "CLAUDE_CONFIG_DIR": str(home / ".claude"),
               "PATH": f"{tools}:/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
               "AGENTDOCKER_HOME": str(state), "AGENTDOCKER_SOCKET": str(endpoint),
               "AGENTDOCKER_NO_AUTOSTART": "1", "AGENTDOCKER_NO_NOTIFICATIONS": "1"}

        def launch(name, scenario, action=None):
            script = root / f"{name}.json"
            script.write_text(json.dumps(scenario))
            command = [str(binary_dir / "agentdocker-ui"), "--smoke-test", str(output / name),
                       "--smoke-scenario", str(script), "--smoke-deadline", "100"]
            if action:
                command += ["--open-notification", json.dumps(action)]
            with (output / f"{name}.log").open("w") as log:
                return subprocess.Popen(command, cwd=first, env=env, stdin=subprocess.DEVNULL,
                                        stdout=log, stderr=subprocess.STDOUT)

        def progress(name, completed):
            if window.poll() is not None:
                raise RuntimeError("desktop exited before notification navigation")
            try:
                return json.loads((output / name / "progress.json").read_text())["completed"] >= completed
            except (FileNotFoundError, json.JSONDecodeError):
                return False

        def action(message, agent, channel=None):
            return {"home": str(state), "socket": str(endpoint), "target": {
                "message": message, "agent": agent["id"],
                "project": channel["project"] if channel else room["project"],
                "channel": channel["id"] if channel else None}}

        try:
            with (output / "daemon.log").open("w") as log:
                daemon = subprocess.Popen([str(binary_dir / "agentd")], cwd=first, env=env,
                                          stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            def ready():
                if daemon.poll() is not None:
                    raise RuntimeError("fixture daemon exited")
                try:
                    return rpc(endpoint, {"op": "ping"}).get("type") == "pong"
                except OSError:
                    return False
            until(ready)
            agents = [rpc(endpoint, {"op": "run", "spec": {"name": name, "runtime": "fixture",
                      "command": ["/bin/sleep", "150"], "workdir": str(project), "restore": False}})["agent"]
                      for name, project in (("first-agent", first), ("second-agent", second))]
            children = [a["pid"] for a in agents]
            human = rpc(endpoint, {"op": "me", "workdir": str(first)})["agent"]
            room = rpc(endpoint, {"op": "channel_open", "agent": agents[1]["id"],
                       "task": "Notification destination", "members": [human["id"]]})["channel"]
            questions = []
            for index, agent in enumerate(agents):
                text = f"Notification question {index}"
                futures.append(pool.submit(rpc, endpoint, {"op": "ask", "from": agent["id"],
                    "to": human["id"], "question": text, "timeout_secs": 120}, 130))
                questions.append(until(lambda: next((q for q in rpc(endpoint, {"op": "questions", "agent": human["id"]})["questions"]
                                                       if q["text"] == text), None)))

            def send(text, to):
                return rpc(endpoint, {"op": "send", "from": agents[1]["id"], "to": to,
                    "kind": "chat", "payload": text})["message"]
            direct = send("OLD DIRECT NOTIFICATION TARGET", human["id"])
            channel_message = send("OLD CHANNEL NOTIFICATION TARGET", f"channel:{room['id']}")
            for i in range(35):
                send(f"newer direct {i}", human["id"])
                send(f"newer channel {i}", f"channel:{room['id']}")

            sequence = [step("click", id="inbox"), step("fill", id=f"answer-{questions[0]['id']}", text="Keep this answer draft"),
                        step("click", id=f"project-{first}"), step("click", id="connections")]
            gates = [(len(sequence), action(questions[1]["id"], agents[1]))]
            sequence += [step("wait_text", text=questions[1]["text"]), step("wait_text", text="Keep this answer draft"),
                         step("capture", name="question-route"), step("click", id="connections")]
            gates.append((len(sequence), action(direct, agents[1])))
            sequence += [step("wait_text", text="OLD DIRECT NOTIFICATION TARGET"), step("capture", name="direct-route"),
                         step("click", id=f"project-{second}"), step("click", id="project-tab-Channels"),
                         step("click", id=f"reply-channel-{room['id']}"), step("fill", id="channel-message", text="Keep this channel draft"),
                         step("click", id="connections")]
            gates.append((len(sequence), action(channel_message, agents[1], room)))
            sequence += [step("wait_text", text="OLD CHANNEL NOTIFICATION TARGET"), step("wait_text", text="Keep this channel draft"),
                         step("capture", name="channel-route"), step("click", id="inbox"),
                         step("wait_text", text="Keep this answer draft"), step("capture", name="retained-answer-draft")]
            window = launch("existing", sequence)
            primary = window.pid
            for gate, target in gates:
                until(lambda: progress("existing", gate), timeout=30)
                result = subprocess.run([str(binary_dir / "agentdocker-ui"), "--open-notification", json.dumps(target)],
                    cwd=first, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=8)
                assert result.returncode == 0, result.stderr
                assert window.poll() is None and window.pid == primary, "existing window was replaced"
                report["accepted_activations"].append(target["target"]["message"])
            window.wait(timeout=40)
            assert window.returncode == 0, (output / "existing.log").read_text()
            existing = json.loads((output / "existing" / "result.json").read_text())
            assert existing["scenario_steps_completed"] == len(sequence), existing
            report["existing_window_steps"] = len(sequence)
            report["checks"] += ["existing_window_forwarding", "question_navigation", "older_direct_message",
                                  "older_channel_message", "answer_draft_preserved", "channel_draft_preserved"]

            cold = [step("wait_text", text="OLD DIRECT NOTIFICATION TARGET"), step("capture", name="cold-route")]
            window = launch("cold", cold, action(direct, agents[1]))
            window.wait(timeout=30)
            assert window.returncode == 0, (output / "cold.log").read_text()
            assert json.loads((output / "cold" / "result.json").read_text())["scenario_steps_completed"] == len(cold)
            report["cold_start_steps"] = len(cold)
            report["checks"].append("cold_start_navigation")
            # A click must not consume or answer either question.
            assert len(rpc(endpoint, {"op": "questions", "agent": human["id"]})["questions"]) == 2
            for question in questions:
                rpc(endpoint, {"op": "answer", "from": human["id"], "message": question["id"], "text": "fixture cleanup"})
            for future in futures:
                assert future.result(timeout=5)["text"] == "fixture cleanup"
            report["checks"].append("navigation_does_not_answer_or_drain")
            report["result"] = "passed"
        except Exception as error:
            report["error"] = str(error)
            progress_file = output / "existing" / "progress.json"
            if progress_file.exists():
                report["last_progress"] = progress_file.read_text()
            if platform.system() == "Darwin" and window and window.poll() is None:
                subprocess.run(["/usr/bin/sample", str(window.pid), "1", "-file", str(output / "desktop-sample.txt")],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        finally:
            stop(window)
            stop(daemon)
            pool.shutdown(wait=True, cancel_futures=True)
            def alive(pid):
                try:
                    os.kill(pid, 0)
                    return True
                except ProcessLookupError:
                    return False
            report["surviving_children"] = [pid for pid in children if alive(pid)]
            report["binaries_unchanged"] = all(hashlib.sha256((binary_dir / name).read_bytes()).hexdigest() == digest
                                                for name, digest in hashes.items())
            if report["surviving_children"] or not report["binaries_unchanged"]:
                report["result"] = "failed"
            report["elapsed_seconds"] = time.monotonic() - started
            (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    if report["result"] != "passed":
        raise RuntimeError(report)
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(smoke(args.binary_dir, args.output), indent=2))
