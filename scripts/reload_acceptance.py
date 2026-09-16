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


def cleanup(result, sock, daemon, log, provider_process, controller_process, observed_launched):
    """End everything the trial started, each step whatever the one before
    it did, and say what went wrong: a survivor or a cleanup failure fails
    the trial rather than the trial passing over it."""
    cleanup_errors = []

    def attempt(name, action):
        try:
            action()
        except Exception:  # noqa: BLE001 - recorded, never swallowed
            cleanup_errors.append(f"{name}: {traceback.format_exc().strip().splitlines()[-1]}")

    def shutdown():
        try:
            rpc(sock, {"op": "shutdown"})
        except OSError:
            pass
        deadline = time.monotonic() + 15
        while sock.exists() and time.monotonic() < deadline:
            time.sleep(.05)

    def end_daemon():
        if daemon.poll() is None:
            daemon.kill()
        daemon.wait(timeout=10)

    attempt("shutdown", shutdown)
    attempt("daemon", end_daemon)
    attempt("log", log.close)
    survivors = []

    def end_child(name, process):
        if process is not None and process.poll() is None:
            process.kill()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                survivors.append(f"{name} {process.pid}")

    attempt("provider", lambda: end_child("provider", provider_process))
    attempt("controller", lambda: end_child("controller", controller_process))
    # Every process this trial saw a daemon start, whether or not the run
    # got as far as summarising them.
    seen = set(result.get("daemon_pids", []))
    seen.update(observed_launched)
    for pid in sorted(seen):
        try:
            os.kill(pid, 0)
        except (ProcessLookupError, PermissionError):
            continue
        survivors.append(f"pid {pid}")
    result["survivors"] = survivors
    result["cleanup_errors"] = cleanup_errors
    if survivors or cleanup_errors:
        result["passed"] = False
        result["error"] = (result.get("error") or "") + f"\ncleanup: survivors {survivors}, errors {cleanup_errors}"


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
        # From here everything the trial starts is ended in `finally`,
        # whatever happens, and counted: a survivor fails the trial rather
        # than the trial passing over it. The readiness wait is inside too,
        # so a daemon that never answers is still shut down.
        provider_process = None
        controller_process = None
        observed_launched = set()
        try:
            deadline = time.monotonic() + 15
            while True:
                try:
                    assert rpc(sock, {"op": "ping"})["type"] == "pong"
                    break
                except (OSError, AssertionError):
                    assert daemon.poll() is None and time.monotonic() < deadline, "daemon did not start"
                    time.sleep(.05)
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

            # A bound input whose controller ends just before the reloads:
            # the binding's launch descriptor starts a controller that ends
            # on its own, so the restart episode (immediate, 2, 4, 8, 16 s)
            # runs across the handovers and must be continued by each
            # successor, never restarted or doubled, until it is exhausted.
            provider_process = subprocess.Popen(["sleep", "600"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            controller_process = subprocess.Popen(["sleep", "600"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            result["trial_processes"] = {"provider": provider_process.pid, "controller": controller_process.pid}
            provider = rpc(sock, {"op": "register", "spec": {"name": "provider", "workdir": str(work), "labels": {"session_id": "trial-thread"}},
                                  "pid": provider_process.pid, "session": None})["agent"]
            # The daemon knows a process's birth exactly; a throwaway record
            # is how a script learns the controller's.
            probe = rpc(sock, {"op": "register", "spec": {"name": "controller-probe", "workdir": str(work)}, "pid": controller_process.pid, "session": None})["agent"]
            rpc(sock, {"op": "deregister", "agent": probe["id"]})
            bound = rpc(sock, {"op": "bind_input", "agent": provider["id"],
                               "provider": {"process": {"pid": provider_process.pid, "started_at": provider["process_started_at"]},
                                            "session": "trial-thread", "profile": str(work / "profile")},
                               "controller": {"pid": controller_process.pid, "started_at": probe["process_started_at"]},
                               "token": "reload-acceptance-controller-token-0123456789",
                               "launch": {"executable": "/bin/sh", "args": ["-c", "sleep 2; exit 1"], "cwd": str(work), "env": {}}})
            assert bound["type"] != "error", bound
            controller_process.kill()
            controller_process.wait()
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
                # Whatever controller the daemons have launched so far is
                # noted as seen, so cleanup knows it even if the run stops.
                launched_now = (inspect(provider["id"])["input_binding"] or {}).get("restart", {}).get("launched")
                if launched_now:
                    observed_launched.add(launched_now["pid"])
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

            # The restart episode ran to exhaustion under whichever daemon
            # served: five launches with strictly increasing attempt numbers
            # and distinct processes, each earlier one gone before the next,
            # at least one handover accepted between the first and the last,
            # and the record says exhausted.
            deadline = time.monotonic() + 120
            while True:
                binding = inspect(provider["id"])["input_binding"]
                if binding and binding.get("restart", {}).get("exhausted"):
                    break
                assert time.monotonic() < deadline, f"the restart episode did not exhaust: {binding}"
                time.sleep(.2)
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(5)
                connection.connect(str(sock))
                connection.sendall(json.dumps({"op": "events", "replay": 20000}).encode() + b"\n")
                events = []
                with connection.makefile("rb") as stream:
                    try:
                        for line in stream:
                            events.append(json.loads(line))
                    except (socket.timeout, OSError):
                        pass
            kinds = [(e["event"].get("seq", 0), e["event"]["kind"]) for e in events if e.get("type") == "event" and isinstance(e.get("event", {}).get("kind"), dict)]
            launches = [(seq, k) for seq, k in kinds if k.get("event") == "input_controller_launched" and k.get("agent") == provider["id"]]
            ends = [(seq, k) for seq, k in kinds if k.get("event") == "input_controller_ended" and k.get("agent") == provider["id"]]
            accepted = [seq for seq, k in kinds if k.get("event") == "daemon_transfer_accepted"]
            attempts = [k["attempt"] for _, k in launches]
            assert attempts == list(range(1, len(attempts) + 1)) and len(attempts) == 5, f"attempts {attempts}"
            launched_pids = [k["controller"]["pid"] for _, k in launches]
            observed_launched.update(launched_pids)
            assert len(set(launched_pids)) == len(launched_pids), launched_pids
            # Each launched controller was noted ended before the next launch.
            for (seq, _), pid in zip(launches[1:], launched_pids[:-1]):
                assert any(end_seq < seq and k["controller"]["pid"] == pid for end_seq, k in ends), f"launch at {seq} before {pid} was noted ended"
            assert any(launches[0][0] < seq < launches[-1][0] for seq in accepted), "no handover was accepted during the episode"
            assert binding["restart"]["attempts"] == 5 and not binding["restart"].get("launched"), binding
            result["scenarios"].append(f"a bound controller's restart episode ran to exhaustion across the handovers: 5 launches, attempts {attempts}, {sum(launches[0][0] < seq < launches[-1][0] for seq in accepted)} handovers accepted in between")
            result["controller_episode"] = {"attempts": attempts, "launched_pids": launched_pids, "handovers_during": sum(launches[0][0] < seq < launches[-1][0] for seq in accepted)}
            # Every launched controller has ended on its own by now.
            for pid in launched_pids:
                assert retired(pid), f"launched controller {pid} is still running"

            result["passed"] = True
        except Exception:  # noqa: BLE001 - the record must say what failed
            result["error"] = traceback.format_exc()
            result["passed"] = False
        finally:
            cleanup(result, sock, daemon, log, provider_process, controller_process, observed_launched)
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
