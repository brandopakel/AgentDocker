#!/usr/bin/env python3
"""Exercise a real native window against an owned, disposable daemon and process.

On Linux run under xvfb-run with a Mesa renderer. Reports contain fixture counts;
local screenshots may show other discovered processes and must remain private.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def rpc(endpoint, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(request).encode() + b"\n")
        with stream.makefile("rb") as reply:
            return json.loads(reply.readline(1024 * 1024))


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


class TransportCheckFailed(RuntimeError):
    """A refused observation, with enough evidence to diagnose the refusal."""

    def __init__(self, observation):
        self.observation = observation
        super().__init__("native fixture transport check failed: " + json.dumps(observation))


def bounded_output(value):
    if isinstance(value, bytes):
        value = value.decode(errors="replace")
    value = value or ""
    return value[:2048] + (" [truncated]" if len(value) > 2048 else "")


def check_no_tcp(processes, deadline, capture=None):
    linux = sys.platform.startswith("linux")
    if not linux and not shutil.which("lsof"):
        raise RuntimeError("lsof is required to check the native app's transport")
    for index, process in enumerate(processes):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("native fixture transport observation timed out")
        method = "linux-proc" if linux else "lsof"
        observation = {"process_index": index, "pid": process.pid, "method": method}
        command = ([sys.executable, str(Path(__file__).with_name("proc_tcp.py")), str(process.pid)]
                   if linux else ["lsof", "-nP", "-a", "-p", str(process.pid), "-iTCP"])
        tcp_reported = False
        try:
            result = subprocess.run(command, capture_output=True, text=True, errors="replace", timeout=min(5, remaining))
        except subprocess.TimeoutExpired as error:
            observation.update(reason=method + " timed out", returncode=None,
                               stdout=bounded_output(error.stdout), stderr=bounded_output(error.stderr))
        else:
            if linux:
                try:
                    report = json.loads(result.stdout)
                except (ValueError, TypeError):
                    report = None
                if (result.returncode == 0 and not result.stderr and isinstance(report, dict)
                        and type(report.get("tcp")) is bool
                        and type(report.get("socket_count")) is int
                        and 0 <= report["socket_count"] <= 4096):
                    if not report["tcp"]:
                        continue
                    tcp_reported = True
                    result = subprocess.CompletedProcess(command, 0, "TCP socket observed by Linux proc tables", "")
                elif process.poll() is not None:
                    # A child can finish between polling and opening its proc
                    # directory. Its exit status is checked by the caller; no
                    # post-exit sample is claimed as a live observation.
                    continue
            else:
                if result.returncode == 1 and not result.stdout and not result.stderr:
                    continue
                tcp_reported = result.returncode == 0 and bool(result.stdout)
            observation.update(reason="TCP socket reported" if tcp_reported else "transport could not be checked",
                               returncode=result.returncode, stdout=bounded_output(result.stdout), stderr=bounded_output(result.stderr))
        observation["process_status"] = process.poll()
        if capture is not None:
            try:
                capture.mkdir(parents=True, exist_ok=True)
                (capture / "transport-failure.json").write_text(json.dumps(observation, indent=2) + "\n")
            except OSError as error:
                observation["capture_error"] = bounded_output(str(error))
        raise TransportCheckFailed(observation)


# The window gives up at WINDOW_DEADLINE and writes down what it was
# still waiting for. This waits longer on purpose, so a run that fails
# fails *there*, with a reason, rather than here, where all that is
# known is that it never exited.
WINDOW_DEADLINE = 60
HARNESS_MARGIN = 30


def wait_window(daemon, window, capture=None, timeout=WINDOW_DEADLINE + HARNESS_MARGIN):
    """Sample both owned processes through GUI readiness and capture, with one deadline."""
    started = time.monotonic()
    deadline = started + timeout
    samples = 0
    while True:
        if daemon.poll() is not None:
            raise RuntimeError("fixture daemon exited during graphical acceptance")
        check_no_tcp([daemon, window], deadline, capture)
        samples += 1
        status = window.poll()
        if status is not None:
            if status != 0:
                # The window wrote why before it exited; say it here
                # rather than pointing at a file the reader may not have.
                reason = "no report was written"
                if capture is not None:
                    try:
                        report = json.loads((capture / "result.json").read_text())
                        reason = report.get("error", reason)
                    except (OSError, ValueError):
                        pass
                raise RuntimeError(f"graphical acceptance failed: {reason}")
            return {"samples": samples, "elapsed_seconds": time.monotonic() - started,
                    "method": ("Linux proc socket-inode polling" if sys.platform.startswith("linux") else "lsof polling") + " through window exit; short-lived sockets between samples may be missed"}
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("native window did not exit before the graphical acceptance deadline")
        time.sleep(min(0.1, remaining))


def smoke(binary_dir, output):
    binary_dir = binary_dir.resolve(strict=True)
    output = output.absolute()
    output.mkdir(mode=0o700)  # fresh evidence only
    with tempfile.TemporaryDirectory(prefix="ad-gui-", dir="/tmp") as scratch:
        root = Path(scratch)
        home, endpoint = root / "state", root / "agentd.sock"
        project = root / "project"
        project.mkdir()
        (project / "Agentfile").write_text('name = "desktop-fixture"\ncommand = ["sleep", "90"]\n')
        runtime = root / "codex"
        runtime.symlink_to(shutil.which("sleep"))
        env = {**os.environ, "AGENTDOCKER_HOME": str(home), "AGENTDOCKER_SOCKET": str(endpoint),
               "AGENTDOCKER_NO_AUTOSTART": "1", "RUST_LOG": os.environ.get("RUST_LOG", "warn")}
        daemon = fixture = window = None
        previous_umask = os.umask(0o077)
        try:
            with (output / "daemon.log").open("w") as daemon_log, (output / "window.log").open("w") as window_log:
                fixture = subprocess.Popen([str(runtime), "90"], cwd=project, stdin=subprocess.DEVNULL,
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                daemon = subprocess.Popen([str(binary_dir / "agentd")], cwd=project, env=env,
                                          stdin=subprocess.DEVNULL, stdout=daemon_log, stderr=subprocess.STDOUT)
                deadline = time.monotonic() + 15
                while True:
                    if daemon.poll() is not None:
                        raise RuntimeError("fixture daemon exited before readiness")
                    check_no_tcp([daemon], deadline, output / "capture")
                    try:
                        if rpc(endpoint, {"op": "ping"}).get("type") == "pong":
                            break
                    except (OSError, ValueError):
                        pass
                    if time.monotonic() > deadline:
                        raise TimeoutError("fixture daemon did not become ready")
                    time.sleep(0.05)
                window = subprocess.Popen([str(binary_dir / "agentdocker-ui"), "--smoke-test", str(output / "capture"),
                                           "--expect-pid", str(fixture.pid),
                                           "--smoke-deadline", str(WINDOW_DEADLINE)], cwd=project, env=env,
                                          stdin=subprocess.DEVNULL, stdout=window_log, stderr=subprocess.STDOUT)
                observation = wait_window(daemon, window, output / "capture")
                report = json.loads((output / "capture/result.json").read_text())
                if report.get("result") != "passed" or not report.get("fixture_discovered"):
                    raise RuntimeError("graphical acceptance did not discover the fixture")
                png = (output / "capture/window.png").read_bytes()
                if png[:8] != b"\x89PNG\r\n\x1a\n" or len(png) < 1000:
                    raise RuntimeError("native renderer produced no usable screenshot")
                report.update({"transport": "unix-socket", "tcp_socket_observation": "none",
                               "tcp_observation": observation,
                               "scope": "native window, daemon connection, runtime inventory, running process discovery"})
                (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
                return report
        finally:
            stop(window)
            if daemon is not None and daemon.poll() is None:
                try:
                    rpc(endpoint, {"op": "shutdown"})
                    daemon.wait(timeout=10)
                except (OSError, ValueError, subprocess.TimeoutExpired):
                    stop(daemon)
            stop(fixture)
            os.umask(previous_umask)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(smoke(args.binary_dir, args.output), indent=2))
