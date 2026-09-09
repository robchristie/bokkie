#!/usr/bin/env python3
"""Submit the retained knowledge-workspace intent to a task-scoped Bokkie runtime.

Opt-in account use. This driver prepares synthetic inputs and observes durable
state; it never answers supervisor questions or supplies subsequent worker briefs.
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
import struct
import subprocess
import time
import urllib.request
import zlib


def dump(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def hashes(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(root.rglob('*')) if p.is_file()}


def sample_knowledge(root):
    root.mkdir()
    (root / 'attachments').mkdir()
    (root / 'guides').mkdir()
    (root / 'Home.md').write_text('''# A small shared knowledge workspace

Synthetic qualification content — no private documents.

Read the [long reading sample](guides/Reading.md), visit [Unicode notes](Unicode.md),
or inspect a [missing page](Missing.md). A [missing image](attachments/missing.png)
and an [unsupported attachment](attachments/example.dat) must be explained honestly.

![A synthetic colour gradient](attachments/gradient.png)

An [outside path](../outside.md) must not escape the selected directory.
An [external website](https://example.invalid/) must not execute automatically.

<script>document.body.dataset.injected = 'unsafe';</script>
''')
    (root / 'Unicode.md').write_text('# Unicode notes\n\nCafé, naïve, 日本語, Ελληνικά, 😀 and Australian colour spelling.\n\n[Back home](Home.md)\n')
    (root / 'guides/Reading.md').write_text('''# Reading calibration

[Home](../Home.md) · [Unicode](../Unicode.md)

```rust
fn greeting(name: &str) -> String {
    format!("Hello, {name}")
}
```

| Feature | Expected behaviour |
| --- | --- |
| Long page | Readable scrolling |
| Relative link | Resolved within the knowledge directory |
| Source | Preserved byte for byte |

'''+ '\n\n'.join(f'## Section {i}\n\nThis is synthetic paragraph {i}. Search for calibration-needle-47 in section 47. Long lines and wrapping should stay readable.' if i == 47 else f'## Section {i}\n\nThis is synthetic paragraph {i}, with **strong text**, *emphasis*, and `inline code`.\n\n- One reading detail\n- Another reading detail' for i in range(1,61)))
    (root / 'attachments/example.dat').write_bytes(b'Synthetic unsupported attachment.\n')
    width, height = 480, 160
    pixels = b''.join(b'\0' + bytes(v for x in range(width) for v in (35+x*150//width, 80+y*100//height, 150)) for y in range(height))
    def chunk(kind, data):
        return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data) & 0xffffffff)
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!IIBBBBB', width, height, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b'')
    (root / 'attachments/gradient.png').write_bytes(png)



def read_snapshot(database, outcome):
    with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
        row = connection.execute('SELECT v.snapshot_json, b.state FROM engineering_outcomes o JOIN engineering_versions v ON v.outcome_id=o.id AND v.revision=o.state_revision JOIN obligations b ON b.id=o.root_obligation_id WHERE o.id=?', (outcome,)).fetchone()
        if row is None:
            raise ValueError('retained outcome is absent from its database')
        value = json.loads(row[0]); value['observed_root_state'] = row[1]
        return value


def retained_intake(history):
    original = [event for event in history if event['kind'] == 'intent_saved']
    if len(original) != 1:
        raise ValueError('resume requires exactly one retained intake')
    current = original[0]
    seen = {current['receipt']['outcome_id']}
    for event in history:
        if event['kind'] != 'continuation_intake_saved':
            continue
        if event['previous_outcome_id'] != current['receipt']['outcome_id']:
            raise ValueError('continuation provenance does not form one chain')
        if event['receipt']['outcome_id'] in seen:
            raise ValueError('duplicate continuation outcome')
        seen.add(event['receipt']['outcome_id'])
        current = event
    return current


def retained_path(root, intake, field, default):
    name = intake.get(field, default)
    if Path(name).name != name:
        raise ValueError('retained input path must remain in the run directory')
    return root / name


def continuation_profile(profile, state):
    if state['observed_root_state'] != 'cancelled' or state.get('acceptance'):
        raise ValueError('replacement intake requires a cancelled, unaccepted attempt')
    if any(not e['cessation_verified'] for e in state['executions']):
        raise ValueError('previous execution responsibility is not reconciled')
    budget = state['contracts'][-1]['contract']['budget']
    remaining = dict(profile)
    consumed = {'max_turns': state['turns_used'], 'max_packages': len(state['packages']),
                'max_repairs': len(state['repairs']), 'max_recoveries': state['recoveries_used'],
                'max_questions': len(state['questions']),
                'max_checkpoints': sum(len(e['checkpoints']) for e in state['executions'])}
    for field, used in consumed.items():
        remaining[field] = budget[field] - used
        if remaining[field] <= 0:
            raise ValueError(f'original {field} budget exhausted; no reset is permitted')
    remaining['deadline_at'] = budget['deadline']
    remaining['deadline_seconds'] = max(1, int(budget['deadline'] - time.time()))
    if budget['deadline'] <= time.time():
        raise ValueError('original outcome deadline exhausted')
    return remaining


def retained_run(root, workspace, allow_cancelled=False):
    """Validate a continuation without rewriting intent, source, profile or budget."""
    history = json.loads((root / 'journey.json').read_text())
    intake = retained_intake(history)
    raw_profile = retained_path(root, intake, 'profile_file', 'profile.json').read_bytes()
    profile = json.loads(raw_profile)
    if (profile['workspace'] != str(workspace)
            or profile['broker_root'] != str(root / 'brokers')
            or hashlib.sha256(raw_profile).hexdigest() != intake['profile_sha256']):
        raise ValueError('retained workspace or profile identity changed')
    if not workspace.is_dir():
        raise ValueError('retained application workspace is absent')
    outcome = intake['receipt']['outcome_id']
    state = read_snapshot(root / 'supervision.sqlite', outcome)
    intent = json.loads(retained_path(root, intake, 'intent_file', 'submitted-intent.json').read_text())['intent']
    if state['contracts'][0]['contract']['intent'] != intent:
        raise ValueError('retained original intent does not match the outcome')
    if state['observed_root_state'] == 'completed' or (state['observed_root_state'] == 'cancelled' and not allow_cancelled):
        raise ValueError('resume requires a non-terminal outcome')
    if state['contracts'][-1]['contract']['budget']['deadline'] <= time.time():
        raise ValueError('original outcome deadline exhausted; resume cannot reset it')
    before = json.loads((root / 'source-before.json').read_text())
    if hashes(root / 'knowledge') != before:
        raise ValueError('supplied knowledge changed since original intake')
    return profile, history, before, outcome


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-live', action='store_true', required=True)
    parser.add_argument('--runtime-root', type=Path, required=True)
    parser.add_argument('--workspace', type=Path, required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--resume', action='store_true', help='resume the retained non-terminal outcome without a new intake')
    modes.add_argument('--continue-after-interruption', action='store_true', help='replace a previously cancelled qualification attempt using only its remaining budgets')
    parser.add_argument('--intervention', type=Path, help='retained infrastructure intervention record inside the runtime root')
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[1]
    root, workspace = args.runtime_root.resolve(), args.workspace.resolve()
    profile_path = root / 'profile.json'
    continuing = args.continue_after_interruption
    retained = args.resume or continuing
    intervention = args.intervention.resolve() if args.intervention else root / 'infrastructure-intervention.json'
    if args.intervention and (not intervention.is_file() or not intervention.is_relative_to(root)):
        parser.error('intervention must be an existing record inside the runtime root')
    previous_outcome = None
    if retained:
        profile, history, initial_hashes, outcome = retained_run(root, workspace, allow_cancelled=continuing)
        intake = retained_intake(history)
        profile_path = retained_path(root, intake, 'profile_file', 'profile.json')
        knowledge = root / 'knowledge'
        if continuing:
            if not intervention.is_file():
                parser.error('continuation requires a retained infrastructure intervention')
            previous_outcome = outcome
            previous_state = read_snapshot(root / 'supervision.sqlite', outcome)
            profile = continuation_profile(profile, previous_state)
            profile_path = root / f'profile-continuation-{time.time_ns()}.json'
            dump(profile_path, profile)
            outcome = None
    else:
        if root.exists() or workspace.exists():
            parser.error('both runtime root and application workspace must be new')
        root.mkdir(mode=0o700); workspace.mkdir()
        (workspace / '.gitignore').write_text('/target/\n/.runtime-scratch/\n/.qualification/\n/node_modules/\n')
        subprocess.run(['git', 'init', '-q', str(workspace)], check=True)
        subprocess.run(['git', '-C', str(workspace), 'add', '.gitignore'], check=True)
        subprocess.run(['git', '-C', str(workspace), 'commit', '-qm', 'Initialise local application workspace'], check=True)
        scratch = workspace / '.runtime-scratch'; scratch.mkdir(mode=0o700)
        knowledge = root / 'knowledge'; sample_knowledge(knowledge)
        (root / 'outside.md').write_text('# Outside the selected directory\nMust not be opened through a relative escape.\n')
        initial_hashes = hashes(knowledge); dump(root / 'source-before.json', initial_hashes)
        brokers = root / 'brokers'; brokers.mkdir(mode=0o700)
        profile = json.loads((source / 'instructions/profiles/engineering-local.json').read_text())
        profile.update(workspace=str(workspace), broker_root=str(brokers), broker=str(source / 'tools/engineering-runtime/broker.py'), codex=str(Path(shutil.which('codex')).resolve()), supervisor_instructions=str(source / 'instructions/engineering-supervisor.md'), worker_instructions=str(source / 'instructions/engineering-worker.md'), worker_scratch=str(scratch), worker_network_access=True)
        dump(root / 'profile.json', profile)
        history = []
        outcome = None
    database = root / 'supervision.sqlite'
    with socket.socket() as reserve:
        reserve.bind(('127.0.0.1', 0)); port = reserve.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    def record(kind, **values):
        history.append(dict(kind=kind, at=time.time(), **values)); dump(root / 'journey.json', history)
        print(json.dumps(dict(kind=kind, **values)), flush=True)
    def call(path, body=None):
        headers = {}
        if body is not None:
            bootstrap = json.load(urllib.request.urlopen(base + '/bootstrap', timeout=3))
            headers = {'Content-Type':'application/json','X-Bokkie-Mutation-Token':bootstrap['mutation_token']}
        return json.load(urllib.request.urlopen(urllib.request.Request(base+path, data=None if body is None else json.dumps(body).encode(), headers=headers), timeout=10))
    def snapshot(outcome):
        return read_snapshot(database, outcome)
    log_name = f'controller-resume-{time.time_ns()}.log' if retained else 'controller.log'
    log = (root / log_name).open('x')
    process = subprocess.Popen([str(source/'target/debug/bokkie'), '--database', str(database), 'serve', '--bind', f'127.0.0.1:{port}', '--poll-ms', '500', '--engineering-profile', str(profile_path), '--ui-dir', str(source/'apps/bokkie-attention-ui/web')], stdout=log, stderr=log, start_new_session=True)
    accepted = False
    try:
        until = time.monotonic()+20
        while time.monotonic()<until:
            if process.poll() is not None: raise RuntimeError('controller stopped; inspect its retained log')
            try: call('/health'); break
            except OSError: time.sleep(.2)
        else: raise TimeoutError('controller startup')
        if args.resume:
            record('runtime_resumed', outcome_id=outcome, source_revision=subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD'],text=True).strip(), profile_sha256=hashlib.sha256(profile_path.read_bytes()).hexdigest(), intervention_sha256=hashlib.sha256(intervention.read_bytes()).hexdigest() if intervention.exists() else None, operator_url=base+'/ui/', controller_log=log_name)
        elif continuing:
            intent = json.loads((root / 'submitted-intent.json').read_text())['intent']
            intent += f'\nContinuation context: the repository now contains retained work from interrupted outcome {previous_outcome}, which did not reach final product acceptance. Continue the same milestone from the existing repository and its evidence; preserve useful work. The infrastructure remedy is recorded at {intervention}. No acceptance criterion or authority boundary is waived. This intake uses only the preceding attempt’s remaining execution, package, repair and recovery budgets and its original absolute deadline.\n'
            intent_path = root / f'submitted-intent-continuation-{time.time_ns()}.json'
            dump(intent_path, {'intent': intent})
            receipt = call('/engineering/outcomes', {'command_id': 'pagefold-continuation-' + previous_outcome, 'intent': intent})
            outcome = receipt['outcome_id']
            record('continuation_intake_saved', receipt=receipt, previous_outcome_id=previous_outcome, source_revision=subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD'],text=True).strip(), profile_file=profile_path.name, intent_file=intent_path.name, profile_sha256=hashlib.sha256(profile_path.read_bytes()).hexdigest(), intervention_sha256=hashlib.sha256(intervention.read_bytes()).hexdigest(), remaining_budgets={key:profile[key] for key in ('max_turns','max_packages','max_repairs','max_recoveries','deadline_at')}, operator_url=base+'/ui/')
        else:
            intent = (source/'docs/supervision-evidence/knowledge-workspace-intent.md').read_text().split('## Qualification ownership')[0]
            intent += f'\nThe empty local application repository is {workspace}. Prepared synthetic source knowledge is at {knowledge}; use it for reading calibration without changing those supplied files. Tests may create their own isolated copies to simulate external changes. Store derived state separately. Polyorama is available at /nvme/development/polyorama. The provisional name is Pagefold.\n'
            dump(root/'submitted-intent.json', {'intent':intent})
            receipt = call('/engineering/outcomes', {'command_id':'pagefold-milestone-1', 'intent':intent})
            outcome = receipt['outcome_id']
            record('intent_saved', receipt=receipt, source_revision=subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD'],text=True).strip(), profile_sha256=hashlib.sha256(profile_path.read_bytes()).hexdigest(), operator_url=base+'/ui/')
        deadline = snapshot(outcome)['contracts'][-1]['contract']['budget']['deadline']
        until = time.monotonic() + max(0, deadline - time.time())
        last = None
        while time.monotonic()<until:
            if process.poll() is not None: raise RuntimeError('controller stopped; retain external execution responsibility')
            state = snapshot(outcome)
            status = (state['observed_root_state'],len(state['packages']),len(state['questions']),len(state['submissions']),len(state['repairs']))
            if status != last:
                record('durable_progress', state=status[0],packages=status[1],questions=status[2],submissions=status[3],repairs=status[4]);last=status
            if state['observed_root_state']=='completed':
                dump(root/'accepted-outcome.json',state)
                after=hashes(knowledge);dump(root/'source-after.json',after)
                if after != initial_hashes: raise AssertionError('supplied knowledge source was modified')
                if not state['acceptance']: raise AssertionError('completion lacks acceptance')
                record('product_accepted',outcome_id=outcome,acceptance=state['acceptance'],source_unchanged=True)
                drain=time.monotonic()+30
                while time.monotonic()<drain:
                    if all(e['cessation_verified'] for e in snapshot(outcome)['executions']):
                        accepted=True;record('all_boundaries_reconciled');return
                    time.sleep(.5)
                raise TimeoutError('accepted supervisor boundary did not reconcile')
            if state['observed_root_state']=='attention' and (any(q['kind'] in ('new_authority','missing_information') and not q.get('resolution') for q in state['questions']) or 'runtime failed' in str(state['root'].get('last_error','')) or state['turns_used']>=profile['max_turns']):
                dump(root/'attention-outcome.json',state)
                raise RuntimeError('supervised milestone requires a recorded intervention; not qualified')
            time.sleep(2)
        raise TimeoutError('finite supervision deadline exhausted')
    except BaseException as error:
        record('qualification_interrupted',error=str(error));raise
    finally:
        # Stopping a qualification observer must not cancel its product outcome.
        # Stop scheduling first, then request bounded external cessation. The next
        # runtime reconciles broker proof through the authoritative Store path.
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:process.wait(timeout=15)
            except subprocess.TimeoutExpired:process.kill();process.wait();record('controller_forced_stop')
        if outcome and not accepted:
            try:
                state = snapshot(outcome)
                pending = [e['id'] for e in state['executions'] if not e['cessation_verified']]
                for execution in pending:
                    directory = Path(profile['broker_root']) / execution
                    if not directory.is_dir():
                        continue
                    temporary = directory / f'.observer-stop-{time.time_ns()}'
                    with temporary.open('x') as handle:
                        json.dump({'reason':'qualification observer interrupted; preserve outcome'}, handle)
                        handle.flush(); os.fsync(handle.fileno())
                    temporary.replace(directory / 'cancel.json')
                record('interruption_retained', outcome_id=outcome,
                       state=state['observed_root_state'],
                       executions_requiring_reconciliation=pending,
                       outcome_cancelled_by_observer=False)
            except Exception as error:record('cleanup_requires_reconciliation',error=str(error))
        log.close()

if __name__=='__main__':main()
