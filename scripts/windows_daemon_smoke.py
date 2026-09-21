"""The first Windows daemon/CLI slice, exercised on a real runner: a daemon
on a private home answers over the local transport (a named pipe on
Windows, a Unix socket elsewhere) and the CLI registers, lists, sends,
reads and stops through it. Nothing here needs a provider, a PTY, a
service or the desktop, which are later slices; what those answer on
Windows is checked to be an explicit refusal, never a hang or a crash.

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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary_dir = args.binary_dir.resolve(strict=True)
    exe = ".exe" if os.name == "nt" else ""
    cli = binary_dir / f"agentdocker{exe}"
    daemon_binary = binary_dir / f"agentd{exe}"
    report = {
        "scope": "Windows daemon/CLI slice one: a private daemon over the local transport, the CLI's ping/register/ps/send/inbox/stop, and explicit refusals for what is not delivered on Windows",
        "platform": f"{platform.system()} {platform.release()} {platform.machine()}",
        "binary_sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in (cli, daemon_binary)},
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
    # nothing the daemon owns sits under one.
    base = Path(tempfile.gettempdir()).resolve()
    token = secrets.token_hex(4)
    home = base / f"agentdocker-smoke-{token}"
    fresh = base / f"agentdocker-smoke-{token}-fresh"
    root = Path(tempfile.mkdtemp(prefix="agentdocker-smoke-")).resolve()
    project = root / "project"
    project.mkdir()
    daemon_log = root / "smoke-daemon.log"
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
    env["AGENTDOCKER_HOME"] = str(home)
    env["AGENTDOCKER_NO_AUTOSTART"] = "1"
    daemon = None
    log = None

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
            owner = subprocess.run(["powershell", "-NoProfile", "-Command", f"(Get-Acl -LiteralPath '{candidate}').Owner"], capture_output=True, text=True, timeout=30)
            lines.append(f"{candidate} owner={owner.stdout.strip()}\n{listing.stdout.strip()}")
        return "\n" + "\n".join(lines)

    def transport_endpoint():
        """Where the daemon listens, as `daemon status` prints it."""
        status = run("daemon", "status")
        for line in status.stdout.splitlines():
            if line.startswith("daemon ") and " at " in line:
                return line.split(" at ", 1)[1].split(" (pid")[0].strip()
        raise AssertionError(f"daemon status names no endpoint: {status.stdout!r}")

    class Wire:
        """A line-framed connection to the daemon: a Unix socket or a
        named pipe, opened the way any script would open it. Every read is
        bounded: a thread reads lines and hands them over, and a read that
        gets nothing in time answers with a timeout mark rather than
        holding the run (the third runner sat in a pipe read until the
        job's own timeout, and left no report)."""

        TIMED_OUT = object()

        def __init__(self, where):
            import queue
            if os.name == "nt":
                self.file = open(where, "r+b", buffering=0)
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

        def send(self, frame):
            self.file.write((json.dumps(frame) + "\n").encode())
            self.file.flush()

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
            try:
                self.file.close()
            finally:
                if self.sock is not None:
                    self.sock.close()

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
        line = wait_status("smoke-pipes", "exited")
        logs = run("logs", "smoke-pipes", check=False)
        step("a piped managed command runs under a session owner and its output reaches its log", "exited" in line and "piped hello" in logs.stdout and "to stderr" in logs.stdout, (line + " | " + logs.stdout.strip())[:600])
        run("run", "--name", "smoke-tty", "--tty", "--runtime", "custom", "--", sys.executable, "-c", "import sys; print('tty hello', flush=True); line = sys.stdin.readline(); print('got ' + line.strip(), flush=True)")
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
            while time.time() < deadline and b"got abc" not in screen:
                frame = wire.line(timeout=max(1, deadline - time.time()))
                if frame is None or frame is Wire.TIMED_OUT:
                    frames.append("end" if frame is None else "timed out")
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
        line = wait_status("smoke-tty", "exited")
        logs = run("logs", "smoke-tty", check=False)
        step("the terminal session ends and its screen is in its log", "exited" in line and "got abc" in logs.stdout, (line + " | " + logs.stdout.strip())[-400:])
        run("run", "--name", "smoke-stop", "--tty", "--runtime", "custom", "--", sys.executable, "-c", "import time; print('sleeping', flush=True); time.sleep(120)")
        line = wait_status("smoke-stop", "running")
        stopped = run("stop", "smoke-stop", check=False, timeout=20)
        line = wait_status("smoke-stop", "exited", seconds=15)
        step("stop ends a managed terminal session through its owner", stopped.returncode == 0 and "exited" in line, (stopped.stderr.strip() or stopped.stdout.strip()) + " | " + line)
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
            daemon.kill()
            step("the daemon is ended abruptly, leaving the session to its owner", wait_exit(daemon), str(daemon.returncode))
            # The ordinary first run: a client with nothing to talk to starts
            # the daemon itself and waits for it to listen.
            started = run("daemon", "start", extra_env={"AGENTDOCKER_NO_AUTOSTART": ""}, timeout=30)
            step("daemon start brings up a daemon on demand for this home", started.returncode == 0 and "agentd" in started.stdout, started.stdout.strip())
            line = wait_status("smoke-survivor", "running", seconds=15)
            step("the new daemon finds the session still running under its owner", "running" in line, line)
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
        else:
            daemon.terminate()
            step("the private daemon is ended directly, since this user has a service installed", wait_exit(daemon), str(user_service))
        report["result"] = "passed"
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.kill()
            daemon.wait()
        if log is not None:
            log.close()
        # A daemon a client started for either home outlives the client;
        # ask it to exit so nothing of this run is left behind (never where
        # `daemon stop` would reach the user's own service).
        for made in (home, fresh) if user_service is None else ():
            try:
                run("daemon", "stop", check=False, timeout=15, extra_env={"AGENTDOCKER_HOME": str(made), "AGENTDOCKER_NO_AUTOSTART": "1"})
            except Exception as error:
                report.setdefault("cleanup", []).append(f"{made}: {error}")
        if os.name == "nt":
            report["root_acl"] = acl_report(root).strip()
            report["home_acl"] = acl_report(home).strip()
        for made in (home, fresh):
            shutil.rmtree(made, ignore_errors=True)
        try:
            report["daemon_log_tail"] = daemon_log.read_text(errors="replace")[-2000:]
        except OSError:
            pass
        shutil.rmtree(root, ignore_errors=True)
        write_report()
        print(json.dumps(report, indent=2))
    return 0 if report["result"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
