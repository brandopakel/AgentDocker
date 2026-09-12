#!/usr/bin/env python3
"""Exercise native installation/activation/rollback only beneath a disposable prefix.

The second generation uses the same binaries with distinct package metadata,
unless --previous-source supplies a separately built older package. No real
provider configuration, user launchers or existing daemon is changed.
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


def rpc(path, operation):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(2)
        connection.connect(str(path))
        connection.sendall(json.dumps({"op": operation}).encode() + b"\n")
        with connection.makefile("rb") as reply:
            return json.loads(reply.readline())


def trial(args):
    args.output.mkdir(parents=True, mode=0o700)
    result = {"passed": False, "scenarios": [], "synthetic_second_generation": args.previous_source is None}
    with tempfile.TemporaryDirectory(prefix="ad-install-") as temporary:
        root = Path(temporary).resolve()
        prefix = root / "user"
        source = payload(args.source)
        controller = source / BIN / "agentdocker"
        first = payload(args.previous_source) if args.previous_source else source
        second = root / "second" / PAYLOAD
        if MAC:
            subprocess.run(["/usr/bin/ditto", str(source), str(second)], check=True)
        else:
            shutil.copytree(source, second)
        if args.previous_source is None:
            metadata = json.loads((second / META).read_text())
            metadata["installation_acceptance_generation"] = 2
            (second / META).write_text(json.dumps(metadata, indent=2) + "\n")
            if MAC:
                subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(second)], check=True)
        environment = {**os.environ, "AGENTDOCKER_HOME": str(root / "state"),
                       "AGENTDOCKER_SOCKET": str(root / "daemon.sock"), "AGENTDOCKER_NO_AUTOSTART": "1"}
        for key in ["AGENTDOCKER_AGENT_ID", "AGENTDOCKER_TOKEN_FILE"]:
            environment.pop(key, None)

        def cli(*arguments, success=True):
            command = [str(controller), "desktop", "--prefix", str(prefix), *map(str, arguments)]
            if arguments[0] in {"install", "rollback"} and MAC:
                command.append("--local-preview")
            output = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=120)
            with (args.output / "operations.jsonl").open("a") as log:
                log.write(json.dumps({"argv": command, "exit": output.returncode,
                                      "stdout": output.stdout, "stderr": output.stderr}) + "\n")
            if success:
                assert output.returncode == 0, output.stderr
                return json.loads(output.stdout)
            assert output.returncode != 0, "unexpected successful operation"

        daemon = None
        try:
            assert cli("status")["installation"] is None
            preview = cli("install", "--from", first, "--preview")
            assert not prefix.exists(), "read-only preview created an installation"
            first_id = preview["candidate"]["id"]
            cli("install", "--from", first, "--expect-release", first_id, "--expect-current", "none")
            result["scenarios"].append("preview without writes and pinned initial installation")
            root_install = prefix / ".local/share/agentdocker/desktop"
            assert root_install.stat().st_mode & 0o777 == 0o700
            assert (root_install / "current/activation.json").stat().st_mode & 0o777 == 0o600
            binaries = prefix / ".local/bin"
            with (args.output / "daemon.log").open("wb") as daemon_log:
                daemon = subprocess.Popen([str(binaries / "agentd"), "--home", environment["AGENTDOCKER_HOME"],
                                           "--socket", environment["AGENTDOCKER_SOCKET"]], env=environment,
                                          stdin=subprocess.DEVNULL, stdout=daemon_log, stderr=daemon_log,
                                          start_new_session=True)
                deadline = time.monotonic() + 15
                while True:
                    try:
                        assert rpc(environment["AGENTDOCKER_SOCKET"], "ping")["type"] == "pong"
                        break
                    except (OSError, AssertionError):
                        assert daemon.poll() is None and time.monotonic() < deadline, "fixture daemon did not become ready"
                        time.sleep(.05)
                preview = cli("install", "--from", second, "--preview")
                second_id = preview["candidate"]["id"]
                assert second_id != first_id
                cli("install", "--from", second, "--expect-release", second_id, "--expect-current", first_id)
                assert cli("status")["installation"]["current"]["id"] == second_id
                assert daemon.poll() is None
                assert rpc(environment["AGENTDOCKER_SOCKET"], "ping")["type"] == "pong"
                result["scenarios"].append("activation keeps the existing fixture daemon responsive")
                if MAC:
                    # A real bundle named AgentDocker, not a symlink: Launchpad
                    # and Spotlight only list bundles they can read in place.
                    launcher = prefix / "Applications/AgentDocker.app"
                    assert launcher.is_dir() and not launcher.is_symlink()
                    plist = (launcher / "Contents/Info.plist").read_text()
                    assert "<key>CFBundleName</key><string>AgentDocker</string>" in plist
                    script = (launcher / "Contents/MacOS/AgentDocker").read_text()
                    assert str(root_install / "current/payload/Contents/MacOS/agentdocker-ui") in script
                    assert (launcher / "Contents/Resources/managed-launcher.json").is_file()
                    result["scenarios"].append("the Mac launcher is a named bundle that runs the active payload")
                cli("install", "--from", first, "--expect-current", first_id, success=False)
                cli("install", "--from", first, "--expect-release", second_id, success=False)
                assert cli("status")["installation"]["current"]["id"] == second_id
                result["scenarios"].append("changed preview payload and active generation are refused")
                rollback = cli("rollback", "--preview")
                assert rollback["candidate"]["id"] == first_id
                cli("rollback", "--expect-release", first_id, "--expect-current", second_id)
                assert cli("status")["installation"]["current"]["id"] == first_id
                for name in ["agentdocker", "agentd", "agentdocker-ui"]:
                    assert (binaries / name).resolve() == root_install / "versions" / first_id / PAYLOAD / BIN / name
                assert (root_install / "versions" / second_id).is_dir()
                result["scenarios"].append("compatible rollback switches all commands and retains both releases")
                with (second / BIN / "agentdocker").open("ab") as executable:
                    executable.write(b"corrupt fixture\n")
                cli("install", "--from", second, success=False)
                assert cli("status")["installation"]["current"]["id"] == first_id
                result["scenarios"].append("tampered executable is refused without changing activation")
                if not MAC:
                    subprocess.run(["desktop-file-validate", str(prefix / ".local/share/applications/agentdocker.desktop")], check=True)
                if json.loads((source / META).read_text()).get("installation_lock") == 1:
                    # Real binaries, synthetic metadata generations: make the
                    # running first release neither current nor rollback.
                    extra_ids = []
                    for generation in [3, 4]:
                        extra = root / f"generation-{generation}" / PAYLOAD
                        if MAC:
                            subprocess.run(["/usr/bin/ditto", str(source), str(extra)], check=True)
                        else:
                            shutil.copytree(source, extra)
                        metadata = json.loads((extra / META).read_text())
                        metadata["installation_acceptance_generation"] = generation
                        (extra / META).write_text(json.dumps(metadata, indent=2) + "\n")
                        if MAC:
                            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(extra)], check=True)
                        installed = cli("install", "--from", extra)
                        extra_ids.append(installed["candidate"]["id"])
                    cleanup = cli("prune", "--preview")
                    second_path = root_install / "versions" / second_id
                    assert cleanup["maintenance"]["remove"] == [str(second_path)]
                    first_retained = next(entry for entry in cleanup["maintenance"]["retained"]
                                          if Path(entry["path"]).name == first_id)
                    pinned = json.loads((first / META).read_text()).get("installation_lock") == 1
                    assert first_retained["reason"] == ("running process uses this version" if pinned
                                                        else "older release has no lifetime pin contract")
                    cli("prune", "--expect-plan", cleanup["plan_id"])
                    assert not second_path.exists()
                    assert rpc(environment["AGENTDOCKER_SOCKET"], "ping")["type"] == "pong"
                    result["scenarios"].append("cleanup retains active, rollback and running or legacy binaries")
                    removal = cli("uninstall", "--preview")
                    cli("uninstall", "--expect-plan", removal["plan_id"])
                    assert cli("status")["installation"] is None
                    assert all(not (binaries / name).is_symlink()
                               for name in ["agentdocker", "agentd", "agentdocker-ui"])
                    assert not (prefix / "Applications/AgentDocker.app").exists()
                    assert rpc(environment["AGENTDOCKER_SOCKET"], "ping")["type"] == "pong"
                    assert Path(environment["AGENTDOCKER_HOME"]).is_dir()
                    result["scenarios"].append("uninstall preserves the live daemon and its state")
                    before_exit = cli("prune", "--preview")
                    rpc(environment["AGENTDOCKER_SOCKET"], "shutdown")
                    daemon.wait(timeout=10)
                    if pinned:
                        cli("prune", "--expect-plan", before_exit["plan_id"], success=False)
                        result["scenarios"].append("daemon exit releases its pin and invalidates stale cleanup preview")
                    after_exit = cli("prune", "--preview")
                    cli("prune", "--expect-plan", after_exit["plan_id"])
                    remaining = {entry.name for entry in (root_install / "versions").iterdir()}
                    assert remaining == (set() if pinned else {first_id})
                    assert all((root_install / "pins" / f"{identity}.lock").exists()
                               for identity in [second_id, *extra_ids])
                    assert Path(environment["AGENTDOCKER_HOME"]).is_dir()
                    result["scenarios"].append("unused payloads removed; permanent pins and daemon state retained")
                result["first"] = preview["previous"]
                result["second"] = preview["candidate"]
                result["passed"] = True
        finally:
            if daemon is not None and daemon.poll() is None:
                try:
                    rpc(environment["AGENTDOCKER_SOCKET"], "shutdown")
                    daemon.wait(timeout=10)
                except (OSError, subprocess.TimeoutExpired):
                    os.killpg(daemon.pid, signal.SIGTERM)
                    daemon.wait(timeout=10)
            (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            for path in args.output.iterdir():
                path.chmod(0o600)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--previous-source", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    print(json.dumps(trial(parser.parse_args()), indent=2))
