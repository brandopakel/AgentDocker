#!/usr/bin/env python3
"""Exercise opt-in update scheduling and restarts through native Iced controls.

The sibling CLI fixture records saved reservations and rejects any desktop
operation except status/check. Real feed/archive validation has a separate driver.
"""
import argparse
import sys
from iced_workflow_smoke import rpc, until, step
from desktop_smoke import stop, wait_window
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

def smoke(binary_dir, output):
    binary_dir = binary_dir.resolve(strict=True)
    output = output.absolute()
    output.mkdir(mode=0o700)
    report = {'result': 'failed', 'checks': [], 'scope': 'Native rendered controls with a controlled sibling CLI reply; no real network/download/install or physical input acceptance', 'binary_sha256': {name: hashlib.sha256((binary_dir / name).read_bytes()).hexdigest() for name in ('agentd', 'agentdocker-ui')}}
    daemon = window = None
    with tempfile.TemporaryDirectory(prefix='ad-daily-', dir='/tmp') as scratch:
        root = Path(scratch).resolve()
        state, home, binaries, project = [root / name for name in ('state', 'home', 'bin', 'project')]
        for directory in (state, home, binaries, project):
            directory.mkdir(mode=0o700)
        endpoint = root / 'd.sock'
        calls = root / 'calls.jsonl'
        failure = root / 'fail'
        forbidden = root / 'forbidden.jsonl'
        shutil.copy2(binary_dir / 'agentdocker-ui', binaries / 'agentdocker-ui')
        shim = binaries / 'agentdocker'
        shim.write_text("#!" + sys.executable + "\n" + r"""import json, os, pathlib, sys, time
args = sys.argv[1:]
if args == ['desktop', 'update', '--check']:
    saved = json.loads(pathlib.Path(os.environ['AGENTDOCKER_HOME'], 'workspace.json').read_text())
    assert saved['updates']['enabled'] and saved['updates']['last_attempt'] is not None
    with open(os.environ['DAILY_CALLS'], 'a') as log:
        log.write(json.dumps({'args':args, 'saved_attempt': saved['updates']['last_attempt']}) + '\n')
    if pathlib.Path(os.environ['DAILY_FAILURE']).exists():
        sys.exit('controlled offline failure')
    print(json.dumps({'update': {'update_available': True, 'available': {'version': '0.2.0'}, 'installed_version': '0.1.0', 'channel': 'stable', 'daemon': 'unchanged'}}))
elif args == ['desktop', 'status']:
    print(json.dumps({'installation': None}))
elif args[:1] == ['desktop']:
    with open(os.environ['DAILY_FORBIDDEN'], 'a') as log:
        log.write(json.dumps(args) + '\n')
    sys.exit('unrequested desktop mutation')
else:
    os.execv(os.environ['DAILY_REAL_CLI'], [os.environ['DAILY_REAL_CLI'], *args])
""")
        shim.chmod(0o700)
        env = {**os.environ, 'HOME': str(home), 'CODEX_HOME': str(home / '.codex'), 'CLAUDE_CONFIG_DIR': str(home / '.claude'), 'XDG_CONFIG_HOME': str(home / '.config'), 'AGENTDOCKER_HOME': str(state), 'AGENTDOCKER_SOCKET': str(endpoint), 'AGENTDOCKER_NO_AUTOSTART': '1', 'AGENTDOCKER_NO_NOTIFICATIONS': '1', 'DAILY_CALLS': str(calls), 'DAILY_FAILURE': str(failure), 'DAILY_FORBIDDEN': str(forbidden), 'DAILY_REAL_CLI': str(binary_dir / 'agentdocker'), 'RUST_LOG': 'warn'}

        def launch(name, steps):
            nonlocal window
            script = root / (name + '.json')
            script.write_text(json.dumps(steps))
            capture = output / name
            with (output / (name + '.log')).open('w') as log:
                window = subprocess.Popen([str(binaries / 'agentdocker-ui'), '--smoke-test', str(capture), '--smoke-scenario', str(script), '--smoke-deadline', '60'], cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
                wait_window(daemon, window, capture)
            result = json.loads((capture / 'result.json').read_text())
            assert result['scenario_steps_completed'] == len(steps), result
            report.setdefault('native_steps', 0)
            report['native_steps'] += len(steps)

        def records():
            return [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []

        def catalog():
            return json.loads((state / 'workspace.json').read_text())
        try:
            with (output / 'daemon.log').open('w') as log:
                daemon = subprocess.Popen([str(binary_dir / 'agentd')], cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)

            def ready():
                if daemon.poll() is not None:
                    raise RuntimeError('daemon exited')
                try:
                    return rpc(endpoint, {'op': 'ping'})['type'] == 'pong'
                except OSError:
                    return False
            until(ready)
            launch('default-off', [step('click', id='settings'), step('wait_text', text='Daily update checks: off'), step('pause', millis=1500)])
            assert not records() and (not forbidden.exists())
            report['checks'].append('new_preferences_default_to_no_automatic_requests')
            launch('enable', [step('click', id='settings'), step('wait_text', text='Daily update checks: off'), step('pause', millis=1500), step('click', id='automatic-update-checks'), step('wait_text', text='Daily update checks: on'), step('wait_control', id='open-available-update', present=True), step('click', id='open-available-update'), step('wait_text', text='Desktop installation'), step('wait_text', text='Version 0.2.0 is available'), step('wait_text', text='No managed installation at this prefix.'), step('wait_text', text='Download and preview 0.2.0'), step('capture', name='available-update'), step('pause', millis=1200)])
            assert len(records()) == 1, records()
            assert catalog()['updates']['enabled']
            attempt = catalog()['updates']['last_attempt']
            report['checks'].append('opt_in_persists_before_first_check_and_opens_available_release')
            launch('reopen', [step('click', id='settings'), step('wait_text', text='Daily update checks: on'), step('pause', millis=2200), step('capture', name='daily-enabled')])
            assert len(records()) == 1 and catalog()['updates']['last_attempt'] == attempt
            report['checks'].append('restart_does_not_repeat_the_daily_check')
            saved = catalog()
            saved['updates']['last_attempt'] = int(time.time()) - 86401
            (state / 'workspace.json').write_text(json.dumps(saved))
            failure.touch()
            launch('offline', [step('click', id='settings'), step('wait_text', text='Couldn’t check for updates.'), step('pause', millis=1500), step('capture', name='offline-check')])
            assert len(records()) == 2
            report['checks'].append('due_check_failure_is_visible_and_reserved')
            launch('offline-reopen', [step('click', id='settings'), step('wait_text', text='Daily update checks: on'), step('pause', millis=2200), step('click', id='automatic-update-checks'), step('wait_text', text='Daily update checks: off'), step('pause', millis=500)])
            assert len(records()) == 2 and (not catalog()['updates']['enabled'])
            report['checks'].append('failed_check_is_throttled_across_restart_and_can_be_disabled')
            saved = catalog()
            saved['updates']['last_attempt'] = None
            (state / 'workspace.json').write_text(json.dumps(saved))
            launch('disabled-reopen', [step('click', id='settings'), step('wait_text', text='Daily update checks: off'), step('pause', millis=2200)])
            assert len(records()) == 2
            report['checks'].append('disabled_reopen_never_checks_even_when_due')
            assert not forbidden.exists(), forbidden.read_text()
            report['requests'] = records()
            report['result'] = 'passed'
        except BaseException as error:
            report['error'] = str(error)
            raise
        finally:
            stop(window)
            stop(daemon)
            report['owned_cleanup'] = all((p is None or p.poll() is not None for p in (daemon, window)))
            (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    return report

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    previous = os.umask(0o077)
    try:
        print(json.dumps(smoke(args.binary_dir, args.output), indent=2))
    finally:
        os.umask(previous)
if __name__ == '__main__':
    main()
