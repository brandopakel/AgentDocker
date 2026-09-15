#!/usr/bin/env python3
"""Successive gated reloads of a private daemon under pressure, recorded.

Runs one release `agentd` beneath a disposable home with the reload gate open,
then, while a batch agent, a PTY agent and two agents printing numbered lines
as fast as they can keep running, reloads it many times in a row with clients
sending numbered messages, launching and stopping short agents throughout.
Afterwards every agent has the same process, every log is complete, every
message arrived once and in order, a question asked before the first reload
is answered after the last, and a lease claimed before keeps its identity and
expiry. Writes result.json under --output; exits non-zero on any failure.
"""
import argparse
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback


def rpc(sock, request, timeout=60):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(str(sock))
        connection.sendall(json.dumps(request).encode() + b"\n")
        with connection.makefile("rb") as reply:
            return json.loads(reply.readline())


def build_info(agentd):
    return json.loads(subprocess.check_output([str(agentd), "--build-info"], text=True))


def trial(args):
    args.output.mkdir(parents=True, exist_ok=True)
    binary_dir = args.binary_dir.resolve(strict=True)
    agentd = binary_dir / "agentd"
    cli = binary_dir / "agentdocker"
    result = {"passed": False, "reloads": args.reloads, "build_info": build_info(agentd), "scenarios": []}
    with tempfile.TemporaryDirectory(prefix="ad-reload-acceptance-") as temporary:
        root = Path(temporary).resolve()
        home = root / "state"
        sock = root / "agentd.sock"
        work = root / "work"
        work.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=work, check=True)
        # The disposable checkout's one commit needs no identity or signing
        # from the caller's configuration.
        subprocess.run(["git", "-c", "user.name=reload-acceptance", "-c", "user.email=reload-acceptance@localhost",
                        "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "init"], cwd=work, check=True)
        env = {**os.environ, "AGENTDOCKER_HOME": str(home), "AGENTDOCKER_SOCKET": str(sock),
               "AGENTDOCKER_NO_AUTOSTART": "1"}
        for key in ["AGENTDOCKER_AGENT_ID", "AGENTDOCKER_TOKEN_FILE", "AGENTDOCKER_RELOAD_CANDIDATE"]:
            env.pop(key, None)
        log = open(args.output / "daemon.log", "ab")
        daemon = subprocess.Popen([str(agentd), "--home", str(home), "--socket", str(sock)],
                                  env={**env, "AGENTDOCKER_EXPERIMENTAL_RELOAD": "1", "RUST_LOG": "info"},
                                  stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        deadline = time.monotonic() + 15
        while True:
            try:
                assert rpc(sock, {"op": "ping"})["type"] == "pong"
                break
            except (OSError, AssertionError):
                assert daemon.poll() is None and time.monotonic() < deadline, "daemon did not start"
                time.sleep(.05)
        try:
            def run(name, script, tty=False):
                r = rpc(sock, {"op": "run", "spec": {"name": name, "workdir": str(work), "tty": tty,
                                                     "command": ["sh", "-c", script]}})
                assert r["type"] == "agent", r
                return r["agent"]

            def inspect(agent):
                r = rpc(sock, {"op": "inspect", "agent": agent})
                assert r["type"] == "agent", r
                return r["agent"]

            # The population under pressure: two loggers printing numbered
            # lines as fast as sh allows, one batch and one PTY fixture that
            # exit with exact codes when released.
            loggers = {}
            for name in ("logger-a", "logger-b"):
                loggers[name] = run(name, f"i=0; while ! test -f {name}-stop; do i=$((i+1)); echo line-$i; done; echo done-$i > {name}-count")
            fixtures = {}
            for name, tty, code in (("batch", False, 7), ("terminal", True, 3)):
                fixtures[name] = {"agent": run(name, f"printf '{name}-before\\n'; while ! test -f {name}-go; do sleep 0.05; done; printf '{name}-after\\n'; exit {code}", tty), "exit": code}
            recipient = rpc(sock, {"op": "register", "spec": {"name": "recipient", "workdir": str(work)}, "pid": None, "session": None})["agent"]
            asker = rpc(sock, {"op": "register", "spec": {"name": "asker", "workdir": str(work)}, "pid": None, "session": None})["agent"]
            lease = rpc(sock, {"op": "claim", "agent": recipient["id"], "resource": "task:kept", "mode": "exclusive", "ttl_secs": 600, "wait_secs": 0})
            assert lease["type"] == "lease", lease
            lease = lease["lease"]
            question = rpc(sock, {"op": "post_question", "from": asker["id"], "to": recipient["id"], "question": "still there after the switches?", "timeout_secs": 3600})
            assert question["type"] == "sent", question
            question_id = question["message"]
            expiry_before = next(q["expires_at"] for q in rpc(sock, {"op": "questions", "agent": None})["questions"] if q["id"] == question_id)
            time.sleep(.5)

            # Concurrent clients through the CLI, so a transferring answer
            # is retried the way every client retries it.
            stop = threading.Event()
            sent = []
            send_errors = []
            launched = []
            launch_errors = []
            stop_errors = []
            worker_failures = []

            def worker(body):
                def run_body():
                    try:
                        body()
                    except Exception:  # noqa: BLE001 - recorded and failed below
                        worker_failures.append(traceback.format_exc())
                return threading.Thread(target=run_body, daemon=True)

            # An inbox holds 1,000 messages; the recipient never acknowledges,
            # so the sender stops short of that and the order check covers
            # everything it sent.
            def sender():
                n = 0
                while not stop.is_set() and n < 900:
                    n += 1
                    p = subprocess.run([str(cli), "send", "--from", asker["id"], "--to", recipient["id"], f"m{n}"],
                                       env=env, capture_output=True, text=True, timeout=120)
                    if p.returncode == 0:
                        sent.append(n)
                    else:
                        send_errors.append((n, p.stderr.strip()[-200:]))
                    time.sleep(.02)

            def launcher():
                n = 0
                while not stop.is_set():
                    n += 1
                    p = subprocess.run([str(cli), "run", "--name", f"short-{n}", "--", "sh", "-c", "sleep 0.2; exit 0"],
                                       env=env, cwd=work, capture_output=True, text=True, timeout=120)
                    if p.returncode == 0:
                        launched.append(p.stdout.strip())
                    else:
                        launch_errors.append((n, p.stderr.strip()[-200:]))
                    # Stop every other one early, so stops race the reloads
                    # too. A stop that finds the agent already gone is not a
                    # failure of the stop; anything else the daemon refuses is.
                    if p.returncode == 0 and n % 2 == 0:
                        stopped = subprocess.run([str(cli), "stop", p.stdout.strip()], env=env, capture_output=True, text=True, timeout=120)
                        if stopped.returncode != 0 and "already finished" not in stopped.stderr:
                            stop_errors.append((n, stopped.stderr.strip()[-200:]))
                    time.sleep(.05)

            threads = [worker(sender), worker(launcher)]
            for t in threads:
                t.start()

            def retired(pid):
                """Whether the process is gone: the original is our child,
                its successors were reparented and are reaped by init."""
                if pid == daemon.pid:
                    return daemon.poll() is not None
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    return True
                return False

            timings = result["reload_seconds"] = []
            pids = result["daemon_pids"] = [daemon.pid]
            retirements = result["predecessor_retired_seconds"] = []
            for i in range(args.reloads):
                started = time.monotonic()
                p = subprocess.run([str(cli), "daemon", "reload"], env=env, capture_output=True, text=True, timeout=180)
                timings.append(round(time.monotonic() - started, 3))
                assert p.returncode == 0, f"reload {i + 1}: {p.stderr}"
                pong = rpc(sock, {"op": "ping"})
                assert pong["type"] == "pong" and pong["pid"] != pids[-1], pong
                # The predecessor leaves once its successor serves; a
                # daemon that stayed would be a second coordinator.
                deadline = time.monotonic() + 15
                while not retired(pids[-1]):
                    assert time.monotonic() < deadline, f"reload {i + 1}: predecessor {pids[-1]} did not retire"
                    time.sleep(.02)
                retirements.append(round(time.monotonic() - started, 3))
                pids.append(pong["pid"])
                for name, agent in loggers.items():
                    current = inspect(agent["id"])
                    assert current["pid"] == agent["pid"] and current["status"]["state"] == "running", (name, current)
                for name, fixture in fixtures.items():
                    current = inspect(fixture["agent"]["id"])
                    assert current["pid"] == fixture["agent"]["pid"] and current["status"]["state"] == "running", (name, current)
                time.sleep(args.pause)
            stop.set()
            for t in threads:
                t.join(timeout=180)
            assert not any(t.is_alive() for t in threads), "a workload worker did not stop"
            assert not worker_failures, worker_failures
            assert not send_errors, send_errors
            assert not launch_errors, launch_errors
            assert not stop_errors, stop_errors
            assert sent and launched, "the workload produced nothing"

            # Everything that was sent arrived once, in order.
            inbox = rpc(sock, {"op": "inbox", "agent": recipient["id"], "drain": False})
            assert inbox["type"] == "messages", inbox
            texts = [m["payload"].get("text") for m in inbox["messages"] if m["from"] == asker["id"] and m["payload"].get("text", "").startswith("m")]
            expected = [f"m{n}" for n in sent]
            assert texts == expected, f"inbox {texts[:5]}..{texts[-5:]} ({len(texts)}) vs sent {len(expected)}"
            result["scenarios"].append(f"{len(sent)} messages sent through {args.reloads} reloads arrived once, in order; {len(send_errors)} send errors")
            result["sends"] = {"delivered": len(sent), "errors": send_errors}

            # Every launch reached a durable end under whichever daemon served.
            deadline = time.monotonic() + 60
            for agent in launched:
                while inspect(agent)["status"]["state"] not in ("exited", "failed", "killed"):
                    assert time.monotonic() < deadline, f"{agent} never finished"
                    time.sleep(.05)
            result["scenarios"].append(f"{len(launched)} agents launched (half stopped early) during the reloads all finished; {len(launch_errors)} launch errors")
            result["launches"] = {"finished": len(launched), "errors": launch_errors}

            # The loggers never lost a line: their logs are contiguous up to
            # the count each wrote when told to stop.
            for name, agent in loggers.items():
                (work / f"{name}-stop").write_text("")
            deadline = time.monotonic() + 30
            for name, agent in loggers.items():
                while inspect(agent["id"])["status"]["state"] != "exited":
                    assert time.monotonic() < deadline, f"{name} did not exit"
                    time.sleep(.05)
                count = int((work / f"{name}-count").read_text().strip().split("-")[1])
                lines = (home / "logs" / f"{agent['id']}.log").read_text(errors="replace").splitlines()
                numbers = [int(m.group(1)) for m in (re.search(r" line-(\d+)$", l) for l in lines) if m]
                assert numbers == list(range(1, count + 1)), f"{name}: {len(numbers)} of {count} lines, first gap near {next((i for i, n in enumerate(numbers, 1) if n != i), None)}"
                result.setdefault("log_lines", {})[name] = count
            result["scenarios"].append("both loggers' logs are contiguous across every reload")

            # The fixtures end with exact exits under the last daemon.
            for name, fixture in fixtures.items():
                (work / f"{name}-go").write_text("")
            deadline = time.monotonic() + 30
            for name, fixture in fixtures.items():
                while True:
                    current = inspect(fixture["agent"]["id"])
                    if current["status"]["state"] == "exited":
                        assert current["status"]["code"] == fixture["exit"], current
                        break
                    assert time.monotonic() < deadline, f"{name} did not exit"
                    time.sleep(.05)
                text = (home / "logs" / f"{fixture['agent']['id']}.log").read_text(errors="replace")
                assert f"{name}-before" in text and f"{name}-after" in text
            result["scenarios"].append("batch and PTY fixtures exit with exact codes under the last daemon with complete logs")

            # The lease and the question crossed every switch untouched.
            leases = rpc(sock, {"op": "leases", "agent": None, "resource": None})["leases"]
            kept = next(l for l in leases if l["id"] == lease["id"])
            assert kept["expires_at"] == lease["expires_at"], (kept, lease)
            expiry_after = next(q["expires_at"] for q in rpc(sock, {"op": "questions", "agent": None})["questions"] if q["id"] == question_id)
            assert expiry_after == expiry_before
            answered = rpc(sock, {"op": "answer", "from": recipient["id"], "message": question_id, "text": "yes"})
            assert answered["type"] in ("sent", "ok"), answered
            asker_inbox = rpc(sock, {"op": "inbox", "agent": asker["id"], "drain": False})["messages"]
            assert any(m.get("reply_to") == question_id and m["payload"].get("text") == "yes" for m in asker_inbox), asker_inbox
            result["scenarios"].append("a lease and a pending question kept their identity and expiry across every switch; the question was answered after the last")

            result["passed"] = True
        except Exception:  # noqa: BLE001 - the record must say what failed
            result["error"] = traceback.format_exc()
            result["passed"] = False
        finally:
            try:
                rpc(sock, {"op": "shutdown"})
            except OSError:
                pass
            deadline = time.monotonic() + 15
            while sock.exists() and time.monotonic() < deadline:
                time.sleep(.05)
            if daemon.poll() is None:
                daemon.kill()
            daemon.wait(timeout=10)
            log.close()
            # Every warning or error any daemon in the chain logged, counted
            # by message with agent ids removed, so the record carries what
            # the daemons said and not only what the clients saw.
            warnings = {}
            for line in (args.output / "daemon.log").read_text(errors="replace").splitlines():
                line = re.sub(r"\x1b\[[0-9;]*m", "", line)
                for level in (" WARN ", " ERROR "):
                    if level in line:
                        key = re.sub(r" agent=\S+", "", line.split(level, 1)[1]).strip()
                        warnings[key] = warnings.get(key, 0) + 1
            result["daemon_log_warnings"] = warnings
    (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--reloads", type=int, default=10)
    parser.add_argument("--pause", type=float, default=0.3, help="seconds between reloads")
    sys.exit(trial(parser.parse_args()))


if __name__ == "__main__":
    main()
