#!/usr/bin/env python3
"""Opt-in live fixture through Bokkie's HTTP intake and implemented controller.

Creates only synthetic task-scoped resources. It never supplies supervisor
answers or worker instructions after the initial fixture/intent is saved.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.request


def dump(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def snapshot(database, outcome):
    with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
        row = connection.execute('SELECT v.snapshot_json, b.state FROM engineering_outcomes o JOIN engineering_versions v ON v.outcome_id=o.id AND v.revision=o.state_revision JOIN obligations b ON b.id=o.root_obligation_id WHERE o.id=?', (outcome,)).fetchone()
        value = json.loads(row[0]); value['observed_root_state'] = row[1]
        return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-live', action='store_true', required=True)
    parser.add_argument('--runtime-root', type=Path, required=True)
    parser.add_argument('--timeout-seconds', type=int, default=2400)
    args = parser.parse_args()
    if not 60 <= args.timeout_seconds <= 3600:
        parser.error('timeout must be 60–3600 seconds')
    source = Path(__file__).resolve().parents[1]
    root = args.runtime_root.resolve()
    if root.exists():
        parser.error('runtime root must be new; retained evidence is never overwritten')
    root.mkdir(mode=0o700)
    workspace = root / 'workspace'; workspace.mkdir()
    brokers = root / 'brokers'; brokers.mkdir(mode=0o700)
    scratch = workspace / '.runtime-scratch'; scratch.mkdir(mode=0o700)
    (workspace / '.gitignore').write_text('/.runtime-scratch/\n__pycache__/\n')
    (workspace / 'tests').mkdir()
    (workspace / 'README.md').write_text('Synthetic arithmetic fixture. The required module name is arithmetic. Add and multiply integers, including negative values and zero. Verify with python3 -m unittest discover -s tests.\n')
    (workspace / 'tests/test_arithmetic.py').write_text('import unittest\nimport arithmetic\nclass ArithmeticTests(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(arithmetic.add(-3, 3), 0)\n    def test_multiply(self):\n        self.assertEqual(arithmetic.multiply(-3, 4), -12)\n        self.assertEqual(arithmetic.multiply(7, 0), 0)\n')
    (workspace / 'AGENTS.md').write_text('''# Synthetic qualification fixture
Use Australian English. Work is local-only; local commits are allowed, remote publication is not. Canonical verification: python3 -m unittest discover -s tests.

The following fault injection is intentional fixture setup, not product acceptance. On the first engineering execution only (when injection-consumed.txt is absent), ask Bokkie which module name is required; the answer is already in README and the saved intent. Use the durable bokkie_question tool and wait for the supervisor answer. Create injection-consumed.txt, implement addition, but deliberately leave multiplication absent. Run checks and obtain an independent review; submit the incomplete result honestly so Bokkie must commission a linked repair. A repair execution implements the remaining requirements and verifies them.

Before your first final submission, after evidence and review are ready, run python3 -c "import time; print('bokkie-offline-window', flush=True); time.sleep(12)". The qualification controller may disconnect during that command. For this fixture return the complete EngineeringSubmissionInput as bare JSON in your final message; do not call submit_result. Bokkie's broker retains the final result for post-reap submission. Repair execution may use the ordinary submit_result route. Do not wait for the outer agent or bypass Bokkie supervision.
''')
    subprocess.run(['git', 'init', '-q', str(workspace)], check=True)
    subprocess.run(['git', '-C', str(workspace), 'add', '.'], check=True)
    subprocess.run(['git', '-C', str(workspace), 'commit', '-qm', 'Create synthetic supervision fixture'], check=True)
    profile = json.loads((source / 'instructions/profiles/engineering-local.json').read_text())
    profile.update(workspace=str(workspace), broker_root=str(brokers), broker=str(source / 'tools/engineering-runtime/broker.py'), codex=str(Path(shutil.which('codex')).resolve()), supervisor_instructions=str(source / 'instructions/engineering-supervisor.md'), worker_instructions=str(source / 'instructions/engineering-worker.md'), worker_scratch=str(scratch), worker_network_access=True)
    profile_path = root / 'profile.json'; dump(profile_path, profile)
    database = root / 'fixture.sqlite'
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0)); port = reservation.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    process = None; logs = []
    history = []
    def record(kind, **values):
        history.append(dict(kind=kind, at=time.time(), **values)); dump(root / 'journey.json', history)
        print(json.dumps(dict(kind=kind, **values)), flush=True)
    def call(path, body=None):
        headers = {}
        if body is not None:
            bootstrap = json.load(urllib.request.urlopen(base + '/bootstrap', timeout=3))
            headers = {'Content-Type': 'application/json', 'X-Bokkie-Mutation-Token': bootstrap['mutation_token']}
        request = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
        return json.load(urllib.request.urlopen(request, timeout=10))
    def start():
        nonlocal process
        stream = (root / f'controller-{len(logs)}.log').open('w'); logs.append(stream)
        process = subprocess.Popen([str(source / 'target/debug/bokkie'), '--database', str(database), 'serve', '--bind', f'127.0.0.1:{port}', '--poll-ms', '250', '--engineering-profile', str(profile_path)], stdout=stream, stderr=stream, start_new_session=True)
        until = time.monotonic() + 20
        while time.monotonic() < until:
            if process.poll() is not None: raise RuntimeError('controller exited; inspect retained log')
            try: call('/health'); return
            except (OSError, ValueError): time.sleep(.2)
        raise TimeoutError('controller did not become ready')
    def stop():
        nonlocal process
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try: process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(); record('controller_forced_stop')
    def event_logs(state):
        for execution in state['executions']:
            path = brokers / execution['id'] / 'events.jsonl'
            if path.exists():
                for line in path.read_text().splitlines():
                    try: yield execution, json.loads(line)
                    except ValueError: pass
    source_revision = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
    fixture_revision = subprocess.check_output(['git', '-C', str(workspace), 'rev-parse', 'HEAD'], text=True).strip()
    record('fixture_created', source_revision=source_revision, fixture_revision=fixture_revision, profile_sha256=hashlib.sha256(profile_path.read_bytes()).hexdigest())
    body = {'command_id': 'fixture-intake-v1', 'intent': 'Deliver a local Python arithmetic module named arithmetic with addition and multiplication of integers, including negatives and zero, canonical unittest verification and usage documentation. Follow the synthetic fixture instructions: the first submission is deliberately incomplete, but final acceptance requires both operations and a linked repair of that incomplete submission. Answer the routine module-name question from this saved intent without human relay. Keep independent engineering review separate from your product acceptance. This is local-only; no remote publication or external authority is granted.'}
    outcome = None
    passed = False
    try:
        start(); receipt = call('/engineering/outcomes', body)
        record('intake_saved', receipt=receipt)
        # The wire response owns its exact saved outcome identity.
        outcome = receipt.get('outcome_id') or receipt.get('receipt', {}).get('outcome_id')
        if not outcome: raise RuntimeError('intake did not return outcome identity')
        restarted = False; offline = False
        until = time.monotonic() + args.timeout_seconds
        while time.monotonic() < until:
            state = snapshot(database, outcome)
            logs_now = list(event_logs(state))
            if not restarted and any(e['role'] == 'worker' and v['kind'] == 'thread_identity' for e, v in logs_now):
                stop(); record('restart_after_dispatch', executions=[e['id'] for e in state['executions']]); start()
                replay = call('/engineering/outcomes', body)
                if {k:v for k,v in replay.items() if k != 'service'} != {k:v for k,v in receipt.items() if k != 'service'}: raise AssertionError('intake replay changed saved receipt')
                record('lost_ack_replay_passed'); restarted = True
            if not offline and any(e['role'] == 'worker' and v['kind'] == 'item/started' and v['value'].get('item', {}).get('type') == 'commandExecution' and 'bokkie-offline-window' in v['value']['item'].get('command', '') for e, v in logs_now):
                stop(); record('controller_unavailable_during_worker')
                offline_until = time.monotonic() + 120
                while time.monotonic() < offline_until:
                    new = list(event_logs(state))
                    if any(e['role'] == 'worker' and v['kind'] == 'boundary_reaped' for e, v in new): break
                    time.sleep(1)
                else: raise TimeoutError('worker did not complete/reap while controller unavailable')
                record('worker_completed_offline'); start(); offline = True
            if state['observed_root_state'] == 'completed':
                if not restarted or not offline or not state['repairs'] or not state['acceptance']:
                    raise AssertionError('completion missing required restart/offline/repair evidence')
                if not any(q.get('resolution') for q in state['questions']):
                    raise AssertionError('no retained autonomous question resolution')
                dump(root / 'accepted-outcome.json', state)
                result = subprocess.run(['python3', '-m', 'unittest', 'discover', '-s', 'tests'], cwd=workspace, capture_output=True, text=True)
                (root / 'application-check.txt').write_text(result.stdout + result.stderr)
                if result.returncode: raise AssertionError('accepted fixture fails canonical check')
                record('fixture_accepted', outcome_id=outcome, acceptance=state['acceptance'])
                # A separate fixture outcome exercises genuinely absent publication
                # authority. The requested effect remains prohibited; nothing is published.
                authority = call('/engineering/outcomes', {
                    'command_id': 'fixture-authority-v1',
                    'intent': 'I want this synthetic arithmetic repository published as a new public remote repository. Publication is outside the saved local-only authority. Do not publish or attempt an external mutation: persist one precise new_authority escalation explaining the required decision, and leave the outcome awaiting the operator.'})
                authority_id = authority['outcome_id']
                record('authority_intake_saved', outcome_id=authority_id)
                authority_until = time.monotonic() + 180
                while time.monotonic() < authority_until:
                    authority_state = snapshot(database, authority_id)
                    questions = [q for q in authority_state['questions'] if q['kind'] == 'new_authority' and not q.get('resolution')]
                    if authority_state['observed_root_state'] == 'attention' and len(questions) == 1:
                        dump(root / 'authority-outcome.json', authority_state)
                        record('actionable_authority_escalation', outcome_id=authority_id, question=questions[0])
                        cancellation = {'command_id': 'fixture-authority-cleanup-v1', 'expected': {'outcome_id':authority_id, 'contract_revision':authority_state['contract_revision'], 'state_revision':authority_state['state_revision']}}
                        call('/engineering/outcomes/' + authority_id + '/cancel', cancellation)
                        break
                    time.sleep(1)
                else:
                    outcome = authority_id
                    raise TimeoutError('authority escalation did not become actionable')
                drain = time.monotonic() + 30
                while time.monotonic() < drain:
                    if all(e['cessation_verified'] for oid in [outcome, authority_id] for e in snapshot(database, oid)['executions']):
                        record('all_fixture_boundaries_reconciled')
                        passed = True
                        return
                    time.sleep(.5)
                outcome = authority_id
                raise TimeoutError('final fixture boundaries did not reconcile')
            if state['observed_root_state'] == 'attention' and ('runtime failed' in str(state['root'].get('last_error', '')) or any(q['kind'] in ('new_authority', 'missing_information') and not q.get('resolution') for q in state['questions']) or state['turns_used'] >= profile['max_turns']):
                dump(root / 'attention-outcome.json', state)
                record('unexpected_attention', outcome_id=outcome)
                raise RuntimeError('fixture needs intervention; qualification did not pass')
            time.sleep(1)
        raise TimeoutError('finite fixture deadline exhausted')
    except Exception as error:
        record('qualification_failed', error=str(error))
        raise
    finally:
        if outcome and not passed:
            try:
                if process is None or process.poll() is not None: start()
                state = snapshot(database, outcome)
                if state['observed_root_state'] not in ('completed', 'cancelled'):
                    cancellation = {'command_id': 'fixture-cleanup-cancel-v1', 'expected': {'outcome_id':outcome, 'contract_revision':state['contract_revision'], 'state_revision':state['state_revision']}}
                    call('/engineering/outcomes/' + outcome + '/cancel', cancellation)
                    record('failed_fixture_cancellation_requested')
                    drain = time.monotonic() + 30
                    while time.monotonic() < drain:
                        state = snapshot(database, outcome)
                        if all(e['cessation_verified'] for e in state['executions']): break
                        time.sleep(.5)
                    record('failed_fixture_cleanup_observed', reconciled=all(e['cessation_verified'] for e in state['executions']))
            except Exception as cleanup_error:
                record('cleanup_requires_reconciliation', error=str(cleanup_error))
        stop()
        for stream in logs: stream.close()
        # Retain resources for exact diagnosis. Brokers retain their finite
        # execution deadlines; controller shutdown is never reported as cancellation.

if __name__ == '__main__': main()
