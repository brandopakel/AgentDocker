#!/usr/bin/env python3
"""Actual-daemon inbox retention and pressure trial using private owned state.

This proves daemon acceptance/replay, not provider processing or idle wake.
"""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

from restart_smoke import digest, eventually, snapshot


def rpc(endpoint, value, *, error=None):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(value).encode() + b"\n")
        with stream.makefile("rb") as reader:
            response = json.loads(reader.readline(8 * 1024 * 1024))
    if error:
        assert response.get("type") == "error" and response.get("code") == error, response
    else:
        assert response.get("type") != "error", response
    return response


def run(args):
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    current = output / "agentd-current"
    report = {
        "result": "failed", "steps": [], "scope": "durable daemon queue, not provider input acceptance",
        "driver_sha256": digest(Path(__file__)),
        "helper_sha256": digest(Path(__file__).with_name("restart_smoke.py")),
        "current": snapshot(args.binary.resolve(), current),
    }
    previous = None
    if args.previous_binary:
        previous = output / "agentd-previous"
        report["previous"] = snapshot(args.previous_binary.resolve(), previous)
        assert report["previous"]["metadata"]["state_schema"] == 9
    assert report["current"]["metadata"]["state_schema"] >= 10
    started = time.monotonic()
    processes = []
    with tempfile.TemporaryDirectory(prefix="ad-queue-", dir="/tmp") as scratch, ExitStack() as resources:
        root = Path(scratch).resolve()
        endpoint, home = root / "sock", root / "state"
        env = {key: value for key, value in os.environ.items() if not key.startswith("AGENTDOCKER_")}
        env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=str(endpoint),
                   AGENTDOCKER_NO_AUTOSTART="1", AGENTDOCKER_NO_NOTIFICATIONS="1")

        def start(binary):
            log = resources.enter_context((output / f"daemon-{len(processes)}.log").open("wb"))
            process = subprocess.Popen([str(binary)], cwd=root, env=env, stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=subprocess.STDOUT)
            processes.append(process)
            eventually(lambda: process.poll() is None and rpc(endpoint, {"op": "ping"})["type"] == "pong")
            return process

        def send(sender, recipient, payload, error=None):
            return rpc(endpoint, {"op": "send", "from": sender, "to": recipient,
                                  "kind": "fixture", "payload": payload}, error=error)

        def inbox(agent):
            return rpc(endpoint, {"op": "inbox", "agent": agent, "drain": False})["messages"]

        def ack(agent, messages):
            rpc(endpoint, {"op": "ack_inbox", "agent": agent, "messages": [item["id"] for item in messages]})

        def subscribe(agent):
            stream = resources.enter_context(socket.socket(socket.AF_UNIX))
            stream.settimeout(5)
            stream.connect(str(endpoint))
            stream.sendall(json.dumps({"op": "subscribe", "agent": agent}).encode() + b"\n")
            reader = resources.enter_context(stream.makefile("rb"))
            return stream, reader

        try:
            daemon = start(previous or current)
            agents = [rpc(endpoint, {"op": "register", "spec": {
                "name": name, "runtime": "fixture", "workdir": str(root)}})["agent"]["id"]
                for name in ["receiver", "peer"]]
            receiver, peer = agents
            large = {"text": "x" * (512 * 1024)}
            if previous:
                for _ in range(8):
                    send("user", receiver, large)
                legacy = inbox(receiver)
                daemon.kill()
                daemon.wait(timeout=10)
                daemon = start(current)
                assert inbox(receiver) == legacy, "upgrade evicted legacy work above the new byte limit"
                send("user", receiver, {"text": "pressure"}, error="backpressure")
                ack(receiver, legacy)
                report["steps"].append("schema 9 crash/upgrade retained an over-limit inbox and resumed after acknowledgement")

            accepted = [send("user", receiver, {"ordinal": 0})["message"]]
            stream, reader = subscribe(receiver)
            first = json.loads(reader.readline())
            assert first["message"]["id"] == accepted[0]
            for ordinal, sender in enumerate([peer, "user", peer, "user"], 1):
                accepted.append(send(sender, receiver, {"ordinal": ordinal})["message"])
            streamed = [json.loads(reader.readline())["message"] for _ in range(4)]
            assert [item["id"] for item in streamed] == accepted[1:]
            queued = inbox(receiver)
            assert [item["id"] for item in queued] == accepted
            assert [item["from"] for item in queued] == ["user", peer, "user", peer, "user"]
            reader.close()
            stream.close()
            accepted.append(send(peer, receiver, {"ordinal": 5})["message"])
            stream, reader = subscribe(receiver)
            assert [json.loads(reader.readline())["message"]["id"] for _ in accepted] == accepted
            reader.close()
            stream.close()
            report["steps"].append("human/peer arrivals retained order during streaming and replayed after disconnect")

            daemon.kill()
            daemon.wait(timeout=10)
            daemon = start(current)
            queued = inbox(receiver)
            assert [item["id"] for item in queued] == accepted
            ack(receiver, queued[:2])
            ack(receiver, queued[:2])
            assert inbox(receiver) == queued[2:]
            ack(receiver, queued[2:])
            report["steps"].append("crash replay preserved IDs; repeated selective acknowledgement preserved later arrivals")

            for ordinal in range(1000):
                send("user", receiver, {"ordinal": ordinal})
            full = inbox(receiver)
            send("user", "all", {"text": "atomic broadcast"}, error="backpressure")
            assert inbox(receiver) == full and inbox(peer) == []
            ack(receiver, full)
            sent = send("user", "all", {"text": "retry after capacity"})["message"]
            for agent in agents:
                messages = inbox(agent)
                assert len(messages) == 1 and messages[0]["id"] == sent
                ack(agent, messages)
            report["steps"].append("full recipient rejected the whole broadcast without eviction; explicit retry reached both recipients")

            for _ in range(7):
                send("user", receiver, large)
            full = inbox(receiver)
            send("user", receiver, large, error="backpressure")
            daemon.kill()
            daemon.wait(timeout=10)
            daemon = start(current)
            send("user", receiver, large, error="backpressure")
            assert inbox(receiver) == full
            ack(receiver, full[:1])
            send("user", receiver, large)
            assert inbox(receiver)[:6] == full[1:]
            report["steps"].append("byte pressure survived crash; acknowledging one item admitted exactly one replacement")

            rpc(endpoint, {"op": "shutdown"})
            assert daemon.wait(timeout=10) == 0
            assert digest(current) == report["current"]["sha256"]
            if previous:
                assert digest(previous) == report["previous"]["sha256"]
            assert digest(Path(__file__)) == report["driver_sha256"]
            assert digest(Path(__file__).with_name("restart_smoke.py")) == report["helper_sha256"]
            report["result"] = "passed"
        except Exception as error:
            report["error"] = str(error)
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
            report["remaining_owned_processes"] = sum(process.poll() is None for process in processes)
            report["duration_seconds"] = time.monotonic() - started
            (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return int(report["result"] != "passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--previous-binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    raise SystemExit(run(parser.parse_args()))
