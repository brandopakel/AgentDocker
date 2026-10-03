#!/usr/bin/env python3
"""Provider-only Windows Codex remote-TUI startup capability probe.

Uses a fresh private profile, a loopback model and an authenticated dedicated
app-server. Without --binary-dir, no AgentDocker binding is exercised. The
optional receiver trial manually binds exact generations after an explicit
fixture user turn and tests shared-server delivery/replacement and original
terminal MCP and PostToolUse hook identity; automatic
bootstrap and zero-prompt delivery remain unaccepted. No real account, physical
input or production configuration is exercised. The optional approval
probe uses one private print command and a synthetic native Return. A pass is capability evidence,
not acceptance of AgentDocker's native controller.
"""
import argparse
import csv
import datetime
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
import re
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import traceback

from windows_native_codex_smoke import current_user_objects, remove_fixture, response_events


class ProviderRefusal(Exception):
    def __init__(self, method, error):
        super().__init__(f'{method}: {error}')
        self.method, self.error = method, error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--codex', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--approval', action='store_true', help='Check a private native command approval')
    parser.add_argument('--binary-dir', type=Path, help='Also test AgentDocker shared-server receiver after one explicit user turn')
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('requires native Windows')
    import psutil
    from websockets.sync.client import connect
    from winpty import PtyProcess
    from winpty.enums import Backend

    current_user_objects()
    codex = args.codex.resolve(strict=True)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix='AgentDocker remote Codex ü ')).resolve()
    report = {'result': 'failed', 'scope': __doc__, 'steps': [], 'requests': [],
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'provider_sha256': hashlib.sha256(codex.read_bytes()).hexdigest(),
              'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'cleanup_errors': [], 'retired_processes': [], 'reader_errors': []}
    owned, output = [], []
    closing = threading.Event()
    provider = tui = channel = server = reader = receiver = None
    queue_text = 'AD_PRIVATE_FIRST_QUEUE_' + secrets.token_hex(8)
    draft_text = 'AD_PRIVATE_RETAINED_DRAFT_' + secrets.token_hex(8)
    approval_marker = 'AD_PRIVATE_APPROVAL_' + secrets.token_hex(8)
    approval_code = "print('" + approval_marker + "')"
    approval_command = 'python -c "' + approval_code + '"'
    approval_called = False
    report['approval_requested'] = args.approval
    report['observer_approval_requests'] = []
    report['receiver_requested'] = args.binary_dir is not None
    report['source_commit'] = subprocess.check_output(
        ['git', 'rev-parse', 'HEAD'], cwd=Path(__file__).resolve().parents[1], text=True, timeout=10).strip()

    def approval_events(number, body):
        namespace, found = None, False
        for tool in body.get('tools', []):
            if tool.get('type') == 'function' and tool.get('name') == 'exec_command':
                found = True
            if (tool.get('type') == 'namespace' and
                    any(t.get('name') == 'exec_command' for t in tool.get('tools', []))):
                namespace, found = tool['name'], True
        assert found, 'provider did not advertise exec_command'
        response = response_events(number)[-1]['response']
        item = {'type': 'function_call', 'id': 'fc_private_approval',
                'call_id': 'call_private_approval', 'name': 'exec_command', 'status': 'completed',
                'arguments': json.dumps({'cmd': approval_command, 'max_output_tokens': 1000})}
        if namespace:
            item['namespace'] = namespace
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


    def step(name, passed, detail=None):
        report['steps'].append({'step': name, 'passed': bool(passed), 'detail': detail})
        assert passed, name

    def remember(pid):
        process = psutil.Process(pid)
        assert os.path.samefile(process.exe(), codex)
        owned.append(process)
        return process

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            self.send_error(404)

        def do_POST(self):
            nonlocal approval_called
            try:
                self.connection.settimeout(5)
                length = int(self.headers.get('Content-Length', 0))
                assert 0 < length <= 8 * 1024 * 1024
                assert len(report['requests']) < 20
                body = json.loads(self.rfile.read(length))
                encoded = json.dumps(body)
                report['requests'].append({
                    'path': self.path, 'queue_nonce_present': queue_text in encoded,
                    'draft_nonce_present': draft_text in encoded,
                    'tool_result_present': any(
                        v.get('type') == 'function_call_output' and approval_marker in str(v.get('output', ''))
                        for v in body.get('input', []) if isinstance(v, dict)),
                    'title_request': 'Generate a concise, single-line task title' in encoded})
                for item in body.get('input', []):
                    if item.get('type') == 'function_call_output' and item.get('call_id') == 'call_private_identity':
                        report['mcp_output'] = item
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                if (args.approval and not approval_called and queue_text in encoded and
                        not report['requests'][-1]['title_request']):
                    approval_called = True
                    events = approval_events(len(report['requests']), body)
                elif (receiver is not None and not report.get('mcp_requested') and queue_text in encoded
                      and not report['requests'][-1]['title_request']):
                    report['mcp_requested'] = True
                    events = receiver.mcp_events(len(report['requests']), body)
                else:
                    events = response_events(len(report['requests']))
                for event in events:
                    self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
                    self.wfile.flush()
            except Exception as error:
                report.setdefault('fixture_errors', []).append(str(error))

    with (out / 'app-server.log').open('wb') as log:
        try:
            # Restrict only this newly created directory before writing the
            # ephemeral capability token; no saved account/profile is accessed.
            identity = subprocess.check_output(['whoami.exe', '/user', '/fo', 'csv', '/nh'],
                                               text=True, timeout=10)
            sid = next(csv.reader(io.StringIO(identity.strip())))[1]
            assert sid.startswith('S-1-5-')
            subprocess.run(['icacls.exe', str(root), '/inheritance:r', '/grant:r',
                            '*' + sid + ':(OI)(CI)F'], check=True, capture_output=True, timeout=10)
            profile, repo = root / 'profile', root / 'project'
            profile.mkdir(); repo.mkdir()
            subprocess.run(['git', 'init', '-q', str(repo)], check=True, timeout=10)
            server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', 0))
                port = reservation.getsockname()[1]
            endpoint = f'ws://127.0.0.1:{port}'
            token = secrets.token_urlsafe(32)
            token_file = root / 'token'
            token_file.write_text(token, encoding='utf-8')
            config = ('model = "fixture-model"\nmodel_provider = "fixture"\n'
                      'approval_policy = "on-request"\nsandbox_mode = "danger-full-access"\n'
                      'check_for_update_on_startup = false\n'
                      '[features]\napps = false\n[analytics]\nenabled = false\n'
                      '[model_providers.fixture]\nname = "Private startup probe"\n'
                      'base_url = ' + json.dumps(f'http://127.0.0.1:{server.server_port}/v1') + '\n'
                      'wire_api = "responses"\nenv_key = "AGENTDOCKER_FIXTURE_KEY"\n'
                      'request_max_retries = 0\nstream_max_retries = 0\nsupports_websockets = false\n'
                      '[projects.' + json.dumps(str(repo)) + ']\ntrust_level = "trusted"\n'
                      '[tui]\nscreen_reader_detection_done = true\nshow_tooltips = false\n')
            env = {k: v for k, v in os.environ.items() if k.upper() in {
                'PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TEMP', 'TMP',
                'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'PROGRAMFILES', 'PROGRAMFILES(X86)'}}
            env.update(CODEX_HOME=str(profile), AGENTDOCKER_FIXTURE_KEY='fixture-only',
                       TERM='xterm-256color', AD_PRIVATE_WS_TOKEN=token)
            if args.binary_dir is not None:
                from windows_remote_receiver_fixture import Receiver
                helper = Path(__file__).with_name('windows_remote_receiver_fixture.py')
                report['receiver_helper_sha256'] = hashlib.sha256(helper.read_bytes()).hexdigest()
                receiver = Receiver(args.binary_dir, root, repo, profile, out, env, report)
                # Hooks inherit the provider environment. MCP has explicit
                # config, but hooks also need this private ledger home.
                env = receiver.env.copy()
                receiver.start_daemon()
                config += receiver.mcp_config()
                config = config.replace('[features]\n', '[features]\nhooks = true\n')
                receiver.configure_hook()
            (profile / 'config.toml').write_text(config, encoding='utf-8')
            if args.approval:
                (profile / 'rules').mkdir()
                rule = profile / 'rules' / 'private-approval.rules'
                rule_text = ('prefix_rule(pattern = ' + json.dumps(['python', '-c', approval_code]) +
                             ', decision = "prompt", justification = "Private native approval fixture")\n')
                rule.write_text(rule_text, encoding='utf-8')
                decision = subprocess.run(
                    [str(codex), 'execpolicy', 'check', '--rules', str(rule), '--', 'python', '-c', approval_code],
                    cwd=repo, env=env, capture_output=True, text=True, timeout=10, check=True)
                report['rule_decision'] = json.loads(decision.stdout)
                assert report['rule_decision']['decision'] == 'prompt'
            report['provider_version'] = subprocess.check_output(
                [str(codex), '--version'], env=env, text=True, timeout=10).strip()
            assert report['provider_version'] == 'codex-cli 0.160.0'
            provider = subprocess.Popen(
                [str(codex), 'app-server', '--listen', endpoint, '--ws-auth', 'capability-token',
                 '--ws-token-file', str(token_file)], cwd=repo, env=env,
                stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            remember(provider.pid)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                try:
                    channel = connect(endpoint, additional_headers={'Authorization': 'Bearer ' + token},
                                      proxy=None, open_timeout=2, close_timeout=2,
                                      ping_interval=None, max_size=2 * 1024 * 1024)
                    break
                except OSError:
                    assert provider.poll() is None, 'owned app-server exited before listening'
                    time.sleep(0.1)
            assert channel is not None, 'private app-server did not become ready'
            try:
                wrong = connect(endpoint, additional_headers={'Authorization': 'Bearer invalid-fixture-token'},
                                proxy=None, open_timeout=2, close_timeout=2, ping_interval=None)
            except Exception as error:
                status = getattr(getattr(error, 'response', None), 'status_code', None)
            else:
                wrong.close()
                status = None
            step('dedicated server rejects an invalid capability token', status in (401, 403), status)
            tui = PtyProcess.spawn(
                [str(codex), '--no-alt-screen'] +
                (['--dangerously-bypass-hook-trust'] if receiver is not None else []) +
                ['--remote', endpoint,
                 '--remote-auth-token-env', 'AD_PRIVATE_WS_TOKEN'],
                cwd=str(repo), env=env, dimensions=(40, 160), backend=Backend.ConPTY)
            remember(tui.pid)

            def drain():
                tail = ''
                try:
                    while tui.isalive():
                        data = tui.read(65536)
                        output.append(data)
                        if '\x1b[6n' in tail + data:
                            tui.write('\x1b[1;1R')
                        tail = data[-3:]
                        if sum(map(len, output)) > 2 * 1024 * 1024:
                            raise ValueError('terminal output exceeded its bound')
                except EOFError:
                    pass
                except Exception as error:
                    if not closing.is_set():
                        report['reader_errors'].append(str(error))

            reader = threading.Thread(target=drain, daemon=True)
            reader.start()
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline and 'fixture-model' not in ''.join(output):
                assert tui.isalive(), 'native TUI exited before initialization'
                time.sleep(0.1)
            step('native TUI initializes before the observing client', 'fixture-model' in ''.join(output))
            sequence = 0

            def call(method, params):
                nonlocal sequence
                sequence += 1
                channel.send(json.dumps({'id': sequence, 'method': method, 'params': params}))
                deadline = time.monotonic() + 10
                for _ in range(100):
                    remaining = deadline - time.monotonic()
                    assert remaining > 0, 'provider request deadline exceeded'
                    value = json.loads(channel.recv(timeout=remaining))
                    if value.get('id') == sequence and 'method' not in value:
                        if 'error' in value:
                            raise ProviderRefusal(method, value['error'])
                        assert 'result' in value, {'method': method, 'reply': value}
                        return value['result']
                    if 'method' in value and 'id' in value:
                        assert args.approval, 'unexpected provider request in startup-only probe'
                        assert value['method'] == 'item/commandExecution/requestApproval', value['method']
                        assert len(report['observer_approval_requests']) < 20
                        report['observer_approval_requests'].append({'id': value['id'], 'method': value['method']})
                        # Observe only. The native terminal must own the decision;
                        # this client never responds to server-initiated requests.
                raise AssertionError('provider exceeded notification bound')

            call('initialize', {'clientInfo': {'name': 'agentdocker_private_startup_probe', 'version': '0'},
                                'capabilities': {'experimentalApi': True}})
            channel.send(json.dumps({'method': 'initialized'}))
            deadline = time.monotonic() + 20
            ids = []
            while time.monotonic() < deadline and not ids:
                assert tui.isalive(), 'native TUI exited before thread discovery'
                ids = call('thread/loaded/list', {}).get('data', [])
                if not ids:
                    time.sleep(0.2)
            step('dedicated server exposes exactly one native thread before any prompt', len(ids) == 1)
            thread = ids[0]
            metadata = call('thread/read', {'threadId': thread, 'includeTurns': False})['thread']
            report['initial_thread'] = metadata
            step('empty thread belongs to the private checkout', os.path.samefile(metadata['cwd'], repo))
            step('initialization caused no model requests', not report['requests'])
            report['thread'] = thread
            report['reported_source'] = metadata.get('source')

            def read_thread(include_turns=False):
                # Native materialization briefly exposes an empty rollout.
                # Retry only this exact private read error; never resubmit input
                # or treat an arbitrary provider refusal as a transient success.
                for attempt in range(20):
                    try:
                        return call('thread/read', {'threadId': thread, 'includeTurns': include_turns})['thread']
                    except ProviderRefusal as error:
                        expected = f"rollout at {metadata['path']} is empty"
                        if (error.error.get('code') != -32603 or
                                not error.error.get('message', '').endswith(expected) or attempt == 19):
                            raise
                        report.setdefault('materialization_read_retries', []).append(error.error)
                        time.sleep(0.2)

            def turn_page(cursor=None):
                value = call('thread/turns/list', {'threadId': thread, 'limit': 1,
                            'itemsView': 'full', 'sortDirection': 'desc', 'cursor': cursor})
                assert isinstance(value.get('data'), list) and len(value['data']) <= 1
                for turn in value['data']:
                    assert turn.get('id') and isinstance(turn.get('items'), list)
                return value

            # This provider explicitly refuses turn pages until the first
            # ordinary user message. Retain that limitation; the production
            # receiver must not turn a failed history read into empty history.
            try:
                turn_page()
            except ProviderRefusal as error:
                expected = (f'thread {thread} is not materialized yet; '
                            'thread/turns/list is unavailable before first user message')
                step('fresh thread explicitly refuses pre-materialization turn pagination',
                     error.error.get('code') == -32600 and error.error.get('message') == expected,
                     error.error)
            else:
                raise AssertionError('pinned provider unexpectedly accepted unmaterialized turn history')

            if args.binary_dir is not None:
                receiver.prepare(codex, tui, provider, thread, port, token_file)
                first_message = receiver.send(queue_text)
                receiver.refuse_empty_history()
                step('AgentDocker refuses pre-first-message history without binding or input', not report['requests'])
                report['explicit_initial_user_turn'] = 'AD_EXPLICIT_ESTABLISHED_SESSION'
                tui.write(report['explicit_initial_user_turn']); time.sleep(0.3); tui.write('\r')
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if report['requests'] and read_thread()['status']['type'] == 'idle':
                        break
                    time.sleep(0.2)
                else:
                    raise TimeoutError('explicit fixture user turn did not finish')

            # The API's source label is diagnostic only, never process identity.
            time.sleep(1)
            tui.write(draft_text)
            time.sleep(0.3)
            if receiver is not None:
                receiver.start()
            else:
                call('thread/queue/add', {'threadId': thread, 'clientUserMessageId': 'ad-private-' + secrets.token_hex(8),
                                        'input': [{'type': 'text', 'text': queue_text, 'text_elements': []}]})

            def history(expected):
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    # Exercise the receiver's bounded legacy-store fallback:
                    # one complete turn per read-only page, no full hydration.
                    users, cursor, cursors = [], None, set()
                    for page_number in range(4):
                        page = turn_page(cursor)
                        users.extend(item for turn in page['data'] for item in turn['items']
                                     if item.get('type') == 'userMessage')
                        cursor = page.get('nextCursor')
                        if cursor is None:
                            break
                        assert isinstance(cursor, str) and cursor and cursor not in cursors
                        cursors.add(cursor)
                    else:
                        raise AssertionError('private history exceeded four pages')
                    if any(expected in json.dumps(item) for item in users):
                        return users
                    time.sleep(0.2)
                raise TimeoutError('provider history did not contain submitted input')

            if args.approval:
                deadline = time.monotonic() + 25
                while time.monotonic() < deadline:
                    screen = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', ''.join(output))
                    if ('Would you like to run the following command?' in screen and
                            'Yes, proceed' in screen and approval_marker in screen):
                        break
                    read_thread()
                    time.sleep(0.2)
                else:
                    raise AssertionError('native TUI did not display the private command approval')
                step('native TUI displays the queued turn command approval', True)
                assert not any(r['tool_result_present'] for r in report['requests'])
                time.sleep(2)
                step('private command stays pending until native approval',
                     not any(r['tool_result_present'] for r in report['requests']))
                tui.write('\r')
                deadline = time.monotonic() + 25
                while time.monotonic() < deadline and not any(r['tool_result_present'] for r in report['requests']):
                    read_thread()
                    time.sleep(0.2)
                step('one native Return approves the private print command',
                     any(r['tool_result_present'] for r in report['requests']))

            deadline = time.monotonic() + 25
            idle = False
            while time.monotonic() < deadline:
                status = read_thread()['status']
                idle = status['type'] == 'idle'
                if report['requests'] and idle:
                    break
                time.sleep(0.2)
            first = history(queue_text)
            step('first queued input finishes without submitting the typed draft',
                 idle and any(v['queue_nonce_present'] for v in report['requests']) and
                 not any(v['draft_nonce_present'] for v in report['requests']) and
                 sum(queue_text in json.dumps(v) for v in first) == 1)
            if receiver is not None:
                report['receiver_first_receipt'] = receiver.received(first_message)
                step('AgentDocker records the exact original queued-message receipt',
                     report['receiver_first_receipt']['receipt']['thread'] == thread)
                receiver.check_mcp_identity(report.get('mcp_output'), tui, provider)
                step('dedicated MCP call identifies the original terminal without a helper registration', True)
                receiver.check_hook_identity(thread)
                step('actual native hooks keep original identity with no outer registration or config change', True)
            tui.write('\r')
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline and not any(v['draft_nonce_present'] for v in report['requests']):
                time.sleep(0.1)
            users = history(draft_text)
            step('native Return submits the preserved draft exactly once',
                 any(v['draft_nonce_present'] for v in report['requests']) and len(users) == 2 + int(receiver is not None) and
                 sum(queue_text in json.dumps(v) for v in users) == 1 and
                 sum(draft_text in json.dumps(v) for v in users) == 1 and tui.isalive())
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline:
                if read_thread()['status']['type'] == 'idle':
                    break
                time.sleep(0.2)
            else:
                raise TimeoutError('draft turn did not finish before cleanup')
            if receiver is not None:
                receiver.replace()
                replacement_text = 'AD_PRIVATE_REPLACEMENT_' + secrets.token_hex(8)
                second_message = receiver.send(replacement_text)
                receiver.received(second_message)
                users = history(replacement_text)
                step('replacement preserves native server terminal and all exact receipts without replay',
                     provider.poll() is None and tui.isalive() and len(users) == 4 and
                     all(sum(text in json.dumps(item) for item in users) == 1
                         for text in (queue_text, draft_text, replacement_text)) and
                     [item['message'] for item in receiver.ledger()['completed']] == [first_message, second_message])
                report['receiver_receipts'] = receiver.ledger()['completed']
            if args.approval:
                step('native one-time decision did not change the private prompt rule',
                     rule.read_text(encoding='utf-8') == rule_text)
            report['user_receipts'] = users
            step('private configuration is unchanged',
                 (profile / 'config.toml').read_text(encoding='utf-8') == config)
            report['result'] = 'receiver_observed' if receiver is not None else 'capability_observed'
        except Exception:
            report['error'] = traceback.format_exc()
        finally:
            closing.set()
            if channel is not None:
                try:
                    channel.close()
                except Exception as error:
                    report['cleanup_errors'].append(str(error))
            if tui is not None and tui.isalive():
                try:
                    tui.write('\x03\x03')
                    time.sleep(1)
                except Exception as error:
                    report['cleanup_errors'].append(str(error))
            # The provider can detach an app-server from the native TUI. Only
            # an exact image inside this fresh profile's private cache qualifies.
            cache = root / 'profile/packages/app-server-daemon/releases'
            for pid in psutil.pids():
                try:
                    process = psutil.Process(pid)
                    image = Path(process.exe()).resolve(strict=True)
                    if image.is_relative_to(cache):
                        assert image.name.lower() == 'codex.exe' and 'app-server' in process.cmdline()
                        owned.append(process)
                except (psutil.NoSuchProcess, psutil.AccessDenied, FileNotFoundError):
                    pass
                except Exception as error:
                    report['cleanup_errors'].append(str(error))
            # Only captured fixture processes and their still-owned descendants.
            # psutil pins kernel birth identities before signalling.
            for process in reversed(owned):
                try:
                    if process.is_running():
                        children = process.children(recursive=True)
                        for child in [*reversed(children), process]:
                            if child.is_running():
                                report['retired_processes'].append({'pid': child.pid, 'birth': child.create_time()})
                                child.kill()
                        _, alive = psutil.wait_procs([process, *children], timeout=5)
                        assert not alive, 'owned processes remain alive'
                except psutil.NoSuchProcess:
                    pass
                except Exception as error:
                    report['cleanup_errors'].append(str(error))
            if provider is not None:
                try:
                    provider.wait(timeout=5)
                except Exception as error:
                    report['cleanup_errors'].append(str(error))
            if receiver is not None:
                try:
                    receiver.close()
                except Exception as error:
                    report['cleanup_errors'].append('receiver cleanup: ' + str(error))
            if tui is not None:
                # pywinpty 3.0.5 reads through a forwarding socket, which may
                # remain blocked after its child exits. isalive() also marks
                # the wrapper closed, making close() skip these handles.
                # Cancel only this fixture's PTY I/O and shut down its socket
                # before joining either reader; never signal a cached PID here.
                try:
                    tui.pty.cancel_io()
                except Exception as error:
                    report['cleanup_errors'].append('cancel PTY I/O: ' + str(error))
                try:
                    tui.fileobj.shutdown(socket.SHUT_RDWR)
                except OSError as error:
                    if error.winerror not in (10038, 10057, 10058):
                        report['cleanup_errors'].append(str(error))
                except Exception as error:
                    report['cleanup_errors'].append(str(error))
                finally:
                    tui.fileobj.close()
                    tui._server.close()
                tui._thread.join(timeout=2)
                if tui._thread.is_alive():
                    report['cleanup_errors'].append('PTY forwarding reader did not retire')
            if reader is not None:
                reader.join(timeout=2)
                if reader.is_alive():
                    report['cleanup_errors'].append('terminal reader did not retire')
            if server is not None:
                server.shutdown(); server.server_close()
            (out / 'terminal.txt').write_text(''.join(output), encoding='utf-8')
            try:
                remove_fixture(root)
            except Exception as error:
                report['cleanup_errors'].append(str(error))
            report['scratch_removed'] = not root.exists()
            if report['cleanup_errors'] or report['reader_errors'] or report.get('fixture_errors'):
                report['result'] = 'failed'
            (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: report[k] for k in ('result', 'steps', 'cleanup_errors', 'scratch_removed')}))
    return 0 if report['result'] in ('capability_observed', 'receiver_observed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
