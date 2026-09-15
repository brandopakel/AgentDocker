#!/usr/bin/env python3
"""Install, update and roll back a managed desktop beneath a disposable prefix
while a gated daemon serves from it, and check that each activation hands the
daemon over to the release just activated without stopping its agent.

The second generation is the same binaries with distinct package metadata, as
in desktop_install_smoke.py. Nothing outside the prefix and the private state
directory is touched; no real daemon is used.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

MAC = sys.platform == "darwin"
PAYLOAD = "AgentDocker.app" if MAC else "agentdocker-desktop"
BIN = Path("Contents/MacOS") if MAC else Path("bin")
META = Path("Contents/Resources/build.json") if MAC else Path("build.json")


def payload(source):
    source = source.resolve(strict=True)
    return source / PAYLOAD if (source / PAYLOAD).is_dir() else source


def rpc(path, request):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(40)
        connection.connect(str(path))
        connection.sendall(json.dumps(request).encode() + b"\n")
        with connection.makefile("rb") as reply:
            return json.loads(reply.readline())


def second_generation(source, root, generation):
    copy = root / f"generation-{generation}" / PAYLOAD
    if MAC:
        subprocess.run(["/usr/bin/ditto", str(source), str(copy)], check=True)
    else:
        shutil.copytree(source, copy)
    metadata = json.loads((copy / META).read_text())
    metadata["installation_acceptance_generation"] = generation
    (copy / META).write_text(json.dumps(metadata, indent=2) + "\n")
    if MAC:
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(copy)], check=True)
    return copy


def trial(args):
    args.output.mkdir(parents=True, mode=0o700)
    result = {"passed": False, "scenarios": []}
    with tempfile.TemporaryDirectory(prefix="ad-reload-install-") as temporary:
        root = Path(temporary).resolve()
        prefix = root / "user"
        source = payload(args.source)
        controller = source / BIN / "agentdocker"
        second = second_generation(source, root, 2)
        home = root / "state"
        sock = root / "daemon.sock"
        environment = {**os.environ, "AGENTDOCKER_HOME": str(home), "AGENTDOCKER_SOCKET": str(sock),
                       "AGENTDOCKER_NO_AUTOSTART": "1"}
        for key in ["AGENTDOCKER_AGENT_ID", "AGENTDOCKER_TOKEN_FILE", "AGENTDOCKER_RELOAD_CANDIDATE"]:
            environment.pop(key, None)

        def cli(*arguments):
            command = [str(controller), "desktop", "--prefix", str(prefix), *map(str, arguments)]
            if arguments[0] in {"install", "rollback"} and MAC:
                command.append("--local-preview")
            output = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=120)
            with (args.output / "operations.jsonl").open("a") as log:
                log.write(json.dumps({"argv": command, "exit": output.returncode,
                                      "stdout": output.stdout, "stderr": output.stderr}) + "\n")
            assert output.returncode == 0, output.stderr
            return json.loads(output.stdout)

        def serving_release(report_daemon):
            executable = Path(report_daemon["serving"]["executable"])
            versions = (prefix / ".local/share/agentdocker/desktop/versions").resolve()
            relative = executable.resolve().relative_to(versions)
            return relative.parts[0]

        first_id = cli("install", "--from", source, "--preview")["candidate"]["id"]
        installed = cli("install", "--from", source, "--expect-release", first_id, "--expect-current", "none")
        assert installed["daemon"]["answered"] is False, installed["daemon"]
        result["scenarios"].append("with no daemon running, activation says so and asks nothing")

        # The daemon serves from the first release, through the launcher
        # link, with the reload gate open.
        binaries = prefix / ".local/bin"
        with (args.output / "daemon.log").open("wb") as daemon_log:
            daemon = subprocess.Popen([str(binaries / "agentd"), "--home", str(home), "--socket", str(sock)],
                                      env={**environment, "AGENTDOCKER_EXPERIMENTAL_RELOAD": "1", "RUST_LOG": "info"},
                                      stdin=subprocess.DEVNULL, stdout=daemon_log, stderr=daemon_log,
                                      start_new_session=True)
            deadline = time.monotonic() + 15
            while True:
                try:
                    pong = rpc(sock, {"op": "ping"})
                    assert pong["type"] == "pong"
                    break
                except (OSError, AssertionError):
                    assert daemon.poll() is None and time.monotonic() < deadline, "fixture daemon did not become ready"
                    time.sleep(.05)
            assert pong["pid"] == daemon.pid
            assert Path(pong["executable"]).resolve().parts[-4:][0] == first_id or first_id in str(Path(pong["executable"]).resolve())
            work = root / "work"
            work.mkdir()
            agent = rpc(sock, {"op": "run", "spec": {"name": "keeper", "workdir": str(work),
                                                     "command": ["sh", "-c", "while :; do sleep 1; done"]}})
            assert agent["type"] == "agent", agent
            keeper = agent["agent"]
            # Whoever serves after each handoff, so cleanup can reach it
            # even when the shutdown request itself fails.
            serving_pid = daemon.pid
            try:
                # Install the second generation: the daemon hands over to it.
                second_id = cli("install", "--from", second, "--preview")["candidate"]["id"]
                assert second_id != first_id
                report = cli("install", "--from", second, "--expect-release", second_id, "--expect-current", first_id)
                assert report["daemon"]["reloaded"] is True, report["daemon"]
                assert serving_release(report["daemon"]) == second_id, report["daemon"]
                assert report["daemon"]["before"]["pid"] == daemon.pid
                assert report["daemon"]["serving"]["pid"] != daemon.pid
                serving_pid = report["daemon"]["serving"]["pid"]
                deadline = time.monotonic() + 10
                while daemon.poll() is None:
                    assert time.monotonic() < deadline, "the first daemon did not leave"
                    time.sleep(.05)
                status = cli("status")
                assert status["installation"]["current"]["id"] == second_id
                assert status["daemon"]["pid"] == report["daemon"]["serving"]["pid"]
                inspected = rpc(sock, {"op": "inspect", "agent": keeper["id"]})
                assert inspected["agent"]["pid"] == keeper["pid"] and inspected["agent"]["status"]["state"] == "running", inspected
                result["scenarios"].append("installing a release reloads the running daemon to it; its agent keeps its process")
                result["install_report_daemon"] = report["daemon"]

                # Roll back: the same schema, so the daemon hands over again,
                # to the first release.
                rollback = cli("rollback", "--expect-release", first_id, "--expect-current", second_id)
                assert rollback["daemon"]["reloaded"] is True, rollback["daemon"]
                assert serving_release(rollback["daemon"]) == first_id, rollback["daemon"]
                assert rollback["daemon"]["before"]["pid"] == report["daemon"]["serving"]["pid"]
                serving_pid = rollback["daemon"]["serving"]["pid"]
                inspected = rpc(sock, {"op": "inspect", "agent": keeper["id"]})
                assert inspected["agent"]["pid"] == keeper["pid"] and inspected["agent"]["status"]["state"] == "running", inspected
                result["scenarios"].append("rolling back reloads the daemon to the previous release; its agent keeps its process")
                result["rollback_report_daemon"] = rollback["daemon"]

                # The daemon's own status names the release it serves.
                status_text = subprocess.run([str(controller), "daemon", "status"], env=environment,
                                             capture_output=True, text=True, timeout=30).stdout
                assert f"pid {rollback['daemon']['serving']['pid']}" in status_text, status_text
                assert first_id in status_text, status_text
                result["daemon_status"] = status_text
                result["scenarios"].append("daemon status names the serving pid and executable")
            finally:
                try:
                    rpc(sock, {"op": "shutdown"})
                except OSError:
                    pass
                deadline = time.monotonic() + 10
                while sock.exists() and time.monotonic() < deadline:
                    time.sleep(.05)
                if sock.exists():
                    # The serving successor did not stop on request: end its
                    # own process group (each successor starts in one) so
                    # nothing outlives this step.
                    try:
                        os.killpg(serving_pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
                if daemon.poll() is None:
                    daemon.kill()
                daemon.wait(timeout=10)
        result["passed"] = True
    (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="a built desktop payload or its parent directory")
    parser.add_argument("--output", type=Path, required=True)
    trial(parser.parse_args())


if __name__ == "__main__":
    main()
