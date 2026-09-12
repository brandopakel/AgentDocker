#!/usr/bin/env python3
"""Bounded 1/10/100 supervised-agent trials with isolated IPC and retained evidence.

Measures the daemon separately from synthetic children; no provider or user
daemon is involved. Long runs are opt-in through --seconds (per population).
"""
import argparse
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import shutil
import socket
import sqlite3
import statistics
import subprocess
import tempfile
import threading
import time


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save_report(path, report):
    """Replace one owned report atomically; previous evidence survives a failed write."""
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w") as stream:
        json.dump(report, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def stop_daemon(daemon, endpoint):
    """Reap an exited child before considering a signal; retain all cleanup failures."""
    cleanup = {"forced": False, "errors": []}
    if daemon.poll() is not None:
        cleanup["exit"] = daemon.returncode
        return cleanup
    try:
        rpc(endpoint, {"op": "shutdown"})
    except (OSError, ValueError, RuntimeError) as error:
        cleanup["errors"].append(f"shutdown: {type(error).__name__}: {error}")
    try:
        # A disappearing socket can mean graceful shutdown is already in flight.
        daemon.wait(timeout=30)
    except subprocess.TimeoutExpired:
        cleanup["forced"] = True
        if daemon.poll() is None:
            try:
                # The unreaped owned leader still reserves the process group.
                os.killpg(daemon.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError) as error:
                cleanup["errors"].append(f"signal: {type(error).__name__}: {error}")
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired as error:
            cleanup["errors"].append(f"wait: {error}")
    cleanup["exit"] = daemon.returncode
    return cleanup


def rpc(endpoint, request):
    started = time.monotonic()
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(request).encode() + b"\n")
        with stream.makefile("rb") as reader:
            response = json.loads(reader.readline(4 * 1024 * 1024))
    if response.get("type") == "error":
        raise RuntimeError(request["op"] + ": " + json.dumps(response))
    return response, (time.monotonic() - started) * 1000


def sample(pid, state):
    values = subprocess.check_output(["ps", "-p", str(pid), "-o", "rss=,%cpu="], text=True, timeout=5).split()
    fds = None
    if platform.system() == "Linux":
        fds = len(list(Path(f"/proc/{pid}/fd").iterdir()))
    elif platform.system() == "Darwin":
        data = subprocess.check_output(["lsof", "-nP", "-p", str(pid), "-Ff"], text=True, timeout=5)
        fds = sum(line.startswith("f") and line[1:].isdigit() for line in data.splitlines())
    with closing(sqlite3.connect(f"file:{state / 'state.db'}?mode=ro", uri=True, timeout=5)) as db:
        counts = {table: db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                  for table in ["agents", "inbox", "leases", "events", "journal"]}
    return {"at": time.monotonic(), "rss_kib": int(values[0]), "cpu_percent": float(values[1]),
            "descriptors": fds, "state_bytes": sum(p.stat().st_size for p in state.rglob("*") if p.is_file()),
            "rows": counts}


