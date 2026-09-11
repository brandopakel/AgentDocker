#!/usr/bin/env python3
"""Exercise the actual offline repair CLI and daemon against owned legacy state.

The duplicate is deliberately seeded after daemon shutdown. No provider or user
database is edited; the only provider process is an owned sleeping fixture.
"""
import argparse
from contextlib import closing
import copy
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import time
import uuid

from restart_smoke import digest, eventually, request


def run(args):
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    binaries = {}
    for name in ["agentdocker", "agentd"]:
        source = args.binary_dir.resolve() / name
        target = output / name
        before = digest(source)
        shutil.copyfile(source, target)
        target.chmod(0o500)
        assert digest(target) == digest(source) == before, "binary changed during snapshot"
        binaries[name] = {"sha256": before}
    if args.manifest:
        manifest = json.loads(args.manifest.read_text())
        for name in binaries:
            assert binaries[name]["sha256"] == manifest["binary_sha256"][name]
    else:
        manifest = None
    report = {"result": "failed", "steps": [], "binaries": binaries,
              "driver_sha256": digest(Path(__file__)),
              "helper_sha256": digest(Path(__file__).with_name("restart_smoke.py")),
              "manifest": manifest,
              "scope": "owned synthetic legacy records, actual CLI and daemon; no real provider"}
    processes, logs = [], []
    start = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix="ad-repair-", dir="/tmp") as directory:
            root = Path(directory).resolve()
            home, endpoint = root / "state", root / "custom.sock"
            env = {key: value for key, value in os.environ.items() if not key.startswith("AGENTDOCKER_")}
            env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=str(endpoint),
                       AGENTDOCKER_NO_AUTOSTART="1", AGENTDOCKER_NO_NOTIFICATIONS="1", RUST_LOG="warn")

            def start_daemon():
                log = (output / f"daemon-{len(logs)}.log").open("wb")
                logs.append(log)
                process = subprocess.Popen([str(output / "agentd")], cwd=root, env=env,
                                           stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
                processes.append(process)
                eventually(lambda: process.poll() is None and request(endpoint, {"op": "ping"})["type"] == "pong")
                return process

            daemon = start_daemon()
            host = subprocess.Popen(["sleep", "120"], cwd=root, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            processes.append(host)
            canonical = request(endpoint, {"op": "register", "pid": host.pid, "spec": {
                "name": "canonical-fixture", "runtime": "claude-code", "workdir": str(root),
                "labels": {"session_id": "owned-legacy-session"}}})["agent"]
            kept = canonical["id"]
            assert canonical.get("process_started_at"), "missing actual process birth evidence"
            request(endpoint, {"op": "send", "from": "user", "to": kept,
                               "kind": "fixture",
                               "payload": {"text": "first accepted input"}})
            request(endpoint, {"op": "deregister", "agent": kept})
            host.terminate()
            host.wait(timeout=5)
            request(endpoint, {"op": "shutdown"})
            daemon.wait(timeout=10)
            database = home / "state.db"
            retired = uuid.uuid4().hex
            with closing(sqlite3.connect(database)) as connection, connection:
                row = connection.execute("SELECT json FROM agents WHERE id=?", (kept,)).fetchone()
                duplicate = json.loads(row[0])
                duplicate["id"] = retired
                duplicate["spec"]["name"] = "legacy-transport-fixture"
                duplicate["spec"]["labels"].pop("session_id", None)
                assert duplicate["status"]["state"] not in ["created", "running", "stopping"]
                connection.execute("INSERT INTO agents VALUES(?,?,?,?,?)",
                                   (retired, duplicate["spec"]["name"], 0,
                                    duplicate["created_at"], json.dumps(duplicate)))
                original = json.loads(connection.execute("SELECT json FROM inbox WHERE agent=?", (kept,)).fetchone()[0])
                second = copy.deepcopy(original)
                second.update(id=uuid.uuid4().hex, to={"kind": "agent", "value": retired},
                              payload={"text": "second accepted input", "literal_id": retired})
                connection.execute("INSERT INTO inbox(agent,message_id,json) VALUES(?,?,?)",
                                   (retired, second["id"], json.dumps(second)))
                connection.execute("UPDATE meta SET value='10' WHERE key='schema_version'")
            base = [str(output / "agentdocker"), "identity-repair", "--home", str(home),
                    "--keep", kept, "--retire", retired]

            def repair(*extra, succeeds=True):
                result = subprocess.run(base + list(extra), env=env, cwd=root, capture_output=True, text=True, timeout=10)
                assert (result.returncode == 0) == succeeds, result.stderr
                return json.loads(result.stdout) if succeeds else result.stderr

            before = database.read_bytes()
            plan = repair()
            assert not plan["applied"] and database.read_bytes() == before
            report["steps"].append("read-only CLI preview leaves schema-10 records and database bytes unchanged")
            error = repair("--apply", "0" * 64, succeeds=False)
            assert "changed" in error and database.read_bytes() == before
            report["steps"].append("wrong plan digest refuses without database changes")
            with closing(sqlite3.connect(database)) as idle:
                idle.execute("SELECT COUNT(*) FROM agents").fetchone()
                error = repair("--apply", plan["plan_sha256"], succeeds=False)
                assert "open elsewhere" in error or "locked" in error, error
            report["steps"].append("idle existing database connection blocks apply independently of daemon socket")
            applied = repair("--apply", plan["plan_sha256"])
            assert applied["applied"] and applied["canonical"] == kept
            assert repair("--apply", plan["plan_sha256"])["applied"]
            report["steps"].append("exact plan applies atomically and repeated apply returns the same receipt")
            with closing(sqlite3.connect(database)) as connection:
                assert connection.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone()[0] == "11"
                archive = json.loads(connection.execute("SELECT json FROM documents WHERE kind='identity_reconciliation' AND id=?", (retired,)).fetchone()[0])
                assert archive["before"]["retired"]["id"] == retired
            daemon = start_daemon()
            listing = request(endpoint, {"op": "list", "all": True})
            assert listing["aliases"][retired] == kept
            assert not any(record["id"] == retired for record in listing["agents"])
            assert request(endpoint, {"op": "inspect", "agent": retired})["agent"]["id"] == kept
            for agent in [kept, retired]:
                inbox = request(endpoint, {"op": "inbox", "agent": agent, "drain": False})["messages"]
                assert inbox == [original, second], "messages reordered, rewritten, duplicated or lost"
            report["steps"].append("daemon restart exposes one canonical agent, exact alias and original FIFO messages through both IDs")
            request(endpoint, {"op": "ack_inbox", "agent": retired, "messages": [original["id"], second["id"]]})
            assert request(endpoint, {"op": "inbox", "agent": kept, "drain": False})["messages"] == []
            report["steps"].append("receipt through retired ID acknowledges the canonical inbox")
            request(endpoint, {"op": "shutdown"})
            daemon.wait(timeout=10)
            report["result"] = "passed"
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        for log in logs:
            log.close()
        report["owned_processes_running"] = sum(process.poll() is None for process in processes)
        report["duration_seconds"] = round(time.monotonic() - start, 3)
        (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"result": report["result"], "steps": len(report["steps"]), "report": str(output / "result.json")}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    run(parser.parse_args())
