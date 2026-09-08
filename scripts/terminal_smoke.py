#!/usr/bin/env python3
"""Exercise packaged CLI/daemon PTYs in private disposable state.

Uses real terminals for the CLI and child, including a keyboard kept open when
the child exits. Raw output and fixture identities remain private; result.json
contains only aggregate checks and binary hashes. This does not drive GUI input.
"""
import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import pty
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid


FIXTURE = r'''
import os, signal, sys, termios
settings = termios.tcgetattr(0)
settings[3] &= ~termios.ECHO
termios.tcsetattr(0, termios.TCSANOW, settings)
def resized(*_):
    size = os.get_terminal_size(0)
    os.write(1, f"RESIZED:{size.columns}:{size.lines}\n".encode())
signal.signal(signal.SIGWINCH, resized)
sys.stdout.write("READY>")
sys.stdout.flush()
for command in sys.stdin:
    command = command.rstrip("\n")
    if command.startswith("echo:"):
        sys.stdout.write("\x1b[31mECHO:" + command[5:] + "\x1b[0m\n")
    elif command == "size":
        size = os.get_terminal_size(0)
        sys.stdout.write(f"SIZE:{size.columns}:{size.lines}\n")
    elif command == "noise":
        for n in range(2048):
            sys.stdout.write(f"ROW:{n:04d}:" + "x" * 56 + "\n")
        sys.stdout.write("NOISE-DONE\n")
    elif command == "exit":
        sys.stdout.write("GOODBYE\n")
        sys.stdout.flush()
        break
    else:
        raise RuntimeError("unexpected fixture command")
    sys.stdout.flush()
'''


def rpc(endpoint, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(request).encode() + b"\n")
        with stream.makefile("rb") as reader:
            reply = json.loads(reader.readline(1024 * 1024))
    if reply.get("type") == "error":
        raise RuntimeError("fixture request rejected: " + str(reply))
    return reply


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def identity(pid):
    observed = subprocess.run(["ps", "-p", str(pid), "-o", "lstart=,comm="],
                              capture_output=True, text=True, timeout=5)
    if observed.returncode == 1 and not observed.stdout.strip() and not observed.stderr:
        return None
    if observed.returncode or observed.stderr:
        raise RuntimeError("owned process identity could not be checked")
    return observed.stdout.strip()


class Attached:
    def __init__(self, binary, agent, env, project, log):
        self.master, self.slave = pty.openpty()
        self.before = termios.tcgetattr(self.slave)
        self.resize(96, 28)
        self.log = log
        self.pending = bytearray()
        self.total_bytes = 0
        self.process = None
        try:
            self.process = subprocess.Popen(
                [str(binary), "attach", agent], cwd=project, env=env,
                stdin=self.slave, stdout=self.slave, stderr=self.slave,
                start_new_session=True,
            )
        except BaseException:
            os.close(self.master)
            os.close(self.slave)
            raise

    def resize(self, cols, rows):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        if getattr(self, "process", None) is not None:
            self.process.send_signal(signal.SIGWINCH)

    def send(self, data):
        # Inputs in this bounded campaign fit within one small terminal write.
        if os.write(self.master, data) != len(data):
            raise RuntimeError("incomplete fixture keyboard write")

    def read(self, timeout):
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return
        try:
            data = os.read(self.master, 16384)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            data = b""
        self.total_bytes += len(data)
        if self.total_bytes > 512 * 1024:
            raise RuntimeError("fixture output exceeded 512 KiB budget")
        self.log.write(data)
        self.log.flush()
        self.pending.extend(data)

    def until(self, marker, timeout=10):
        deadline = time.monotonic() + timeout
        while marker not in self.pending:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("terminal marker deadline: " + repr(marker))
            self.read(min(remaining, 0.1))
        end = self.pending.index(marker) + len(marker)
        seen = bytes(self.pending[:end])
        del self.pending[:end]
        return seen

    def exited(self):
        # Keep both ends of the keyboard PTY open. Closing stdin here would hide
        # an attach process stranded in a blocking read after daemon End.
        deadline = time.monotonic() + 5
        while self.process.poll() is None:
            if time.monotonic() >= deadline:
                raise TimeoutError("CLI did not exit while its keyboard remained open")
            self.read(0.05)
        if self.process.returncode != 0:
            raise RuntimeError("attached CLI exited unsuccessfully")
        before, after = self.before.copy(), termios.tcgetattr(self.slave)
        if platform.system() == "Darwin":
            # XNU sets PENDIN when changing back to canonical input. A standalone
            # Python raw-mode round trip reproduces this exact status-bit change.
            # Compare every configurable attribute, including ECHO/ICANON/ISIG.
            before[3] &= ~termios.PENDIN
            after[3] &= ~termios.PENDIN
        if after != before:
            raise RuntimeError("CLI did not restore the terminal settings")

    def close(self):
        stop(self.process)
        os.close(self.master)
        os.close(self.slave)