def population(binary, output, count, seconds, files, interrupted=None):
    interrupted = interrupted if interrupted is not None else threading.Event()
    report = {"agents": count, "requested_seconds": seconds, "checkout_files": files,
              "result": "failed", "samples": [], "scope": "supervised sleep processes plus concurrent IPC clients"}
    with tempfile.TemporaryDirectory(prefix="ad-soak-", dir="/tmp") as scratch:
        root = Path(scratch).resolve()
        state, endpoint, checkout = root / "state", root / "sock", root / "checkout"
        checkout.mkdir()
        for index in range(files):
            folder = checkout / str(index // 1000)
            folder.mkdir(exist_ok=True)
            (folder / f"input-{index}.txt").write_text("original\n")
        env = {key: value for key, value in os.environ.items() if not key.startswith("AGENTDOCKER_")}
        env.update(AGENTDOCKER_HOME=str(state), AGENTDOCKER_SOCKET=str(endpoint),
                   AGENTDOCKER_NO_AUTOSTART="1", RUST_LOG="warn,agentd_state_timing=debug")
        daemon = None
        child_pids = []
        stop_workers = threading.Event()
        latencies = deque(maxlen=100_000)
        cycles = 0
        sample_lock = threading.Lock()
        started = time.monotonic()
        save_report(output / f"population-{count}.json", report)
        try:
            with (output / f"daemon-{count}.log").open("wb") as log:
                daemon = subprocess.Popen([str(binary)], cwd=checkout, env=env,
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
                deadline = time.monotonic() + 20
                while True:
                    try:
                        if rpc(endpoint, {"op": "ping"})[0]["type"] == "pong":
                            break
                    except (OSError, ValueError):
                        pass
                    if daemon.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("fixture daemon startup failed")
                    time.sleep(0.05)
                agents = []
                for index in range(count):
                    if interrupted.is_set():
                        break
                    reply, _ = rpc(endpoint, {"op": "run", "spec": {"name": f"soak-{index}",
                        "runtime": "fixture", "workdir": str(checkout),
                        "command": ["/bin/sleep", str(seconds + 300)]}})
                    agents.append(reply["agent"]["id"])
                    child_pids.append(reply["agent"]["pid"])
                report["startup_seconds"] = time.monotonic() - started
                deadline = time.monotonic() + seconds

                def worker(agent):
                    nonlocal cycles
                    index = 0
                    while not stop_workers.is_set() and not interrupted.is_set() and time.monotonic() < deadline:
                        durations = []
                        token = {"sequence": index, "agent": agent}
                        sent, duration = rpc(endpoint, {"op": "send", "from": agent,
                            "to": agent, "kind": "fixture", "payload": token})
                        durations.append(duration)
                        peek, duration = rpc(endpoint, {"op": "inbox", "agent": agent, "drain": False})
                        durations.append(duration)
                        assert len(peek["messages"]) == 1 and peek["messages"][0]["id"] == sent["message"], "inbox message lost or duplicated"
                        assert peek["messages"][0]["payload"] == token, "inbox payload changed"
                        _, duration = rpc(endpoint, {"op": "ack_inbox", "agent": agent, "messages": [sent["message"]]})
                        durations.append(duration)
                        lease, duration = rpc(endpoint, {"op": "claim", "agent": agent,
                            "resource": "task:soak-" + agent, "ttl_secs": 30})
                        durations.append(duration)
                        _, duration = rpc(endpoint, {"op": "release", "agent": agent, "lease": lease["lease"]["id"]})
                        durations.append(duration)
                        with sample_lock:
                            latencies.extend(durations)
                            cycles += 1
                        index += 1
                        stop_workers.wait(0.25)

                with ThreadPoolExecutor(max_workers=count) as pool:
                    workers = [pool.submit(worker, agent) for agent in agents]
                    try:
                        while time.monotonic() < deadline and not interrupted.is_set():
                            for worker_result in workers:
                                if worker_result.done():
                                    worker_result.result()
                            current = sample(daemon.pid, state)
                            current["at"] -= started
                            report["samples"].append(current)
                            with sample_lock:
                                current["completed_cycles"] = cycles
                            # Keep completed samples even after a hard interruption
                            # or a later exception in cleanup/final serialization.
                            with (output / f"samples-{count}.jsonl").open("a") as sample_log:
                                sample_log.write(json.dumps(current) + "\n")
                                sample_log.flush()
                            interrupted.wait(min(5, max(0, deadline - time.monotonic())))
                        for worker_result in workers:
                            worker_result.result()
                    finally:
                        stop_workers.set()
                for agent in agents:
                    assert not rpc(endpoint, {"op": "inbox", "agent": agent, "drain": False})[0]["messages"]
                    assert not rpc(endpoint, {"op": "leases", "agent": agent})[0]["leases"]
                    record = rpc(endpoint, {"op": "inspect", "agent": agent})[0]["agent"]
                    assert record["status"]["state"] == "running", "supervised child unexpectedly stopped"
                report["result"] = "interrupted" if interrupted.is_set() else "passed"
        except Exception as error:
            report["error"] = str(error)
        finally:
            stop_workers.set()
            if daemon is not None:
                try:
                    report["cleanup"] = stop_daemon(daemon, endpoint)
                    if report["cleanup"]["forced"]:
                        report["result"] = "failed"
                except Exception as error:
                    report["cleanup_error"] = f"{type(error).__name__}: {error}"
                    report["result"] = "failed"
            report["daemon_exit"] = daemon.returncode if daemon else None
            survivors = []
            for pid in child_pids:
                try:
                    os.kill(pid, 0)
                    survivors.append(pid)
                except ProcessLookupError:
                    pass
                except PermissionError:
                    survivors.append(pid)
            report["remaining_child_pids"] = survivors
            if survivors or report["daemon_exit"] != 0:
                report["result"] = "failed"
            report["duration_seconds"] = time.monotonic() - started
            report["interrupted"] = interrupted.is_set()
            report["cycles"] = cycles
            ordered = sorted(latencies)
            report["latency_sample_count"] = len(ordered)
            report["latency_sample_scope"] = "last at most 100000 requests; all cycles counted"
            report["request_ms"] = {str(q): ordered[min(len(ordered)-1, int((len(ordered)-1)*q))]
                                     for q in [0.5, 0.95, 0.99, 1]} if ordered else {}
            if report["samples"]:
                rss = [s["rss_kib"] for s in report["samples"]]
                report["daemon_rss_kib"] = {"first": rss[0], "last": rss[-1], "maximum": max(rss), "median": statistics.median(rss)}
                report["maximum_descriptors"] = max((s["descriptors"] for s in report["samples"] if s["descriptors"] is not None), default=None)
            save_report(output / f"population-{count}.json", report)
    return report


class Interruption:
    """The signal handler only records the first signal; it takes no locks."""
    def __init__(self):
        self.signum = None

    def record(self, signum, _frame):
        if self.signum is None:
            self.signum = signum

    def is_set(self):
        return self.signum is not None

    def wait(self, timeout):
        deadline = time.monotonic() + timeout
        while not self.is_set():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(0.05, remaining))
        return self.is_set()


def finish_report(path, report, interrupted):
    # A signal may arrive while the final atomic write is in progress. The
    # state changes at most once, so one additional write is sufficient.
    while True:
        signum = interrupted.signum
        report["interrupted"] = signum is not None
        if signum is not None:
            report["signal"] = {"number": signum, "name": signal.Signals(signum).name}
            if report["result"] != "failed":
                report["result"] = "interrupted"
        save_report(path, report)
        if interrupted.signum == signum:
            break
    return 130 if report["result"] == "interrupted" else int(report["result"] != "passed")


def run(args, interrupted):
    os.umask(0o077)
    args.output.mkdir(mode=0o700)
    original = args.binary.resolve(strict=True)
    original_digest = digest(original)
    # Cargo may rebuild the shared executable while a long trial is running.
    # Every population must execute the same bytes, independent of that cache.
    binary = args.output.resolve() / "agentd-tested"
    shutil.copyfile(original, binary)
    binary.chmod(0o500)
    if digest(binary) != original_digest or digest(original) != original_digest:
        raise RuntimeError("input executable changed while the trial snapshot was created")
    report = {"binary_sha256": original_digest, "driver_sha256": digest(Path(__file__)),
              "started_at": datetime.now(timezone.utc).isoformat(),
              "platform": platform.platform(), "result": "running", "populations": []}
    save_report(args.output / "result.json", report)
    for count in args.agents:
        if interrupted.is_set():
            report["result"] = "interrupted"
            break
        print(f"starting {count} supervised agents for {args.seconds} seconds", flush=True)
        result = population(binary, args.output, count, args.seconds, args.files, interrupted)
        report["populations"].append(result)
        save_report(args.output / "result.json", report)
        print(json.dumps({key: result.get(key) for key in ["agents", "result", "error", "cycles", "request_ms", "daemon_rss_kib"]}), flush=True)
        if result["result"] != "passed":
            report["result"] = result["result"]
            break
    else:
        report["result"] = "passed"
    report["binary_unchanged"] = digest(binary) == report["binary_sha256"]
    report["driver_unchanged"] = digest(Path(__file__)) == report["driver_sha256"]
    if not report["binary_unchanged"] or not report["driver_unchanged"]:
        report["result"] = "failed"
    return finish_report(args.output / "result.json", report, interrupted)


def main(args):
    interrupted = Interruption()
    old_handlers = {}
    old_mask = os.umask(0o077)
    try:
        for signum in [signal.SIGINT, signal.SIGTERM]:
            old_handlers[signum] = signal.signal(signum, interrupted.record)
        return run(args, interrupted)
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
        os.umask(old_mask)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=300)
    parser.add_argument("--agents", type=int, nargs="+", default=[1, 10, 100])
    parser.add_argument("--files", type=int, default=1000)
    args = parser.parse_args()
    if not 5 <= args.seconds <= 24 * 3600 or any(n not in [1, 10, 100] for n in args.agents) or not 1 <= args.files <= 100_000:
        parser.error("use 5–86400 seconds, populations 1/10/100, and 1–100000 files")
    raise SystemExit(main(args))
