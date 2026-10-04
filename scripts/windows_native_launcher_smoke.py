#!/usr/bin/env python3
"""Actual Windows Codex launched entirely by the experimental product command.

Fresh private profile/daemon, loopback model and synthetic ConPTY input. The
fixture never creates a provider binding or server record. No account or saved
configuration is used. Exercises first input, original-TUI MCP, receiver recovery,
preserved draft, native exit and explicit UUID reopen; not physical-keyboard acceptance.
"""
import argparse
import csv
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import traceback
from types import SimpleNamespace

from windows_native_codex_smoke import current_user_objects, remove_fixture, response_events, wait
from windows_remote_receiver_fixture import Receiver, fixture_controller, process_birth


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--codex', type=Path, required=True)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('requires native Windows')
    import psutil
    from winpty import PtyProcess
    from winpty.enums import Backend
    from websockets.sync.client import connect
    current_user_objects()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix='AgentDocker native launcher ü ')).resolve()
    report = {'result': 'failed', 'scope': __doc__, 'steps': [], 'requests': [],
              'cleanup_errors': [], 'reader_errors': [], 'forced_processes': [],
              'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'],
                                                       cwd=Path(__file__).resolve().parents[1], text=True).strip(),
              'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'receiver_helper_sha256': hashlib.sha256(Path(__file__).with_name('windows_remote_receiver_fixture.py').read_bytes()).hexdigest(),
              'provider_sha256': hashlib.sha256(args.codex.read_bytes()).hexdigest()}
    codex = args.codex.resolve(strict=True)
    receiver = channel = terminal = reader = server = None
    owned = []; output = []; closing = threading.Event()
    queue_text = 'AD_AUTO_QUEUE_' + secrets.token_hex(8)
    draft_text = 'AD_AUTO_DRAFT_' + secrets.token_hex(8)
    reopen_text = 'AD_AUTO_REOPEN_' + secrets.token_hex(8)
    closed_consoles = set()

    def close_console():
        if terminal is None or id(terminal) in closed_consoles:
            return
        closing.set()
        terminal.pty.cancel_io()
        try: terminal.fileobj.shutdown(socket.SHUT_RDWR)
        except OSError as error:
            if error.winerror not in (10038, 10057, 10058): raise
        terminal.fileobj.close(); terminal._server.close(); terminal._thread.join(timeout=2)
        assert not terminal._thread.is_alive()
        if reader is not None:
            reader.join(timeout=2)
            assert not reader.is_alive(), 'terminal reader did not retire'
        closed_consoles.add(id(terminal))

    def normalized_birth(value):
        assert value.endswith('Z')
        whole, _, fraction = value[:-1].partition('.')
        return whole + '.' + fraction.ljust(9, '0')

    def step(name, ok):
        report['steps'].append({'step': name, 'passed': bool(ok)})
        assert ok, name

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                self.connection.settimeout(5)
                length = int(self.headers.get('Content-Length', 0))
                assert 0 < length <= 8 * 1024 * 1024 and len(report['requests']) < 20
                body = json.loads(self.rfile.read(length)); encoded = json.dumps(body)
                title = 'Generate a concise, single-line task title' in encoded
                report['requests'].append({'queue': queue_text in encoded, 'draft': draft_text in encoded, 'reopen': reopen_text in encoded, 'title': title})
                for item in body.get('input', []):
                    if item.get('type') == 'function_call_output' and item.get('call_id') == 'call_private_identity':
                        report['mcp_output'] = item
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
                if queue_text in encoded and not title and not report.get('mcp_requested'):
                    report['mcp_requested'] = True
                    events = receiver.mcp_events(len(report['requests']), body)
                else:
                    events = response_events(len(report['requests']))
                for event in events:
                    self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode()); self.wfile.flush()
            except Exception as error:
                report.setdefault('fixture_errors', []).append(str(error))

    try:
        identity = subprocess.check_output(['whoami.exe', '/user', '/fo', 'csv', '/nh'], text=True, timeout=10)
        sid = next(csv.reader(io.StringIO(identity.strip())))[1]
        assert sid.startswith('S-1-5-')
        subprocess.run(['icacls.exe', str(root), '/inheritance:r', '/grant:r', '*' + sid + ':(OI)(CI)F'], check=True, capture_output=True, timeout=10)
        repo, profile = root / 'project', root / 'profile'; repo.mkdir(); profile.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], check=True, timeout=10)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        env = {k: v for k, v in os.environ.items() if k.upper() in {
            'PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TEMP', 'TMP',
            'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'PROGRAMFILES', 'PROGRAMFILES(X86)'}}
        env.update(CODEX_HOME=str(profile), AGENTDOCKER_FIXTURE_KEY='fixture-only', TERM='xterm-256color')
        receiver = Receiver(args.binary_dir, root, repo, profile, out, env, report); receiver.start_daemon()
        config = ('model = "fixture-model"\nmodel_provider = "fixture"\napproval_policy = "on-request"\n'
                  'sandbox_mode = "danger-full-access"\ncheck_for_update_on_startup = false\n'
                  '[features]\napps = false\n[analytics]\nenabled = false\n'
                  '[model_providers.fixture]\nname = "Private automatic launch"\nbase_url = ' + json.dumps(f'http://127.0.0.1:{server.server_port}/v1') + '\n'
                  'wire_api = "responses"\nenv_key = "AGENTDOCKER_FIXTURE_KEY"\nsupports_websockets = false\n'
                  'request_max_retries = 0\nstream_max_retries = 0\n'
                  '[projects.' + json.dumps(str(repo)) + ']\ntrust_level = "trusted"\n'
                  '[tui]\nscreen_reader_detection_done = true\nshow_tooltips = false\n' + receiver.mcp_config())
        (profile / 'config.toml').write_text(config, encoding='utf-8')
        invalid_program = root / 'not-a-provider.exe'
        invalid_program.write_bytes(b'private non-executable startup fixture')
        refused = subprocess.run([str(receiver.cli), '--socket', receiver.socket, 'codex-native',
                    '--program', str(invalid_program), '--profile', str(profile), '--cwd', str(repo)],
                    cwd=repo, env=receiver.env, capture_output=True, text=True, encoding='utf-8', timeout=20)
        report['failed_spawn'] = {'exit_code': refused.returncode, 'stderr': refused.stderr[-4096:]}
        step('failed server creation revokes its private capability before returning',
             refused.returncode != 0 and 'cannot start dedicated Codex server' in refused.stderr
             and not list((receiver.home / 'codex-native').glob('*/capability'))
             and not report['requests'])
        terminal = PtyProcess.spawn([str(receiver.cli), '--socket', receiver.socket, 'codex-native',
                    '--program', str(codex), '--profile', str(profile), '--cwd', str(repo), '--name', 'native-auto-fixture'],
                    cwd=str(repo), env=receiver.env, dimensions=(40, 160), backend=Backend.ConPTY)
        launcher = psutil.Process(terminal.pid); owned.append(launcher)

        def drain():
            tail = ''
            try:
                while terminal.isalive():
                    data = terminal.read(65536); output.append(data)
                    if '\x1b[6n' in tail + data: terminal.write('\x1b[1;1R')
                    tail = data[-3:]
                    assert sum(map(len, output)) <= 2 * 1024 * 1024
            except EOFError:
                pass
            except Exception as error:
                if not closing.is_set(): report['reader_errors'].append(str(error))
        reader = threading.Thread(target=drain, daemon=True); reader.start()

        def ready():
            assert terminal.isalive(), 'automatic launcher exited: ' + ''.join(output)[-2000:]
            return 'AgentDocker native input ready:' in ''.join(output)
        wait(ready, 60)
        records = list((receiver.home / 'codex-native').glob('*/server.json')); assert len(records) == 1
        record = records[0]; descriptor = json.loads(record.read_text(encoding='utf-8'))
        ledgers = list((receiver.home / 'codex-queue').glob('*/delivery.json')); assert len(ledgers) == 1
        receiver.ledger_path = ledgers[0]; receiver.agent = receiver.ledger()['binding']['agent']
        receiver.peer = receiver.rpc({'op': 'register', 'spec': {'name': 'native-auto-peer'}, 'pid': None})['agent']['id']
        report['descriptor'] = descriptor
        report['receiver_generation'] = descriptor['provider']
        report['receiver_initial_binding'] = receiver.binding()
        step('product launcher publishes its own exact birth and binding before any input',
             descriptor['version'] == 2 and descriptor['birth']['launcher']['pid'] == terminal.pid and
             normalized_birth(descriptor['birth']['launcher']['started_at']) == normalized_birth(process_birth(terminal.pid)) and
             receiver.binding()['provider'] == descriptor['provider'] and not report['requests'])
        for process in launcher.children(recursive=True): owned.append(process)
        native = SimpleNamespace(pid=descriptor['provider']['process']['pid'])
        provider = SimpleNamespace(pid=descriptor['server']['pid'])
        step('server and native terminal remain direct children of the product launcher',
             psutil.Process(native.pid).ppid() == terminal.pid and psutil.Process(provider.pid).ppid() == terminal.pid)
        token = Path(descriptor['token_file']).read_text(encoding='utf-8')
        channel = connect(f"ws://127.0.0.1:{descriptor['port']}", additional_headers={'Authorization': 'Bearer ' + token},
                          proxy=None, open_timeout=3, close_timeout=2, ping_interval=None, max_size=2 * 1024 * 1024)
        sequence = 0

        def call(method, params):
            nonlocal sequence
            sequence += 1; channel.send(json.dumps({'id': sequence, 'method': method, 'params': params}))
            for _ in range(100):
                value = json.loads(channel.recv(timeout=5))
                if value.get('id') == sequence:
                    assert 'result' in value, value
                    return value['result']
                assert 'id' not in value, 'observer cannot answer provider requests'
            raise AssertionError('provider notification bound exceeded')
        call('initialize', {'clientInfo': {'name': 'agentdocker_private_auto_observer', 'version': '0'}, 'capabilities': {'experimentalApi': True}})
        channel.send(json.dumps({'method': 'initialized'}))
        thread = descriptor['provider']['session']
        terminal.write(draft_text); time.sleep(0.3)
        message = receiver.send(queue_text); first = receiver.received(message)
        wait(lambda: report.get('mcp_output'), 30)
        receiver.check_mcp_identity(report['mcp_output'], native, provider)
        step('first original receipt and actual MCP identify the native TUI without a warmup', first['message'] == message and not any(r['draft'] for r in report['requests']))
        receiver.replace()
        step('receiver replacement preserves the original receipt and provider generation', receiver.ledger()['completed'] == [first] and receiver.binding()['provider'] == descriptor['provider'])
        wait(lambda: call('thread/read', {'threadId': thread, 'includeTurns': False})['thread']['status']['type'] == 'idle', 20)
        terminal.write('\r')

        def draft_received():
            page = call('thread/turns/list', {'threadId': thread, 'limit': 10, 'itemsView': 'full'})
            users = [i for turn in page['data'] for i in turn['items'] if i['type'] == 'userMessage']
            return users if any(draft_text in json.dumps(i) for i in users) and any(r['draft'] for r in report['requests']) else None
        users = wait(draft_received, 30); report['user_receipts'] = users; report['first_receipt'] = first
        step('retained draft submits exactly once after replacement', len(users) == 2 and sum(i.get('clientId') == message for i in users) == 1 and sum(draft_text in json.dumps(i) for i in users) == 1)
        wait(lambda: call('thread/read', {'threadId': thread, 'includeTurns': False})['thread']['status']['type'] == 'idle', 20)
        channel.close(); channel = None
        terminal.write('\x04')
        wait(lambda: not terminal.isalive(), 30)
        wait(lambda: not Path(descriptor['token_file']).exists(), 10)
        step('native exit retires launcher and revokes capability', not terminal.isalive() and not Path(descriptor['token_file']).exists())
        wait(lambda: not any(p.is_running() for p in owned + receiver.owned
                            if p.pid != receiver.daemon.pid), 15)
        assert receiver.daemon.poll() is None, 'private daemon must survive provider reopen'
        close_console()
        (out / 'first-terminal.txt').write_text(''.join(output), encoding='utf-8')
        old_descriptor = descriptor
        report['first_mcp_output'] = report.pop('mcp_output')
        report['mcp_requested'] = False
        queued = receiver.send(reopen_text)
        report['queued_while_provider_down'] = queued
        output.clear(); closing.clear()
        terminal = PtyProcess.spawn([str(receiver.cli), '--socket', receiver.socket, 'codex-native',
                    '--program', str(codex), '--profile', str(profile), '--cwd', str(repo),
                    '--name', 'native-auto-resumed', '--resume', thread],
                    cwd=str(repo), env=receiver.env, dimensions=(40, 160), backend=Backend.ConPTY)
        launcher = psutil.Process(terminal.pid); owned.append(launcher)
        reader = threading.Thread(target=drain, daemon=True); reader.start()
        wait(ready, 65)
        step('reopen binds the original canonical agent without a new prompt',
             'AgentDocker native input ready: ' + receiver.agent in ''.join(output))
        records = [p for p in (receiver.home / 'codex-native').glob('*/server.json') if p != record]
        assert len(records) == 1
        record = records[0]; descriptor = json.loads(record.read_text(encoding='utf-8'))
        report['reopen_descriptor'] = descriptor
        step('reopen uses the exact conversation and ordinary history with no birth allowance',
             descriptor['version'] == 1 and 'birth' not in descriptor and
             descriptor['provider']['session'] == thread and
             descriptor['provider']['profile'] == old_descriptor['provider']['profile'] and
             descriptor['provider']['process'] != old_descriptor['provider']['process'])
        for process in launcher.children(recursive=True): owned.append(process)
        receiver.owned.append(fixture_controller(psutil, receiver.binding(), receiver.cli))
        report['receiver_generation'] = descriptor['provider']
        report['reopen_binding'] = receiver.binding()
        native = SimpleNamespace(pid=descriptor['provider']['process']['pid'])
        provider = SimpleNamespace(pid=descriptor['server']['pid'])
        second = receiver.received(queued)
        wait(lambda: report.get('mcp_output'), 30)
        receiver.check_mcp_identity(report['mcp_output'], native, provider)
        step('offline original input and actual MCP retain the canonical identity across reopen',
             receiver.ledger()['completed'] == [first, second] and
             any(r['reopen'] for r in report['requests']))
        token = Path(descriptor['token_file']).read_text(encoding='utf-8')
        channel = connect(f"ws://127.0.0.1:{descriptor['port']}", additional_headers={'Authorization': 'Bearer ' + token},
                          proxy=None, open_timeout=3, close_timeout=2, ping_interval=None, max_size=2 * 1024 * 1024)
        sequence = 0
        call('initialize', {'clientInfo': {'name': 'agentdocker_private_reopen_observer', 'version': '0'}, 'capabilities': {'experimentalApi': True}})
        channel.send(json.dumps({'method': 'initialized'}))
        wait(lambda: call('thread/read', {'threadId': thread, 'includeTurns': False})['thread']['status']['type'] == 'idle', 20)
        page = call('thread/turns/list', {'threadId': thread, 'limit': 10, 'itemsView': 'full'})
        users = [i for turn in page['data'] for i in turn['items'] if i['type'] == 'userMessage']
        report['reopen_user_receipts'] = users; report['reopen_receipts'] = [first, second]
        step('reopen preserves both original receipts and the draft with no extra user turn',
             len(users) == 3 and sum(i.get('clientId') == queued for i in users) == 1 and
             sum(i.get('clientId') == message for i in users) == 1 and
             sum(draft_text in json.dumps(i) for i in users) == 1)
        channel.close(); channel = None
        terminal.write('\x04')
        wait(lambda: not terminal.isalive(), 30)
        wait(lambda: not Path(descriptor['token_file']).exists(), 10)
        step('resumed terminal exits natively and revokes its capability',
             not terminal.isalive() and not Path(descriptor['token_file']).exists())
        step('private provider configuration is unchanged', (profile / 'config.toml').read_text(encoding='utf-8') == config)
        report['result'] = 'passed'
    except Exception:
        report['error'] = traceback.format_exc()
    finally:
        closing.set()
        if channel is not None: channel.close()
        if receiver is not None:
            for directory in (receiver.home / 'codex-native').glob('*'):
                for name in ['server.log', 'receiver.log', 'server.json']:
                    path = directory / name
                    if path.is_file() and path.stat().st_size <= 2 * 1024 * 1024:
                        (out / (directory.name + '-' + name)).write_bytes(path.read_bytes())
            if hasattr(receiver, 'ledger_path') and receiver.ledger_path.exists():
                (out / 'retained-ledger.json').write_bytes(receiver.ledger_path.read_bytes())
        for process in reversed(owned):
            try:
                if process.is_running():
                    children = process.children(recursive=True)
                    _, alive = psutil.wait_procs([process, *children], timeout=5)
                    for child in alive:
                        report['forced_processes'].append({'pid': child.pid, 'birth': child.create_time()})
                        child.kill(); child.wait(timeout=5)
            except psutil.NoSuchProcess:
                pass
            except Exception as error:
                report['cleanup_errors'].append(str(error))
        if receiver is not None:
            try: receiver.close()
            except Exception as error: report['cleanup_errors'].append(str(error))
        if terminal is not None:
            try: close_console()
            except Exception as error: report['cleanup_errors'].append(str(error))
        if reader is not None:
            reader.join(timeout=2)
            if reader.is_alive(): report['cleanup_errors'].append('terminal reader did not retire')
        if server is not None: server.shutdown(); server.server_close()
        (out / 'terminal.txt').write_text(''.join(output), encoding='utf-8')
        try: remove_fixture(root)
        except Exception as error: report['cleanup_errors'].append(str(error))
        report['scratch_removed'] = not root.exists()
        if report['cleanup_errors'] or report['reader_errors'] or report['forced_processes'] or report.get('fixture_errors'):
            report['result'] = 'failed'
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: report.get(k) for k in ['result', 'steps', 'error', 'cleanup_errors', 'forced_processes', 'scratch_removed']}))
    return 0 if report['result'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
