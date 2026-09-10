#!/usr/bin/env python3
"""Exercise real MCP stdio receipt boundaries using isolated daemon state.

This is transport acceptance, not an actual model or provider idle-wake trial.
"""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time

from message_queue_smoke import rpc
from restart_smoke import digest, eventually


def write(process, value):
    process.stdin.write(json.dumps(value).encode() + b"\n")
    process.stdin.flush()


def call(process, value):
    write(process, value)
    deadline, data = time.monotonic() + 5, bytearray()
    while time.monotonic() < deadline:
        if select.select([process.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError("MCP closed before its response")
            data.extend(chunk)
            assert len(data) <= 8 * 1024 * 1024, "MCP response exceeded fixture limit"
            if data.endswith(b"\n"):
                response = json.loads(data)
                assert response.get("id") == value["id"], "unexpected MCP response ID"
                return response
    raise RuntimeError("MCP response deadline exceeded")


def tool(process, name, arguments=None):
    response = call(process, {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                             "params": {"name": name, "arguments": arguments or {}}})
    assert "error" not in response and not response["result"].get("isError"), response
    return json.loads(response["result"]["content"][0]["text"])


def run(args):
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    hashes = {}
    for name in ["agentd", "agentdocker"]:
        source, target = args.binary_dir.resolve() / name, output / name
        before = digest(source)
        shutil.copyfile(source, target)
        target.chmod(0o500)
        assert before == digest(source) == digest(target)
        hashes[name] = before
    sources = [Path(__file__), Path(__file__).with_name("message_queue_smoke.py"),
               Path(__file__).with_name("restart_smoke.py")]
    source_hashes = {path.name: digest(path) for path in sources}
    report = {"result": "failed", "scope": __doc__, "steps": [], "binary_sha256": hashes,
              "driver_sha256": source_hashes}
    processes, started = [], time.monotonic()
    with tempfile.TemporaryDirectory(prefix="ad-mcp-receipt-", dir="/tmp") as scratch, ExitStack() as resources:
        root = Path(scratch).resolve()
        endpoint = root / "sock"
        env = {key: value for key, value in os.environ.items() if not key.startswith("AGENTDOCKER_")}
        env.update(AGENTDOCKER_HOME=str(root / "state"), AGENTDOCKER_SOCKET=str(endpoint),
                   AGENTDOCKER_NO_AUTOSTART="1", AGENTDOCKER_NO_NOTIFICATIONS="1")

        def spawn(argv, child_env):
            log = resources.enter_context((output / f"process-{len(processes)}.log").open("wb"))
            process = subprocess.Popen(argv, cwd=root, env=child_env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=log, bufsize=0)
            processes.append(process)
            resources.callback(process.stdin.close)
            resources.callback(process.stdout.close)
            return process

        try:
            daemon = spawn([str(output / "agentd")], env)
            eventually(lambda: daemon.poll() is None and rpc(endpoint, {"op": "ping"})["type"] == "pong")
            receiver = rpc(endpoint, {"op": "register", "spec": {
                "name": "receiver", "runtime": "fixture", "workdir": str(root)}})["agent"]["id"]
            peer_env = {**env, "AGENTDOCKER_AGENT_ID": receiver}

            def start_mcp():
                process = spawn([str(output / "agentdocker"), "mcp"], peer_env)
                response = call(process, {"jsonrpc": "2.0", "id": 0, "method": "initialize",
                                         "params": {"protocolVersion": "2025-06-18"}})
                assert "acknowledge_messages" in response["result"]["instructions"]
                return process

            def send(text):
                return rpc(endpoint, {"op": "send", "from": "user", "to": receiver,
                                      "kind": "chat", "payload": {"text": text}})["message"]

            def queued():
                return [message["id"] for message in rpc(endpoint, {
                    "op": "inbox", "agent": receiver, "drain": False})["messages"]]

            accepted = [send("first"), send("second")]
            process = start_mcp()
            assert [message["id"] for message in tool(process, "wait_for_messages", {"timeout_secs": 0})["messages"]] == accepted
            assert queued() == accepted
            process.kill()
            process.wait(timeout=5)
            process = start_mcp()
            assert [message["id"] for message in tool(process, "read_inbox")["messages"]] == accepted
            report["steps"].append("wait/read results retained IDs across MCP process death and reconnect")

            accepted.append(send("newer arrival"))
            for _ in range(2):
                tool(process, "acknowledge_messages", {"messages": accepted[:1]})
            assert queued() == accepted[1:]
            rejected = call(process, {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                                     "params": {"name": "acknowledge_messages",
                                                "arguments": {"messages": accepted[1:], "agent": "someone-else"}}})
            assert rejected["error"]["code"] == -32602 and queued() == accepted[1:]
            report["steps"].append("selective repeated receipt preserved newer arrivals and refused a foreign-agent argument")

            process.stdout.close()
            write(process, {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "read_inbox"}})
            process.wait(timeout=5)
            assert queued() == accepted[1:]
            process = start_mcp()
            assert [message["id"] for message in tool(process, "read_inbox")["messages"]] == accepted[1:]
            tool(process, "acknowledge_messages", {"messages": accepted[1:]})
            assert queued() == []
            process.stdin.close()
            assert process.wait(timeout=5) == 0
            report["steps"].append("broken MCP output retained messages for explicit receipt after reconnect")

            first, later = send("CLI receipt"), send("keep after CLI receipt")
            displayed = subprocess.run([str(output / "agentdocker"), "inbox", "--as", receiver],
                                       cwd=root, env=env, check=True, capture_output=True,
                                       text=True, timeout=5).stdout
            assert first in displayed and later in displayed, "plain inbox output omitted acknowledgement IDs"
            assert queued() == [first, later], "displaying IDs consumed messages"
            subprocess.run([str(output / "agentdocker"), "inbox", "--as", receiver, "--ack", first],
                           cwd=root, env=env, check=True, capture_output=True, timeout=5)
            assert queued() == [later]
            rejected = subprocess.run([str(output / "agentdocker"), "inbox", "--as", receiver,
                                       "--ack", later, "--drain"], cwd=root, env=env,
                                      capture_output=True, timeout=5)
            assert rejected.returncode != 0 and queued() == [later]
            report["steps"].append("plain CLI inbox exposed receipt IDs without consuming; selective acknowledgement retained a later arrival and refused simultaneous drain")
            rpc(endpoint, {"op": "shutdown"})
            assert daemon.wait(timeout=5) == 0
            assert all(digest(output / name) == value for name, value in hashes.items())
            assert all(digest(path) == source_hashes[path.name] for path in sources)
            report["result"] = "passed"
        except Exception as error:
            report["error"] = str(error)
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
            report["remaining_owned_processes"] = sum(process.poll() is None for process in processes)
            report["duration_seconds"] = time.monotonic() - started
            (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return int(report["result"] != "passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    raise SystemExit(run(parser.parse_args()))
