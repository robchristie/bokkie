#!/usr/bin/env python3
"""One finite model-backed one-off probe continuing the saved reminder fixture.

Requires the successful recurring journey, the same authorised private profile
and marked synthetic store. Records the current executable and candidate, and
uses the fixture's schema compatibility check on resume. Sends no mail.
"""
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import time
import urllib.request
import uuid
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

root = Path.cwd()
evidence = Path(os.environ.get('BOKKIE_REMINDER_EVIDENCE', '.ui-qualification-runtime/reminders-live')).resolve()
prefix = json.loads((evidence / 'qualification.json').read_text())
binary = root / 'target/debug/bokkie-conversation-fixture'
assert prefix['passed'] and prefix['mode'].startswith('live-model')
assert prefix['model_dispatches'] + 2 <= prefix['budget']['model_dispatches']
profile = Path(os.environ['BOKKIE_REMINDER_PROFILE']).resolve(strict=True)
report = {'mode': 'live-model with synthetic notification outcomes; no external mail',
          'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
          'fixture_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
          'prefix_sha256': hashlib.sha256((evidence / 'qualification.json').read_bytes()).hexdigest(),
          'checks': [], 'passed': False}
process = subprocess.Popen([str(binary), '--root', prefix['initial']['root'], '--resume',
                           '--ui-dir', str(root / 'apps/bokkie-attention-ui/web'),
                           '--profile', str(profile), '--synthetic-reminders'],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

def line():
    assert select.select([process.stdout], [], [], 20)[0], 'fixture response exceeded finite bound'
    return json.loads(process.stdout.readline())

def control(**command):
    process.stdin.write(json.dumps(command) + '\n'); process.stdin.flush()
    result = line()
    assert 'error' not in result, result.get('error')
    return result

def check(condition, text):
    assert condition, text
    report['checks'].append(text)

try:
    initial = line(); origin = 'http://' + initial['address']
    def get(path):
        with urllib.request.urlopen(origin + path, timeout=5) as response:
            return json.load(response)
    bootstrap = get('/bootstrap')
    def post(path, value):
        request = urllib.request.Request(origin + path, json.dumps(value).encode(), method='POST', headers={
            'Content-Type': 'application/json', 'Origin': origin,
            'Sec-Fetch-Site': 'same-origin', 'X-Bokkie-Mutation-Token': bootstrap['mutation_token']})
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)
    before = control()
    assert before['model_calls'] + 2 <= prefix['budget']['model_dispatches'], 'Aggregate model budget exhausted'
    chat = str(uuid.uuid4()); view = get('/conversations/' + chat)
    request_id = str(uuid.uuid4())
    post('/conversations/turn', {'command_id': request_id, 'conversation_id': chat,
         'expected_revision': view['revision'], 'text': 'Tomorrow at 8 am, remind me to drink a glass of water.'})
    deadline = time.monotonic() + 210
    while time.monotonic() < deadline:
        view = get('/conversations/' + chat)
        if not view['busy'] and any(m['role'] == 'assistant' and m['request_id'] == request_id for m in view['messages']):
            break
        time.sleep(0.2)
    check(not view['busy'] and view['request_error'] is None, 'One-off interpretation completed within its finite bound')
    definition = view['review']['preview']['definition']
    expected = (datetime.fromtimestamp(before['now'], ZoneInfo('Australia/Adelaide')) + timedelta(days=1)).replace(hour=8, minute=0, second=0, microsecond=0)
    check(definition['trigger'] == {'kind': 'once', 'local_datetime': expected.strftime('%Y-%m-%dT%H:%M'), 'timezone': 'Australia/Adelaide'}, 'Tomorrow resolves to one concrete 8 am Adelaide date')
    check(definition['capability'] == 'reminder' and definition['destination'] == 'fixture-recipient@example.invalid', 'One-off retains the reviewed reminder effect and single synthetic destination')
    check(view['task']['status'] == 'draft' and len(view['review']['preview']['occurrences']) == 1, 'One-off draft is inactive and previews exactly one occurrence')
    confirmation = {'command_id': str(uuid.uuid4()), 'conversation_id': chat,
                    'proposal_id': view['review']['id'], 'session_id': bootstrap['service']['session_id']}
    confirmed = post('/conversations/confirm', confirmation)
    replayed = post('/conversations/confirm', confirmation)
    check(confirmed['task']['id'] == replayed['task']['id'] and confirmed['receipt'] == replayed['receipt'], 'Lost confirmation response replay returns the same task and receipt')
    due = view['review']['preview']['occurrences'][0]
    result = control(now=due, reminder_tick=True, delivery='accepted')
    # The recurring task can also be due; each manual tick claims at most one.
    for _ in range(3):
        task = next(d for d in result['details'] if d['id'] == confirmed['task']['id'])
        if task['status'] == 'completed' and any(r.get('delivery', {}).get('status') == 'accepted_by_relay' for r in task['runs'] if r.get('delivery')):
            break
        result = control(reminder_tick=True, delivery='accepted')
    check(task['status'] == 'completed' and sum(r['result'] is not None for r in task['runs']) == 1, 'Due one-off saves one result and completes its schedule')
    check(task['runs'][0]['delivery']['status'] == 'accepted_by_relay', 'One-off delivery is distinct from its completed occurrence')
    repeated = control(reminder_tick=True, delivery='accepted')
    after = next(d for d in repeated['details'] if d['id'] == task['id'])
    check(after == task, 'Repeated ticks cannot rearm or redeliver the completed one-off')
    check(result['model_calls'] - before['model_calls'] <= 2 and repeated['model_calls'] == result['model_calls'], 'Due execution and repeated polling make no additional model dispatches')
    report.update(passed=True, model_dispatches=result['model_calls'] - before['model_calls'],
                  aggregate_model_dispatches=result['model_calls'], definition=definition, completed=task)
except Exception as error:
    report['error'] = str(error)
    raise
finally:
    if process.poll() is None:
        process.stdin.write('{"stop":true}\n'); process.stdin.flush()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill(); process.wait(timeout=5)
    report['stderr'] = process.stderr.read()
    (evidence / 'one-off.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({key: report.get(key) for key in ('passed', 'error', 'model_dispatches', 'aggregate_model_dispatches')}))
