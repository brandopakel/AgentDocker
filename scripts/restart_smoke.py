#!/usr/bin/env python3
"""Crash/restart acceptance with owned external processes and private daemon state.

An optional older binary exercises a real schema upgrade and downgrade refusal.
This does not claim supervised child/PTY handover or provider-context recovery.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def snapshot(source, destination):
    before = digest(source)
    shutil.copyfile(source, destination)
    destination.chmod(0o500)
    if digest(source) != before or digest(destination) != before:
        raise RuntimeError("executable changed while preparing the trial")
    metadata = json.loads(subprocess.check_output([str(destination), "--build-info"], timeout=5))
    return {"sha256": before, "metadata": metadata}


def request(endpoint, value):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(value).encode() + b"\n")
        with stream.makefile("rb") as reader:
            reply = json.loads(reader.readline(4 * 1024 * 1024))
    if reply.get("type") == "error":
        raise RuntimeError(value["op"] + ": " + json.dumps(reply))
    return reply


def eventually(check, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, ValueError):
            pass
        time.sleep(0.025)
    raise RuntimeError("fixture readiness deadline exceeded")


def run(args):
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    current = output / "agentd-current"
    report = {"result": "failed", "steps": [], "driver_sha256": digest(Path(__file__)),
              "current": snapshot(args.binary.resolve(), current),
              "scope": "externally owned processes, durable coordination and actual daemon crashes"}
    previous = current
    if args.previous_binary:
        previous = output / "agentd-previous"
        report["previous"] = snapshot(args.previous_binary.resolve(), previous)
    daemon, children = None, []
    logs = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="ad-restart-", dir="/tmp") as scratch:
        root = Path(scratch).resolve()
        home, endpoint = root / "state", root / "sock"
        env = {key: value for key, value in os.environ.items() if not key.startswith("AGENTDOCKER_")}
        env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=str(endpoint), AGENTDOCKER_NO_AUTOSTART="1")

        def start(binary):
            log = (output / f"daemon-{len(logs)}.log").open("wb")
            logs.append(log)
            process = subprocess.Popen([str(binary)], cwd=root, env=env,
                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            # Retain ownership even when readiness fails so finally reaps it.
            nonlocal daemon
            daemon = process
            eventually(lambda: process.poll() is None and request(endpoint, {"op": "ping"})["type"] == "pong")

        def stable(agents, lease, message):
            records = request(endpoint, {"op": "list", "all": True})["agents"]
            assert sorted(row["id"] for row in records) == sorted(agents), "agent identity changed or duplicated"
            assert all(row["status"]["state"] == "running" for row in records), "external process was retired"
            assert all(child.poll() is None for child in children), "external process was stopped"
            held = request(endpoint, {"op": "leases", "agent": agents[0]})["leases"]
            assert len(held) == 1 and held[0]["id"] == lease["id"] and held[0]["expires_at"] == lease["expires_at"], "lease changed during restart"
            inbox = request(endpoint, {"op": "inbox", "agent": agents[0], "drain": False})["messages"]
            assert sum(item["id"] == message for item in inbox) == 1, "queued message lost or duplicated"

        try:
            start(previous)
            agents = []
            for name in ["asker", "recipient"]:
                child = subprocess.Popen(["/bin/sleep", "300"], cwd=root, env=env,
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                children.append(child)
                agent = request(endpoint, {"op": "register", "spec": {
                    "name": name, "runtime": "fixture", "workdir": str(root)}, "pid": child.pid})["agent"]
                agents.append(agent["id"])
            lease = request(endpoint, {"op": "claim", "agent": agents[0], "resource": "task:restart", "ttl_secs": 300})["lease"]
            queued = request(endpoint, {"op": "send", "from": agents[1], "to": agents[0],
                "kind": "fixture", "payload": {"text": "retain across crashes"}})["message"]
            stable(agents, lease, queued)
            report["steps"].append("two live identities, one exact lease and queued message established")
            if args.previous_binary:
                with closing(sqlite3.connect(home / "state.db")) as source:
                    with closing(sqlite3.connect(output / "pre-upgrade.db")) as backup:
                        source.backup(backup)
                daemon.kill()
                daemon.wait(timeout=10)
                start(current)
                stable(agents, lease, queued)
                report["steps"].append("distinct-binary crash and upgrade preserved identities, lease expiry and inbox")

            for cycle in range(args.cycles):
                with socket.socket(socket.AF_UNIX) as waiting:
                    waiting.settimeout(5)
                    waiting.connect(str(endpoint))
                    waiting.sendall(json.dumps({"op": "ask", "from": agents[0], "to": agents[1],
                        "question": f"continue after crash {cycle}?", "timeout_secs": 300}).encode() + b"\n")
                    pending = eventually(lambda: request(endpoint, {"op": "questions"})["questions"])
                    assert len(pending) == 1
                daemon.kill()
                daemon.wait(timeout=10)
                start(current)
                stable(agents, lease, queued)
                assert request(endpoint, {"op": "questions"})["questions"] == pending, "pending question lost on restart"
                answer = request(endpoint, {"op": "answer", "from": agents[1],
                    "message": pending[0]["id"], "text": f"continue {cycle}"})["message"]
                assert not request(endpoint, {"op": "questions"})["questions"]
                inbox = request(endpoint, {"op": "inbox", "agent": agents[0], "drain": False})["messages"]
                replies = [item for item in inbox if item["id"] == answer]
                assert len(replies) == 1 and replies[0]["reply_to"] == pending[0]["id"]
                assert replies[0]["from"] == agents[1] and replies[0]["payload"]["text"] == f"continue {cycle}"
                request(endpoint, {"op": "ack_inbox", "agent": agents[0], "messages": [answer]})
                report["steps"].append(f"crash {cycle + 1}: pending question restored, correlated reply delivered and acknowledged")
            request(endpoint, {"op": "release", "agent": agents[0], "lease": lease["id"]})
            request(endpoint, {"op": "shutdown"})
            assert daemon.wait(timeout=10) == 0
            assert all(child.poll() is None for child in children)
            report["steps"].append("graceful shutdown preserved externally owned processes")
            if args.previous_binary and report["previous"]["metadata"]["state_schema"] < report["current"]["metadata"]["state_schema"]:
                before = {path.name: digest(path) for path in home.glob("state.db*") if not path.name.endswith("-shm")}
                log = (output / "downgrade.log").open("wb")
                logs.append(log)
                daemon = subprocess.Popen([str(previous)], cwd=root, env=env,
                    stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
                assert daemon.wait(timeout=10) != 0, "older daemon accepted newer schema"
                after = {path.name: digest(path) for path in home.glob("state.db*") if not path.name.endswith("-shm")}
                assert before == after, "refused downgrade changed durable state"
                report["steps"].append("older daemon refused newer schema without changing database or WAL bytes")
            assert digest(current) == report["current"]["sha256"]
            if args.previous_binary:
                assert digest(previous) == report["previous"]["sha256"]
            assert digest(Path(__file__)) == report["driver_sha256"]
            report["result"] = "passed"
        except Exception as error:
            report["error"] = str(error)
        finally:
            if daemon is not None and daemon.poll() is None:
                daemon.kill()
                daemon.wait(timeout=10)
            for child in children:
                if child.poll() is None:
                    child.terminate()
                child.wait(timeout=10)
            for log in logs:
                log.close()
            report["remaining_owned_processes"] = sum(process.poll() is None for process in [*children, *([daemon] if daemon else [])])
            report["duration_seconds"] = time.monotonic() - started
            (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return int(report["result"] != "passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--previous-binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cycles", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.cycles <= 10:
        parser.error("use 1–10 crash cycles")
    raise SystemExit(run(args))
