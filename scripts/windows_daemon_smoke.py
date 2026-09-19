"""The first Windows daemon/CLI slice, exercised on a real runner: a daemon
on a private home answers over the local transport (a named pipe on
Windows, a Unix socket elsewhere) and the CLI registers, lists, sends,
reads and stops through it. Nothing here needs a provider, a PTY, a
service or the desktop, which are later slices; what those answer on
Windows is checked to be an explicit refusal, never a hang or a crash.

Portable on purpose: the same steps run on macOS/Linux, so the script is
checked before the Windows runner ever sees it."""
import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
from pathlib import Path


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
    home = Path(tempfile.mkdtemp(prefix="agentdocker-smoke-")).resolve()
    project = Path(tempfile.mkdtemp(prefix="agentdocker-smoke-project-")).resolve()
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
    env["AGENTDOCKER_HOME"] = str(home)
    env["AGENTDOCKER_NO_AUTOSTART"] = "1"
    daemon = None

    def step(name, ok, detail=""):
        report["steps"].append({"step": name, "ok": bool(ok), "detail": str(detail)[:600]})
        if not ok:
            raise AssertionError(f"{name}: {detail}")

    def run(*argv, check=True, timeout=30):
        result = subprocess.run([str(cli), *argv], cwd=project, env=env, capture_output=True, text=True, timeout=timeout)
        if check and result.returncode != 0:
            raise AssertionError(f"{argv}: exit {result.returncode}: {result.stderr.strip()}")
        return result

    try:
        subprocess.run(["git", "init", "-q"], cwd=project, check=True)
        log = open(home / "smoke-daemon.log", "wb")
        daemon = subprocess.Popen([str(daemon_binary)], cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        for _ in range(100):
            time.sleep(0.1)
            probe = run("ping", check=False, timeout=10)
            if probe.returncode == 0:
                break
        step("the daemon answers ping over the local transport", probe.returncode == 0, probe.stderr.strip())
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
        if os.name == "nt":
            attach = run("attach", "smoke-one", check=False, timeout=20)
            step("attach is refused on Windows in words, not with a hang", attach.returncode != 0 and "not available on Windows" in attach.stderr, attach.stderr.strip())
            reload = run("daemon", "reload", check=False, timeout=20)
            step("daemon reload is refused on Windows in words", reload.returncode != 0 and "Windows" in (reload.stderr + reload.stdout), (reload.stderr + reload.stdout).strip())
            launch = run("run", "--name", "smoke-managed", "--", "cmd", "/c", "echo", "hi", check=False, timeout=20)
            step("a managed launch is refused on Windows in words", launch.returncode != 0 and "not available on Windows" in (launch.stderr + launch.stdout), (launch.stderr + launch.stdout).strip())
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
        stop = run("daemon", "stop")
        for _ in range(100):
            if daemon.poll() is not None:
                break
            time.sleep(0.1)
        step("daemon stop ends the daemon", daemon.poll() is not None, stop.stdout.strip())
        report["result"] = "passed"
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.kill()
            daemon.wait()
        try:
            report["daemon_log_tail"] = (home / "smoke-daemon.log").read_text(errors="replace")[-2000:]
        except OSError:
            pass
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / "windows-daemon-smoke.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(report, indent=2))
    return 0 if report["result"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
