#!/usr/bin/env python3
"""Actual hooks/MCP transport lifecycle with an owned synthetic provider host.

The host is a Python fixture, not Claude or a model. No provider configuration,
credentials, user daemon or real session is touched.
"""
import argparse
import hashlib
import json
import os
import select
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

def group_alive(process):
    try:
        os.killpg(process.pid, 0)
        return True
    except ProcessLookupError:
        return False


def stop(process):
    if process.returncode is not None:
        return
    # This unreaped child reserves the process-group ID. Stop the whole
    # owned fixture group before wait() can release that identity for reuse.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)


class Lines:

    def __init__(self, process):
        self.process, self.pending = (process, b'')

    def request(self, value, timeout=5):
        self.process.stdin.write(json.dumps(value).encode() + b'\n')
        self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        while b'\n' not in self.pending:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('fixture response deadline')
            if not select.select([self.process.stdout], [], [], remaining)[0]:
                raise TimeoutError('fixture response deadline')
            block = os.read(self.process.stdout.fileno(), 16384)
            if not block:
                raise RuntimeError('fixture process closed without response')
            self.pending += block
            if len(self.pending) > 262144:
                raise RuntimeError('fixture response exceeded byte bound')
        line, self.pending = self.pending.split(b'\n', 1)
        return json.loads(line)

def helper(binary):
    for line in sys.stdin.buffer:
        event = json.loads(line)
        result = subprocess.run([binary, 'hook', 'claude-code'], input=json.dumps(event).encode(), capture_output=True, timeout=4)
        print(json.dumps({'exit': result.returncode, 'output': result.stdout.decode(), 'stderr': result.stderr.decode()}), flush=True)

