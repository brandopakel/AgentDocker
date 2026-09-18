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
from datetime import datetime, timezone

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
                "name": name, "runtime": "claude-code", "workdir": str(root),
                "labels": {"session_id": "receipt-fixture"} if name == "receiver" else {}},
                "pid": os.getpid() if name == "receiver" else None})["agent"]["id"]
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
            initial_ready = rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]
            assert initial_ready.get("received") is None
            heartbeat_deadline = time.monotonic() + 36
            while True:
                refreshed = rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]
                if refreshed["paused"]:
                    break
                assert time.monotonic() < heartbeat_deadline, "unreceived offer did not expose its delivery pause"
                assert connection.read(0.25) is None, "heartbeat duplicated an unacknowledged offer"
            assert refreshed["reported_at"] > initial_ready["reported_at"]
            assert refreshed.get("received") is None
            assert accepted[0] in refreshed["pause_reason"]
            paused_at = refreshed["reported_at"]
            heartbeat_deadline = time.monotonic() + 36
            while True:
                refreshed = rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]
                assert refreshed["paused"] is True and refreshed.get("received") is None
                if refreshed["reported_at"] > paused_at:
                    break
                assert time.monotonic() < heartbeat_deadline, "missing-receipt pause did not refresh"
                assert connection.read(0.25) is None, "paused channel duplicated an offer"
            assert queued() == accepted
            report["steps"].append("an unreceived offer became durably paused after 30 seconds and stayed paused through refresh without inventing a receipt, replaying input or consuming the queue")
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
            recovered = rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]
            assert recovered["paused"] is False and recovered["received"]["messages"] == accepted[:1]
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
            last_offer = connection.offer()
            assert last_offer["meta"]["message_id"] == accepted[2]
            assert last_offer["meta"]["reply_destination"] == human

            # Actual CLI hook against the actual private daemon. This models
            # Claude's transcript grammar, not a live provider response.
            transcript = root / "receipt-fixture.jsonl"
            def record(kind, uuid, parent, **fields):
                return {"type": kind, "uuid": uuid, "parentUuid": parent,
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "sessionId": "receipt-fixture", "isSidechain": False, **fields}
            def escape(value):
                return value.replace("&", "&amp;").replace('"', "&quot;")
            metadata = " ".join(f'{key}="{escape(value)}"' for key, value in last_offer["meta"].items())
            content = f'<channel source="agentdocker" {metadata}>\n{last_offer["content"]}\n</channel>'
            recorded = [record("user", "input", "previous", isMeta=True,
                               origin={"kind": "channel", "server": "agentdocker"},
                               message={"role": "user", "content": content})]
            padding = ""
            def receipt_hook(write=True):
                if write:
                    transcript.write_text(padding + "\n".join(json.dumps(value) for value in recorded) + "\n" + padding)
                result = subprocess.run([str(output / "agentdocker"), "hook", "claude-code"],
                    env=channel_env, cwd=root, input=json.dumps({"hook_event_name": "PreToolUse",
                    "session_id": "receipt-fixture", "cwd": str(root), "transcript_path": str(transcript),
                    "tool_name": "Bash", "tool_input": {}}).encode(), capture_output=True, timeout=3)
                assert result.returncode == 0, result.stderr.decode()
            receipt_hook()
            assert queued() == accepted[2:], "a channel record without a model response was acknowledged"
            recorded.append(record("attachment", "context", "input"))
            recorded.append(record("assistant", "response", "context", requestId="fixture-request",
                message={"id": "fixture-response", "role": "assistant", "model": "claude-fixture",
                         "content": [{"type": "tool_use", "name": "Bash"}]}))
            # Move the proof outside the fast suffix and first history window.
            # Recovery must advance its private offset across hook invocations.
            padding = (json.dumps({"type": "progress", "padding": "x" * 1024}) + "\n") * 3100
            receipt_hook()
            assert queued() == accepted[2:], "history fixture unexpectedly fit the fast window"
            history_hooks = 1
            while queued() and history_hooks < 8:
                receipt_hook(write=False)
                history_hooks += 1
            assert queued() == [] and history_hooks > 1
            received = rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]
            assert received["received"]["messages"] == accepted[2:]
            assert received["received"]["receipt"]["provider"] == "claude_channel"
            receipt_hook(write=False)
            assert queued() == []
            report["steps"].append("MCP reconnect replayed the same ID; daemon crash retained order and duplicate receipts preserved later messages")
            report["steps"].append("without an explicit model ACK, the real hook retained channel input alone and committed receipt before removing its exact head only after the modeled assistant continuation; repeated hooks were harmless")
            report["history_recovery_hook_calls"] = history_hooks

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

            project_pause = rpc(endpoint, {"op": "send", "from": "user", "to": f"project:{root}",
                "kind": "message", "payload": {"text": "FIXTURE: pause in everyone"}})["message"]
            offered = connection.offer()
            assert offered["meta"]["message_id"] == project_pause
            project = json.loads(offered["meta"]["destination"])["value"]
            assert offered["meta"]["reply_destination"] == f"project:{project}"
            connection.send({"jsonrpc": "2.0", "id": 202, "method": "tools/call", "params": {
                "name": "send_message", "arguments": {"to": offered["meta"]["reply_destination"],
                "reply_to": project_pause, "text": "FIXTURE: response in the same everyone chat"}}})
            reply = json.loads(connection.response(202)["result"]["content"][0]["text"])
            assert reply["sent"] is True
            peer_queue = rpc(endpoint, {"op": "inbox", "agent": peer, "drain": False})["messages"]
            matching = [message for message in peer_queue if message["id"] == reply["message_id"]]
            assert len(matching) == 1 and matching[0]["reply_to"] == project_pause
            assert matching[0]["to"] == {"kind": "project", "value": project}
            connection.ack(203, [project_pause])
            assert queued() == []
            report["steps"].append("project fan-out carried a reply destination that the actual MCP send_message tool routed back to the same everyone conversation with its original message ID")

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
            assert rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]["paused"] is True
            connection = start_channel()
            assert connection.offer()["meta"]["message_id"] == stalled
            connection.ack(107, [stalled])
            receipt = rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]
            assert receipt["paused"] is False and receipt["received"]["messages"] == [stalled]
            assert receipt["received"]["receipt"]["provider"] == "claude_channel"
            assert rpc(endpoint, {"op": "activity", "agent": receiver})["activity"][0]["queued_inputs"] == 0
            connection.process.stdin.close()
            assert connection.process.wait(timeout=5) == 0
            report["steps"].append("an unread full stdout pipe exited within its write bound while stdin stayed open; the oversized-pipe message replayed intact")

            # The same entry under a session launched without the opt-in is
            # the ordinary MCP server: no channel capability, no offer, the
            # queue left for the hooks adapter and the tools, and no owner
            # lock, so a channel session can still start beside it.
            plain = Connection(spawn(command, {**channel_env, "AGENTDOCKER_CLAUDE_CHANNEL_INPUT": "0"}))
            left = send(peer, "for the hooks")
            plain.send({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                        "params": {"protocolVersion": "2025-06-18"}})
            assert "claude/channel" not in plain.response(0)["result"]["capabilities"].get("experimental", {})
            plain.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            assert plain.read(0.5) is None, "a plain server offered a channel message"
            assert queued() == [left]
            assert rpc(endpoint, {"op": "inspect", "agent": receiver})["agent"]["input_delivery"]["paused"] is False
            connection = start_channel()
            assert connection.offer()["meta"]["message_id"] == left, "the plain server held no channel lock"
            connection.ack(108, [left])
            assert queued() == []
            connection.process.stdin.close()
            assert connection.process.wait(timeout=5) == 0
            plain.process.stdin.close()
            assert plain.process.wait(timeout=5) == 0
            report["steps"].append("without the parent-session opt-in the channel entry served the ordinary MCP: no capability, no offer, no owner lock, queue left to hooks")
            if args.resume:
                project = root / "resume-project"
                project.mkdir()
                subprocess.run(["git", "init", "-q", str(project)], check=True)

                def register_life(name, process, session=None):
                    labels = {"session_id": session} if session else {}
                    response = rpc(endpoint, {"op": "register", "spec": {
                        "name": name, "runtime": "claude-code", "workdir": str(project),
                        "labels": labels}, "pid": process.pid})
                    assert response["type"] == "agent", response
                    return response["agent"]["id"]

                def send_to(agent, text):
                    return rpc(endpoint, {"op": "send", "from": "user", "to": agent,
                        "kind": "chat", "payload": {"text": text}})["message"]

                def inbox(agent):
                    return [m["id"] for m in rpc(endpoint, {
                        "op": "inbox", "agent": agent, "drain": False})["messages"]]

                def channel_for(agent, initialize):
                    child_env = {**channel_env, "AGENTDOCKER_AGENT_ID": agent}
                    opened = Connection(spawn(command, child_env))
                    opened.send({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                        "params": {"protocolVersion": "2025-06-18"}})
                    assert "error" not in opened.response(0)
                    if initialize:
                        opened.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    return opened

                for name in ["earlier.txt", "fresh.txt"]:
                    (project / name).write_text(f"{name} fixture\n")
                old_parent = spawn(["sleep", "120"], env)
                canonical = register_life("resume-old", old_parent, "fixture-resumed-session")
                old_reads = rpc(endpoint, {"op": "observe", "agent": canonical, "paths": ["earlier.txt"]})
                assert old_reads["type"] == "reads", old_reads
                old_message = send_to(canonical, "older queued message")
                old_parent.kill(); old_parent.wait(timeout=5)
                retired = rpc(endpoint, {"op": "deregister", "agent": canonical})
                assert retired["type"] == "agent" and retired["agent"]["finished_at"], retired
                new_parent = spawn(["sleep", "120"], env)
                fresh = register_life("resume-new", new_parent)
                assert fresh != canonical
                fresh_reads = rpc(endpoint, {"op": "observe", "agent": fresh, "paths": ["fresh.txt"]})
                assert fresh_reads["type"] == "reads", fresh_reads
                expected_reads = sorted(old_reads["reads"] + fresh_reads["reads"], key=lambda read: read["path"])
                room = rpc(endpoint, {"op": "channel_open", "agent": fresh,
                    "task": "resume membership", "members": [human], "name": "resume-membership"})["channel"]
                # The fixture has consumed its own channel-created notice; it
                # is not part of the older backlog or a model receipt claim.
                assert rpc(endpoint, {"op": "ack_inbox", "agent": fresh,
                    "messages": inbox(fresh)})["type"] == "ok"
                card = rpc(endpoint, {"op": "task_create", "from": fresh,
                    "title": fresh, "acceptance": "preserve this work", "column": "ready"})["task"]
                assert rpc(endpoint, {"op": "task_pull", "agent": fresh, "task": card["id"]})["type"] == "task"
                card = rpc(endpoint, {"op": "task_move", "agent": fresh,
                    "task": card["id"], "column": "done"})["task"]
                early = channel_for(fresh, initialize=False)
                assert register_life("resume-hook", new_parent, "fixture-resumed-session") == canonical
                cards = rpc(endpoint, {"op": "tasks", "project": str(project)})["tasks"]
                restored_card = next(task for task in cards if task["id"] == card["id"])
                assert restored_card == {**card, "assignee": canonical, "created_by": canonical}, restored_card
                assert rpc(endpoint, {"op": "reads", "agent": canonical})["reads"] == expected_reads
                assert rpc(endpoint, {"op": "reads", "agent": fresh})["reads"] == expected_reads
                # The first server still holds its pre-fold agent-ID lock.
                # Only process-generation ownership excludes this second one.
                duplicate = spawn(command, {**channel_env, "AGENTDOCKER_AGENT_ID": canonical})
                try:
                    assert duplicate.wait(timeout=3) != 0
                except subprocess.TimeoutExpired as error:
                    raise AssertionError("second MCP remained running after ID fold; duplicate channel admitted") from error
                early.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                assert early.offer()["meta"]["message_id"] == old_message
                early.ack(400, [old_message])
                assert inbox(canonical) == []
                restored_rooms = rpc(endpoint, {"op": "channels", "project": str(project)})["channels"]
                restored_room = next(channel for channel in restored_rooms if channel["id"] == room["id"])
                assert set(restored_room["members"]) == {canonical, human}, restored_room
                assert restored_room["opened_by"] == canonical and not restored_room.get("closed_at")
                channel_message = send_to("channel:" + room["id"], "resumed channel message")
                assert early.offer()["meta"]["message_id"] == channel_message
                assert inbox(canonical) == [channel_message] and inbox(fresh) == [channel_message]
                early.ack(402, [channel_message])
                assert inbox(canonical) == []
                early.process.stdin.close()
                assert early.process.wait(timeout=5) == 0
                report["steps"].append("pre-initialization session fold preserved observations, open memberships and completed card identity references without changing their text or state, retained one channel owner across different agent IDs, and delivered the older queue plus a new room message through the canonical alias")

                retained = send_to(canonical, "retained older backlog")
                new_parent.kill(); new_parent.wait(timeout=5)
                retired = rpc(endpoint, {"op": "deregister", "agent": canonical})
                assert retired["type"] == "agent" and retired["agent"]["finished_at"], retired
                next_parent = spawn(["sleep", "120"], env)
                next_id = register_life("resume-next", next_parent)
                initialized = channel_for(next_id, initialize=True)
                offered = send_to(next_id, "already offered fresh input")
                assert initialized.offer()["meta"]["message_id"] == offered
                assert register_life("resume-hook-next", next_parent, "fixture-resumed-session") == next_id
                assert inbox(canonical) == [retained] and inbox(next_id) == [offered]
                initialized.ack(401, [offered])
                assert inbox(canonical) == [retained] and inbox(next_id) == []
                initialized.process.stdin.close()
                assert initialized.process.wait(timeout=5) == 0
                next_parent.kill(); next_parent.wait(timeout=5)
                report["steps"].append("an initialized channel was not folded behind its already-offered head; both identities and queues stayed intact without acknowledging older backlog")
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
    parser.add_argument("--resume", action="store_true", help="also check session-fold ordering and channel ownership with fixture provider processes")
    raise SystemExit(run(parser.parse_args()))