def smoke(binary_dir, output, source):
    if platform.system() not in {"Darwin", "Linux"}:
        raise RuntimeError("this campaign requires native Unix PTYs")
    binary_dir = binary_dir.resolve(strict=True)
    previous_umask = os.umask(0o077)
    try:
        return _smoke(binary_dir, output, source)
    finally:
        os.umask(previous_umask)


def _smoke(binary_dir, output, source):
    output = output.absolute()
    output.mkdir(mode=0o700)
    root = None
    daemon = attached = None
    owned = {}
    checks = []
    report = {"source_commit": source, "checks": checks, "result": "failed"}
    started = time.monotonic()
    try:
        root = Path(tempfile.mkdtemp(prefix="ad-pty-", dir="/tmp"))
        project, state, endpoint = root / "project", root / "state", root / "d.sock"
        project.mkdir()
        (project / "Agentfile.toml").write_text('name = "terminal-fixture"\ncommand = ["sleep", "90"]\n')
        fixture = project / "fixture.py"
        fixture.write_text(FIXTURE)
        env = {"HOME": str(root / "home"), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
               "AGENTDOCKER_HOME": str(state), "AGENTDOCKER_SOCKET": str(endpoint),
               "AGENTDOCKER_NO_AUTOSTART": "1", "RUST_LOG": "warn"}
        Path(env["HOME"]).mkdir()
        label = "terminal-" + uuid.uuid4().hex
        report.update({"driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                       "os": platform.system(), "os_version": platform.release(), "architecture": platform.machine(),
                       "scope": "real packaged CLI and daemon PTYs; no GUI input automation or provider integration",
                       "binaries": {name: {"bytes": (binary_dir / name).stat().st_size,
                                           "sha256": hashlib.sha256((binary_dir / name).read_bytes()).hexdigest()}
                                    for name in ["agentd", "agentdocker"]}})
        with (output / "daemon.log").open("w") as log, (output / "terminal.raw").open("wb") as terminal_log:
            daemon = subprocess.Popen([str(binary_dir / "agentd")], cwd=project, env=env,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + 15
            while True:
                if daemon.poll() is not None:
                    raise RuntimeError("fixture daemon exited before readiness")
                try:
                    if rpc(endpoint, {"op": "ping"}).get("type") == "pong":
                        break
                except (OSError, ValueError):
                    pass
                if time.monotonic() >= deadline:
                    raise TimeoutError("fixture daemon readiness deadline")
                time.sleep(0.05)
            agent = rpc(endpoint, {"op": "run", "spec": {"name": label, "runtime": "fixture",
                        "command": [sys.executable, "-u", str(fixture)], "workdir": str(project),
                        "labels": {"fixture": label}, "tty": True, "restore": False}})["agent"]
            owned[agent["pid"]] = identity(agent["pid"])
            if owned[agent["pid"]] is None:
                raise RuntimeError("created child identity unavailable")
            (output / "owned-private.json").write_text(json.dumps({"root": str(root), "agent": agent}, indent=2) + "\n")
            attached = Attached(binary_dir / "agentdocker", agent["id"], env, project, terminal_log)
            attached.until(b"READY>")
            checks.append("attach_handshake_and_no_newline_output")
            attached.send("echo:fixture-\u03bb\n".encode())
            attached.until("\x1b[31mECHO:fixture-\u03bb\x1b[0m".encode())
            checks.append("unicode_input_and_ansi_output")
            attached.send(b"size\n")
            attached.until(b"SIZE:96:28")
            checks.append("initial_terminal_dimensions")
            attached.resize(111, 37)
            # Acknowledgement comes from the actual child. No fixed sleep and no
            # repeated size request can disguise a missing SIGWINCH propagation.
            attached.until(b"RESIZED:111:37")
            attached.send(b"size\n")
            attached.until(b"SIZE:111:37")
            checks.append("sigwinch_resize_reaches_child")
            attached.send(b"noise\n")
            attached.until(b"NOISE-DONE")
            checks.append("noisy_output_above_replay_capacity")
            attached.send(b"\x1d")
            attached.until(b"it is still running")
            attached.exited()
            attached.close()
            attached = None
            live = rpc(endpoint, {"op": "list", "labels": {"fixture": label}})["agents"]
            if len(live) != 1 or live[0]["id"] != agent["id"] or live[0]["pid"] != agent["pid"]:
                raise RuntimeError("detaching changed the fixture process")
            checks.append("ctrl_bracket_detaches_and_restores_terminal_without_stopping_child")
            attached = Attached(binary_dir / "agentdocker", agent["id"], env, project, terminal_log)
            replay = attached.until(b"NOISE-DONE")
            if b"READY>" in replay or len(replay) > 66 * 1024:
                raise RuntimeError("reattach did not preserve the bounded output tail")
            checks.append("reattach_replays_tail_with_one_live_child")
            attached.send(b"exit\n")
            attached.until(b"GOODBYE")
            attached.until(b" ended")
            attached.exited()
            checks.append("natural_exit_closes_cli_with_keyboard_open_and_restores_terminal")
            report["result"] = "passed"
    except Exception as error:
        (output / "failure-private.txt").write_text(repr(error) + "\n")
        report["failure_class"] = type(error).__name__
    finally:
        cleanup_errors = []
        if attached is not None:
            try:
                attached.close()
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
        if daemon is not None and daemon.poll() is None:
            try:
                for record in rpc(endpoint, {"op": "list", "labels": {"fixture": label}})["agents"]:
                    if record.get("pid"):
                        owned.setdefault(record["pid"], identity(record["pid"]))
                    rpc(endpoint, {"op": "stop", "agent": record["id"]})
                deadline = time.monotonic() + 10
                while rpc(endpoint, {"op": "list", "labels": {"fixture": label}})["agents"]:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("fixture child stop deadline")
                    time.sleep(0.05)
                rpc(endpoint, {"op": "shutdown"})
                daemon.wait(timeout=10)
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
        try:
            stop(daemon)
        except Exception as error:
            cleanup_errors.append(type(error).__name__)
        for pid, created in owned.items():
            try:
                if created is not None and identity(pid) == created:
                    os.kill(pid, signal.SIGTERM)
                    cleanup_errors.append("owned_child_survived_normal_stop")
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
        if not cleanup_errors and root is not None:
            try:
                shutil.rmtree(root)
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
        report["elapsed_seconds"] = time.monotonic() - started
        report["cleanup"] = {"daemon_exited": daemon is None or daemon.poll() is not None,
                             "created_children": len(owned), "errors": cleanup_errors,
                             "fixture_removed": root is None or not root.exists()}
        if cleanup_errors:
            report["result"] = "failed"
        (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source", required=True)
    args = parser.parse_args()
    result = smoke(args.binary_dir, args.output, args.source)
    print(json.dumps(result, indent=2))
    sys.exit(0 if result["result"] == "passed" else 1)
