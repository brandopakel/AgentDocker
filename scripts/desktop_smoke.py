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


def check_no_tcp(processes):
    if not shutil.which("lsof"):
        raise RuntimeError("lsof is required to check the native app's transport")
    for process in processes:
        result = subprocess.run(["lsof", "-nP", "-a", "-p", str(process.pid), "-iTCP"], capture_output=True, text=True)
        if result.returncode != 1 or result.stdout or result.stderr:
            raise RuntimeError("native fixture has a TCP socket or its transport could not be checked")


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
               "AGENTDOCKER_NO_AUTOSTART": "1", "RUST_LOG": "warn"}
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
                    try:
                        if rpc(endpoint, {"op": "ping"}).get("type") == "pong":
                            break
                    except (OSError, ValueError):
                        pass
                    if time.monotonic() > deadline:
                        raise TimeoutError("fixture daemon did not become ready")
                    time.sleep(0.05)
                window = subprocess.Popen([str(binary_dir / "agentdocker-ui"), "--smoke-test", str(output / "capture"),
                                           "--expect-pid", str(fixture.pid)], cwd=project, env=env,
                                          stdin=subprocess.DEVNULL, stdout=window_log, stderr=subprocess.STDOUT)
                # Observe transport while the actual window is alive.
                check_no_tcp([daemon, window])
                if window.wait(timeout=45) != 0:
                    raise RuntimeError("graphical acceptance failed; inspect private window.log")
                report = json.loads((output / "capture/result.json").read_text())
                if report.get("result") != "passed" or not report.get("fixture_discovered"):
                    raise RuntimeError("graphical acceptance did not discover the fixture")
                png = (output / "capture/window.png").read_bytes()
                if png[:8] != b"\x89PNG\r\n\x1a\n" or len(png) < 1000:
                    raise RuntimeError("native renderer produced no usable screenshot")
                report.update({"transport": "unix-socket", "tcp_socket_observation": "none",
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
