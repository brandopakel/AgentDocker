"""Private AgentDocker receiver for the remote ConPTY acceptance driver.

Manual fixture registration establishes exact identities; it is not automatic
product bootstrap. All daemon/receiver processes and capabilities belong to the
driver's new private directory. No installed service or saved profile is used.
"""
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

from windows_native_codex_smoke import fixture_controller, read_snapshot, response_events, wait
from windows_smoke_pipe import WindowsSmokePipe


def canonical(path):
    """Match Rust canonicalize's verbatim Windows spelling, including Unicode."""
    value = str(Path(path).resolve(strict=True))
    if value.startswith('\\\\?\\'):
        return value
    if value.startswith('\\\\'):
        return '\\\\?\\UNC\\' + value[2:]
    return '\\\\?\\' + value


def process_birth(pid):
    """Keep all 100ns FILETIME precision for the provider generation record."""
    import ctypes as c
    from ctypes import wintypes as w
    api = c.WinDLL('kernel32', use_last_error=True)
    api.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    api.OpenProcess.restype = w.HANDLE
    api.GetProcessTimes.argtypes = [w.HANDLE, *(c.POINTER(w.FILETIME),) * 4]
    api.CloseHandle.argtypes = [w.HANDLE]
    handle = api.OpenProcess(0x1000, False, pid)
    if not handle:
        raise c.WinError(c.get_last_error())
    try:
        values = [w.FILETIME() for _ in range(4)]
        if not api.GetProcessTimes(handle, *(c.byref(v) for v in values)):
            raise c.WinError(c.get_last_error())
        ticks = (values[0].dwHighDateTime << 32) | values[0].dwLowDateTime
        seconds, fractions = divmod(ticks - 116444736000000000, 10000000)
        return (datetime.datetime.fromtimestamp(seconds, datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')
                + f'.{fractions * 100:09d}Z')
    finally:
        api.CloseHandle(handle)


def decoded_values(value):
    yield value
    if isinstance(value, dict):
        for child in value.values():
            yield from decoded_values(child)
    elif isinstance(value, list):
        for child in value:
            yield from decoded_values(child)
    elif isinstance(value, str):
        try:
            parsed = json.loads(value)
        except ValueError:
            return
        if not isinstance(parsed, str):
            yield from decoded_values(parsed)


class Receiver:
    def __init__(self, binaries, root, repo, profile, out, env, report):
        import psutil
        self.psutil = psutil
        self.cli = binaries.resolve(strict=True) / 'agentdocker.exe'
        self.daemon_exe = self.cli.with_name('agentd.exe')
        self.root, self.repo, self.profile, self.out = root, repo, profile, out
        self.home = root / 'state'
        self.socket = '\\\\.\\pipe\\agentdocker-remote-' + uuid.uuid4().hex
        self.env = dict(env, AGENTDOCKER_HOME=str(self.home), AGENTDOCKER_SOCKET=self.socket,
                        AGENTDOCKER_NO_AUTOSTART='1', AGENTDOCKER_NO_NOTIFICATIONS='1')
        self.report = report
        self.children, self.owned, self.logs = [], [], []
        self.daemon = None
        report['receiver_binary_sha256'] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                             for p in (self.cli, self.daemon_exe)}

    def spawn(self, command, name):
        log = (self.out / name).open('wb'); self.logs.append(log)
        child = subprocess.Popen([str(x) for x in command], cwd=self.repo, env=self.env,
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        self.children.append(child)
        process = self.psutil.Process(child.pid)
        assert os.path.samefile(process.exe(), command[0])
        self.owned.append(process)
        return child

    def rpc(self, value):
        channel = WindowsSmokePipe(self.socket, timeout=2, read_timeout=5, write_timeout=5)
        try:
            channel.write((json.dumps(value) + '\n').encode())
            result = json.loads(channel.readline())
            assert result.get('type') != 'error', result
            return result
        finally:
            channel.close()

    def start_daemon(self):
        self.daemon = self.spawn([self.daemon_exe, '--socket', self.socket], 'receiver-daemon.log')
        def ready():
            assert self.daemon.poll() is None, 'private daemon exited'
            try: return self.rpc({'op': 'ping'})
            except (OSError, TimeoutError): return None
        wait(ready, 15)

    def mcp_config(self):
        return ('[mcp_servers.agentdocker]\ncommand = ' + json.dumps(str(self.cli))
                + '\nargs = ["mcp", "--runtime", "codex"]\nrequired = true\n'
                '[mcp_servers.agentdocker.env]\nAGENTDOCKER_HOME = ' + json.dumps(str(self.home))
                + '\nAGENTDOCKER_SOCKET = ' + json.dumps(self.socket)
                + '\nAGENTDOCKER_NO_AUTOSTART = "1"\n')

    def configure_hook(self):
        runner = self.root / 'capture_hook.py'
        self.hook_log = self.out / 'hooks.jsonl'
        # Relay provider JSON bytes unchanged: Windows Python's stream encoding
        # and subprocess locale can differ, corrupting non-ASCII checkout paths.
        runner.write_text('import sys,subprocess,json,os\n'
            + 'raw=sys.stdin.buffer.read()\n'
            + 'p=subprocess.run(' + repr([str(self.cli), '--socket', self.socket, 'hook', 'codex'])
            + ',input=raw,capture_output=True,timeout=8)\n'
            + 'with open(' + repr(str(self.hook_log)) + ',"a",encoding="utf-8") as f: '
            + 'f.write(json.dumps({"input":json.loads(raw),"stdout":p.stdout.decode("utf-8"),"stderr":p.stderr.decode("utf-8"),'
            + '"code":p.returncode,"parent":os.getppid()})+"\\n")\n'
            + 'sys.stdout.buffer.write(p.stdout)\nsys.exit(p.returncode)\n', encoding='utf-8')
        self.hooks_config = json.dumps({'hooks': {'PostToolUse': [{'hooks': [{'type': 'command',
            'command': subprocess.list2cmdline([sys.executable, str(runner)])}]}]}})
        (self.profile / 'hooks.json').write_text(self.hooks_config, encoding='utf-8')
        self.report['hook_trust'] = 'one-off trust for sole vetted private fixture command; no saved account policy changed'

    def check_hook_identity(self, thread):
        calls = [json.loads(line) for line in self.hook_log.read_text(encoding='utf-8').splitlines()]
        self.report['hook_calls'] = calls
        assert calls and all(h['code'] == 0 and not h['stderr'] and h['input']['session_id'] == thread
                             for h in calls), 'native hook returned an error or different session'
        assert all(os.path.samefile(h['input']['cwd'], self.repo) for h in calls)
        agent = self.rpc({'op': 'inspect', 'agent': self.agent})['agent']
        assert agent['adapter_contacts']['hooks']['process_started_at'] == agent['process_started_at']
        agents = self.rpc({'op': 'list', 'all': True})['agents']
        assert [a['id'] for a in agents if a['spec']['runtime'] == 'codex'] == [self.agent]
        assert (self.profile / 'hooks.json').read_text(encoding='utf-8') == self.hooks_config
        self.report['hook_observed_agent'] = agent

    def mcp_events(self, number, body):
        matches = [(tool['name'], child) for tool in body.get('tools', [])
                   if tool.get('type') == 'namespace' for child in tool.get('tools', [])
                   if child.get('name') == 'whoami']
        assert len(matches) == 1 and matches[0][0] == 'mcp__agentdocker'
        self.report['mcp_tool_schema'] = matches[0][1]
        item = {'type': 'function_call', 'id': 'fc_private_identity',
                'call_id': 'call_private_identity', 'namespace': matches[0][0],
                'name': 'whoami', 'arguments': json.dumps({'verbose': True}), 'status': 'completed'}
        response = response_events(number)[-1]['response']
        response['output'] = [item]
        return [
            {'type': 'response.created', 'response': dict(response, status='in_progress', output=[])},
            {'type': 'response.output_item.added', 'output_index': 0,
             'item': dict(item, arguments='', status='in_progress')},
            {'type': 'response.function_call_arguments.delta', 'item_id': item['id'],
             'output_index': 0, 'delta': item['arguments']},
            {'type': 'response.function_call_arguments.done', 'item_id': item['id'],
             'output_index': 0, 'arguments': item['arguments']},
            {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
            {'type': 'response.completed', 'response': response}]

    def check_mcp_identity(self, output, tui, server):
        identified = [v for v in decoded_values(output) if isinstance(v, dict)
                      and v.get('id') == self.agent and v.get('pid') == tui.pid and v.get('input_binding')]
        assert len(identified) == 1, 'MCP did not resolve to the original bound terminal'
        assert identified[0]['input_binding']['provider'] == self.report['receiver_generation']
        agents = self.rpc({'op': 'list', 'all': True})['agents']
        assert not any(a.get('pid') == server.pid and a.get('spec', {}).get('runtime') == 'codex'
                       for a in agents), 'MCP registered the app-server as a conversation'

    def prepare(self, codex, tui, server, thread, port, token_file):
        assert self.daemon is not None and self.daemon.poll() is None
        agent = self.rpc({'op': 'register', 'spec': {'name': 'native-remote-fixture', 'runtime': 'codex',
                         'workdir': str(self.repo), 'labels': {'session_id': thread}}, 'pid': tui.pid})['agent']
        self.agent = agent['id']
        self.peer = self.rpc({'op': 'register', 'spec': {'name': 'remote-fixture-peer'}, 'pid': None})['agent']['id']
        descriptor = {'version': 1,
                      'provider': {'process': {'pid': tui.pid, 'started_at': agent['process_started_at']},
                                   'session': thread, 'profile': canonical(self.profile)},
                      'server': {'pid': server.pid, 'started_at': process_birth(server.pid)},
                      'executable': canonical(codex), 'cwd': canonical(self.repo), 'port': port,
                      'token_file': canonical(token_file),
                      'token_sha256': hashlib.sha256(token_file.read_bytes()).hexdigest()}
        record = self.root / 'server.json'
        record.write_text(json.dumps(descriptor), encoding='utf-8')
        self.command = [self.cli, '--socket', self.socket, 'codex-queue', '--agent', self.agent,
                        '--pid', tui.pid, '--started-at', agent['process_started_at'].replace('Z', '+00:00'),
                        '--thread', thread, '--profile', descriptor['provider']['profile'],
                        '--cwd', descriptor['cwd'], '--program', descriptor['executable'],
                        '--app-server-record', canonical(record)]
        self.ledger_path = self.home / 'codex-queue' / self.agent / 'delivery.json'
        self.report['receiver_generation'] = descriptor['provider']

    def send(self, text):
        return self.rpc({'op': 'send', 'from': self.peer, 'to': self.agent,
                         'kind': 'chat', 'payload': {'text': text}})['message']

    def binding(self):
        return self.rpc({'op': 'inspect', 'agent': self.agent})['agent'].get('input_binding')

    def refuse_empty_history(self):
        child = self.spawn(self.command, 'receiver-empty-history.log')
        child.wait(timeout=20)
        assert child.returncode != 0 and self.binding() is None
        assert 'Codex rejected thread/turns/list' in (self.out / 'receiver-empty-history.log').read_text(encoding='utf-8')

    def start(self):
        self.receiver = self.spawn(self.command, 'receiver.log')
        bound = wait(self.binding, 30)
        assert bound['provider'] == self.report['receiver_generation']
        assert '--app-server-record' in bound['launch']['args']
        fixture_controller(self.psutil, bound, self.cli)
        self.report['receiver_initial_binding'] = bound

    def ledger(self):
        return read_snapshot(self.ledger_path)

    def received(self, message):
        return wait(lambda: next((x for x in self.ledger()['completed'] if x['message'] == message), None), 30)

    def replace(self):
        before = self.ledger()
        old = fixture_controller(self.psutil, self.binding(), self.cli)
        # Explicit receiver-crash injection, never a provider or service signal.
        old.terminate(); old.wait(timeout=10)
        def replaced():
            bound = self.binding()
            return bound if bound and bound['controller']['pid'] != old.pid else None
        bound = wait(replaced, 30)
        self.owned.append(fixture_controller(self.psutil, bound, self.cli))
        after = self.ledger()
        assert after['token'] == before['token'] and after['completed'] == before['completed']
        self.report['receiver_replacement_binding'] = bound

    def close(self):
        errors = self.report['cleanup_errors']
        if self.daemon is not None and self.daemon.poll() is None:
            try:
                self.rpc({'op': 'shutdown'}); self.daemon.wait(timeout=20)
            except Exception as error:
                errors.append('private receiver daemon shutdown: ' + str(error))
        for process in reversed(self.owned):
            try:
                if process.is_running():
                    try:
                        process.wait(timeout=5)
                    except self.psutil.TimeoutExpired:
                        errors.append(f'private receiver process required forced cleanup: {process.pid}')
                        process.kill(); process.wait(timeout=5)
            except self.psutil.NoSuchProcess:
                pass
            except Exception as error:
                errors.append('private receiver cleanup: ' + str(error))
        for child in self.children:
            try: child.wait(timeout=5)
            except Exception as error: errors.append(str(error))
        for log in self.logs: log.close()