def trial(binary_dir, output, manifest_path):
    output.mkdir(mode=0o700)
    cli = binary_dir / 'agentdocker'
    daemon_binary = binary_dir / 'agentd'
    manifest = json.loads(manifest_path.read_text())
    assert manifest.get('source_commit') and manifest.get('source_tree'), 'package source identity is required'
    for binary in [cli, daemon_binary]:
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == manifest['binary_sha256'][binary.name], 'package executable hash mismatch'
    report = {'scope': 'Actual packaged MCP and Claude hooks with an owned synthetic Python host; no model or real provider', 'source_commit': manifest.get('source_commit'), 'source_tree': manifest.get('source_tree'), 'binary_sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in [cli, daemon_binary]}, 'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), 'checks': [], 'result': 'failed'}
    started = time.monotonic()
    processes = []
    daemon = host = None
    root = None
    try:
        with tempfile.TemporaryDirectory(prefix='ad-identity-', dir='/tmp') as scratch:
            root = Path(scratch)
            project = root / 'project'
            project.mkdir()
            env = {k: v for k, v in os.environ.items() if not k.startswith('AGENTDOCKER_')}
            env.update(AGENTDOCKER_HOME=str(root / 'state'), AGENTDOCKER_SOCKET=str(root / 'sock'), AGENTDOCKER_NO_AUTOSTART='1', RUST_LOG='warn')

            def rpc(value):
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.settimeout(3)
                    stream.connect(env['AGENTDOCKER_SOCKET'])
                    stream.sendall(json.dumps(value).encode() + b'\n')
                    with stream.makefile('rb') as file:
                        result = json.loads(file.readline(1048576))
                if result.get('type') == 'error':
                    raise RuntimeError('fixture RPC rejected: ' + json.dumps(result))
                return result
            with (output / 'daemon.log').open('wb') as dlog, (output / 'adapters.log').open('wb') as alog:
                try:
                    daemon = subprocess.Popen([str(daemon_binary)], cwd=project, env=env, stdin=subprocess.DEVNULL, stdout=dlog, stderr=subprocess.STDOUT, start_new_session=True)
                    processes.append(daemon)
                    deadline = time.monotonic() + 15
                    while True:
                        try:
                            if rpc({'op': 'ping'}).get('type') == 'pong':
                                break
                        except (OSError, ValueError):
                            pass
                        if daemon.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError('fixture daemon unavailable')
                        time.sleep(0.05)
                    host = subprocess.Popen([sys.executable, '-u', str(Path(__file__).resolve()), '--host-helper', str(cli)], cwd=project, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=alog, start_new_session=True)
                    processes.append(host)
                    hook = Lines(host)
                    session = str(uuid.uuid4())

                    def start_mcp():
                        p = subprocess.Popen([str(cli), 'mcp', '--runtime', 'claude-code', '--pid', str(host.pid)], cwd=project, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=alog, start_new_session=True)
                        processes.append(p)
                        channel = Lines(p)
                        result = channel.request({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'identity-fixture', 'version': '1'}}})
                        assert 'result' in result, 'MCP initialization failed'
                        return p

                    def agents():
                        return [a for a in rpc({'op': 'list', 'all': True})['agents'] if a.get('pid') == host.pid]
                    first = start_mcp()
                    initial = agents()
                    assert len(initial) == 1, 'first MCP has one identity'
                    owner = initial[0]['id']
                    report['checks'].append('MCP-first registration')
                    result = hook.request({'hook_event_name': 'SessionStart', 'session_id': session, 'cwd': str(project)})
                    assert result['exit'] == 0, 'actual hook failed'
                    assert 'additionalContext' in result['output'], 'actual hook produced no context'
                    joined = agents()
                    report['identity_count_after_hook'] = len(joined)
                    assert len(joined) == 1 and joined[0]['id'] == owner, 'hooks and MCP produced duplicate identities'
                    report['checks'].append('actual SessionStart hook joins MCP identity')
                    lease = rpc({'op': 'claim', 'agent': owner, 'resource': 'task:identity-fixture', 'ttl_secs': 120})['lease']['id']
                    token = 'fixture-' + uuid.uuid4().hex
                    message = rpc({'op': 'send', 'from': 'user', 'to': owner, 'kind': 'fixture', 'payload': {'text': token}})['message']
                    second = start_mcp()
                    assert len(agents()) == 1 and agents()[0]['id'] == owner, 'second MCP changed identity'
                    for p in [first, second]:
                        p.stdin.close()
                        p.wait(timeout=5)
                        assert p.returncode == 0, 'MCP shutdown failed'
                        a = rpc({'op': 'inspect', 'agent': owner})['agent']
                        report.setdefault('status_after_mcp_shutdown', []).append(a['status'])
                        report['host_alive_after_mcp_shutdown'] = host.poll() is None
                        assert a['status'] == {'state': 'running'}, 'MCP shutdown ended the live host identity'
                        assert any((l['id'] == lease for l in rpc({'op': 'leases', 'agent': owner})['leases'])), 'MCP shutdown released the lease'
                        assert any((m['id'] == message for m in rpc({'op': 'inbox', 'agent': owner, 'drain': False})['messages'])), 'MCP shutdown consumed queued message'
                        assert host.poll() is None, 'fixture host died'
                    report['checks'].append('both actual MCP shutdowns preserve live identity, lease and inbox')
                    third = start_mcp()
                    assert len(agents()) == 1 and agents()[0]['id'] == owner, 'MCP reconnection changed identity'
                    report['checks'].append('MCP reconnection reuses bound identity')
                    result = hook.request({'hook_event_name': 'UserPromptSubmit', 'session_id': session, 'cwd': str(project)})
                    assert token in result['output'], 'hook did not deliver queued token'
                    assert not any((m['id'] == message for m in rpc({'op': 'inbox', 'agent': owner, 'drain': False})['messages'])), 'delivered token was not acknowledged'
                    report['checks'].append('actual prompt hook outputs pending token then acknowledges it')
                    observation = rpc({'op': 'inspect', 'agent': owner})['agent']['reported_activity']
                    assert observation['activity'] == 'working', 'MCP-first identity did not receive hook activity'
                    report['checks'].append('prompt activity reaches the identity originally named by MCP')
                    result = hook.request({'hook_event_name': 'Stop', 'session_id': session, 'cwd': str(project)})
                    assert result['exit'] == 0, 'Stop failed'
                    observation = rpc({'op': 'inspect', 'agent': owner})['agent']['reported_activity']
                    assert observation['activity'] == 'idle', 'Stop did not report idle to the joined identity'
                    assert not rpc({'op': 'leases', 'agent': owner})['leases'], 'Stop retained lease'
                    report['checks'].append('Stop reports idle and releases the joined identity lease')
                    rpc({'op': 'claim', 'agent': owner, 'resource': 'task:session-end-fixture', 'ttl_secs': 120})
                    result = hook.request({'hook_event_name': 'SessionEnd', 'session_id': session, 'cwd': str(project)})
                    assert result['exit'] == 0, 'SessionEnd failed'
                    assert not rpc({'op': 'leases', 'agent': owner})['leases'], 'SessionEnd retained lease'
                    a = rpc({'op': 'inspect', 'agent': owner})['agent']
                    assert a['status'] != {'state': 'running'}, 'SessionEnd left running identity'
                    report['checks'].append('actual SessionEnd releases and ends joined identity')
                    third.stdin.close()
                    third.wait(timeout=5)
                    report['result'] = 'passed'
                finally:
                    for p in reversed(processes):
                        stop(p)
                    for p in processes:
                        for stream in [p.stdin, p.stdout]:
                            if stream is not None:
                                stream.close()
    except Exception as error:
        report['error'] = str(error)
    finally:
        report['elapsed_seconds'] = time.monotonic() - started
        report['cleanup'] = {
            'owned_processes_remaining': sum(p.poll() is None for p in processes),
            'owned_process_groups_remaining': sum(group_alive(p) for p in processes),
            'scratch_removed': root is None or not root.exists(),
            'provider_config_changed': False,
            'production_daemon_changed': False,
        }
        if (report['cleanup']['owned_processes_remaining']
                or report['cleanup']['owned_process_groups_remaining']
                or not report['cleanup']['scratch_removed']):
            report['result'] = 'failed'
            report['cleanup_error'] = 'fixture cleanup incomplete'
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return 0 if report['result'] == 'passed' else 1
if __name__ == '__main__':
    os.umask(0o077)
    if len(sys.argv) > 1 and sys.argv[1] == '--host-helper':
        helper(sys.argv[2])
        sys.exit(0)
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary-dir', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--manifest', type=Path, required=True)
    args = p.parse_args()
    sys.exit(trial(args.binary_dir.resolve(strict=True), args.output.absolute(), args.manifest.resolve(strict=True)))
