#!/usr/bin/env python3
"""Actual Windows Codex TUI/native receiver acceptance against a loopback model.

Uses a private profile, daemon and ConPTY; no provider account, external model,
saved user configuration, installed service or physical keyboard is involved.
Checks SessionStart bootstrap, idle delivery, preserved drafts, busy FIFO,
exact receipts, receiver crash/restart without replay and recovery preview.
Requires pywinpty 3.0.5 and psutil 7.0.0 on native Windows.
"""
import argparse
import datetime
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def wait(predicate, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise TimeoutError("acceptance condition was not reached")


def current_user_objects():
    """Only this fixture process/children: avoid elevated-runner group ownership.

    Match a normal desktop user's default object owner before creating private
    files or the TUI. Never adopt or rewrite an existing file's owner or ACL.
    """
    import ctypes as c
    from ctypes import wintypes as w

    kernel = c.WinDLL("kernel32", use_last_error=True)
    security = c.WinDLL("advapi32", use_last_error=True)
    kernel.GetCurrentProcess.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    security.OpenProcessToken.argtypes = [w.HANDLE, w.DWORD, c.POINTER(w.HANDLE)]
    security.GetTokenInformation.argtypes = [w.HANDLE, c.c_int, c.c_void_p, w.DWORD, c.POINTER(w.DWORD)]
    security.SetTokenInformation.argtypes = [w.HANDLE, c.c_int, c.c_void_p, w.DWORD]
    token = w.HANDLE()
    if not security.OpenProcessToken(kernel.GetCurrentProcess(), 0x80 | 0x8, c.byref(token)):
        raise c.WinError(c.get_last_error())
    try:
        size = w.DWORD()
        security.GetTokenInformation(token, 1, None, 0, c.byref(size))
        if not 0 < size.value <= 65536:
            raise OSError("invalid current-user token size")
        data = c.create_string_buffer(size.value)
        if not security.GetTokenInformation(token, 1, data, size, c.byref(size)):
            raise c.WinError(c.get_last_error())
        # TOKEN_USER starts with SID_AND_ATTRIBUTES; TOKEN_OWNER contains PSID.
        owner = c.c_void_p.from_buffer_copy(data)
        if not security.SetTokenInformation(token, 4, c.byref(owner), c.sizeof(owner)):
            raise c.WinError(c.get_last_error())
    finally:
        kernel.CloseHandle(token)


def fixture_controller(psutil, binding, executable):
    """Pin the daemon-verified launch, including the hook-spawned first child.

    Only later crash replacements are spawned by agentd; Windows cannot supply
    a live ancestry chain once the initial hook exits. Never select by name.
    """
    process = psutil.Process(binding['controller']['pid'])
    expected_birth = datetime.datetime.fromisoformat(binding['controller']['started_at']).timestamp()
    # psutil and datetime expose microsecond/float timestamps, whereas the
    # product's authenticated binding retains the native 100-nanosecond value.
    assert abs(process.create_time() - expected_birth) < 0.000002
    assert os.path.samefile(process.exe(), executable)
    assert process.cmdline()[1:] == binding['launch']['args']
    return process


def remove_fixture(root):
    def clear_readonly(function, path, error):
        candidate = Path(path)
        metadata = candidate.lstat()
        # Git creates read-only pack files. Only clear that attribute on a
        # regular file inside this newly created fixture; ACL failures remain.
        if (not isinstance(error, PermissionError) or not stat.S_ISREG(metadata.st_mode)
                or not metadata.st_file_attributes & stat.FILE_ATTRIBUTE_READONLY
                or candidate.is_symlink() or not candidate.resolve().is_relative_to(root)):
            raise error
        candidate.chmod(metadata.st_mode | stat.S_IWRITE)
        function(path)
    shutil.rmtree(root, onexc=clear_readonly)


def response_events(number):
    text = f"WINDOWS_FIXTURE_OK_{number}"
    item = {"id": f"msg_{number}", "type": "message", "role": "assistant",
            "status": "completed", "content": [{"type": "output_text", "text": text, "annotations": []}]}
    response = {"id": f"resp_{number}", "object": "response", "model": "fixture-model",
                "status": "completed", "output": [item],
                "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7}}
    return [
        {"type": "response.created", "response": dict(response, status="in_progress", output=[])},
        {"type": "response.output_item.added", "output_index": 0,
         "item": dict(item, status="in_progress", content=[])},
        {"type": "response.content_part.added", "item_id": item["id"], "output_index": 0,
         "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}},
        {"type": "response.output_text.delta", "item_id": item["id"], "output_index": 0,
         "content_index": 0, "delta": text},
        {"type": "response.output_text.done", "item_id": item["id"], "output_index": 0,
         "content_index": 0, "text": text},
        {"type": "response.output_item.done", "output_index": 0, "item": item},
        {"type": "response.completed", "response": response},
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--scenario", choices=("startup", "established"), default="startup",
                        help="strict zero-prompt startup, or explicitly begin one fixture turn before delivery")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("requires native Windows; use native_codex_queue_smoke.py on Unix")
    import psutil
    from winpty import PtyProcess
    from winpty.enums import Backend
    from windows_smoke_pipe import WindowsSmokePipe

    current_user_objects()
    binaries = args.binary_dir.resolve(strict=True)
    cli, daemon_exe = binaries / "agentdocker.exe", binaries / "agentd.exe"
    codex = args.codex.resolve(strict=True)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix="AgentDocker Codex ü ")).resolve()
    profile, repo, home = root / "profile", root / "project", root / "state"
    profile.mkdir()
    repo.mkdir()
    initial_prompt = "WINDOWS_FIXTURE_WARMUP" if args.scenario == "established" else None
    request_offset = int(initial_prompt is not None)
    report = {"result": "failed", "scope": __doc__, "scenario": args.scenario,
              "initial_prompt": initial_prompt, "requests": [], "sent": [],
              "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "binary_sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                for p in (cli, daemon_exe, codex)},
              "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    busy, release, closing = threading.Event(), threading.Event(), threading.Event()
    output, reader_errors = [], []
    provider = daemon = server = None
    owned = []
    reader = None
    daemon_log = (out / "daemon.log").open("wb")
    sock = rf"\\.\pipe\agentdocker-codex-trial-{uuid.uuid4().hex}"
    # Explicit allowlist: no account/provider credentials or inherited AD binding.
    env = {k: v for k, v in os.environ.items() if k.upper() in {
        "PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP",
        "USERPROFILE", "APPDATA", "LOCALAPPDATA", "HOMEDRIVE", "HOMEPATH"}}
    env.update(CODEX_HOME=str(profile), AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=sock,
               AGENTDOCKER_NO_AUTOSTART="1", AGENTDOCKER_NO_NOTIFICATIONS="1",
               AGENTDOCKER_FIXTURE_KEY="fixture-only", TERM="xterm-256color")

    def rpc(value, timeout=5):
        channel = WindowsSmokePipe(sock, timeout=min(3, timeout), read_timeout=timeout,
                                   write_timeout=timeout)
        try:
            channel.write((json.dumps(value) + "\n").encode())
            result = json.loads(channel.readline())
            assert result.get("type") != "error", result
            return result
        finally:
            channel.close()

    def check_cli(*argv):
        result = subprocess.run([str(cli), *argv], cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, encoding="utf-8", timeout=30)
        assert result.returncode == 0, result.stderr
        return result.stdout

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                length = int(self.headers["Content-Length"])
                assert 0 < length <= 4 * 1024 * 1024
                assert self.headers.get("Authorization") == "Bearer fixture-only"
                body = json.loads(self.rfile.read(length))
                users = [item for item in body.get("input", []) if item.get("role") == "user"]
                latest = json.dumps(users[-1] if users else {}, ensure_ascii=False)
                # Auxiliary title requests cannot stand in for conversation turns.
                if "Generate a concise, single-line task title" in json.dumps(body):
                    number = 0
                else:
                    number = len(report["requests"]) + 1
                    report["requests"].append({"latest_user": latest, "at": time.monotonic()})
                    if "BUSY_NATIVE" in latest:
                        busy.set()
                        assert release.wait(60), "busy fixture was not released"
                data = "".join("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n"
                               for event in response_events(number)).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception:
                report.setdefault("fixture_errors", []).append(traceback.format_exc())
                self.close_connection = True

    try:
        subprocess.run(["git", "init", "-q", str(repo)], check=True, timeout=10)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        config = ('model = "fixture-model"\nmodel_provider = "fixture"\napproval_policy = "never"\n'
                  'sandbox_mode = "danger-full-access"\ncheck_for_update_on_startup = false\n'
                  '[features]\nhooks = true\napps = false\n'
                  '[model_providers.fixture]\nname = "Local fixture"\nbase_url = '
                  + json.dumps(f"http://127.0.0.1:{server.server_port}/v1")
                  + '\nwire_api = "responses"\nenv_key = "AGENTDOCKER_FIXTURE_KEY"\n'
                  'request_max_retries = 0\nstream_max_retries = 0\nsupports_websockets = false\n'
                  '[projects.' + json.dumps(str(repo)) + ']\ntrust_level = "trusted"\n'
                  '[mcp_servers.agentdocker]\ncommand = ' + json.dumps(str(cli))
                  + '\nargs = ["mcp", "--runtime", "codex"]\nrequired = true\n'
                  '[mcp_servers.agentdocker.env]\nAGENTDOCKER_HOME = ' + json.dumps(str(home))
                  + '\nAGENTDOCKER_SOCKET = ' + json.dumps(sock)
                  + '\nAGENTDOCKER_NO_AUTOSTART = "1"\n')
        (profile / "config.toml").write_text(config, encoding="utf-8")
        hook = root / "hook.py"
        hook.write_text(
            "import subprocess,sys\nfrom pathlib import Path\n"
            "raw=sys.stdin.read()\n"
            f"p=subprocess.run({[str(cli), 'hook', 'codex']!r},input=raw,text=True,capture_output=True,timeout=20)\n"
            f"Path({str(out / 'hook-stderr.log')!r}).write_text(p.stderr,encoding='utf-8')\n"
            "print(p.stdout,end='')\nsys.exit(p.returncode)\n", encoding="utf-8")
        (profile / "hooks.json").write_text(json.dumps({"hooks": {"SessionStart": [{"hooks": [{
            "type": "command", "command": subprocess.list2cmdline([sys.executable, str(hook)])}]}]}}),
            encoding="utf-8")
        profile_hashes = {name: hashlib.sha256((profile / name).read_bytes()).hexdigest()
                          for name in ("config.toml", "hooks.json")}
        report["provider_version"] = subprocess.check_output([str(codex), "--version"],
            env=env, text=True, timeout=10).strip()
        daemon = subprocess.Popen([str(daemon_exe)], cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                  stdout=daemon_log, stderr=daemon_log)
        owned.append(psutil.Process(daemon.pid))
        started = time.monotonic()

        def ready():
            if daemon.poll() is not None:
                raise RuntimeError("private daemon exited before readiness")
            remaining = 10 - (time.monotonic() - started)
            if remaining <= 0:
                raise TimeoutError("private daemon did not answer within ten seconds")
            try:
                return rpc({"op": "ping"}, timeout=min(0.2, remaining / 3)).get("type") == "pong"
            except OSError:
                return False

        # Observe the same manually started process; do not restart it or rely
        # on CLI autostart. The product's ten-second readiness bound is kept.
        wait(ready, 10)
        report["daemon_ready_seconds"] = time.monotonic() - started
        check_cli("ping")
        # One-off trust is limited to the fixture's sole SessionStart hook.
        provider_argv = [str(codex), "--no-alt-screen", "--dangerously-bypass-hook-trust"]
        if initial_prompt is not None:
            provider_argv.append(initial_prompt)
        provider = PtyProcess.spawn(provider_argv,
                                    cwd=str(repo), env=env, dimensions=(40, 160), backend=Backend.ConPTY)
        owned.append(psutil.Process(provider.pid))
        report["provider_pid"] = provider.pid

        def read_terminal():
            tail = ""
            try:
                while provider.isalive():
                    data = provider.read(65536)
                    output.append(data)
                    combined = tail + data
                    if "\x1b[6n" in combined:
                        provider.write("\x1b[1;1R")
                    tail = combined[-3:]
                    if sum(map(len, output)) > 8 * 1024 * 1024:
                        raise ValueError("terminal output exceeded its bound")
            except EOFError:
                pass
            except Exception as error:
                if not closing.is_set():
                    reader_errors.append(str(error))

        reader = threading.Thread(target=read_terminal, daemon=True)
        reader.start()
        agent = wait(lambda: next((a for a in rpc({"op": "list", "all": True})["agents"]
                                  if a.get("pid") == provider.pid and a.get("input_binding")), None), 45)
        assert not agent["managed"]
        wait(lambda: len(report["requests"]) >= request_offset)
        assert len(report["requests"]) == request_offset
        if initial_prompt is not None:
            assert initial_prompt in report["requests"][0]["latest_user"]
            wait(lambda: "WINDOWS_FIXTURE_OK_1" in "".join(output))
        assert agent["input_binding"].get("launch")
        aid = agent["id"]
        report["agent"] = aid
        report["binding"] = agent["input_binding"]
        controller = fixture_controller(psutil, agent["input_binding"], cli)
        owned.append(controller)
        peer = rpc({"op": "register", "spec": {"name": "native-fixture-peer"}, "pid": None})["agent"]["id"]

        def send(text, sender=peer):
            response = rpc({"op": "send", "from": sender, "to": aid, "kind": "chat", "payload": {"text": text}})
            report["sent"].append({"text": text, "id": response["message"]})
            assert rpc({"op": "delivery_queue", "agent": aid})["type"] == "input_owned"
            assert rpc({"op": "inbox", "agent": aid, "drain": False})["type"] == "input_owned"

        def received(number, text):
            number += request_offset
            wait(lambda: len(report["requests"]) >= number)
            assert len(report["requests"]) == number
            assert text in report["requests"][number - 1]["latest_user"]
            wait(lambda: f"WINDOWS_FIXTURE_OK_{number}" in "".join(output))

        send("PEER_NATIVE_IDLE")
        received(1, "PEER_NATIVE_IDLE")
        provider.write("UNSUBMITTED_NATIVE_DRAFT")
        time.sleep(1)
        send("PEER_NATIVE_DRAFT")
        received(2, "PEER_NATIVE_DRAFT")
        assert "UNSUBMITTED_NATIVE_DRAFT" not in report["requests"][1 + request_offset]["latest_user"]
        provider.write("\r")
        received(3, "UNSUBMITTED_NATIVE_DRAFT")
        provider.write("BUSY_NATIVE")
        time.sleep(0.3)
        provider.write("\r")
        assert busy.wait(25), "TUI did not enter the held provider turn"
        send("PEER_NATIVE_BUSY")
        send("HUMAN_NATIVE_BUSY", "user")
        time.sleep(2)
        assert len(report["requests"]) == 4 + request_offset
        release.set()
        assert "BUSY_NATIVE" in report["requests"][3 + request_offset]["latest_user"]
        wait(lambda: f"WINDOWS_FIXTURE_OK_{4 + request_offset}" in "".join(output))
        wait(lambda: len(report["requests"]) >= 6 + request_offset, 45)
        assert len(report["requests"]) == 6 + request_offset
        assert "PEER_NATIVE_BUSY" in report["requests"][4 + request_offset]["latest_user"]
        assert "HUMAN_NATIVE_BUSY" in report["requests"][5 + request_offset]["latest_user"]
        wait(lambda: f"WINDOWS_FIXTURE_OK_{6 + request_offset}" in "".join(output))
        wait(lambda: not rpc({"op": "peek_input", "agent": aid})["messages"])
        ledgerpath = home / "codex-queue" / aid / "delivery.json"
        wait(lambda: len(json.loads(ledgerpath.read_text(encoding="utf-8"))["completed"]) == len(report["sent"]))
        before = json.loads(ledgerpath.read_text(encoding="utf-8"))
        assert [r["message"] for r in before["completed"]] == [r["id"] for r in report["sent"]]
        thread = agent["input_binding"]["provider"]["session"]
        assert all(r["receipt"]["thread"] == thread for r in before["completed"])
        report["recovery_preview"] = json.loads(check_cli("codex-queue-resolve", "--agent", aid))
        controller.kill()
        controller.wait(timeout=5)
        def replacement():
            agent = rpc({"op": "inspect", "agent": aid})["agent"]
            binding = agent.get("input_binding") or {}
            pid = binding.get("controller", {}).get("pid")
            return agent if pid is not None and pid != controller.pid else None

        rebound = wait(replacement, 30)
        new_controller = fixture_controller(psutil, rebound["input_binding"], cli)
        assert daemon.pid in [p.pid for p in new_controller.parents()]
        owned.append(new_controller)
        wait(lambda: rpc({"op": "inspect", "agent": aid})["agent"]["input_delivery"].get("paused") is False)
        time.sleep(2)
        assert len(report["requests"]) == 6 + request_offset, "receiver restart replayed provider input"
        send("PEER_NATIVE_RECOVERED")
        received(7, "PEER_NATIVE_RECOVERED")
        wait(lambda: len(json.loads(ledgerpath.read_text(encoding="utf-8"))["completed"]) == len(report["sent"]))
        after = json.loads(ledgerpath.read_text(encoding="utf-8"))
        assert after["completed"][:len(before["completed"])] == before["completed"]
        assert [r["message"] for r in after["completed"]] == [r["id"] for r in report["sent"]]
        assert all(r["receipt"]["thread"] == thread for r in after["completed"])
        assert profile_hashes == {name: hashlib.sha256((profile / name).read_bytes()).hexdigest() for name in profile_hashes}
        report.update(result="passed", completed_receipts=after["completed"], thread=thread,
                      automatic_bootstrap=True, startup_without_prompt=initial_prompt is None,
                      draft_preserved=True, receiver_restart_no_replay=True)
    except Exception:
        report["error"] = traceback.format_exc()
        if provider is not None and provider.isalive():
            try:
                provider.write("\x14")  # Capture Codex's startup-issue details.
                time.sleep(0.3)
            except Exception:
                pass
    finally:
        closing.set()
        release.set()
        cleanup = []
        # Codex 0.160 can detach its app-server and PID updater from the TUI.
        # A kernel image inside this fresh profile's cache proves fixture
        # ownership even after the original parent exits. Never select by name.
        private_cache = (profile / "packages/app-server-daemon/releases").resolve()
        detached = []
        for pid in psutil.pids():
            try:
                process = psutil.Process(pid)
                image = Path(process.exe()).resolve(strict=True)
                if not image.is_relative_to(private_cache):
                    continue
                if image.name.lower() != "codex.exe" or "app-server" not in process.cmdline():
                    raise RuntimeError("unrecognized executable in private provider cache")
                if process.is_running():
                    detached.append({"pid": pid, "created": process.create_time(),
                                     "executable": str(image)})
                    owned.append(process)
            except (psutil.NoSuchProcess, psutil.AccessDenied, FileNotFoundError):
                continue
            except Exception as error:
                cleanup.append(str(error))
        report["detached_fixture_processes"] = detached
        # Retire only descendants of captured fixture processes. psutil verifies
        # their birth identities before signalling, protecting against PID reuse.
        for process in reversed(owned):
            try:
                if process.is_running():
                    children = process.children(recursive=True)
                    for child in reversed(children):
                        child.kill()
                    process.kill()
                    _, alive = psutil.wait_procs([process, *children], timeout=5)
                    if alive:
                        cleanup.append("owned processes did not exit: " + str([p.pid for p in alive]))
            except psutil.NoSuchProcess:
                pass
            except Exception as error:
                cleanup.append(str(error))
        if daemon is not None:
            try:
                daemon.wait(timeout=5)
            except Exception as error:
                cleanup.append(str(error))
        if reader is not None:
            reader.join(timeout=2)
        if server is not None:
            server.shutdown()
            server.server_close()
        daemon_log.close()
        report["cleanup_errors"] = cleanup
        report["reader_errors"] = reader_errors
        if cleanup or reader_errors or report.get("fixture_errors"):
            report["result"] = "failed"
        (out / "terminal.txt").write_text("".join(output), encoding="utf-8")
        if home.exists():
            for path in (home / "codex-queue").glob("*/*"):
                if path.name in ("controller.log", "delivery.json"):
                    shutil.copy2(path, out / path.name)
        for path in profile.glob("sessions/**/*.jsonl"):
            shutil.copy2(path, out / path.name)
        for path in (profile / "log").glob("*.log"):
            if path.is_file() and not path.is_symlink():
                with path.open("rb") as log:
                    log.seek(max(0, path.stat().st_size - 2 * 1024 * 1024))
                    (out / ("provider-" + path.name)).write_bytes(log.read(2 * 1024 * 1024))
        if not cleanup:
            try:
                remove_fixture(root)
            except Exception as error:
                cleanup.append(str(error))
                report["result"] = "failed"
        if cleanup:
            report["retained_scratch"] = str(root)
        (out / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"result": report["result"], "output": str(out)}))
    return 0 if report["result"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
