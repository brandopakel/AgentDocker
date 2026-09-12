#!/usr/bin/env python3
"""Test the real AgentDocker Claude-channel transport with private daemon state.

The fixture speaks MCP; it is not Claude and does not prove model idle wake.
It verifies idle transport offers, ordered explicit receipts and recovery.
"""
import argparse
from collections import deque
from contextlib import ExitStack
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time
import traceback

from message_queue_smoke import rpc
from restart_smoke import digest, eventually


class Connection:
    def __init__(self, process):
        self.process = process
        self.buffer = bytearray()
        self.offers = deque()

    def send(self, value):
        self.process.stdin.write(json.dumps(value).encode() + b"\n")
        self.process.stdin.flush()

    def read(self, timeout=5):
        deadline = time.monotonic() + timeout
        while True:
            if b"\n" in self.buffer:
                line, _, rest = self.buffer.partition(b"\n")
                self.buffer = bytearray(rest)
                return json.loads(line)
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.process.stdout], [], [], remaining)[0]:
                return None
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError(f"MCP output closed (exit={self.process.poll()})")
            self.buffer.extend(chunk)
            assert len(self.buffer) <= 8 * 1024 * 1024, "fixture frame bound exceeded"

    def response(self, identifier):
        for _ in range(18):
            value = self.read()
            assert value is not None, f"missing response {identifier}"
            if value.get("method") == "notifications/claude/channel":
                self.offers.append(value)
                assert len(self.offers) <= 16
            else:
                assert value.get("id") == identifier, value
                return value
        raise RuntimeError("too many interleaved offers")

    def offer(self):
        value = self.offers.popleft() if self.offers else self.read()
        assert value and value.get("method") == "notifications/claude/channel", value
        return value["params"]

    def ack(self, identifier, messages):
        self.send({"jsonrpc": "2.0", "id": identifier, "method": "tools/call",
                   "params": {"name": "acknowledge_messages", "arguments": {"messages": messages}}})
        response = self.response(identifier)
        assert "error" not in response and not response["result"].get("isError"), response


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
    with tempfile.TemporaryDirectory(prefix="ad-channel-", dir="/tmp") as scratch, ExitStack() as resources:
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

        def start_daemon():
            process = spawn([str(output / "agentd")], env)
            eventually(lambda: process.poll() is None and rpc(endpoint, {"op": "ping"})["type"] == "pong")
            return process

        try:
            daemon = start_daemon()
            human = rpc(endpoint, {"op": "me", "workdir": str(root)})["agent"]["id"]
            receiver, peer = [rpc(endpoint, {"op": "register", "spec": {
                "name": name, "runtime": "claude-code", "workdir": str(root)}})["agent"]["id"]
                for name in ["receiver", "peer"]]
            channel_env = {**env, "AGENTDOCKER_AGENT_ID": receiver, "AGENTDOCKER_CLAUDE_CHANNEL_INPUT": "1"}
            command = [str(output / "agentdocker"), "mcp", "--runtime", "claude-code", "--claude-channel"]

            def send(sender, text):
                return rpc(endpoint, {"op": "send", "from": sender, "to": receiver,
                                      "kind": "chat", "payload": {"text": text}})["message"]

            def queued():
                return [message["id"] for message in rpc(endpoint, {
                    "op": "inbox", "agent": receiver, "drain": False})["messages"]]

            def start_channel(initialize=True):
                connection = Connection(spawn(command, channel_env))
                connection.send({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                                 "params": {"protocolVersion": "2025-06-18"}})
                response = connection.response(0)
                assert response["result"]["capabilities"]["experimental"]["claude/channel"] == {}
                if initialize:
                    connection.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                return connection

            accepted = [send(sender, f"ordinal {index}") for index, sender in enumerate(["user", peer, "user"])]
            connection = start_channel(initialize=False)
            assert connection.read(0.35) is None, "offered before initialization"
            connection.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            first = connection.offer()
            assert first["meta"]["message_id"] == accepted[0] and first["meta"]["from_agent"] == human
            assert json.loads(first["content"])["text"] == "ordinal 0"
            assert queued() == accepted
            assert connection.read(0.35) is None, "advanced without receipt"
            duplicate = spawn(command, channel_env)
            assert duplicate.wait(timeout=5) != 0 and queued() == accepted
            report["steps"].append("channel waited for initialization, retained its offer, bounded delivery to one unacknowledged head and refused a second owner")

            for identifier in range(10, 18):
                resource = f"task:channel-wait-{identifier}"
                held = rpc(endpoint, {"op": "claim", "agent": peer, "resource": resource, "ttl_secs": 120})
                assert held["type"] == "lease" and held["lease"]["holder"] == peer
                connection.send({"jsonrpc": "2.0", "id": identifier, "method": "tools/call",
                                 "params": {"name": "claim", "arguments": {"resource": resource, "wait_secs": 120}}})
            connection.send({"jsonrpc": "2.0", "id": 100, "method": "ping"})
            assert connection.response(100)["result"] == {}
            connection.send({"jsonrpc": "2.0", "id": 101, "method": "tools/call", "params": {"name": "read_inbox"}})
            assert connection.response(101)["error"]["code"] == -32000
            connection.ack(102, accepted[:1])
            assert connection.offer()["meta"]["message_id"] == accepted[1]
            assert queued() == accepted[1:]
            report["steps"].append("eight waiting calls did not block ping or explicit receipt; excess work received visible backpressure")

            connection.process.kill()
            connection.process.wait(timeout=5)
            connection = start_channel()
            assert connection.offer()["meta"]["message_id"] == accepted[1]
            daemon.kill()
            daemon.wait(timeout=5)
            daemon = start_daemon()
            assert queued() == accepted[1:]
            connection.ack(103, accepted[:2])
            assert connection.offer()["meta"]["message_id"] == accepted[2]
            connection.ack(104, accepted[2:])
            assert queued() == []
            report["steps"].append("MCP reconnect replayed the same ID; daemon crash retained order and duplicate receipts preserved later messages")

            connection.send({"jsonrpc": "2.0", "id": 200, "method": "tools/call", "params": {
                "name": "ask_human", "arguments": {"question": "Which fixture route?", "timeout_secs": 120}}})
            posted = json.loads(connection.response(200)["result"]["content"][0]["text"])
            assert posted["posted"] is True and posted["answer_delivery"] == "channel"
            assert queued() == []
            answered = rpc(endpoint, {"op": "answer", "from": human, "message": posted["question_id"], "text": "Blue"})["message"]
            offered = connection.offer()
            assert offered["meta"]["message_id"] == answered
            assert offered["meta"]["reply_to"] == posted["question_id"]
            assert offered["meta"]["from_agent"] == human and json.loads(offered["content"]) == {"text": "Blue"}
            assert queued() == [answered]
            connection.ack(201, [answered])
            assert queued() == [] and connection.read(0.35) is None
            report["steps"].append("a posted human question returns before its answer, which arrives once through the channel with its reply relationship and explicit receipt")

            idle = send(peer, "IDLE-TRANSPORT-OFFER")
            assert connection.offer()["meta"]["message_id"] == idle
            connection.ack(105, [idle])
            assert queued() == []
            connection.process.stdout.close()
            broken = send("user", "retained after broken output")
            assert connection.process.wait(timeout=5) != 0
            assert queued() == [broken]
            connection = start_channel()
            assert connection.offer()["meta"]["message_id"] == broken
            connection.ack(106, [broken])
            connection.process.stdin.close()
            assert connection.process.wait(timeout=5) == 0 and queued() == []
            report["steps"].append("an idle transport offered peer input without another request; broken stdout retained the message for reconnect and receipt")

            connection = start_channel()
            stalled = send(peer, "x" * (512 * 1024))
            # Leave stdout unread and stdin open. A full pipe must not hold the
            # Tokio runtime alive after the five-second write deadline expires.
            assert connection.process.wait(timeout=8) != 0
            assert queued() == [stalled]
            connection = start_channel()
            assert connection.offer()["meta"]["message_id"] == stalled
            connection.ack(107, [stalled])
            connection.process.stdin.close()
            assert connection.process.wait(timeout=5) == 0
            report["steps"].append("an unread full stdout pipe exited within its write bound while stdin stayed open; the oversized-pipe message replayed intact")

            rejected = spawn(command, {**channel_env, "AGENTDOCKER_CLAUDE_CHANNEL_INPUT": "0"})
            assert rejected.wait(timeout=5) != 0
            report["steps"].append("missing parent-session opt-in refused channel startup instead of racing hook delivery")
            rpc(endpoint, {"op": "shutdown"})
            assert daemon.wait(timeout=5) == 0
            assert all(digest(output / name) == value for name, value in hashes.items())
            assert all(digest(path) == source_hashes[path.name] for path in sources)
            report["result"] = "passed"
        except Exception as error:
            report["error"] = f"{type(error).__name__}: {error}"
            frames = [frame for frame in traceback.extract_tb(error.__traceback__) if Path(frame.filename).name == Path(__file__).name]
            report["failed_driver_line"] = frames[-1].lineno if frames else None
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
