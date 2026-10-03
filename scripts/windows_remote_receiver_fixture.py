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
import uuid

from windows_native_codex_smoke import fixture_controller, read_snapshot, wait
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

    def prepare(self, codex, tui, server, thread, port, token_file):
        self.daemon = self.spawn([self.daemon_exe, '--socket', self.socket], 'receiver-daemon.log')
        def ready():
            assert self.daemon.poll() is None, 'private daemon exited'
            try: return self.rpc({'op': 'ping'})
            except (OSError, TimeoutError): return None
        wait(ready, 15)
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
                        '--pid', tui.pid, '--started-at', agent['process_started_at'], '--thread', thread,
                        '--profile', self.profile, '--cwd', self.repo, '--program', codex,
                        '--app-server-record', record]
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
