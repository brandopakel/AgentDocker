"""Real stopped-task retention, selective deletion and subsequent daemon startup.

Runs only inside windows_install_smoke's random private installation. No
provider, account, logon or production task is involved.
"""
from contextlib import ExitStack, contextmanager
import hashlib
import json
import os
from pathlib import Path
import shutil
import time
import uuid


def cleanup_owned_processes(stop, discover, identities, detail, save):
    """Attempt every owned retirement, report failures, then re-raise task errors."""
    import psutil

    first_error = None
    # Attempt the exact task stop before fallback cleanup can provoke its
    # supervisor. A failed scheduler query must not skip known identities.
    for stage, action in [('discover before stop', discover), ('task stop', stop),
                          ('discover after stop', discover)]:
        try:
            action()
        except BaseException as error:
            if first_error is None:
                first_error = error
            detail['cleanup_errors'].append(f'{stage} failed: {type(error).__name__}: {error}')
    for process in identities.values():
        try:
            process.wait(timeout=5)
        except psutil.NoSuchProcess:
            pass
        except psutil.TimeoutExpired:
            detail['cleanup_errors'].append(f'owned process {process.pid} forced retirement')
            try:
                process.kill()
                process.wait(timeout=10)
            except psutil.NoSuchProcess:
                pass
            except (psutil.Error, OSError) as error:
                detail['cleanup_errors'].append(f'owned process {process.pid} kill/wait failed: {error}')
        except (psutil.Error, OSError) as error:
            detail['cleanup_errors'].append(f'owned process {process.pid} wait failed: {error}')
    if detail['cleanup_errors']:
        detail['result'] = 'failed'
    try:
        save()
    except BaseException:
        if first_error is not None:
            raise first_error
        raise
    if first_error is not None:
        raise first_error


def exercise(scratch, store, second_app, retained, current, desktop, run,
             stopped_task, daemon_action, step, report, save):
    import psutil
    import msvcrt
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
    original = desktop('status')['installation']
    assert original['current']['id'] == current and original['previous'] is not None
    previous = original['previous']['id']

    @contextmanager
    def pinned(identity):
        with (store / 'pins' / (identity + '.lock')).open('r+b') as held:
            msvcrt.locking(held.fileno(), msvcrt.LK_NBLCK, 1)
            try:
                yield
            finally:
                held.seek(0)
                msvcrt.locking(held.fileno(), msvcrt.LK_UNLCK, 1)

    # Use the real installer to create its protected version directory. A
    # manually copied folder is not proof of private installer ownership.
    # Lifetime pins preserve the original inactive/rollback releases while
    # normal activation/rollback restores the exact original current pair.
    with ExitStack() as pins:
        pins.enter_context(pinned(previous))
        pins.enter_context(pinned(retained))
        desktop('install', '--from', spare, '--local-preview',
                '--expect-current', current, '--expect-release', unrelated)
        run('--version', executable=destination / 'agentdocker.exe', raw=True)
        pins.enter_context(pinned(unrelated))
        desktop('rollback', '--local-preview', '--expect-current', unrelated, '--expect-release', current)
        desktop('install', '--from', store / 'versions' / previous / 'AgentDocker',
                '--local-preview', '--expect-current', current, '--expect-release', previous)
        desktop('rollback', '--local-preview', '--expect-current', previous, '--expect-release', current)
    detail['provisioned_plan'] = desktop('prune', '--preview')
    save()
    step('real installer creates an eligible unrelated build and restores the original activation pair',
         {Path(p).name for p in detail['provisioned_plan']['maintenance']['remove']} == {retained, unrelated}
         and desktop('status')['installation'] == original)
    controller = store / 'bin/agentdocker.exe'
    selected_controller = store / 'versions' / current / 'AgentDocker/agentdocker.exe'
    daemon = store / 'versions' / retained / 'AgentDocker/agentd.exe'
    home = scratch / ('retained service ' + uuid.uuid4().hex)
    endpoint = '\\\\.\\pipe\\agentdocker-retained-' + uuid.uuid4().hex
    assert not home.exists()
    arguments = daemon_action(daemon, home, endpoint)
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
                detail['reviewed_plan'] = before
                save()
                assert {Path(p).name for p in before['maintenance']['remove']} == {unrelated}
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
                cleanup_owned_processes(task['stop'], owned_processes, identities, detail, save)
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
