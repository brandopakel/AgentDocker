#!/usr/bin/env python3
"""Exercise configuration-lock contention through packaged CLIs and an owned provider fixture."""
import argparse
import fcntl
import hashlib
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from sustained_use import stop_daemon

def rpc(ep, r):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(5)
        s.connect(str(ep))
        s.sendall(json.dumps(r).encode() + b'\n')
        with s.makefile('rb') as f:
            return json.loads(f.readline())

def wait(fn, seconds=10):
    d = time.monotonic() + seconds
    while time.monotonic() < d:
        if fn():
            return
        time.sleep(0.05)
    raise TimeoutError('owned fixture deadline')

def read(p):
    return p.read_bytes() if p.exists() else None

def smoke(binary_dir, output):
    binary_dir = binary_dir.resolve(strict=True)
    output = output.absolute()
    output.mkdir(mode=448)
    os.umask(63)
    report = {'result': 'failed', 'checks': [], 'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), 'binary_sha256': {n: hashlib.sha256((binary_dir / n).read_bytes()).hexdigest() for n in ['agentdocker', 'agentd']}}
    proc = daemon = None
    with tempfile.TemporaryDirectory(prefix='ad-config-', dir='/tmp') as tmp:
        root = Path(tmp).resolve()
        project = root / 'project'
        profile = root / 'profile'
        tools = root / 'bin'
        project.mkdir()
        profile.mkdir()
        tools.mkdir()
        alias = root / 'alias'
        alias.symlink_to(profile, target_is_directory=True)
        endpoint = root / 'd.sock'
        env = {k: v for k, v in os.environ.items() if not k.startswith('AGENTDOCKER_')}
        env.update(AGENTDOCKER_HOME=str(root / 'daemon'), AGENTDOCKER_SOCKET=str(endpoint), AGENTDOCKER_NO_AUTOSTART='1', AGENTDOCKER_NO_NOTIFICATIONS='1', CLAUDE_CONFIG_DIR=str(profile), PATH=str(tools) + ':' + env.get('PATH', ''))
        (tools / 'claude').write_text('#!/bin/sh\necho "Claude Code fixture"\n')
        (tools / 'claude').chmod(448)
        provider = root / 'provider.py'
        provider.write_text('import json,pathlib,sys,time\nmode,path,entry,ready,release=sys.argv[1:]\np=pathlib.Path(path)\nif mode=="add":\n pathlib.Path(ready).touch()\n while not pathlib.Path(release).exists():time.sleep(.05)\n p.write_text(json.dumps({"mcpServers":{"agentdocker":json.loads(entry)}}))\nelse:p.write_text("{}")\n')
        ready = root / 'ready'
        release = root / 'release'
        shared = profile / '.claude.json'
        shared.write_text('{}')
        hook = project / '.codex/hooks.json'
        other = project / 'second.json'

        def plan(home, path, provider_path):
            (home / 'setup').mkdir(parents=True, mode=448)
            mid = str(uuid.uuid4())
            entry = {'type': 'stdio', 'command': str(binary_dir / 'agentdocker'), 'args': ['mcp', '--runtime', 'claude-code'], 'env': {'AGENTDOCKER_SETUP_RECEIPT': mid}}
            value = {'format': 1, 'id': mid, 'phase': 'prepared', 'executable': str(binary_dir / 'agentdocker'), 'changes': [{'runtime': 'claude-code', 'channel': 'hooks', 'path': str(path), 'target': str(path.resolve()), 'before': None, 'after': '{"fixture":"' + mid + '"}\n'}], 'delegated': [{'runtime': 'claude-code', 'channel': 'mcp', 'path': str(provider_path), 'add': [sys.executable, str(provider), 'add', str(provider_path), json.dumps(entry), str(ready), str(release)], 'remove': [sys.executable, str(provider), 'remove', str(provider_path), json.dumps(entry), str(ready), str(release)], 'created': False, 'expected': entry, 'config_dir': str(profile)}], 'notes': []}
            file = home / 'setup' / f'{mid}.json'
            file.write_text(json.dumps(value))
            return (mid, file)
        a = root / 'state-a'
        b = root / 'state-b'
        aid, af = plan(a, hook, shared)
        bid, bf = plan(b, other, alias / '.claude.json')

        def run(home, args):
            return subprocess.run([str(binary_dir / 'agentdocker'), *args], cwd=project, env={**env, 'AGENTDOCKER_HOME': str(home)}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=12)

        def blocked(home, args, files):
            before = {p: read(p) for p in files}
            r = run(home, args)
            assert r.returncode != 0 and b'another AgentDocker setup operation' in r.stderr, (r.returncode, r.stderr.decode())
            assert all((read(p) == v for p, v in before.items())), 'blocked operation changed fixture state'
        try:
            with (output / 'daemon.log').open('w') as log:
                daemon = subprocess.Popen([str(binary_dir / 'agentd')], cwd=project, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)

            def ready_daemon():
                try:
                    return rpc(endpoint, {'op': 'ping'}).get('type') == 'pong'
                except OSError:
                    return False
            wait(ready_daemon)
            with (output / 'first-apply.stdout').open('wb') as stdout, (output / 'first-apply.stderr').open('wb') as stderr:
                proc = subprocess.Popen([str(binary_dir / 'agentdocker'), 'setup', '--apply', aid, '--json'], cwd=project, env={**env, 'AGENTDOCKER_HOME': str(a)}, stdout=stdout, stderr=stderr)
            wait(lambda: ready.exists() or proc.poll() is not None)
            assert ready.exists(), 'first delegated provider did not start'
            blocked(b, ['setup', '--apply', bid, '--json'], [bf, other, shared, hook])
            report['checks'].append('different_homes_and_symlink_alias_contend_before_receipt_or_file_writes')
            blocked(root / 'hooks-state', ['hook', 'install', 'codex'], [hook, shared])
            report['checks'].append('standalone_hook_install_contends_with_guided_apply')
            blocked(root / 'legacy-state', ['setup', 'claude-code'], [hook, shared, profile / 'settings.json'])
            report['checks'].append('legacy_setup_contends_with_guided_provider_registration')
            release.touch()
            assert proc.wait(timeout=10) == 0
            assert json.loads(af.read_text())['phase'] == 'applied'
            key = hashlib.sha256(os.fsencode(shared.resolve())).hexdigest()
            lock = Path(f'/tmp/agentdocker-config-locks-{os.geteuid()}') / (key + '.lock')
            with lock.open('r+') as held:
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                blocked(a, ['setup', '--undo', aid, '--json'], [af, hook, shared])
            report['checks'].append('undo_contention_preserves_applied_receipt_and_provider_entry')
            assert run(a, ['setup', '--undo', aid, '--json']).returncode == 0
            assert not hook.exists()
            assert json.loads(shared.read_text()).get('mcpServers', {}).get('agentdocker') is None
            assert run(b, ['setup', '--apply', bid, '--json']).returncode == 0
            assert run(b, ['setup', '--undo', bid, '--json']).returncode == 0
            assert not other.exists()
            report['checks'].append('both_saved_plans_apply_and_undo_after_lock_release')
            assert run(root / 'hooks-state', ['hook', 'install', 'codex']).returncode == 0
            before = read(hook)
            assert run(root / 'hooks-state', ['hook', 'install', 'codex']).returncode == 0
            assert before == read(hook)
            report['checks'].append('standalone_hook_install_remains_idempotent_after_contention')
            report['result'] = 'passed'
        except BaseException as e:
            report['error'] = str(e)
        finally:
            release.touch()
            if proc and proc.poll() is None:
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.terminate()
                    proc.wait(timeout=5)
            if daemon:
                report['cleanup'] = stop_daemon(daemon, endpoint)
            (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return 0 if report['result'] == 'passed' else 1
if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(smoke(args.binary_dir, args.output))
