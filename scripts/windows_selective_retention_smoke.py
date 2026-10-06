"""Real stopped-task retention, selective deletion and subsequent daemon startup.

Runs only inside windows_install_smoke's random private installation. No
provider, account, logon or production task is involved.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import time
import uuid


def exercise(scratch, store, second_app, retained, current, desktop, run,
             stopped_task, step, report, save):
    import psutil
    from windows_smoke_pipe import WindowsSmokePipe

    detail = {'result': 'running', 'processes': [], 'cleanup_errors': []}
    report['selective_retention'] = detail
    spare = scratch / 'unrelated retained payload'
    shutil.copytree(second_app, spare)
    with (spare / 'README.txt').open('a', encoding='utf-8') as stream:
        stream.write('\nAdditional inactive selective-pruning fixture.\n')
    unrelated = desktop('install', '--from', spare, '--local-preview', '--preview')['candidate']['id']
    assert unrelated not in (retained, current)
    destination = store / 'versions' / unrelated / 'AgentDocker'
    shutil.copytree(spare, destination)
    # Create its lifetime pin through an actual managed entrypoint, then let
    # that invocation retire before maintenance. Activation stays unchanged.
    run('--version', executable=destination / 'agentdocker.exe', raw=True)
    assert (store / 'pins' / (unrelated + '.lock')).is_file()
    controller = store / 'bin/agentdocker.exe'
    selected_controller = store / 'versions' / current / 'AgentDocker/agentdocker.exe'
    daemon = store / 'versions' / retained / 'AgentDocker/agentd.exe'
    home = scratch / ('retained service ' + uuid.uuid4().hex)
    endpoint = '\\\\.\\pipe\\agentdocker-retained-' + uuid.uuid4().hex
    assert not home.exists()
    def quoted(value):
        return "'" + ''.join(c * 2 if c in "'‘’‚‛" else c for c in str(value)) + "'"
    script = (f'& {quoted(controller)} daemon supervise --home {quoted(home)} '
              f'--agentd {quoted(daemon)} --endpoint {quoted(endpoint)}; exit $LASTEXITCODE')
    encoded = base64.b64encode(script.encode('utf-16le')).decode('ascii')
    arguments = '-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand ' + encoded
    shell = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
    identities = {}
    detail.update(retained=retained, unrelated=unrelated, home=str(home), endpoint=endpoint,
                  retained_daemon_sha256=hashlib.sha256(daemon.read_bytes()).hexdigest())
    save()

    def rpc(operation):
        pipe = WindowsSmokePipe(endpoint, timeout=1, read_timeout=2, write_timeout=2)
        try:
            data = memoryview((json.dumps({'op': operation}) + '\n').encode())
            while data:
                count = pipe.write(data)
                assert count
                data = data[count:]
            pipe.flush()
            return json.loads(pipe.readline())
        finally:
            pipe.close()

    def remember(process):
        identity = (process.pid, process.create_time())
        if identity not in identities:
            identities[identity] = process
            detail['processes'].append({'pid': process.pid, 'birth': identity[1],
                                        'exe': process.exe(), 'argv': process.cmdline()})
            save()

    def owned_processes():
        # Discover only exact immutable/launcher executables with this random
        # home and endpoint, or this task's exact encoded PowerShell action.
        for process in psutil.process_iter():
            try:
                exe, argv = process.exe(), process.cmdline()
                if os.path.samefile(exe, shell) and argv[1:] == arguments.split():
                    remember(process)
                elif any(os.path.samefile(exe, path) for path in (daemon, controller, selected_controller)):
                    if ('--home' in argv and Path(argv[argv.index('--home') + 1]) == home
                            and endpoint in argv):
                        remember(process)
            except (OSError, psutil.Error, IndexError):
                continue

    try:
        with stopped_task('selective retained service', arguments, execution_seconds=120) as task:
            try:
                before = desktop('prune', '--preview')
                assert {Path(p).name for p in before['maintenance']['remove']} == {unrelated}
                detail['reviewed_plan'] = before
                save()
                desktop('prune', '--expect-plan', before['plan_id'])
                step('stopped service keeps its exact old version while unrelated verified build is removed',
                     daemon.is_file() and not destination.parent.exists()
                     and desktop('status')['installation']['current']['id'] == current)
                task['start']()
                deadline = time.monotonic() + 30
                last = None
                while time.monotonic() < deadline:
                    try:
                        value = rpc('ping')
                        assert value['type'] == 'pong' and os.path.samefile(value['executable'], daemon)
                        process = psutil.Process(value['pid'])
                        assert os.path.samefile(process.exe(), daemon)
                        argv = process.cmdline()
                        assert '--home' in argv and os.path.samefile(argv[argv.index('--home') + 1], home)
                        assert endpoint in argv
                        remember(process)
                        owned_processes()
                        assert len(identities) >= 3
                        detail['started_daemon'] = value
                        break
                    except (OSError, psutil.Error, TimeoutError) as error:
                        last = str(error)
                        time.sleep(0.1)
                else:
                    raise TimeoutError(f'retained service startup: {last}')
                step('previously stopped task starts the retained daemon after selective deletion',
                     hashlib.sha256(daemon.read_bytes()).hexdigest() == detail['retained_daemon_sha256'])
                rpc('shutdown')
                for process in identities.values():
                    process.wait(timeout=15)
                step('retained service and its recorded controller generations retire cleanly',
                     all(not p.is_running() for p in identities.values()))
                detail['result'] = 'passed'
            finally:
                owned_processes()
                # Even a failed readiness check must retire its exact task
                # before fallback process cleanup can provoke a supervisor.
                task['stop']()
                owned_processes()
                for process in identities.values():
                    try:
                        process.wait(timeout=5)
                    except psutil.TimeoutExpired:
                        detail['cleanup_errors'].append(f'owned process {process.pid} forced retirement')
                        process.kill()
                        process.wait(timeout=10)
                if detail['cleanup_errors']:
                    detail['result'] = 'failed'
                save()
        if detail['result'] != 'passed':
            raise AssertionError('selective retention cleanup did not pass')
    except BaseException as error:
        detail['result'] = 'failed'
        detail['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        log = home / 'agentd.log'
        if log.is_file():
            detail['daemon_log_tail'] = log.read_text(encoding='utf-8', errors='replace')[-32768:]
        save()
