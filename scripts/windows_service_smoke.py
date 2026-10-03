#!/usr/bin/env python3
"""Bounded Task Scheduler lifecycle trial using an extracted Windows package.

Only the random private home's owned task and processes may be changed. This
tests install/start/stop/restart/uninstall and crash recovery in the current
interactive logon; it does not establish login/reboot or provider survival.
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
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--installed-prefix', type=Path)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('this trial requires native Windows and an interactive logon')
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
    # The product creates the protected home, not the elevated fixture process.
    home = Path(tempfile.gettempdir()).resolve() / ('AgentDocker service ü ' + token)
    assert not home.exists()
    endpoint = '\\\\.\\pipe\\agentdocker-service-smoke-' + token
    env = {k: v for k, v in os.environ.items() if not k.startswith('AGENTDOCKER_')}
    env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=endpoint,
               AGENTDOCKER_NO_AUTOSTART='1', AGENTDOCKER_STARTUP_TRACE='1')
    report = {'result': 'failed', 'scope': __doc__, 'home': str(home), 'installed_launchers': bool(args.installed_prefix),
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'binary_sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                for p in (cli, daemon)}, 'steps': [], 'cleanup_errors': []}
    processes = []
    installed = False
    foreign_task = None
    foreign_xml = None

    def quoted(value):
        return "'" + value.replace("'", "''") + "'"

    def powershell(script):
        executable = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
        script = ("$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';"
                  "[Console]::OutputEncoding=[Text.UTF8Encoding]::new();" + script)
        encoded = base64.b64encode(script.encode('utf-16le')).decode('ascii')
        result = subprocess.run([str(executable), '-NoProfile', '-NonInteractive',
                                 '-EncodedCommand', encoded], env=env, cwd=binaries,
                                stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
        observed = {'script': script, 'exit': result.returncode,
                    'stdout': result.stdout.decode('utf-8', errors='replace'),
                    'stderr': result.stderr.decode('utf-8', errors='replace')}
        with (output / 'scheduler-fixture.jsonl').open('a', encoding='utf-8') as stream:
            stream.write(json.dumps(observed) + '\n')
        assert result.returncode == 0, observed
        return observed['stdout'].strip()

    def foreign_definition():
        name = quoted(foreign_task)
        return powershell(
            f"$found=@(Get-ScheduledTask -ErrorAction Stop | Where-Object {{"
            f"$_.TaskPath -ieq '\\' -and $_.TaskName -ieq {name}}});"
            "if($found.Count -gt 1){throw 'ambiguous fixture task'};"
            f"if($found.Count -eq 1){{Export-ScheduledTask -TaskPath '\\' -TaskName {name}}}")

    def save():
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')

    def step(name, condition, detail=None):
        report['steps'].append({'step': name, 'ok': bool(condition), 'detail': detail})
        save()
        print(('PASS ' if condition else 'FAIL ') + name, flush=True)
        assert condition, name

    def run(*arguments, check=True, timeout=90):
        # File-backed capture cannot hang after the CLI exits if a descendant
        # mistakenly inherits its output. Every invocation still has a bound.
        with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
            started = time.monotonic()
            result = subprocess.run([str(cli), *arguments], env=env, cwd=binaries,
                                    stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                    timeout=timeout)
            stdout.seek(0); stderr.seek(0)
            observed = {'args': arguments, 'exit': result.returncode,
                        'seconds': time.monotonic() - started,
                        'stdout': stdout.read().decode('utf-8', errors='replace'),
                        'stderr': stderr.read().decode('utf-8', errors='replace')}
        with (output / 'commands.jsonl').open('a', encoding='utf-8') as stream:
            stream.write(json.dumps(observed) + '\n')
        if check:
            assert result.returncode == 0, observed
        return observed

    def ping():
        pipe = WindowsSmokePipe(endpoint, timeout=1, read_timeout=2, write_timeout=2)
        try:
            data = memoryview(b'{"op":"ping"}\n')
            while data:
                count = pipe.write(data)
                assert count, 'pipe write made no progress'
                data = data[count:]
            pipe.flush()
            value = json.loads(pipe.readline())
            assert value.get('type') == 'pong', value
            return value
        finally:
            pipe.close()

    def serving(previous=None, seconds=15):
        deadline = time.monotonic() + seconds
        last = None
        while time.monotonic() < deadline:
            try:
                value = ping()
                process = psutil.Process(value['pid'])
                assert os.path.samefile(value['executable'], selected_daemon), value
                assert os.path.samefile(process.exe(), selected_daemon), value
                argv = process.cmdline()
                assert '--home' in argv and os.path.samefile(argv[argv.index('--home') + 1], home)
                identity = (process.pid, process.create_time())
                if previous is None or identity != previous:
                    supervisor = process.parent()
                    if args.installed_prefix:
                        assert supervisor is not None and os.path.samefile(supervisor.exe(), daemon)
                        processes.append(supervisor)
                        supervisor = supervisor.parent()
                    assert supervisor is not None and os.path.samefile(supervisor.exe(), selected_cli)
                    if args.installed_prefix:
                        bootstrap = supervisor.parent()
                        assert bootstrap is not None and os.path.samefile(bootstrap.exe(), cli)
                        processes.append(bootstrap)
                    parent_args = supervisor.cmdline()
                    assert parent_args[1:3] == ['daemon', 'supervise'], parent_args
                    assert '--home' in parent_args and os.path.samefile(
                        parent_args[parent_args.index('--home') + 1], home)
                    processes.append(process)
                    processes.append(supervisor)
                    return process, identity
            except (OSError, psutil.Error) as error:
                last = str(error)
            time.sleep(0.1)
        raise TimeoutError(f'owned service did not reach readiness: {last}')

    def absent_for(seconds=3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                ping()
            except (OSError, TimeoutError):
                time.sleep(0.1)
                continue
            return False
        return True

    save()
    try:
        cold = run('daemon', 'status', timeout=10)
        step('fresh home has no owned service or running daemon',
             'no owned Task Scheduler service' in cold['stdout']
             and 'not running' in cold['stdout'])
        # Register cleanup before install: a failed readiness wait can leave a
        # valid task and receipt behind. Uninstall checks their exact ownership.
        installed = True
        run('daemon', 'install')
        first, identity = serving()
        receipt = json.loads((home / 'windows-service.json').read_text(encoding='utf-8'))
        assert os.path.samefile(receipt['current']['home'], home)
        report['task'] = receipt['current']['task']
        status = run('daemon', 'status')
        step('installed task starts the extracted daemon', 'Task Scheduler, Running' in status['stdout'], identity)
        run('daemon', 'stop')
        first.wait(timeout=10)
        step('explicit stop stays stopped despite crash supervision', absent_for())
        if args.installed_prefix:
            blocked = run('desktop', '--prefix', str(args.installed_prefix), 'uninstall', '--preview', check=False)
            step('a stopped installed service prevents launcher removal', blocked['exit'] != 0 and
                 'service references this installation' in blocked['stderr'])
            maintenance = json.loads(run('desktop', '--prefix', str(args.installed_prefix), 'prune', '--preview')['stdout'])
            step('a service on stable paths does not protect unrelated inactive versions',
                 all(item['reason'] != 'a stopped service may reference retained binaries'
                     for item in maintenance['maintenance']['retained']))
        run('daemon', 'start')
        second, identity = serving(previous=identity)
        step('owned service starts again', True, identity)
        run('daemon', 'restart')
        second.wait(timeout=10)
        third, identity = serving(previous=identity)
        step('restart replaces the daemon through the same owned task', True, identity)
        # psutil pins PID plus creation time for its kill operation. The image
        # and --home were checked against this exact private package and home.
        third.kill()
        third.wait(timeout=10)
        recovered, replacement = serving(previous=identity)
        step('supervisor replaces a crashed daemon without client autostart', True, replacement)
        run('daemon', 'uninstall')
        installed = False
        recovered.wait(timeout=10)
        step('uninstall removes ownership and stops the daemon',
             not (home / 'windows-service.json').exists() and absent_for())
        # A second uninstall verifies the exact derived task is absent; the
        # product refuses an unrelated task without an ownership receipt.
        repeat = run('daemon', 'uninstall')
        step('repeat uninstall verifies the task is absent', 'not installed' in repeat['stdout'])

        # Scheduler names are case-insensitive. A foreign definition with
        # different capitalization must still reach the product ownership
        # guard, rather than being treated as absent and replaced with -Force.
        foreign_task = report['task'].swapcase()
        assert foreign_task != report['task']
        assert not foreign_definition(), 'fixture task already exists'
        description = 'AgentDocker private collision fixture ' + token
        inert = base64.b64encode('exit 0'.encode('utf-16le')).decode('ascii')
        shell = str(Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe')
        powershell(
            "$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;"
            f"$action=New-ScheduledTaskAction -Execute {quoted(shell)} "
            f"-Argument {quoted('-NoProfile -NonInteractive -EncodedCommand ' + inert)};"
            "$principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited;"
            f"Register-ScheduledTask -TaskPath '\\' -TaskName {quoted(foreign_task)} "
            f"-Action $action -Principal $principal -Description {quoted(description)} | Out-Null")
        foreign_xml = foreign_definition()
        assert foreign_xml and description in foreign_xml
        (output / 'foreign-task-before.xml').write_text(foreign_xml, encoding='utf-8')
        observed_name = powershell(
            f"(Get-ScheduledTask -TaskPath '\\' -TaskName {quoted(report['task'])} -ErrorAction Stop).TaskName")
        step('Scheduler resolves the foreign task through the original capitalization',
             observed_name.casefold() == report['task'].casefold())
        for operation in ('install', 'uninstall'):
            refused = run('daemon', operation, check=False)
            # Retain process ownership even if a regression allowed install.
            if operation == 'install' and refused['exit'] == 0:
                serving()
            after = foreign_definition()
            (output / f'foreign-task-after-{operation}.xml').write_text(after, encoding='utf-8')
            step(f'{operation} refuses and preserves the case-variant foreign task',
                 refused['exit'] != 0 and after == foreign_xml
                 and not (home / 'windows-service.json').exists() and absent_for())
        report['result'] = 'passed'
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
    finally:
        if installed or (home / 'windows-service.json').exists():
            try:
                run('daemon', 'uninstall')
                installed = False
            except Exception as error:
                report['cleanup_errors'].append(f'uninstall: {error}')
        if foreign_task is not None:
            try:
                current = foreign_definition()
                if current:
                    assert foreign_xml is not None and current == foreign_xml, 'fixture ownership changed'
                    powershell(f"Unregister-ScheduledTask -TaskPath '\\' -TaskName {quoted(foreign_task)} -Confirm:$false")
                assert not foreign_definition(), 'fixture task survived cleanup'
                report['foreign_task_removed'] = True
            except Exception as error:
                report['cleanup_errors'].append(f'foreign task: {error}')
        # Supervisors were recorded after their daemons: retire them first so
        # a failed uninstall cannot spawn a replacement during fallback cleanup.
        for process in reversed(processes):
            try:
                process.wait(timeout=10)
            except psutil.TimeoutExpired:
                report['cleanup_errors'].append(f'owned process {process.pid}: still running; forced retirement')
                try:
                    # psutil rechecks PID and the recorded birth before kill.
                    process.kill()
                    process.wait(timeout=10)
                except psutil.Error as error:
                    report['cleanup_errors'].append(f'owned process {process.pid} kill/wait: {error}')
            except psutil.Error as error:
                report['cleanup_errors'].append(f'owned process {process.pid}: {error}')
        # Keep bounded product diagnostics; no provider profile or auth exists.
        for name in ('agentd.log', 'windows-service.json', 'windows-service-trace.log'):
            path = home / name
            if path.is_file():
                (output / name).write_bytes(path.read_bytes()[-1024 * 1024:])
        if report['cleanup_errors'] or installed:
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
