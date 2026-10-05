"""Private interrupted-queue acceptance within the product-owned Windows TUI.

Called only by windows_native_launcher_smoke's explicit recovery scenarios.
Uses its authenticated observer, existing receiver and private loopback model.
"""
import datetime
import json
import subprocess
import time

from windows_native_codex_smoke import wait


def exercise(mode, receiver, call, thread, held_text, recovery_text, held_model,
             release_model, report, step):
    def recover(arguments):
        result = subprocess.run(
            [str(receiver.cli), '--socket', receiver.socket, 'codex-queue-resolve',
             '--agent', receiver.agent, *arguments], cwd=receiver.repo,
            env=receiver.env, stdin=subprocess.DEVNULL, capture_output=True,
            text=True, encoding='utf-8', timeout=90)
        return {'exit_code': result.returncode, 'stdout': result.stdout,
                'stderr': result.stderr}

    held_message = receiver.send(held_text)
    held_receipt = receiver.received(held_message)
    assert held_model.wait(20), 'private response was not held'
    assert call('thread/read', {'threadId': thread, 'includeTurns': False})['thread']['status']['type'] == 'active'
    call('turn/interrupt', {'threadId': thread, 'turnId': held_receipt['receipt']['turn']})

    def interrupted():
        value = call('thread/read', {'threadId': thread, 'includeTurns': True})['thread']
        turns = [turn for turn in value['turns'] if turn['id'] == held_receipt['receipt']['turn']]
        return value if value['status']['type'] == 'idle' and len(turns) == 1 and turns[0]['status'] == 'interrupted' else None

    state = wait(interrupted, 20)
    release_model.set()
    completed = receiver.ledger()['completed']
    assert len(completed) == 2 and held_receipt in completed
    message = receiver.send(recovery_text)
    held_at = time.monotonic()
    while time.monotonic() - held_at < 30:
        assert receiver.ledger()['completed'] == completed
        assert not any(r['recovery'] for r in report['requests'])
        time.sleep(0.1)
    pending = call('thread/queue/list', {'threadId': thread})['data']
    before = receiver.ledger()['attempt']
    assert len(pending) == 1 and pending[0]['clientUserMessageId'] == message
    assert pending[0]['id'] == before['queued'] and before['message'] == message
    assert not before.get('start') and before['receipt'] is None
    preview = recover([]); assert preview['exit_code'] == 0, preview
    shown = json.loads(preview['stdout'])['pending']; choice = shown['start_confirmation']
    assert choice and choice != shown['confirmation'] and shown['start_intent'] is None
    assert shown['message'] == message and shown['queued_submission'] == pending[0]['id']
    proof = {'mode': mode, 'held_receipt': held_receipt, 'interrupted_thread': state,
             'message': message, 'queue_before': pending, 'ledger_before': receiver.ledger(),
             'preview': preview, 'hold_seconds': time.monotonic() - held_at, 'refusals': []}
    report['queued_recovery'] = proof
    step('deliberate interruption retains the exact next queue entry for 30 seconds', True)

    def refused(label, arguments, reason):
        reply = recover(arguments)
        after = receiver.ledger()
        proof['refusals'].append({'case': label, 'reply': reply, 'ledger_after': after})
        assert reply['exit_code'] != 0 and reason in reply['stderr'], reply
        assert after['completed'] == completed and after['attempt'] == before
        assert not any(r['recovery'] for r in report['requests'])

    command = ['--message', message, '--start-queued', choice, '--note', 'Private Windows recovery acceptance']
    if mode == 'holds':
        generation = receiver.binding()['provider']['process']['started_at']
        receiver.rpc({'op': 'report_provider', 'agent': receiver.agent,
                      'process_started_at': generation,
                      'observed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                      'report': {'state': 'blocked', 'issue': {'kind': 'rate'}}})
        availability = receiver.rpc({'op': 'inspect', 'agent': receiver.agent})['agent']['provider_availability']
        proof['provider_hold'] = availability
        refused('provider_rate_hold', command, 'daemon is holding this input')
        receiver.rpc({'op': 'resume_provider', 'agent': receiver.agent,
                      'blocked_at': availability['observed_at']})
        proof['pause'] = receiver.rpc({'op': 'pause', 'from': 'user',
                                      'project': str(receiver.repo),
                                      'reason': 'Private queued recovery guard fixture'})
        refused('project_pause', command, 'project is paused')
        proof['pauses_after'] = receiver.rpc({'op': 'pauses'})
        assert any(p['project'] == proof['pause']['pause']['project'] for p in proof['pauses_after']['pauses'])
        proof['paused_preview'] = recover([])
        assert proof['paused_preview']['exit_code'] == 0
        again = json.loads(proof['paused_preview']['stdout'])['pending']
        assert again['message'] == message and again['start_confirmation'] == choice and again['start_intent'] is None
        proof['completed_after'] = receiver.ledger()['completed']
        step('provider and project holds refuse before intent while preview remains readable', True)
    else:
        refused('wrong_digest', ['--message', message, '--start-queued', '0' * 64,
                                '--note', 'Private stale confirmation'], 'queued input or provider generation changed')
        refused('wrong_message', ['--message', 'other-private-message', '--start-queued', choice,
                                 '--note', 'Private changed message'], 'retained input changed')
        refused('manual_read_is_not_start', ['--message', message, '--confirm-read', choice,
                                           '--note', 'Private distinct action'], 'retained input or provider generation changed')
        foreign = call('thread/queue/add', {'threadId': thread, 'clientUserMessageId': 'private-foreign-head',
                       'input': [{'type': 'text', 'text': 'PRIVATE_FOREIGN_HEAD', 'text_elements': []}]})['queuedSubmission']['id']
        call('thread/queue/reorder', {'threadId': thread, 'queuedSubmissionIds': [foreign, pending[0]['id']]})
        refused('foreign_head', command, 'another or edited input is ahead')
        assert call('thread/queue/delete', {'threadId': thread, 'queuedSubmissionId': foreign})['deleted'] is True
        proof['response'] = recover(command)
        assert proof['response']['exit_code'] == 0, proof['response']
        first = json.loads(proof['response']['stdout'])
        assert first['message'] == message and first['queued_start'] and first['turn'] and not first['already_attempted']
        proof['repeat'] = recover(command)
        assert proof['repeat']['exit_code'] == 0, proof['repeat']
        again = json.loads(proof['repeat']['stdout'])
        assert again['already_attempted'] and all(again[k] == first[k] for k in ['message', 'queued_start', 'turn'])
        receipt = receiver.received(message)
        assert receipt['receipt']['turn'] == first['turn']
        assert receiver.ledger()['completed'] == completed + [receipt]
        wait(lambda: any(r['recovery'] and not r['title'] for r in report['requests']), 20)
        wait(lambda: call('thread/read', {'threadId': thread, 'includeTurns': False})['thread']['status']['type'] == 'idle', 20)
        assert sum(r['recovery'] and not r['title'] for r in report['requests']) == 1
        proof['completed_after'] = receiver.ledger()['completed']
        step('four invalid starts refuse and one explicit start with repeated confirmation preserves the original receipts', True)
    proof['final_history'] = call('thread/items/list', {'threadId': thread, 'limit': 20, 'sortDirection': 'asc'})
    users = [item for item in proof['final_history']['data'] if item['item']['type'] == 'userMessage']
    assert len(users) == len(proof['completed_after'])
    for receipt in proof['completed_after']:
        matching = [item for item in users if item['item'].get('clientId') == receipt['message']]
        assert len(matching) == 1
        item = matching[0]
        assert receipt['receipt'] == {'thread': thread, 'turn': item['turnId'], 'item': item['item']['id']}
    step('every retained input has its original exact provider receipt without an extra user item', True)
