#!/usr/bin/env python3
"""Isolated Windows connector Task Scheduler and HTTP lifecycle acceptance.

Runs only the extracted package with a random private home. The public URL is
inert; no tunnel, provider account, browser login, real consent or reboot is
exercised. Seeded grant bytes establish preservation, not OAuth acceptance.
"""
import argparse
import base64
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid


def read_snapshot(path):
    """Read fixture snapshots without blocking the product's atomic replacement."""
    import ctypes as c
    import msvcrt
    from ctypes import wintypes as w
    kernel = c.WinDLL('kernel32', use_last_error=True)
    kernel.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, c.c_void_p,
                                  w.DWORD, w.DWORD, w.HANDLE]
    kernel.CreateFileW.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    handle = kernel.CreateFileW(str(path), 0x80000000, 7, None, 3, 0, None)
    if handle == c.c_void_p(-1).value:
        raise c.WinError(c.get_last_error())
    try:
        descriptor = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
    except BaseException:
        kernel.CloseHandle(handle)
        raise
    with os.fdopen(descriptor, 'rb') as snapshot:
        raw = snapshot.read(32769)
    assert len(raw) <= 32768, 'fixture service snapshot exceeds its bound'
    return json.loads(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--installed-prefix', type=Path)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('native Windows and an interactive logon are required')
    import psutil
    from windows_smoke_pipe import WindowsSmokePipe

    binaries = args.binary_dir.resolve(strict=True)
    cli, daemon = (binaries / name for name in ('agentdocker.exe', 'agentd.exe'))
    selected = binaries
    if args.installed_prefix:
        store = args.installed_prefix.resolve(strict=True) / 'AgentDocker/desktop'
        assert os.path.samefile(binaries, store / 'bin')
        activation = json.loads((store / 'activation.json').read_text(encoding='utf-8'))
        selected = store / 'versions' / activation['current']['id'] / 'AgentDocker'
    selected_cli, selected_daemon = (selected / name for name in ('agentdocker.exe', 'agentd.exe'))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    token = uuid.uuid4().hex
    home = Path(tempfile.gettempdir()).resolve() / ('AgentDocker connector ü ' + token)
    assert not home.exists()
    endpoint = '\\\\.\\pipe\\agentdocker-connector-smoke-' + token
    env = {k: v for k, v in os.environ.items() if not k.startswith('AGENTDOCKER_')}
    env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=endpoint,
               AGENTDOCKER_NO_AUTOSTART='1')
    root = home / 'connector'
    receipt_path = root / 'windows-service.json'
    running_path = root / 'windows-running.json'
    stop_path = root / 'windows-stop.json'
    state_path = root / 'state.json'
    origin = 'https://connector-smoke.example.invalid'
    serve_args = ['--public-url', origin, '--bind', '127.0.0.1:0']
    report = {'result': 'failed', 'scope': __doc__, 'home': str(home),
              'installed_launchers': bool(args.installed_prefix),
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'binary_sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                for p in (cli, daemon)},
              'steps': [], 'cleanup_errors': [], 'forced_processes': []}
    processes = []
    foreign_task = foreign_xml = None
    modified_task = original_xml = None
    daemon_installed = connector_installed = False

    def save():
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')

    def step(name, ok, detail=None):
        report['steps'].append({'step': name, 'ok': bool(ok), 'detail': detail})
        save()
        print(('PASS ' if ok else 'FAIL ') + name, flush=True)
        assert ok, name

    def run(*arguments, check=True, timeout=90):
        with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
            result = subprocess.run([str(cli), *arguments], env=env, cwd=binaries,
                                    stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                    timeout=timeout)
            stdout.seek(0); stderr.seek(0)
            value = {'args': arguments, 'exit': result.returncode,
                     'stdout': stdout.read().decode('utf-8', errors='replace'),
                     'stderr': stderr.read().decode('utf-8', errors='replace')}
        with (output / 'commands.jsonl').open('a', encoding='utf-8') as stream:
            stream.write(json.dumps(value) + '\n')
        if check:
            assert result.returncode == 0, value
        return value

    def quote(value):
        return "'" + value.replace("'", "''") + "'"

    def powershell(script, check=True):
        executable = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
        script = ("$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';"
                  "[Console]::OutputEncoding=[Text.UTF8Encoding]::new();" + script)
        encoded = base64.b64encode(script.encode('utf-16le')).decode('ascii')
        result = subprocess.run([str(executable), '-NoProfile', '-NonInteractive',
                                 '-EncodedCommand', encoded], env=env, cwd=binaries,
                                stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
        value = {'script': script, 'exit': result.returncode,
                 'stdout': result.stdout.decode('utf-8', errors='replace'),
                 'stderr': result.stderr.decode('utf-8', errors='replace')}
        with (output / 'scheduler-fixture.jsonl').open('a', encoding='utf-8') as stream:
            stream.write(json.dumps(value) + '\n')
        if not check:
            return value
        assert result.returncode == 0, value
        return value['stdout'].strip()

    def task_xml(name):
        return powershell(
            f"$tasks=@(Get-ScheduledTask -ErrorAction Stop | Where-Object {{"
            f"$_.TaskPath -ieq '\\' -and $_.TaskName -ieq {quote(name)}}});"
            "if($tasks.Count -gt 1){throw 'ambiguous fixture task'};"
            f"if($tasks.Count -eq 1){{Export-ScheduledTask -TaskPath '\\' -TaskName {quote(name)}}}")

    def restore_task(name, xml):
        encoded = base64.b64encode(xml.encode('utf-8')).decode('ascii')
        powershell(f"$xml=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}'));"
                   f"Register-ScheduledTask -TaskPath '\\' -TaskName {quote(name)} -Xml $xml -Force | Out-Null")

    def ping():
        pipe = WindowsSmokePipe(endpoint, timeout=1, read_timeout=2, write_timeout=2)
        try:
            data = memoryview(b'{"op":"ping"}\n')
            while data:
                count = pipe.write(data)
                assert count
                data = data[count:]
            pipe.flush()
            value = json.loads(pipe.readline())
            assert value.get('type') == 'pong', value
            return value
        finally:
            pipe.close()

    def remember(pid, executable, arguments):
        process = psutil.Process(pid)
        assert os.path.samefile(process.exe(), executable)
        argv = process.cmdline()
        assert all(argument in argv for argument in arguments), argv
        processes.append(process)
        return process

    def daemon_ready():
        until = time.monotonic() + 20
        while time.monotonic() < until:
            try:
                value = ping()
                process = remember(value['pid'], selected_daemon, ['--home'])
                argv = process.cmdline()
                assert os.path.samefile(argv[argv.index('--home') + 1], home)
                supervisor = process.parent()
                assert supervisor is not None
                if args.installed_prefix:
                    remember(supervisor.pid, daemon, ['--home'])
                    supervisor = supervisor.parent()
                    assert supervisor is not None
                remember(supervisor.pid, selected_cli, ['daemon', 'supervise'])
                if args.installed_prefix:
                    bootstrap = supervisor.parent()
                    assert bootstrap is not None
                    remember(bootstrap.pid, cli, ['daemon', 'supervise'])
                return process
            except (OSError, TimeoutError, psutil.Error):
                time.sleep(0.1)
        raise TimeoutError('owned daemon service did not become ready')

    def connector_ready(previous=None, expected_origin=origin, seconds=25):
        until = time.monotonic() + seconds
        last = None
        while time.monotonic() < until:
            try:
                status = read_snapshot(root / 'serve.json')
                running = read_snapshot(running_path)
                # Separate atomic snapshots can straddle a crash replacement.
                # Only a matching live generation can establish readiness.
                if status['pid'] != running['process']['pid']:
                    time.sleep(0.1)
                    continue
                candidate = psutil.Process(status['pid'])
                birth = datetime.datetime.fromisoformat(running['process']['started_at']).timestamp()
                assert abs(candidate.create_time() - birth) < 0.000002
                process = remember(status['pid'], selected_cli,
                                   ['connector', 'service-run', '--owner', running['owner'], endpoint])
                if args.installed_prefix:
                    bootstrap = process.parent()
                    assert bootstrap is not None
                    remember(bootstrap.pid, cli, ['connector', 'service-run'])
                identity = (process.pid, process.create_time())
                if previous == identity or status['public_url'] != expected_origin:
                    time.sleep(0.1)
                    continue
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                with opener.open('http://' + status['bind'] + '/.well-known/oauth-authorization-server',
                                 timeout=2) as response:
                    metadata = json.load(response)
                assert metadata['issuer'] == expected_origin, metadata
                return process, identity, running
            except (OSError, TimeoutError, ValueError, psutil.Error) as error:
                last = str(error)
                time.sleep(0.1)
        raise TimeoutError(f'owned connector did not become ready: {last}')

    def fixture_json(path, value):
        # Preserve the product-created file's owner/ACL. New synthetic stop/grant
        # files copy the protected running record's ACL, including its owner SID.
        path.write_text(json.dumps(value), encoding='utf-8')
        powershell(f"Set-Acl -LiteralPath {quote(str(path))} -AclObject "
                   f"(Get-Acl -LiteralPath {quote(str(running_path))})")

    save()
    try:
        native_code = "import sys;sys.stderr.write('fixture-stderr\\n');sys.stderr.flush();print('fixture-stdout')"
        controls = {}
        for preference in ('Stop', 'Continue'):
            control_log = output / ('native-stderr-' + preference + '.log')
            controls[preference] = powershell(
                f"$ErrorActionPreference={quote(preference)}; & {quote(sys.executable)} "
                f"-c {quote(native_code)} *> {quote(str(control_log))}; exit $LASTEXITCODE", check=False)
        step('Windows PowerShell Stop treats normal native stderr as a task failure',
             controls['Stop']['exit'] != 0, controls['Stop']['exit'])
        log_bytes = (output / 'native-stderr-Continue.log').read_bytes()
        control_text = log_bytes.decode('utf-16' if log_bytes.startswith(b'\xff\xfe') else 'utf-8', errors='replace')
        step('native exit handling preserves success and captures normal stderr',
             controls['Continue']['exit'] == 0 and 'fixture-stderr' in control_text and 'fixture-stdout' in control_text)
        run('connector', 'enable', *serve_args, '--dry-run')
        step('cold dry-run leaves no state home', not home.exists())
        missing = run('connector', 'enable', *serve_args, check=False)
        step('connector refuses a missing daemon login dependency', missing['exit'] != 0 and
             'daemon login service' in missing['stderr'] and not receipt_path.exists())
        daemon_installed = True
        run('daemon', 'install')
        daemon_process = daemon_ready()
        daemon_identity = (daemon_process.pid, daemon_process.create_time())
        connector_installed = True
        run('connector', 'enable', *serve_args)
        first, identity, running = connector_ready()
        receipt = read_snapshot(receipt_path)
        task = receipt['current']['task']
        report['task'] = task
        step('connector serves HTTP through its owned task and selected daemon endpoint',
             ping()['pid'] == daemon_identity[0], {'connector': identity, 'daemon': daemon_identity})
        seeded = {'clients': {}, 'grants': {'fixture-preserved-grant': {
            'agent_id': 'fixture-only', 'agent_name': 'private fixture', 'runtime': 'browser',
            'project': str(home), 'client_id': 'fixture-only', 'vendor': 'other',
            'created_at': report['started_at'], 'refresh_hash': 'fixture-not-a-real-token'}}}
        fixture_json(state_path, seeded)
        state_bytes = state_path.read_bytes()
        run('connector', 'enable', *serve_args)
        same, same_identity, _ = connector_ready()
        step('enabling the same definition preserves its live process', same_identity == identity)
        stale = dict(running, nonce='stale-' + token)
        fixture_json(stop_path, stale)
        time.sleep(0.5)
        step('stale generation stop request leaves the connector live', connector_ready()[1] == identity)
        changed_args = ['--public-url', origin + '/replacement', '--bind', '127.0.0.1:0']
        refused = run('connector', 'enable', *changed_args, check=False)
        step('desktop enable refuses different settings without stopping the live service',
             refused['exit'] != 0 and 'differently configured' in refused['stderr'] and
             connector_ready()[1] == identity and state_path.read_bytes() == state_bytes)

        # Changed Scheduler ownership must be rejected BEFORE sending a graceful
        # stop request. The fixture owns the original task and restores it exactly.
        modified_task, original_xml = task, task_xml(task)
        altered_description = 'changed private fixture ' + token
        powershell(f"$task=Get-ScheduledTask -TaskPath '\\' -TaskName {quote(task)};"
                   f"$task.Description={quote(altered_description)};"
                   "Set-ScheduledTask -InputObject $task | Out-Null")
        altered_xml = task_xml(task)
        refused = run('connector', 'uninstall', check=False)
        step('changed task ownership is refused before touching the running connector',
             refused['exit'] != 0 and task_xml(task) == altered_xml and
             connector_ready()[1] == identity and read_snapshot(stop_path) == stale)
        restore_task(task, original_xml)
        modified_task = original_xml = None

        # Simulate interrupted registration: the actual old task is previous;
        # the intended current definition was never installed.
        interrupted = dict(receipt, current=dict(receipt['current'], arguments='uninstalled fixture action'),
                           previous=receipt['current'])
        fixture_json(receipt_path, interrupted)
        refused = run('connector', 'enable', *serve_args, check=False)
        step('conservative enable preserves an interrupted registration for explicit repair',
             refused['exit'] != 0 and connector_ready()[1] == identity)
        run('connector', 'install', *changed_args)
        first.wait(timeout=10)
        second, replacement, _ = connector_ready(previous=identity, expected_origin=origin + '/replacement')
        step('explicit install repairs the previous definition and replaces only the connector',
             replacement != identity and ping()['pid'] == daemon_identity[0] and
             state_path.read_bytes() == state_bytes and
             read_snapshot(receipt_path)['previous'] is None)
        second.kill()
        second.wait(timeout=10)
        recovered, recovered_identity, _ = connector_ready(
            previous=replacement, expected_origin=origin + '/replacement', seconds=100)
        step('Scheduler restarts a crashed connector without replacing its daemon or grants',
             recovered_identity != replacement and ping()['pid'] == daemon_identity[0] and
             state_path.read_bytes() == state_bytes)
        run('connector', 'uninstall')
        connector_installed = False
        recovered.wait(timeout=10)
        step('graceful connector uninstall preserves daemon identity and seeded grant bytes',
             not receipt_path.exists() and not running_path.exists() and not stop_path.exists() and
             not (root / 'serve.json').exists() and not task_xml(task) and
             ping()['pid'] == daemon_identity[0] and daemon_process.is_running() and
             state_path.read_bytes() == state_bytes)
        run('connector', 'uninstall')
        step('repeated uninstall leaves retained daemon and grants intact',
             ping()['pid'] == daemon_identity[0] and state_path.read_bytes() == state_bytes)

        foreign_task = task.swapcase()
        assert not task_xml(foreign_task)
        inert = base64.b64encode('exit 0'.encode('utf-16le')).decode('ascii')
        shell = str(Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe')
        description = 'private collision fixture ' + token
        powershell("$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;"
                   f"$action=New-ScheduledTaskAction -Execute {quote(shell)} "
                   f"-Argument {quote('-NoProfile -NonInteractive -EncodedCommand ' + inert)};"
                   "$principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited;"
                   f"Register-ScheduledTask -TaskPath '\\' -TaskName {quote(foreign_task)} "
                   f"-Action $action -Principal $principal -Description {quote(description)} | Out-Null")
        foreign_xml = task_xml(foreign_task)
        assert description in foreign_xml
        for operation in ('enable', 'install', 'uninstall'):
            arguments = [] if operation == 'uninstall' else serve_args
            refused = run('connector', operation, *arguments, check=False)
            step(f'{operation} preserves a foreign case-variant task', refused['exit'] != 0 and
                 task_xml(foreign_task) == foreign_xml and not receipt_path.exists())
        assert task_xml(foreign_task) == foreign_xml
        powershell(f"Unregister-ScheduledTask -TaskPath '\\' -TaskName {quote(foreign_task)} -Confirm:$false")
        assert not task_xml(foreign_task)
        foreign_task = foreign_xml = None
        report['foreign_task_removed'] = True
        if args.installed_prefix:
            connector_installed = True
            run('connector', 'enable', *serve_args)
            stopped, _, generation = connector_ready()
            fixture_json(stop_path, generation)
            stopped.wait(timeout=25)
            step('owned stop exits cleanly while preserving its login registration',
                 not running_path.exists() and not stop_path.exists() and receipt_path.exists() and
                 state_path.read_bytes() == state_bytes)
            run('daemon', 'uninstall')
            daemon_installed = False
            daemon_process.wait(timeout=10)
            blocked = run('desktop', '--prefix', str(args.installed_prefix), 'uninstall', '--preview', check=False)
            step('a stopped connector alone protects installed launchers after daemon uninstall',
                 blocked['exit'] != 0 and 'service references this installation' in blocked['stderr'])
            maintenance = json.loads(run('desktop', '--prefix', str(args.installed_prefix),
                                         'prune', '--preview')['stdout'])
            step('connector registration on stable paths does not pin unrelated inactive versions',
                 all(item['reason'] != 'a stopped service may reference retained binaries'
                     for item in maintenance['maintenance']['retained']))
            run('connector', 'uninstall')
            connector_installed = False
            step('stopped connector can uninstall after its dependency was removed',
                 not receipt_path.exists() and not task_xml(task) and state_path.read_bytes() == state_bytes)
        report['result'] = 'passed'
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
    finally:
        if modified_task:
            try:
                restore_task(modified_task, original_xml)
            except Exception as error:
                report['cleanup_errors'].append(f'restore changed fixture task: {error}')
        if connector_installed or receipt_path.exists():
            try:
                run('connector', 'uninstall')
                connector_installed = False
            except Exception as error:
                report['cleanup_errors'].append(f'connector uninstall: {error}')
        if foreign_task:
            try:
                current = task_xml(foreign_task)
                if current:
                    assert foreign_xml and current == foreign_xml, 'foreign fixture ownership changed'
                    powershell(f"Unregister-ScheduledTask -TaskPath '\\' -TaskName {quote(foreign_task)} -Confirm:$false")
                assert not task_xml(foreign_task)
                report['foreign_task_removed'] = True
            except Exception as error:
                report['cleanup_errors'].append(f'foreign fixture: {error}')
        if daemon_installed:
            try:
                run('daemon', 'uninstall')
                daemon_installed = False
            except Exception as error:
                report['cleanup_errors'].append(f'daemon uninstall: {error}')
        seen = set()
        for process in reversed(processes):
            key = (process.pid, process.create_time())
            if key in seen:
                continue
            seen.add(key)
            try:
                process.wait(timeout=10)
            except psutil.TimeoutExpired:
                report['forced_processes'].append(key)
                try:
                    process.kill()
                    process.wait(timeout=10)
                except psutil.Error as error:
                    report['cleanup_errors'].append(f'owned process {key}: {error}')
            except psutil.Error as error:
                report['cleanup_errors'].append(f'owned process {key}: {error}')
        for path in (home / 'agentd.log', root / 'serve.log', receipt_path, running_path, stop_path):
            if path.is_file():
                (output / ('retained-' + path.name)).write_bytes(path.read_bytes()[-1024 * 1024:])
        if report['cleanup_errors'] or report['forced_processes'] or daemon_installed or connector_installed:
            report['result'] = 'failed'
        else:
            shutil.rmtree(home, ignore_errors=True)
        report['scratch_removed'] = not home.exists()
        if not report['scratch_removed']:
            report['result'] = 'failed'
        save()
    print(json.dumps(report), flush=True)
    return int(report['result'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
