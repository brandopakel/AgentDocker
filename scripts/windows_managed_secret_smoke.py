#!/usr/bin/env python3
"""Private Windows managed Codex secret input on exact extracted package bytes.

A real Codex process talks only to a loopback model with invented input. The
masked desktop form and synthetic ConPTY exercise consent, held ordinary input,
suspended typing, restored Unicode draft and the editor's oversized-line bound.
No account, saved profile, installed service, physical keyboard or screen-reader
acceptance is implied. The experimental secret-input switch remains required.
"""
import argparse
import csv
import datetime
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import re
import secrets
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import traceback


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def question_events(number, body, report, response_events):
    found = []
    for tool in body.get('tools', []):
        if tool.get('type') == 'function' and tool.get('name') == 'request_user_input':
            found.append((None, tool))
        if tool.get('type') == 'namespace':
            found.extend((tool['name'], member) for member in tool.get('tools', [])
                         if member.get('name') == 'request_user_input')
    assert len(found) == 1, 'provider did not advertise one request_user_input tool'
    namespace, tool = found[0]
    fields = tool.get('parameters', {}).get('properties', {}).get('questions', {}).get('items', {}).get('properties', {})
    flag = 'is_secret' if 'is_secret' in fields else 'isSecret'
    report.update(supplied_secret_flag=flag, secret_flag_advertised=flag in fields)
    arguments = {'questions': [{'id': 'synthetic_secret', 'header': 'Fixture',
        'question': 'Enter an invented fixture value; never use a real credential.', flag: True,
        'options': [{'label': 'Fixture A', 'description': 'Synthetic first choice'},
                    {'label': 'Fixture B', 'description': 'Synthetic second choice'}]}]}
    item = {'type': 'function_call', 'id': 'fc_secret_fixture', 'call_id': 'call_secret_fixture',
            'name': 'request_user_input', 'arguments': json.dumps(arguments), 'status': 'completed'}
    if namespace:
        item['namespace'] = namespace
    response = response_events(number)[-1]['response']; response['output'] = [item]
    return [{'type': 'response.created', 'response': dict(response, status='in_progress', output=[])},
            {'type': 'response.output_item.added', 'output_index': 0, 'item': dict(item, arguments='', status='in_progress')},
            {'type': 'response.function_call_arguments.delta', 'item_id': item['id'], 'output_index': 0, 'delta': item['arguments']},
            {'type': 'response.function_call_arguments.done', 'item_id': item['id'], 'output_index': 0, 'arguments': item['arguments']},
            {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
            {'type': 'response.completed', 'response': response}]


def scan_private(base, value, provider):
    """Stream bounded inactive fixture files; do not follow reparse points."""
    count = total = 0; matches = []; links = []
    needles = [value.encode(), value.encode("utf-16-le")]
    for directory, dirs, names in os.walk(base, followlinks=False):
        for name in [*dirs, *names]:
            path = Path(directory) / name; info = path.lstat()
            if info.st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
                relative = path.relative_to(base)
                # Codex can create exact executable helper links. They contain
                # no session data; never traverse a directory or another target.
                assert relative.parts[:2] == ('tmp', 'arg0') and path.resolve(strict=True) == provider
                assert name in ('applypatch', 'apply_patch', 'codex-execve-wrapper',
                                'applypatch.exe', 'apply_patch.exe', 'codex-execve-wrapper.exe')
                assert name not in dirs
                links.append(str(relative)); continue
            if stat.S_ISDIR(info.st_mode):
                continue
            assert stat.S_ISREG(info.st_mode), 'unexpected private fixture entry'
            count += 1; total += info.st_size
            assert count <= 10000 and info.st_size <= 512 * 1024**2 and total <= 1024**3
            digest = hashlib.sha256(); tails = [b'', b'']; occurrences = 0
            with path.open('rb') as source:
                for block in iter(lambda: source.read(1024 * 1024), b''):
                    digest.update(block)
                    for index, needle in enumerate(needles):
                        data = tails[index] + block
                        occurrences += data.count(needle)
                        tails[index] = data[-(len(needle) - 1):]
            if occurrences:
                matches.append({'path': str(path.relative_to(base)), 'occurrences': occurrences,
                                'bytes': info.st_size, 'sha256': digest.hexdigest()})
    return {'files': count, 'bytes': total, 'matches': matches, 'helper_links_not_followed': links}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--codex', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('requires native Windows')
    import psutil
    from winpty import PtyProcess
    from winpty.enums import Backend
    from windows_native_codex_smoke import current_user_objects, read_snapshot, remove_fixture, response_events
    from windows_remote_receiver_fixture import Receiver
    from windows_smoke_pipe import WindowsSmokePipe
    current_user_objects()
    binary = args.binary_dir.resolve(strict=True); codex = args.codex.resolve(strict=True)
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    provenance = json.loads((binary / 'build.json').read_text(encoding='utf-8'))
    assert provenance['source_dirty'] is False and provenance['target'] == 'x86_64-pc-windows-msvc'
    binaries = {name: sha(binary / name) for name in ['agentdocker.exe', 'agentd.exe', 'agentdocker-ui.exe']}
    assert binaries == provenance['binary_sha256']
    report = {'result': 'failed', 'scope': __doc__, 'source_commit': provenance['source_commit'],
              'source_tree': provenance['source_tree'], 'source_dirty': False, 'binary_sha256': binaries,
              'provider_version': subprocess.check_output([str(codex), '--version'], text=True, timeout=10).strip(),
              'provider_sha256': sha(codex), 'driver_sha256': sha(Path(__file__)),
              'helper_sha256': {name: sha(Path(__file__).with_name(name)) for name in
                  ['windows_native_codex_smoke.py', 'windows_remote_receiver_fixture.py', 'windows_smoke_pipe.py']},
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'cleanup_errors': [], 'reader_errors': [], 'model_requests': []}
    root = Path(tempfile.mkdtemp(prefix='AgentDocker managed secret ü ')).resolve()
    report['scratch'] = str(root)
    canary = 'SYNTHETIC_NOT_A_CREDENTIAL_' + secrets.token_hex(12)
    draft = 'SYNTHETIC_DRAFT_café_日本語_' + secrets.token_hex(8)
    suspended = 'SYNTHETIC_DISCARD_' + secrets.token_hex(8)
    late = 'SYNTHETIC_LATE_' + secrets.token_hex(8)
    overlong = 'SYNTHETIC_OVERLONG_' + secrets.token_hex(8)
    after_bound = 'SYNTHETIC_AFTER_BOUND_' + secrets.token_hex(8)
    report['canary_sha256'] = hashlib.sha256(canary.encode()).hexdigest()
    lock = threading.Lock(); tool_sent = False
    receiver = terminal = reader = window = server = agent = None
    output = []; closing = threading.Event(); owned = {}; window_log = None; console_closed = False

    def capture():
        if receiver is None:
            return
        for process in list(receiver.owned):
            try:
                for child in [process, *process.children(recursive=True)]:
                    identity = (child.pid, child.create_time())
                    if identity not in owned:
                        owned[identity] = child
                        if child not in receiver.owned:
                            receiver.owned.append(child)
            except psutil.NoSuchProcess:
                pass

    def wait(predicate, seconds=35):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            capture()
            value = predicate()
            if value:
                return value
            time.sleep(.1)
        raise TimeoutError('bounded private secret acceptance condition timed out')

    def text():
        return re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', ''.join(output))

    def rpc(request, allow_error=False):
        pipe = WindowsSmokePipe(receiver.socket, timeout=5, read_timeout=5, write_timeout=5, max_line=4*1024*1024)
        try:
            pipe.write((json.dumps(request) + '\n').encode('utf-8'))
            value = json.loads(pipe.readline())
        finally:
            pipe.close()
        assert allow_error or value.get('type') != 'error', value.get('code')
        return value

    def inspect():
        value = rpc({'op': 'inspect', 'agent': agent})['agent']
        for pid in [value.get('pid'), (value.get('owner') or {}).get('pid')]:
            if pid:
                process = psutil.Process(pid)
                if process not in receiver.owned:
                    receiver.owned.append(process)
        capture()
        return value

    def ledger():
        inspect()
        path = root / 'state/codex-input' / agent / 'delivery.json'
        return read_snapshot(path) if path.exists() else {}

    def close_console():
        nonlocal console_closed
        if terminal is None or console_closed:
            return
        closing.set(); terminal.pty.cancel_io()
        try:
            terminal.fileobj.shutdown(socket.SHUT_RDWR)
        except OSError as error:
            if error.winerror not in (10038, 10057, 10058):
                raise
        terminal.fileobj.close(); terminal._server.close(); terminal._thread.join(timeout=2)
        assert not terminal._thread.is_alive()
        if reader is not None:
            reader.join(timeout=2); assert not reader.is_alive(), 'terminal reader did not retire'
        # Attachment has already exited; release the final native ConPTY owner
        # after stopping its reader/worker, never as the means of ending it.
        terminal.closed = True  # suppress destructor signalling after orderly exit
        terminal.pty = None
        console_closed = True

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            report.setdefault('unexpected_get', []).append(self.path); self.send_error(404)

        def do_POST(self):
            nonlocal tool_sent
            try:
                size = int(self.headers['Content-Length']); assert 0 < size < 8*1024*1024
                body = json.loads(self.rfile.read(size)); assert self.headers.get('Authorization') == 'Bearer fixture-only'
                rendered = json.dumps(body, ensure_ascii=False)
                auxiliary = 'Generate a concise, single-line task title' in rendered
                outputs = [v for v in body.get('input', []) if isinstance(v, dict) and v.get('type') == 'function_call_output']
                with lock:
                    number = len(report['model_requests']) + 1; assert number <= 10
                    report['model_requests'].append({'path': self.path, 'auxiliary': auxiliary,
                        'canary_present': canary in rendered, 'function_output_count': len(outputs),
                        'canary_in_function_output': canary in json.dumps(outputs),
                        'saved_draft_present': draft + '_COMPLETED' in rendered,
                        'after_bound_present': after_bound in rendered,
                        'discarded_input_present': any(v in rendered for v in [suspended, late, overlong])})
                    emit = not auxiliary and not tool_sent
                    if emit:
                        tool_sent = True
                events = question_events(number, body, report, response_events) if emit else response_events(number)
                data = ''.join('data: ' + json.dumps(e) + '\n\n' for e in events).encode()
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
            except BaseException:
                report.setdefault('model_errors', []).append(traceback.format_exc().replace(canary, '[synthetic value]'))
                self.send_error(500)

    try:
        identity = subprocess.check_output(['whoami.exe', '/user', '/fo', 'csv', '/nh'], text=True, timeout=10)
        sid = next(csv.reader(io.StringIO(identity.strip())))[1]; assert sid.startswith('S-1-5-')
        subprocess.run(['icacls.exe', str(root), '/inheritance:r', '/grant:r', '*'+sid+':(OI)(CI)F'], check=True, capture_output=True, timeout=10)
        project = root/'project'; profile = root/'profile'; internal = root/'internal'
        for path in [project, profile, internal]:
            path.mkdir()
        subprocess.run(['git', 'init', '-q', str(project)], check=True, timeout=10)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler); server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        config = ('model = "fixture-model"\nmodel_provider = "fixture"\napproval_policy = "never"\nsandbox_mode = "read-only"\ncheck_for_update_on_startup = false\n'
                  '[features]\napps = false\ndefault_mode_request_user_input = true\n[analytics]\nenabled = false\n'
                  '[model_providers.fixture]\nname = "Private synthetic secret fixture"\nbase_url = '+json.dumps(f'http://127.0.0.1:{server.server_port}/v1')+'\n'
                  'wire_api = "responses"\nenv_key = "AGENTDOCKER_FIXTURE_KEY"\nrequest_max_retries = 0\nstream_max_retries = 0\nsupports_websockets = false\n'
                  '[projects.'+json.dumps(str(project))+']\ntrust_level = "trusted"\n')
        (profile/'config.toml').write_text(config, encoding='utf-8'); report['config_sha256'] = sha(profile/'config.toml')
        env = {k: v for k, v in os.environ.items() if k.upper() in ['PATH', 'SYSTEMROOT', 'WINDIR', 'TEMP', 'TMP', 'COMSPEC', 'PATHEXT', 'PROGRAMFILES', 'PROGRAMFILES(X86)', 'LANG', 'LC_ALL', 'PYTHONUTF8']}
        env.update(HOME=str(root), USERPROFILE=str(root), APPDATA=str(root/'AppData/Roaming'), LOCALAPPDATA=str(root/'AppData/Local'),
                   CODEX_HOME=str(profile), AGENTDOCKER_HOME=str(root/'state'), AGENTDOCKER_NO_AUTOSTART='1',
                   AGENTDOCKER_NO_NOTIFICATIONS='1', AGENTDOCKER_FIXTURE_KEY='fixture-only', AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT='1')
        receiver = Receiver(binary, root, project, profile, internal, env, report); receiver.start_daemon(); capture()
        human = rpc({'op': 'me', 'workdir': str(project)})['agent']['id']
        cli = binary/'agentdocker.exe'
        result = subprocess.run([str(cli), 'run', '--name', 'managed-secret-fixture', '--runtime', 'codex', '--codex-input', '--tty',
                                 '--workdir', str(project), '--restart', 'no', '--env', 'HOME='+str(root), '--env', 'CODEX_HOME='+str(profile),
                                 '--env', 'AGENTDOCKER_FIXTURE_KEY=fixture-only', '--env', 'AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT=1', '--', str(codex)],
                                cwd=project, env=receiver.env, capture_output=True, text=True, encoding='utf-8', timeout=15)
        assert result.returncode == 0, result.stderr.replace(canary, '[synthetic value]')
        agent = result.stdout.strip(); report['agent'] = agent
        def ready():
            value = inspect(); delivery = value.get('input_delivery')
            assert value.get('pid'), 'managed fixture exited before readiness'
            return value if isinstance(delivery, dict) and not delivery['paused'] and delivery['process_started_at'] == value['process_started_at'] else None
        initial = wait(ready); report['initial_generation'] = {'pid': initial['pid'], 'birth': initial['process_started_at']}
        terminal = PtyProcess.spawn([str(cli), 'attach', agent], cwd=str(project), env=receiver.env, dimensions=(40,160), backend=Backend.ConPTY)
        attach = psutil.Process(terminal.pid); receiver.owned.append(attach); capture()
        def read_terminal():
            tail = ''
            try:
                while terminal.isalive():
                    chunk = terminal.read(65536); output.append(chunk)
                    if '\x1b[6n' in tail + chunk:
                        terminal.write('\x1b[1;1R')
                    tail = (tail + chunk)[-3:]
                    assert sum(map(len, output)) < 4*1024*1024, 'terminal output exceeded its bound'
            except EOFError:
                pass
            except Exception as error:
                if not closing.is_set():
                    report['reader_errors'].append(str(error).replace(canary, '[synthetic value]'))
        reader = threading.Thread(target=read_terminal, daemon=True); reader.start()
        wait(lambda: 'attached to' in text(), 10)
        terminal.write(draft); wait(lambda: draft in text(), 10); report['draft_typed_before_question'] = True
        def send(message):
            return rpc({'op': 'send', 'from': human, 'to': agent, 'kind': 'chat', 'payload': {'text': message}})['message']
        def reviews():
            return rpc({'op': 'secret_reviews', 'recipient': human})['reviews']
        first = send('Run the private synthetic secret question fixture.')
        view = wait(lambda: next(iter(reviews()), None)); report['review_metadata'] = view
        wait(lambda: 'waiting for temporary input' in text(), 10)
        terminal.write(suspended+'\r'); time.sleep(.5); assert suspended not in text()
        report['suspended_terminal_input_not_echoed'] = True
        report['fence_before_answer'] = ledger()['secret_review']; assert not report['fence_before_answer']['response_attempted']
        second = send('A separate ordinary message after temporary input.')
        report['ordinary_message_ids'] = [first, second]
        time.sleep(1); assert not ledger()['completed'] and len(reviews()) == 1
        report['queued_ordinary_message_held'] = True
        refused = rpc({'op': 'answer_secret_review', 'from': human, 'review': view['id'], 'answers': {'synthetic_secret': canary}, 'retention_acknowledged': False}, True)
        assert refused.get('type') == 'error' and refused.get('code') == 'invalid'; report['notice_required'] = True
        route = view['id']; field = 'temporary-'+route+'-synthetic_secret'
        scenario = [{'op': 'wait_control', 'id': field, 'present': True}, {'op': 'fill', 'id': field, 'text': canary},
                    {'op': 'wait_text_absent', 'text': canary}, {'op': 'capture', 'name': 'masked-before-consent'},
                    {'op': 'click', 'id': 'temporary-notice-'+route}, {'op': 'wait_text', 'text': 'Provider retention notice acknowledged'},
                    {'op': 'capture', 'name': 'masked-after-consent'}, {'op': 'click', 'id': 'temporary-submit-'+route},
                    {'op': 'wait_control', 'id': field, 'present': False}, {'op': 'capture', 'name': 'closed'}]
        scenario_path = root/'scenario.json'; scenario_path.write_text(json.dumps(scenario), encoding='utf-8')
        window_log = (internal/'window.log').open('wb')
        window = subprocess.Popen([str(binary/'agentdocker-ui.exe'), '--smoke-test', str(out/'window'), '--smoke-deadline', '60', '--smoke-scenario', str(scenario_path)],
                                  cwd=project, env=receiver.env, stdin=subprocess.DEVNULL, stdout=window_log, stderr=subprocess.STDOUT)
        receiver.children.append(window); receiver.owned.append(psutil.Process(window.pid)); capture()
        wait(lambda: window.poll() is not None, 70); report['window_exit'] = window.returncode
        w = json.loads((out/'window/result.json').read_text(encoding='utf-8')); report['window_report'] = w
        assert window.returncode == 0 and w['result'] == 'passed' and w['scenario_steps_completed'] == len(scenario)
        report['masked_app_submission'] = True
        wait(lambda: len(ledger()['completed']) == 2)
        current = ledger(); assert current['secret_review'] is None and current['attempt'] is None and not reviews()
        assert [v['message'] for v in current['completed']] == [first, second]
        report['secret_fence_closed'] = True
        terminal.write(late+'\r'); wait(lambda: 'Terminal input resumed; your earlier draft is preserved.' in text(), 10)
        assert late not in text()
        terminal.write('_COMPLETED\r'); wait(lambda: len(ledger()['completed']) == 3)
        wait(lambda: any(v['saved_draft_present'] for v in report['model_requests']))
        report['draft_restored_and_delivered'] = True
        terminal.write('x'*16001+overlong+'\r'); wait(lambda: 'Terminal input ignored: it exceeds 16000 bytes.' in text(), 15)
        assert len(ledger()['completed']) == 3
        terminal.write(after_bound+'\r'); wait(lambda: len(ledger()['completed']) == 4)
        wait(lambda: any(v['after_bound_present'] for v in report['model_requests']))
        report['oversized_line_discarded'] = True
        assert not any(v['discarded_input_present'] for v in report['model_requests'])
        report['suspended_and_late_input_never_delivered'] = True
        current = ledger(); report['completed_receipts'] = current['completed']
        assert len({v['message'] for v in current['completed']}) == 4 and [v['message'] for v in current['completed'][:2]] == [first, second]
        assert current['secret_review'] is None and current['attempt'] is None
        report['final_fence_closed'] = True
        replay = rpc({'op': 'answer_secret_review', 'from': human, 'review': route, 'answers': {'synthetic_secret': canary}, 'retention_acknowledged': True}, True)
        assert replay.get('type') == 'error' and replay.get('code') == 'not_found'; report['stale_answer_refused'] = True
        assert not report.get('model_errors') and not report.get('unexpected_get')
        report['config_unchanged'] = sha(profile/'config.toml') == report['config_sha256']; assert report['config_unchanged']
        report['result'] = 'passed'
    except BaseException:
        report['error'] = traceback.format_exc().replace(canary, '[synthetic value]')
    finally:
        try:
            capture()
            report['watched_processes'] = [{'pid': pid, 'birth': birth} for pid, birth in owned]
            if terminal is not None and terminal.isalive():
                terminal.write('\x1d'); wait(lambda: not terminal.isalive(), 10)
            close_console()
            if agent:
                rpc({'op': 'stop', 'agent': agent, 'force': False})
            if receiver is not None:
                receiver.close()
            if window_log:
                window_log.close()
            report['remaining_processes'] = [{'pid': pid, 'birth': birth} for (pid, birth), process in owned.items() if process.is_running()]
            assert not report['remaining_processes'] and not report['cleanup_errors'] and not report['reader_errors']
            report['terminal_canary_present'] = canary in ''.join(output); assert not report['terminal_canary_present']
            report['terminal_sha256'] = hashlib.sha256(''.join(output).encode()).hexdigest()
            report['scans'] = {}
            for category, base in [('agentdocker', root/'state'), ('provider', root/'profile'), ('internal', root/'internal'), ('gui', out/'window')]:
                if base.exists():
                    report['scans'][category] = scan_private(base, canary, codex)
                    if category != 'provider':
                        assert not report['scans'][category]['matches'], category+' retained invented value'
            if report['result'] == 'passed':
                assert report['scans']['provider']['matches'], 'expected provider retention was not observed'
            report['binaries_unchanged'] = binaries == {n: sha(binary/n) for n in binaries}
            report['provider_unchanged'] = sha(codex) == report['provider_sha256']
            assert report['binaries_unchanged'] and report['provider_unchanged']
        except BaseException:
            report['cleanup_errors'].append(traceback.format_exc().replace(canary, '[synthetic value]')); report['result'] = 'failed'
            # Still retire only processes captured through this private daemon
            # or its explicit children. Forced retirement is always a failure.
            if receiver is not None:
                try:
                    receiver.close()
                except BaseException:
                    report['cleanup_errors'].append(traceback.format_exc().replace(canary, '[synthetic value]'))
        if server is not None:
            server.shutdown(); server.server_close()
        if window_log:
            window_log.close()
        if not console_closed and terminal is not None:
            try:
                close_console()
            except BaseException:
                report['cleanup_errors'].append(traceback.format_exc().replace(canary, '[synthetic value]')); report['result'] = 'failed'
        if report['result'] != 'passed':
            try:
                paths = list((root/'internal').glob('*.log')) + list((root/'state/logs').glob('*'))
                report['sanitized_diagnostics'] = []
                for path in paths[:20]:
                    if path.is_file() and not path.is_symlink():
                        with path.open('rb') as source:
                            source.seek(max(0, path.stat().st_size - 6000))
                            tail = source.read(6000).decode('utf-8', errors='replace')
                        report['sanitized_diagnostics'].append({'path': str(path.relative_to(root)), 'tail': tail.replace(canary, '[synthetic value]')})
            except BaseException:
                report['cleanup_errors'].append('sanitized diagnostics: '+traceback.format_exc().replace(canary, '[synthetic value]'))
        try:
            # No raw value, provider transcript or scenario is kept in reports.
            remove_fixture(root)
        except BaseException:
            report['cleanup_errors'].append(traceback.format_exc().replace(canary, '[synthetic value]')); report['result'] = 'failed'
        report['scratch_removed'] = not root.exists()
        report['captures'] = {p.name: sha(p) for p in (out/'window').glob('*.png')} if (out/'window').exists() else {}
        report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        rendered = json.dumps(report, indent=2); assert canary not in rendered
        (out/'result.json').write_text(rendered+'\n', encoding='utf-8')
        print(json.dumps({k: report.get(k) for k in ['result', 'error', 'cleanup_errors', 'scratch_removed', 'window_report']}))
    return report['result'] != 'passed'


if __name__ == '__main__':
    raise SystemExit(main())
