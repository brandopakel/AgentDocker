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


def pin_held(path):
    """Whether somebody holds the pin file: a shared lock refuses an
    exclusive one."""
    import fcntl
    try:
        with open(path, "rb") as handle:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
            fcntl.flock(handle, fcntl.LOCK_UN)
            return False
    except FileNotFoundError:
        return False


def pin_trial(args, root, prefix, source, controller, environment, cli, result):
    """A controller bound to the first release keeps that release across
    two handovers to later generations and a prune that would otherwise
    remove it; unbinding lets it go."""
    home = root / "state"
    sock = root / "daemon.sock"
    store = prefix / ".local/share/agentdocker/desktop"
    first_id = cli("install", "--from", source, "--preview")["candidate"]["id"]
    cli("install", "--from", source, "--expect-release", first_id, "--expect-current", "none")
    binaries = prefix / ".local/bin"
    daemon_env = {**environment, "AGENTDOCKER_EXPERIMENTAL_RELOAD": "1", "RUST_LOG": "info"}
    daemon_log = (args.output / "pin-daemon.log").open("wb")
    daemon = subprocess.Popen([str(binaries / "agentd"), "--home", str(home), "--socket", str(sock)],
                              env=daemon_env, stdin=subprocess.DEVNULL, stdout=daemon_log, stderr=daemon_log,
                              start_new_session=True)
    provider_process = subprocess.Popen(["sleep", "600"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    controller_process = subprocess.Popen(["sleep", "600"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    serving_pid = daemon.pid
    try:
        deadline = time.monotonic() + 15
        while True:
            try:
                pong = rpc(sock, {"op": "ping"})
                assert pong["type"] == "pong"
                break
            except (OSError, AssertionError):
                assert daemon.poll() is None and time.monotonic() < deadline, "fixture daemon did not become ready"
                time.sleep(.05)
        work = root / "pin-work"
        work.mkdir()
        # The launch descriptor lives in the first release: a controller
        # the daemon would start from there, which pins that release.
        first_executable = (store / "versions" / first_id / PAYLOAD / BIN / "agentdocker").resolve()
        assert first_executable.is_file(), first_executable
        provider = rpc(sock, {"op": "register", "spec": {"name": "pin-provider", "workdir": str(work), "labels": {"session_id": "pin-thread"}},
                              "pid": provider_process.pid, "session": None})["agent"]
        probe = rpc(sock, {"op": "register", "spec": {"name": "pin-probe", "workdir": str(work)}, "pid": controller_process.pid, "session": None})["agent"]
        rpc(sock, {"op": "deregister", "agent": probe["id"]})
        bound = rpc(sock, {"op": "bind_input", "agent": provider["id"],
                           "provider": {"process": {"pid": provider_process.pid, "started_at": provider["process_started_at"]},
                                        "session": "pin-thread", "profile": str(work / "profile")},
                           "controller": {"pid": controller_process.pid, "started_at": probe["process_started_at"]},
                           "token": "desktop-reload-pin-trial-token-0123456789",
                           "launch": {"executable": str(first_executable), "args": ["events"], "cwd": str(work),
                                      "env": {"AGENTDOCKER_HOME": str(home), "AGENTDOCKER_SOCKET": str(sock), "AGENTDOCKER_NO_AUTOSTART": "1"}}})
        assert bound["type"] != "error", bound
        pin = store / "pins" / f"{first_id}.lock"
        deadline = time.monotonic() + 10
        while not pin_held(pin):
            assert time.monotonic() < deadline, "the first daemon did not pin the descriptor's release"
            time.sleep(.1)
        # The bound controller (a sleep) stays alive throughout, so the
        # daemon never launches the descriptor: no first-release binary
        # runs, and the only thing that can hold the release is a daemon's
        # pin for the binding. That is the dormant release the trial is
        # about.
        result["scenarios"].append("a bound controller's launch descriptor in the first release pins that release, with the controller alive and nothing launched from it")

        def handover(generation):
            nonlocal serving_pid
            candidate = second_generation(source, root, generation)
            candidate_id = cli("install", "--from", candidate, "--preview")["candidate"]["id"]
            report = cli("install", "--from", candidate, "--expect-release", candidate_id)
            assert report["daemon"]["reloaded"] is True, report["daemon"]
            assert report["daemon"]["serving"]["pid"] != serving_pid
            predecessor = serving_pid
            serving_pid = report["daemon"]["serving"]["pid"]
            # Once the predecessor is gone with its pins, the successor's
            # must be the one holding the release: never a moment with
            # nobody holding it.
            def gone(pid):
                # The first daemon is this trial's child and is reaped here;
                # its successors were reparented and are reaped by init.
                if pid == daemon.pid:
                    return daemon.poll() is not None
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    return True
                return False

            deadline = time.monotonic() + 15
            while not gone(predecessor):
                assert time.monotonic() < deadline, f"predecessor {predecessor} did not leave"
                time.sleep(.05)
            assert pin_held(pin), f"the release pin was dropped across the handover to generation {generation}"
            binding = rpc(sock, {"op": "inspect", "agent": provider["id"]})["agent"]["input_binding"]
            assert binding and not binding.get("restart", {}).get("launched"), f"a controller was launched from the release: {binding}"
            return candidate_id

        second_id = handover(2)
        third_id = handover(3)
        assert len({first_id, second_id, third_id}) == 3
        status = cli("status")
        assert status["installation"]["current"]["id"] == third_id
        assert status["installation"]["previous"]["id"] == second_id
        assert pin_held(pin), "the pin is held under the third generation"
        # Neither current nor previous: a prune would remove the first
        # release, and the pin keeps it.
        plan = cli("prune", "--keep", "0", "--preview")
        removed = [entry.get("id") for entry in plan["maintenance"].get("remove", [])]
        assert first_id not in removed, plan["maintenance"]
        cli("prune", "--keep", "0")
        assert (store / "versions" / first_id).is_dir(), "a pinned release was pruned"
        result["scenarios"].append("with the first release neither current nor previous, a prune keeps it while a controller's binding pins it")
        warnings = [line for line in (args.output / "pin-daemon.log").read_text(errors="replace").splitlines()
                    if " WARN " in line and "pin" in line]
        assert not warnings, warnings
        assert controller_process.poll() is None, "the bound controller ended during the trial"
        # Unbound, the pin goes with the binding, and so does the release.
        # The controller is alive, so the unbind carries its token.
        unbound = rpc(sock, {"op": "unbind_input", "agent": provider["id"], "token": "desktop-reload-pin-trial-token-0123456789"})
        assert unbound["type"] != "error", unbound
        deadline = time.monotonic() + 10
        while pin_held(pin):
            assert time.monotonic() < deadline, "the pin was not released with the binding"
            time.sleep(.1)
        cli("prune", "--keep", "0")
        assert not (store / "versions" / first_id).exists(), "an unpinned, unused release stays"
        result["scenarios"].append("unbinding releases the pin, and the next prune removes the release")
        result["pin_trial"] = {"first": first_id, "second": second_id, "third": third_id, "serving_pid": serving_pid}
    finally:
        # Every step runs whatever the one before it did; a failure or a
        # survivor is recorded and fails the trial.
        cleanup_errors = []

        def attempt(name, action):
            try:
                action()
            except Exception as error:  # noqa: BLE001 - recorded, never swallowed
                cleanup_errors.append(f"{name}: {error!r}")

        def shutdown():
            try:
                rpc(sock, {"op": "shutdown"})
            except OSError:
                pass
            deadline = time.monotonic() + 10
            while sock.exists() and time.monotonic() < deadline:
                time.sleep(.05)
            if sock.exists():
                try:
                    os.killpg(serving_pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass

        def end_daemon():
            if daemon.poll() is None:
                daemon.kill()
            daemon.wait(timeout=10)

        attempt("shutdown", shutdown)
        attempt("daemon", end_daemon)
        attempt("log", daemon_log.close)
        survivors = []
        for name, process in (("provider", provider_process), ("controller", controller_process)):
            def end_child(process=process, name=name):
                if process.poll() is None:
                    process.kill()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        survivors.append(f"{name} {process.pid}")
            attempt(name, end_child)
        try:
            os.kill(serving_pid, 0)
            survivors.append(f"daemon {serving_pid}")
        except (ProcessLookupError, PermissionError):
            pass
        result["pin_cleanup"] = {"errors": cleanup_errors, "survivors": survivors}
        assert not cleanup_errors and not survivors, result["pin_cleanup"]


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

        if args.pin_trial:
            pin_trial(args, root, prefix, source, controller, environment, cli, result)
            result["passed"] = True
            (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(result, indent=2))
            return

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
    parser.add_argument("--pin-trial", action="store_true",
                        help="instead: a bound controller's release stays pinned across two handovers and a prune")
    trial(parser.parse_args())


if __name__ == "__main__":
    main()
