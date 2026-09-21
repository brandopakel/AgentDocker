"""The first Windows daemon/CLI slice, exercised on a real runner: a daemon
on a private home answers over the local transport (a named pipe on
Windows, a Unix socket elsewhere) and the CLI registers, lists, sends,
reads and stops through it, then exercises managed pipes and terminals,
owner lifetime, terminal EOF and stop under keyboard backpressure. No
provider or installed service is needed. Opt-in --desktop also checks a
source-built native Windows window, fresh-home UI daemon startup and capture.

Portable on purpose: the same steps run on macOS/Linux, so the script is
checked before the Windows runner ever sees it."""
import argparse
import base64
import hashlib
import json
import os
import platform
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from types import SimpleNamespace


def terminal_record(record):
    return record is None or record["status"]["state"] in ("exited", "failed")


def wait_terminal(inspect, name, seconds=10):
    deadline = time.monotonic() + seconds
    while True:
        record = inspect(name, missing_ok=True)
        if terminal_record(record) or time.monotonic() >= deadline:
            return record
        time.sleep(0.1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--desktop", action="store_true", help="also exercise native Windows UI startup and capture in a fresh private home")
    args = parser.parse_args()
    if args.desktop and os.name != "nt":
        parser.error("--desktop requires a native Windows session; use desktop_smoke.py on macOS/Linux")
    binary_dir = args.binary_dir.resolve(strict=True)
    exe = ".exe" if os.name == "nt" else ""
    cli = binary_dir / f"agentdocker{exe}"
    daemon_binary = binary_dir / f"agentd{exe}"
    desktop_binary = binary_dir / f"agentdocker-ui{exe}"
    binaries = (cli, daemon_binary, desktop_binary) if args.desktop else (cli, daemon_binary)
    report = {
        "scope": "Windows daemon/CLI and managed-session acceptance: private local transport, registry/messages/leases, terminal input/output/EOF, owner lifetime, and bounded stop under keyboard backpressure",
        "platform": f"{platform.system()} {platform.release()} {platform.machine()}",
        "binary_sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in binaries},
        "steps": [],
        "result": "failed",
    }
    # The daemon creates its home itself, as it does on a person's first run,
    # directly under the temporary directory. A directory made here first
    # would be foreign-owned state on an elevated Windows runner (objects an
    # administrator creates belong to the Administrators group, not the
    # user), and the daemon refuses that by design; what it creates is owned
    # by the user. The first runner also refused a home the daemon made under
    # a directory made here as "writable by another principal": what such a
    # directory inherits there is recorded in the report (`root_acl`) and
    # the final OWNER RIGHTS trial now checks that ancestry directly.
    base = Path(tempfile.gettempdir()).resolve()
    token = secrets.token_hex(4)
    home = base / f"agentdocker-smoke-{token}"
    fresh = base / f"agentdocker-smoke-{token}-fresh"
    homes = [home, fresh]
    home_sockets = {}
    root = Path(tempfile.mkdtemp(prefix="agentdocker-smoke-")).resolve()
    # Smoke capture uses the same private ancestry rules as application state.
    # The Actions checkout drive is owned by NETWORK SERVICE, so capture in
    # private scratch first and export only after the owned window has exited.
    # Keep this separate from the fresh home whose initialization is under test.
    desktop_capture = root / "desktop-capture"
    project = root / "project"
    project.mkdir()
    daemon_log = root / "smoke-daemon.log"
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
    env["AGENTDOCKER_HOME"] = str(home)
    env["AGENTDOCKER_NO_AUTOSTART"] = "1"
    # A launcher directory on the daemon's PATH, as npm's is on a person's:
    # an npm-installed provider is a `.cmd` shim there, and a session
    # started by that bare name must find and run it.
    launchers = root / "launchers"
    launchers.mkdir()
    env["PATH"] = str(launchers) + os.pathsep + env.get("PATH", "")
    # Console close must not block the async worker that drains its output.
    env["TOKIO_WORKER_THREADS"] = "1"
    daemon = None
    window = None
    log = None
    managed_names = []
    owner_handles = []

    def write_report():
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / "windows-daemon-smoke.json").write_text(json.dumps(report, indent=2))

    def step(name, ok, detail=""):
        report["steps"].append({"step": name, "ok": bool(ok), "detail": str(detail)[:4000]})
        # Written after every step and said aloud, so a run the job has
        # to end still says how far it got and what it saw.
        write_report()
        print(f"step {'ok  ' if ok else 'FAIL'} {name}", flush=True)
        if not ok:
            raise AssertionError(f"{name}: {detail}")

    def run(*argv, check=True, timeout=30, extra_env=None):
        """The command with its output captured through pipes, as a script
        or a shell captures it. Bounded twice: the command must exit within
        `timeout`, and its pipes must close when it exits — a daemon it
        started that inherited them would keep a capture waiting for as
        long as it runs (the second runner sat 26 minutes in `daemon
        start` that way), so a pipe still open shortly after the exit fails
        the command here instead of hanging the run."""
        run_env = dict(env, **(extra_env or {}))
        managed_name = argv[argv.index("--name") + 1] if argv and argv[0] == "run" and "--name" in argv else None
        if managed_name is not None:
            managed_names.append(managed_name)
        process = subprocess.Popen([str(cli), *argv], cwd=project, env=run_env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        captured = {}

        def drain(name, stream):
            captured[name] = stream.read()

        pumps = [threading.Thread(target=drain, args=(name, stream), daemon=True) for name, stream in (("stdout", process.stdout), ("stderr", process.stderr))]
        for pump in pumps:
            pump.start()
        timed_out = False
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            process.kill()
            process.wait()
        for pump in pumps:
            pump.join(timeout=5)
        held = [name for name, pump in zip(("stdout", "stderr"), pumps) if pump.is_alive()]
        result = SimpleNamespace(returncode=process.returncode, stdout=captured.get("stdout", ""), stderr=captured.get("stderr", ""))
        if timed_out:
            raise AssertionError(f"{argv}: did not exit within {timeout} s; stderr so far: {result.stderr.strip()[:600]}")
        if held:
            raise AssertionError(f"{argv}: exited {process.returncode} but its {' and '.join(held)} pipe is still held open by another process (a daemon it started inherited it)")
        if check and result.returncode != 0:
            raise AssertionError(f"{argv}: exit {result.returncode}: {result.stderr.strip()}")
        if managed_name is not None and result.returncode == 0 and os.name == "nt":
            record = inspect_agent(managed_name)
            owner = record.get("owner")
            if owner and record["status"]["state"] in ("created", "running"):
                try:
                    owner_handles.append(WindowsProcess(owner["pid"], owner["started_at"]))
                except OSError:
                    # A short command can finish and be acknowledged during
                    # inspection; an owner missing while still live is a bug.
                    current = inspect_agent(managed_name)
                    if current["status"]["state"] in ("created", "running"):
                        raise
        return result

    # `daemon stop`/`start` on macOS and Linux also drive an installed user
    # service, which is filed per user, not per home: on a developer machine
    # that has one, the smoke must not touch it, so it stops its private
    # daemon directly there and says so. Windows has no service yet.
    user_service = None
    if os.name != "nt":
        for candidate in (Path.home() / "Library/LaunchAgents/dev.agentdocker.agentd.plist", Path.home() / ".config/systemd/user/agentd.service"):
            if candidate.is_file():
                user_service = candidate

    def acl_report(path):
        """On Windows, what the daemon saw: the owner and the access-control
        entries of the path and its ancestors, for a refusal the runner is
        the only place to observe."""
        if os.name != "nt":
            return ""
        lines = []
        for candidate in [path, *path.parents]:
            if not candidate.exists():
                continue
            listing = subprocess.run(["icacls", str(candidate)], capture_output=True, text=True, timeout=20)
            try:
                descriptor = windows_sddl(candidate)
            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                descriptor = f"ACL observation failed: {error}"
            lines.append(f"{candidate} security={descriptor}\n{listing.stdout.strip()}")
        return "\n" + "\n".join(lines)

    def transport_endpoint():
        """Where the daemon listens, as `daemon status` prints it."""
        status = run("daemon", "status", timeout=5)
        for line in status.stdout.splitlines():
            if line.startswith("daemon ") and " at " in line:
                return line.split(" at ", 1)[1].split(" (pid")[0].strip()
        raise AssertionError(f"daemon status names no endpoint: {status.stdout!r}")

    class Wire:
        """A line-framed connection to the daemon: a Unix socket or a
        named pipe, opened the way any script would open it. Every read is
        bounded: a thread reads lines and hands them over, and a read that
        gets nothing in time answers with a timeout mark rather than
        holding the run. Windows uses independent overlapped read/write
        operations: a synchronous handle serializes a read before the first
        request can be written and deadlocks the client."""

        TIMED_OUT = object()

        def __init__(self, where):
            import queue
            if os.name == "nt":
                from windows_smoke_pipe import WindowsSmokePipe
                self.file = WindowsSmokePipe(where)
                self.sock = None
            else:
                import socket
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.connect(where)
                self.file = self.sock.makefile("rwb", buffering=0)
            self.lines = queue.Queue()

            def pump(file, lines):
                try:
                    while True:
                        raw = file.readline()
                        if not raw:
                            lines.put(None)
                            return
                        lines.put(raw)
                except Exception as error:
                    lines.put(error)

            threading.Thread(target=pump, args=(self.file, self.lines), daemon=True).start()

        def send(self, frame, timeout=10):
            # A full input pipe must produce a recorded failure, not consume
            # the CI job's timeout before it can save its report.
            import queue
            result = queue.Queue()

            def write():
                try:
                    data = memoryview((json.dumps(frame) + "\n").encode())
                    while data:
                        written = self.file.write(data)
                        if not written:
                            raise OSError("wire write returned no progress")
                        data = data[written:]
                    self.file.flush()
                    result.put(None)
                except Exception as error:
                    result.put(error)

            threading.Thread(target=write, daemon=True).start()
            try:
                error = result.get(timeout=timeout)
            except queue.Empty:
                raise TimeoutError("wire write did not finish") from None
            if error is not None:
                raise error

        def line(self, timeout=15):
            import queue
            try:
                raw = self.lines.get(timeout=timeout)
            except queue.Empty:
                return Wire.TIMED_OUT
            if raw is None:
                return None
            if isinstance(raw, Exception):
                raise raw
            return json.loads(raw)

        def close(self):
            if os.name == "nt":
                # The overlapped helper bounds cancellation itself. Propagate
                # cleanup failures instead of losing them in a daemon thread.
                self.file.close()
                return
            def close():
                try:
                    self.file.close()
                finally:
                    if self.sock is not None:
                        self.sock.close()
            closer = threading.Thread(target=close, daemon=True)
            closer.start()
            closer.join(timeout=2)

    def inspect_agent(name, missing_ok=False):
        wire = Wire(transport_endpoint())
        try:
            wire.send({"op": "inspect", "agent": name})
            response = wire.line(timeout=3)
            if missing_ok and isinstance(response, dict) and response.get("type") == "error" and response.get("code") == "not_found":
                return None
            assert isinstance(response, dict) and response.get("type") == "agent", response
            return response["agent"]
        finally:
            wire.close()

    class WindowsProcess:
        """A fixture's process identity held open across exit, never a Job
        handle that could keep its owner's kill-on-close job alive."""

        def __init__(self, pid, expected_birth=None):
            import ctypes
            from ctypes import wintypes
            from datetime import datetime, timezone
            self.api = ctypes.WinDLL("kernel32", use_last_error=True)
            self.api.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
            self.api.OpenProcess.restype = wintypes.HANDLE
            self.api.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
            self.api.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
            self.api.CloseHandle.argtypes = [wintypes.HANDLE]
            self.api.GetProcessTimes.argtypes = [wintypes.HANDLE, *([ctypes.POINTER(wintypes.FILETIME)] * 4)]
            self.handle = self.api.OpenProcess(0x00100000 | 0x1000 | 1, False, pid)
            if not self.handle:
                raise ctypes.WinError(ctypes.get_last_error())
            created, exited, kernel, user = (wintypes.FILETIME() for _ in range(4))
            if not self.api.GetProcessTimes(self.handle, *map(ctypes.byref, (created, exited, kernel, user))):
                self.api.CloseHandle(self.handle)
                raise ctypes.WinError(ctypes.get_last_error())
            ticks = (created.dwHighDateTime << 32 | created.dwLowDateTime) - 116444736000000000
            date = datetime.fromtimestamp(ticks // 10000000, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
            birth = date + (f".{ticks % 10000000:07d}".rstrip("0").rstrip(".")) + "Z"
            if expected_birth is not None:
                expected = expected_birth.removesuffix("Z")
                if "." in expected:
                    expected = expected.rstrip("0").rstrip(".")
                if birth != expected + "Z":
                    self.api.CloseHandle(self.handle)
                    raise AssertionError(f"fixture pid {pid} changed birth: {birth} != {expected_birth}")

        def exited(self, milliseconds=0):
            return self.api.WaitForSingleObject(self.handle, milliseconds) == 0

        def end(self):
            if not self.exited() and not self.api.TerminateProcess(self.handle, 99):
                raise AssertionError("cannot end the fixture process")

        def close(self):
            self.api.CloseHandle(self.handle)

    def windows_security(path):
        quoted = str(path).replace("'", "''")
        # The workflow runs in PowerShell 7; keep its module environment and
        # host together rather than starting legacy Windows PowerShell inside it.
        host = shutil.which("pwsh") or "powershell"
        result = subprocess.run(
            [host, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
             f"$ErrorActionPreference='Stop'; $acl=Get-Acl -LiteralPath '{quoted}'; "
             "[ordered]@{owner_sid=$acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value; "
             "protected=$acl.AreAccessRulesProtected; sddl=$acl.Sddl} | ConvertTo-Json -Compress"],
            capture_output=True, text=True, timeout=15,
        )
        if result.returncode != 0 or not result.stdout.strip():
            raise RuntimeError(f"cannot inspect ACL of {path}: host={host}; exit={result.returncode}; stdout={result.stdout[:1000]!r}; stderr={result.stderr[:3000]!r}")
        observed = json.loads(result.stdout)
        if not isinstance(observed, dict) or not isinstance(observed.get("owner_sid"), str) or type(observed.get("protected")) is not bool or not isinstance(observed.get("sddl"), str):
            raise RuntimeError(f"invalid ACL observation for {path}: {observed!r}")
        return observed

    def windows_sddl(path):
        return windows_security(path)["sddl"]

    def check_sqlite_security(phase):
        current = subprocess.run(
            [shutil.which("pwsh") or "powershell", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
             "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value"],
            capture_output=True, text=True, timeout=15, check=True,
        ).stdout.strip()
        assert current.startswith("S-1-"), current
        descriptors = {name: windows_security(home / name)
                       for name in ("state.db", "state.db-wal", "state.db-shm")}
        for name, descriptor in descriptors.items():
            # SDDL can abbreviate a user SID (for example LA for the local
            # Administrator account). Compare canonical SIDs from GetOwner,
            # never display aliases, and read the ACL protection flag directly.
            step(f"SQLite {name} is user-owned and protected {phase}",
                 descriptor["owner_sid"] == current and descriptor["protected"],
                 descriptor)

    def agent_status(name):
        listing = run("ps", "--no-discover", "--all")
        for line in listing.stdout.splitlines():
            if f" {name} " in f" {line} ":
                return line
        return ""

    def wait_status(name, wanted, seconds=20):
        deadline = time.time() + seconds
        line = agent_status(name)
        while wanted not in line and time.time() < deadline:
            time.sleep(0.25)
            line = agent_status(name)
        return line

    def desktop_trial():
        nonlocal window
        desktop_home = base / f"agentdocker-smoke-{token}-desktop"
        endpoint = rf"\\.\pipe\agentdocker-smoke-{token}-desktop"
        capture = desktop_capture
        desktop_env = {
            "AGENTDOCKER_HOME": str(desktop_home),
            "AGENTDOCKER_SOCKET": endpoint,
            "AGENTDOCKER_NO_AUTOSTART": "1",
        }
        desktop = {
            "scope": "source-built native Windows window, fresh-home UI daemon autostart, connection, runtime rows and renderer capture",
            "tcp_observation": "not_measured",
            "capture_directory": "desktop",
            "result": "running",
        }
        report["desktop"] = desktop
        write_report()
        try:
            # No command may start or initialize this home before the UI. Its
            # explicit endpoint also prevents forwarding into another window.
            step("the desktop trial starts with no prior home or capture", not desktop_home.exists() and not capture.exists() and not (args.output / "desktop").exists())
            # Register cleanup before any child can create its private daemon.
            homes.append(desktop_home)
            home_sockets[desktop_home] = endpoint
            absent = run("ping", check=False, timeout=5, extra_env=desktop_env)
            step("the desktop endpoint is absent with CLI autostart disabled", absent.returncode != 0 and not desktop_home.exists(), (absent.stderr + absent.stdout).strip())
            started = time.monotonic()
            with (args.output / "desktop.log").open("wb") as window_log:
                window = subprocess.Popen(
                    [str(desktop_binary), "--smoke-test", str(capture), "--smoke-deadline", "60"],
                    cwd=project,
                    env=dict(env, **dict(desktop_env, AGENTDOCKER_NO_AUTOSTART="0")),
                    stdin=subprocess.DEVNULL, stdout=window_log, stderr=subprocess.STDOUT,
                )
                desktop["pid"] = window.pid
                write_report()
                # Let the application's own deadline retain its failure report;
                # bound a deadlocked window separately instead of waiting forever.
                window.wait(timeout=75)
            desktop["exit_code"] = window.returncode
            desktop["elapsed_seconds"] = time.monotonic() - started
            step("the native desktop exits within its smoke deadline", window.returncode == 0, f"exit={window.returncode}, elapsed={desktop['elapsed_seconds']:.3f}s; see desktop.log")
            observed = json.loads((capture / "result.json").read_text(encoding="utf-8"))
            desktop["native_result"] = observed
            step("the native desktop connects and captures a ready window", observed.get("result") == "passed" and observed.get("connected") is True and observed.get("runtime_rows", 0) > 0 and observed.get("screenshot_requested") is True, observed)
            png = (capture / "window.png").read_bytes()
            step("the native renderer retains a nonempty PNG", png[:8] == b"\x89PNG\r\n\x1a\n" and len(png) >= 1000, f"{len(png)} bytes")
            desktop["screenshot_sha256"] = hashlib.sha256(png).hexdigest()
            # The UI has exited. This probe cannot start a daemon itself, so a
            # reply proves the UI started the private endpoint and left it alive.
            alive = run("ping", check=False, timeout=5, extra_env=desktop_env)
            step("the UI-started private daemon answers after the window exits", alive.returncode == 0, (alive.stderr + alive.stdout).strip())
            desktop["result"] = "passed"
            write_report()
        except Exception as error:
            desktop["result"] = "failed"
            desktop["error"] = f"{type(error).__name__}: {error}"
            write_report()
            raise

    def wait_exit(process, seconds=10):
        for _ in range(int(seconds * 10)):
            if process.poll() is not None:
                return True
            time.sleep(0.1)
        return False

    try:
        subprocess.run(["git", "init", "-q"], cwd=project, check=True)
        log = open(daemon_log, "wb")
        daemon = subprocess.Popen([str(daemon_binary)], cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        for _ in range(100):
            time.sleep(0.1)
            probe = run("ping", check=False, timeout=10)
            if probe.returncode == 0 or daemon.poll() is not None:
                break
        detail = probe.stderr.strip()
        if daemon.poll() is not None:
            detail = f"the daemon exited with {daemon.returncode} before answering; ping said: {detail}"
            detail += f"; daemon log: {daemon_log.read_text(errors='replace').strip()[-600:]}" + acl_report(home)
        step("the daemon answers ping over the local transport", probe.returncode == 0, detail)
        status = run("daemon", "status")
        step("daemon status names the serving executable", str(daemon_binary.name) in status.stdout, status.stdout.strip())
        if args.desktop:
            desktop_trial()
        first = run("register", "--name", "smoke-one", "--runtime", "custom", "--pid", str(os.getpid()))
        second = run("register", "--name", "smoke-two", "--runtime", "custom", "--pid", str(daemon.pid))
        ids = (first.stdout.strip(), second.stdout.strip())
        step("two agents register with their pids", all(len(i) == 32 for i in ids), ids)
        listing = run("ps")
        step("ps lists both as running", "smoke-one" in listing.stdout and "smoke-two" in listing.stdout and "running" in listing.stdout, listing.stdout.strip())
        run("send", "--from", "smoke-one", "--to", "smoke-two", "hello from one")
        inbox = run("inbox", "--as", "smoke-two", "--drain")
        step("a message sent to an agent is read from its inbox", "hello from one" in inbox.stdout, inbox.stdout.strip())
        run("send", "--from", "smoke-one", "--to", "project", "hello everyone")
        inbox = run("inbox", "--as", "smoke-two", "--drain")
        step("a project broadcast reaches the other agent", "hello everyone" in inbox.stdout, inbox.stdout.strip())
        lease = run("claim", "path:" + str(project / "file.txt"), "--as", "smoke-one", "--ttl", "60")
        conflict = run("claim", "path:" + str(project / "file.txt"), "--as", "smoke-two", "--ttl", "60", check=False)
        step("a lease is held and a second claim is refused", lease.returncode == 0 and conflict.returncode != 0, conflict.stderr.strip())
        run("release", lease.stdout.strip(), "--as", "smoke-one", "--summary", "smoke done")
        # `attach` from a pipe is refused in words on every platform: it
        # needs a terminal, and this script has none (a person's attach
        # from a real console is the part no runner checks).
        attach = run("attach", "smoke-one", check=False, timeout=20)
        step("attach without a terminal is refused in words, not with a hang", attach.returncode != 0 and "needs a terminal" in attach.stderr, attach.stderr.strip())
        if os.name == "nt":
            install = run("daemon", "install", check=False, timeout=20)
            step("daemon install is refused on Windows in words", install.returncode != 0 and "not available on Windows" in install.stderr, install.stderr.strip())
            reload = run("daemon", "reload", check=False, timeout=20)
            step("daemon reload is refused on Windows in words", reload.returncode != 0 and "Windows" in (reload.stderr + reload.stdout), (reload.stderr + reload.stdout).strip())
        # Managed sessions: the daemon starts a session owner, which holds
        # the child, its pipes or its terminal and its log. A piped command's
        # output reaches its log; a terminal command is typed into through
        # the attach wire and answers on its screen; a stop through the
        # owner ends a child that would otherwise run on.
        piped = run("run", "--name", "smoke-pipes", "--runtime", "custom", "--", sys.executable, "-c", "import sys; print('piped hello'); print('to stderr', file=sys.stderr)")
        if os.name == "nt":
            # The shape of `claude` after `npm install -g`: a .cmd shim on PATH,
            # started by its bare name, with an argument cmd must not rewrite.
            (launchers / "smoke-shim.cmd").write_text("@echo off\r\necho shim-ran %1 %2\r\necho shim-cwd %CD%\r\n")
            run("run", "--name", "smoke-shim", "--runtime", "custom", "--", "smoke-shim", "hello world", "a&b")
            line = wait_status("smoke-shim", "exited")
            logs = run("logs", "smoke-shim", check=False)
            step("a managed session starts from an npm-style .cmd launcher by its bare name, with a space and a cmd metacharacter intact in its arguments", "exited (0)" in line and 'shim-ran "hello world" "a&b"' in logs.stdout, (line + " | " + logs.stdout.strip())[-400:])
            # cmd.exe cannot start in a verbatim directory (it falls back to
            # the Windows directory, and says so on stderr): the launcher
            # must run in the session's own checkout.
            step("the launcher runs in the session's checkout, not the Windows directory", f"shim-cwd {project}".lower() in logs.stdout.lower() and "UNC paths are not supported" not in logs.stdout, logs.stdout.strip()[-400:])
        line = wait_status("smoke-pipes", "exited")
        logs = run("logs", "smoke-pipes", check=False)
        step("a piped managed command runs under a session owner and its output reaches its log", "exited" in line and "piped hello" in logs.stdout and "to stderr" in logs.stdout, (line + " | " + logs.stdout.strip())[:600])
        run("run", "--name", "smoke-tty", "--tty", "--runtime", "custom", "--", sys.executable, "-c", "import sys; print('tty hello', flush=True); line = sys.stdin.readline(); print('got ' + line.strip(), flush=True); print('final partial marker', end='', flush=True)")
        line = wait_status("smoke-tty", "running")
        step("a terminal managed command is running on its own console", "running" in line, line)
        screen = b""
        frames = []
        wire = Wire(transport_endpoint())
        try:
            wire.send({"op": "attach", "agent": "smoke-tty", "cols": 80, "rows": 24})
            ready = wire.line()
            step("the attach wire answers events_ready for a terminal session", isinstance(ready, dict) and ready.get("type") == "events_ready", "timed out" if ready is Wire.TIMED_OUT else json.dumps(ready)[:300])
            # Enter is a carriage return on a Windows console, a newline on a Unix terminal.
            wire.send({"op": "attach_input", "data": base64.b64encode(b"abc\r" if os.name == "nt" else b"abc\n").decode()})
            deadline = time.time() + 15
            while time.time() < deadline:
                frame = wire.line(timeout=max(1, deadline - time.time()))
                if frame is None or frame is Wire.TIMED_OUT:
                    frames.append("eof" if frame is None else "timed out")
                    break
                frames.append(frame.get("type"))
                if frame.get("type") == "output":
                    screen += base64.b64decode(frame.get("data", ""))
                elif frame.get("type") == "end":
                    break
        finally:
            wire.close()
        text = screen.decode(errors="replace")
        typed_ok = "tty hello" in text and "got abc" in text
        # On a failure, what the daemon itself logged for the session and
        # how it stands: the difference between input that never arrived
        # and a screen rendered in a shape the check did not expect.
        seen = "" if typed_ok else f"; logs {run('logs', 'smoke-tty', check=False).stdout.strip()[-400:]!r}; status {agent_status('smoke-tty')[:200]!r}"
        step("what is typed through the attach wire reaches the agent's terminal and its answer comes back on the screen", typed_ok, f"frames {frames[:40]}; screen {text[-600:]!r}{seen}")
        step("terminal output reaches End with its final partial line before the deadline", "end" in frames and "final partial marker" in text, f"frames {frames[-10:]}; screen {text[-600:]!r}")
        line = wait_status("smoke-tty", "exited")
        logs = run("logs", "smoke-tty", check=False)
        step("the terminal session ends and its final partial line is in its log", "exited" in line and "got abc" in logs.stdout and "final partial marker" in logs.stdout, (line + " | " + logs.stdout.strip())[-400:])
        run("run", "--name", "smoke-stop", "--tty", "--runtime", "custom", "--", sys.executable, "-c", "import time; print('sleeping', flush=True); time.sleep(120)")
        line = wait_status("smoke-stop", "running")
        stopped = run("stop", "smoke-stop", check=False, timeout=20)
        line = wait_status("smoke-stop", "exited", seconds=15)
        step("stop ends a managed terminal session through its owner", stopped.returncode == 0 and "exited" in line, (stopped.stderr.strip() or stopped.stdout.strip()) + " | " + line)
        if os.name == "nt":
            # Piped on purpose: closing a console on owner death could hide
            # a missing Job Object policy. Only the owner's last job handle
            # closing should end this otherwise long-lived process tree.
            marker = project / "owner-death-processes.json"
            child_script = "import time; time.sleep(120)"
            leader_script = "import json,os,pathlib,subprocess,sys,time; p=subprocess.Popen([sys.executable,'-c',sys.argv[2]]); path=pathlib.Path(sys.argv[1]); staged=path.with_suffix('.staging'); staged.write_text(json.dumps([os.getpid(),p.pid])); staged.replace(path); time.sleep(120)"
            run("run", "--name", "smoke-owner-death", "--runtime", "custom", "--", sys.executable, "-c", leader_script, str(marker), child_script)
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            pids = json.loads(marker.read_text())
            record = inspect_agent("smoke-owner-death")
            assert pids[0] == record["pid"]
            owner = record["owner"]
            held = []
            try:
                held.append(WindowsProcess(owner["pid"], owner["started_at"]))
                held.extend(WindowsProcess(pid) for pid in pids)
                step("the owner and both piped descendants are alive before the owner crash", all(not process.exited() for process in held), f"owner {owner['pid']}; descendants {pids}")
                held[0].end()
                gone = [process.exited(5000) for process in held]
                step("abrupt owner death ends the piped child and grandchild without Rust cleanup", all(gone), f"process handles signalled: {gone}")
            finally:
                for process in held:
                    process.end()
                    process.close()

            # Disable console line processing and echo, then never read.
            # Fill the actual keyboard queue and demand a bounded stop over
            # another connection, even if a pending pipe write is blocked.
            no_read = "import ctypes,signal,time; k=ctypes.WinDLL('kernel32'); k.GetStdHandle.restype=ctypes.c_void_p; k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_uint]; assert k.SetConsoleMode(k.GetStdHandle(-10),0); signal.signal(signal.SIGINT,signal.SIG_IGN); print('not reading',flush=True); time.sleep(120)"
            run("run", "--name", "smoke-input-full", "--tty", "--runtime", "custom", "--", sys.executable, "-c", no_read)
            wire = Wire(transport_endpoint())
            stop_flood = threading.Event()
            flood_errors = []
            flood = None
            child = None
            exit_observer = None
            child_exit = []
            try:
                wire.send({"op": "attach", "agent": "smoke-input-full", "cols": 80, "rows": 24})
                ready = wire.line()
                assert isinstance(ready, dict) and ready.get("type") == "events_ready", ready
                screen = b""
                deadline = time.monotonic() + 10
                while b"not reading" not in screen and time.monotonic() < deadline:
                    frame = wire.line(timeout=1)
                    if isinstance(frame, dict) and frame.get("type") == "output":
                        screen += base64.b64decode(frame["data"])
                assert b"not reading" in screen, screen[-500:]

                def flood_keyboard():
                    try:
                        encoded = base64.b64encode(b"x" * 65536).decode()
                        for _ in range(512):
                            if stop_flood.is_set():
                                return
                            wire.send({"op": "attach_input", "data": encoded}, timeout=5)
                    except Exception as error:
                        flood_errors.append(str(error))

                flood = threading.Thread(target=flood_keyboard, daemon=True)
                flood.start()
                deadline = time.monotonic() + 20
                while b"input dropped" not in screen and time.monotonic() < deadline:
                    frame = wire.line(timeout=1)
                    if isinstance(frame, dict) and frame.get("type") == "output":
                        screen = (screen + base64.b64decode(frame["data"]))[-65536:]
                stop_flood.set()
                record = inspect_agent("smoke-input-full")
                child = WindowsProcess(record["pid"], record["process_started_at"])

                def observe_exit():
                    if child.exited(15000):
                        child_exit.append(time.monotonic())

                started = time.monotonic()
                exit_observer = threading.Thread(target=observe_exit, daemon=True)
                exit_observer.start()
                stopped = run("stop", "smoke-input-full", check=False, timeout=12)
                line = wait_status("smoke-input-full", "exited", seconds=10)
                elapsed = time.monotonic() - started
                exit_observer.join(timeout=1)
                lifetime = child_exit[0] - started if child_exit else None
                step("native keyboard backpressure was observed", b"input dropped" in screen, f"screen {screen[-300:]!r}; writer {flood_errors}")
                # Observe the actual process, not just the final status: console
                # teardown can delay that status and hide a premature job kill.
                step("a full keyboard preserves the child's two-second stop grace", lifetime is not None and 1.9 <= lifetime < 10, f"child exit {lifetime!r}s after stop dispatch; final status {elapsed:.3f}s")
                step("stop remains bounded when the child ignores Ctrl-C and never reads its full keyboard", stopped.returncode == 0 and "exited" in line and elapsed < 10, f"{elapsed:.3f}s; {line}")
                step("the daemon still answers after terminal backpressure", run("ping").returncode == 0)
            finally:
                try:
                    stop_flood.set()
                    if flood is not None and flood.ident is not None:
                        flood.join(timeout=6)
                finally:
                    try:
                        if child is not None:
                            try:
                                child.end()
                            finally:
                                if exit_observer is not None and exit_observer.ident is not None:
                                    exit_observer.join(timeout=16)
                                if exit_observer is not None and exit_observer.is_alive():
                                    raise AssertionError("exit observer still owns the process handle")
                                child.close()
                    finally:
                        wire.close()
        # Stopping an agent by pid checks the recorded birth before ending anything.
        helper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], cwd=project, env=env)
        try:
            run("register", "--name", "smoke-helper", "--runtime", "custom", "--pid", str(helper.pid))
            stopped = run("stop", "smoke-helper", check=False, timeout=20)
            for _ in range(50):
                if helper.poll() is not None:
                    break
                time.sleep(0.1)
            step("stop ends a registered process whose identity matches its record", stopped.returncode == 0 and helper.poll() is not None, stopped.stderr.strip() or stopped.stdout.strip())
        finally:
            if helper.poll() is None:
                helper.kill()
                helper.wait()
        if user_service is None:
            # A session owner outlives a daemon that dies: this daemon is
            # ended abruptly, as a crash would end it (a deliberate `daemon
            # stop` stops managed sessions first, by design), the next
            # daemon finds the session still running under its owner, and
            # stops it through that owner.
            run("run", "--name", "smoke-survivor", "--tty", "--runtime", "custom", "--", sys.executable, "-c", "import time; print('surviving', flush=True); time.sleep(120)")
            line = wait_status("smoke-survivor", "running")
            step("a terminal session is running before the daemon dies", "running" in line, line)
            if os.name == "nt":
                check_sqlite_security("before the crash")
            daemon.kill()
            step("the daemon is ended abruptly, leaving the session to its owner", wait_exit(daemon), str(daemon.returncode))
            if os.name == "nt":
                check_sqlite_security("after the crash")
            # The ordinary first run: a client with nothing to talk to starts
            # the daemon itself and waits for it to listen.
            started = run("daemon", "start", extra_env={"AGENTDOCKER_NO_AUTOSTART": ""}, timeout=30)
            step("daemon start brings up a daemon on demand for this home", started.returncode == 0 and "agentd" in started.stdout, started.stdout.strip())
            line = wait_status("smoke-survivor", "running", seconds=15)
            step("the new daemon finds the session still running under its owner", "running" in line, line)
            if os.name == "nt":
                check_sqlite_security("after recovery")
            stopped = run("stop", "smoke-survivor", check=False, timeout=20)
            line = wait_status("smoke-survivor", "exited", seconds=15)
            step("the reattached session is stopped through its owner", stopped.returncode == 0 and "exited" in line, (stopped.stderr.strip() or stopped.stdout.strip()) + " | " + line)
            stop = run("daemon", "stop")
            step("daemon stop ends the daemon a client started", "stopped" in stop.stdout and run("ping", check=False, timeout=10).returncode != 0, stop.stdout.strip())
            # A fresh home that no daemon has made yet: the first command a
            # person runs creates it and starts the daemon, and the home it
            # makes is the daemon's own (private, user-owned) so the daemon
            # accepts it — on an elevated Windows shell a plain directory
            # would belong to Administrators and be refused.
            fresh_env = {"AGENTDOCKER_HOME": str(fresh), "AGENTDOCKER_NO_AUTOSTART": ""}
            pinged = run("ping", check=False, extra_env=fresh_env, timeout=30)
            step("the first command on a fresh home creates it and starts a daemon", pinged.returncode == 0, (pinged.stderr + pinged.stdout).strip() + acl_report(fresh))
            stop = run("daemon", "stop", extra_env=fresh_env)
            gone = run("ping", check=False, extra_env=dict(fresh_env, AGENTDOCKER_NO_AUTOSTART="1"), timeout=10)
            step("that daemon is ended too", "stopped" in stop.stdout and gone.returncode != 0, stop.stdout.strip())
            if os.name == "nt":
                # root was created by tempfile.mkdtemp(mode=0700): newer
                # CPython uses SYSTEM, Administrators and OWNER RIGHTS.
                # Verify that exact premise rather than passing on an older
                # Python whose ACL never exercised the compatibility bug.
                before_acl = windows_sddl(root)
                step("the Python private parent has an OWNER RIGHTS permission entry", ";;;OW)" in before_acl, f"Python {platform.python_version()}; {before_acl}")
                nested = root / "owner-rights-home"
                homes.append(nested)
                nested_env = {"AGENTDOCKER_HOME": str(nested), "AGENTDOCKER_NO_AUTOSTART": ""}
                pinged = run("ping", check=False, extra_env=nested_env, timeout=30)
                step("first run works below the private OWNER RIGHTS parent without changing its owner or ACL", pinged.returncode == 0 and windows_sddl(root) == before_acl, (pinged.stderr + pinged.stdout).strip())
                child_acl = windows_sddl(nested)
                step("the new child state has a protected ACL", "D:P" in child_acl, child_acl)
                stop = run("daemon", "stop", extra_env=nested_env)
                gone = run("ping", check=False, extra_env=dict(nested_env, AGENTDOCKER_NO_AUTOSTART="1"), timeout=10)
                step("the OWNER RIGHTS trial daemon is ended too", "stopped" in stop.stdout and gone.returncode != 0, stop.stdout.strip())
        else:
            daemon.terminate()
            step("the private daemon is ended directly, since this user has a service installed", wait_exit(daemon), str(user_service))
        report["result"] = "passed"
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        # Stop only the window created above before tearing down its daemon.
        # Keep capture/result/log artifacts even when startup or cleanup failed.
        if window is not None and window.poll() is None:
            try:
                window.kill()
                window.wait(timeout=5)
            except Exception as error:
                report.setdefault("cleanup", []).append(f"desktop cleanup: {error}")
        if desktop_capture.exists():
            try:
                if window is not None and window.poll() is None:
                    raise RuntimeError("window still running; retaining private capture in scratch")
                shutil.copytree(desktop_capture, args.output / "desktop")
            except Exception as error:
                report.setdefault("cleanup", []).append(f"desktop capture export: {error}; source={desktop_capture}")
        # An owner deliberately outlives its daemon. End fixture sessions
        # while the daemon can still reach them, before killing the daemon
        # or removing its state. If the crash trial lost its replacement,
        # start a private daemon directly, never the person's user service.
        rescue = None
        if managed_names:
            try:
                if run("ping", check=False, timeout=5).returncode != 0:
                    rescue = subprocess.Popen([str(daemon_binary)], cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline and rescue.poll() is None:
                        if run("ping", check=False, timeout=3).returncode == 0:
                            break
                        time.sleep(0.1)
                for name in reversed(managed_names):
                    try:
                        record = inspect_agent(name, missing_ok=True)
                        if terminal_record(record):
                            continue
                        # Also covers run commands that failed/timed out
                        # after creating a session, before run() could keep
                        # the owner's handle for cleanup.
                        if os.name == "nt" and record.get("owner"):
                            owner = record["owner"]
                            try:
                                owner_handles.append(WindowsProcess(owner["pid"], owner["started_at"]))
                            except OSError:
                                if not terminal_record(wait_terminal(inspect_agent, name, seconds=3)):
                                    raise
                        stopped = run("stop", name, check=False, timeout=8)
                        record = wait_terminal(inspect_agent, name)
                        if not terminal_record(record):
                            run("stop", name, "--force", check=False, timeout=8)
                            record = wait_terminal(inspect_agent, name, seconds=5)
                        if not terminal_record(record):
                            raise AssertionError(f"session did not exit; stop returned {stopped.returncode}: {stopped.stderr.strip()}")
                    except Exception as error:
                        report.setdefault("cleanup", []).append(f"stop {name}: {error}")
            except Exception as error:
                report.setdefault("cleanup", []).append(f"session cleanup: {error}")
        # These are process handles captured from this fixture's verified
        # owner records, not broad PID searches or job clones. They are the
        # fallback when the very stop/daemon path under test is broken.
        for owner in owner_handles:
            try:
                owner.end()
                if not owner.exited(5000):
                    raise AssertionError("fixture owner did not exit")
            except Exception as error:
                report.setdefault("cleanup", []).append(f"owner cleanup: {error}")
            finally:
                owner.close()
        if rescue is not None and rescue.poll() is None:
            rescue.kill()
            rescue.wait(timeout=10)
        if daemon is not None and daemon.poll() is None:
            daemon.kill()
            daemon.wait()
        if log is not None:
            log.close()
        # A daemon a client started for either home outlives the client;
        # ask it to exit so nothing of this run is left behind (never where
        # `daemon stop` would reach the user's own service).
        for made in homes if user_service is None else ():
            try:
                cleanup_env = {"AGENTDOCKER_HOME": str(made), "AGENTDOCKER_NO_AUTOSTART": "1"}
                if made in home_sockets:
                    cleanup_env["AGENTDOCKER_SOCKET"] = home_sockets[made]
                run("daemon", "stop", timeout=15, extra_env=cleanup_env)
                if run("ping", check=False, timeout=5, extra_env=cleanup_env).returncode == 0:
                    raise AssertionError("private daemon still answers after stop")
            except Exception as error:
                report.setdefault("cleanup", []).append(f"{made}: {error}")
        if os.name == "nt":
            report["root_acl"] = acl_report(root).strip()
            report["home_acl"] = acl_report(home).strip()
        if not report.get("cleanup"):
            for made in homes:
                shutil.rmtree(made, ignore_errors=True)
        try:
            report["daemon_log_tail"] = daemon_log.read_text(errors="replace")[-2000:]
        except OSError:
            pass
        if not report.get("cleanup"):
            shutil.rmtree(root, ignore_errors=True)
        if report.get("cleanup"):
            report["result"] = "failed"
            report.setdefault("error", "fixture cleanup did not complete cleanly")
        write_report()
        print(json.dumps(report, indent=2))
    return 0 if report["result"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
