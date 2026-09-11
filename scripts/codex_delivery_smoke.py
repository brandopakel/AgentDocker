#!/usr/bin/env python3
"""Actual Codex hook delivery; private daemon, nonce-only messages, no inbox tools.

Uses the installed provider's existing authentication. Never copies credentials,
edits provider configuration, or changes persisted hook trust. Raw output stays
in the explicitly selected private output directory.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None


def rpc(endpoint, value):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(endpoint)
        stream.sendall(json.dumps(value).encode() + b"\n")
        with stream.makefile("rb") as reader:
            line = reader.readline(1024 * 1024)
    result = json.loads(line)
    if result.get("type") == "error":
        raise RuntimeError("fixture RPC refused: " + json.dumps(result))
    return result


def stop(process):
    # The unreaped owned child reserves the group identity until wait().
    if process.returncode is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


@contextlib.contextmanager
def fixture(output, processes):
    with tempfile.TemporaryDirectory(prefix="ad-codex-delivery-", dir="/tmp") as scratch:
        try:
            yield scratch
        finally:
            for process in reversed(processes):
                stop(process)
            events = Path(scratch) / "events.jsonl"
            if events.exists():
                (output / "hook-events.jsonl").write_text(events.read_text())


def helper(config_path):
    config = json.loads(config_path.read_text())
    raw = sys.stdin.buffer.read(1024 * 1024 + 1)
    event = json.loads(raw)
    name = event["hook_event_name"]
    records = config_path.parent / "events.jsonl"
    cli = config["cli"]
    # Establish identity through the real adapter without consuming messages.
    bootstrap = dict(event, hook_event_name="PreToolUse")
    subprocess.run([cli, "hook", "codex"], input=json.dumps(bootstrap).encode(),
                   stdout=subprocess.DEVNULL, check=True, timeout=4)
    agents = rpc(config["socket"], {"op": "list", "all": False})["agents"]
    candidates = [a for a in agents if a["spec"]["runtime"] == "codex"
                  and a["spec"].get("labels", {}).get("session_id") == event["session_id"]]
    assert len(candidates) == 1, "hook did not bind exactly one provider identity"
    agent = candidates[0]["id"]
    marker = config_path.parent / (name + ".sent")
    sent = None
    if not marker.exists() and not event.get("stop_hook_active", False):
        # Every event gets one fresh token, absent from the prompt and workspace.
        token = "ad-hook-" + uuid.uuid4().hex
        response = rpc(config["socket"], {"op": "send", "from": config["peer"],
            "to": agent, "kind": "fixture", "payload": {"text": token}})
        sent = {"event": name, "message": response["message"], "token": token, "agent": agent}
        marker.write_text(json.dumps(sent))
    completed = subprocess.run([cli, "hook", "codex"], input=raw,
                               capture_output=True, check=True, timeout=4)
    output = json.loads(completed.stdout)
    with records.open("a") as writer:
        writer.write(json.dumps({"event": name, "continued": event.get("stop_hook_active", False),
                                 "output": output, "sent": sent,
                                 "stderr": completed.stderr.decode()}) + "\n")
    sys.stdout.buffer.write(completed.stdout)
    sys.stdout.buffer.flush()


def trial(args):
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    cli, daemon_binary = (args.binary_dir.resolve() / name for name in ["agentdocker", "agentd"])
    report = {"result": "failed", "scope": "actual Codex lifecycle context, no inbox tools",
              "driver_sha256": digest(Path(__file__)),
              "binary_sha256": {p.name: digest(p) for p in [cli, daemon_binary]}, "checks": []}
    provider_root = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    monitored = [provider_root / name for name in ["config.toml", "hooks.json", "auth.json"]]
    before = {p: digest(p) for p in monitored}
    processes = []
    started = time.monotonic()
    try:
        with fixture(output, processes) as scratch:
            root = Path(scratch).resolve()
            project = root / "project"
            project.mkdir()
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
            endpoint = str(root / "sock")
            env.update(AGENTDOCKER_HOME=str(root / "state"), AGENTDOCKER_SOCKET=endpoint,
                       AGENTDOCKER_NO_AUTOSTART="1", RUST_LOG="warn")
            with (output / "daemon.log").open("wb") as log:
                daemon = subprocess.Popen([str(daemon_binary)], env=env, cwd=project,
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
                processes.append(daemon)
                deadline = time.monotonic() + 15
                while True:
                    try:
                        if rpc(endpoint, {"op": "ping"})["type"] == "pong":
                            break
                    except (OSError, ValueError):
                        pass
                    if time.monotonic() > deadline or daemon.poll() is not None:
                        raise RuntimeError("fixture daemon startup failed")
                    time.sleep(0.05)
                peer = rpc(endpoint, {"op": "register", "spec": {"name": "delivery-peer",
                    "runtime": "fixture", "workdir": str(project)}})["agent"]["id"]
                config = root / "helper.json"
                config.write_text(json.dumps({"cli": str(cli), "socket": endpoint, "peer": peer}))
                helper_command = shlex.join([sys.executable, str(Path(__file__).resolve()), "--helper", str(config)])
                # Codex filters inherited environment for MCP children. Bind the
                # endpoint explicitly so a fixture can never use the user daemon.
                mcp_env = "{" + ",".join(key + "=" + json.dumps(env[key]) for key in
                    ["AGENTDOCKER_HOME", "AGENTDOCKER_SOCKET", "AGENTDOCKER_NO_AUTOSTART"]) + "}"
                command = [args.codex, "exec", "--ignore-user-config", "--ignore-rules", "--ephemeral",
                    "--skip-git-repo-check", "--sandbox", "read-only", "--dangerously-bypass-hook-trust", "--json",
                    "-c", "features.hooks=true", "-c", "approval_policy=\"never\"",
                    "-c", "mcp_servers.agentdocker={command=" + json.dumps(str(cli)) +
                    ',args=["mcp","--runtime","codex"],enabled_tools=["send_message"],env=' + mcp_env + '}',
                    "-c", 'mcp_servers.agentdocker.tools.send_message.approval_mode="approve"']
                for event in ["UserPromptSubmit", "PostToolUse", "Stop"]:
                    command.extend(["-c", "hooks." + event + "=[{hooks=[{type=\"command\",command=" +
                                    json.dumps(helper_command) + ",timeout=5}]}]"])
                prompt = ("This is an authorized, bounded AgentDocker delivery acceptance test. "
                    "Execute /usr/bin/true once. For each fixture message supplied by AgentDocker hook context, "
                    "use the AgentDocker send_message tool to reply to its from ID, set reply_to to its exact "
                    "message ID, and echo its payload.text unchanged. Do not read inboxes or files. "
                    "Handle any messages supplied when stopping the same way, then finish. "
                    "Do not launch other agents, edit anything, or perform unrelated work.")
                command.append(prompt)
                report["provider_version"] = subprocess.check_output([args.codex, "--version"], text=True).strip()
                with (output / "provider.jsonl").open("wb") as stdout, (output / "provider.log").open("wb") as stderr:
                    provider = subprocess.Popen(command, env=env, cwd=project, stdin=subprocess.DEVNULL,
                        stdout=stdout, stderr=stderr, start_new_session=True)
                    processes.append(provider)
                    # Keep the leader unreaped until descendants have been stopped.
                    deadline = time.monotonic() + args.timeout
                    while time.monotonic() < deadline:
                        events = (root / "events.jsonl")
                        if events.exists():
                            lines = [json.loads(s) for s in events.read_text().splitlines()]
                            if any(e["event"] == "Stop" and e["continued"] for e in lines):
                                break
                        time.sleep(0.1)
                    else:
                        raise TimeoutError("provider did not complete the hook continuation before deadline")
                    replies = rpc(endpoint, {"op": "inbox", "agent": peer, "drain": False})["messages"]
                    sent = [json.loads(p.read_text()) for p in sorted(root.glob("*.sent"))]
                    assert len(sent) == 3, "all three lifecycle boundaries must run"
                    for item in sent:
                        assert any(m.get("reply_to") == item["message"] and item["token"] in
                                   json.dumps(m["payload"]) and m["from"] == item["agent"] for m in replies), \
                            "missing correctly attributed correlated reply for " + item["event"]
                        assert not any(m["id"] == item["message"] for m in rpc(endpoint,
                            {"op": "inbox", "agent": item["agent"], "drain": False})["messages"]), "message not acknowledged"
                        report["checks"].append({"boundary": item["event"], "correlated_reply": True,
                                                 "acknowledged": True})
                    assert len({s["agent"] for s in sent}) == 1, "identity split between hooks"
                    report["identity_count"] = 1
                    (output / "hook-events.jsonl").write_text(events.read_text())
                    report["result"] = "passed"
                stop(provider)
    except Exception as error:
        report["error"] = str(error)
    finally:
        for process in reversed(processes):
            stop(process)
        report["duration_seconds"] = time.monotonic() - started
        report["provider_configuration_unchanged"] = all(digest(p) == value for p, value in before.items())
        if not report["provider_configuration_unchanged"]:
            report["result"] = "failed"
            report["error"] = "a monitored provider file changed; no file was restored"
        report["owned_children_remaining"] = sum(p.returncode is None for p in processes)
        (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return int(report["result"] != "passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=Path)
    parser.add_argument("--binary-dir", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    if args.helper:
        helper(args.helper)
    else:
        if not args.binary_dir or not args.output or not 10 <= args.timeout <= 600:
            parser.error("--binary-dir, --output and a 10–600 second timeout are required")
        raise SystemExit(trial(args))
