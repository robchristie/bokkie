#!/usr/bin/env python3
"""Submit the retained knowledge-workspace intent to a task-scoped Bokkie runtime.

Opt-in account use. This driver prepares synthetic inputs and observes durable
state; it never answers supervisor questions or supplies subsequent worker briefs.
"""
import argparse
import hashlib
import json
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


def retained_run(root, workspace):
    """Validate a continuation without rewriting intent, source, profile or budget."""
    history = json.loads((root / 'journey.json').read_text())
    intakes = [event for event in history if event['kind'] == 'intent_saved']
    if len(intakes) != 1:
        raise ValueError('resume requires exactly one retained intake')
    intake = intakes[0]
    raw_profile = (root / 'profile.json').read_bytes()
    profile = json.loads(raw_profile)
    if (profile['workspace'] != str(workspace)
            or profile['broker_root'] != str(root / 'brokers')
            or hashlib.sha256(raw_profile).hexdigest() != intake['profile_sha256']):
        raise ValueError('retained workspace or profile identity changed')
    if not workspace.is_dir():
        raise ValueError('retained application workspace is absent')
    outcome = intake['receipt']['outcome_id']
    state = read_snapshot(root / 'supervision.sqlite', outcome)
    intent = json.loads((root / 'submitted-intent.json').read_text())['intent']
    if state['contracts'][0]['contract']['intent'] != intent:
        raise ValueError('retained original intent does not match the outcome')
    if state['observed_root_state'] in ('completed', 'cancelled'):
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
    parser.add_argument('--resume', action='store_true', help='resume the retained outcome after a recorded infrastructure repair; preserve its original deadline')
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[1]
    root, workspace = args.runtime_root.resolve(), args.workspace.resolve()
    profile_path = root / 'profile.json'
    if args.resume:
        profile, history, initial_hashes, outcome = retained_run(root, workspace)
        knowledge = root / 'knowledge'
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
    log_name = f'controller-resume-{time.time_ns()}.log' if args.resume else 'controller.log'
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
            intervention = root / 'infrastructure-intervention.json'
            record('runtime_resumed', outcome_id=outcome, source_revision=subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD'],text=True).strip(), profile_sha256=hashlib.sha256(profile_path.read_bytes()).hexdigest(), intervention_sha256=hashlib.sha256(intervention.read_bytes()).hexdigest() if intervention.exists() else None, operator_url=base+'/ui/', controller_log=log_name)
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
        if outcome and not accepted and process.poll() is None:
            try:
                state=snapshot(outcome)
                if state['observed_root_state'] not in ('completed','cancelled'):
                    call('/engineering/outcomes/'+outcome+'/cancel',{'command_id':'pagefold-cleanup-cancel','expected':{'outcome_id':outcome,'contract_revision':state['contract_revision'],'state_revision':state['state_revision']}})
                    drain=time.monotonic()+30
                    while time.monotonic()<drain and not all(e['cessation_verified'] for e in snapshot(outcome)['executions']):time.sleep(.5)
                    record('cleanup_observed',reconciled=all(e['cessation_verified'] for e in snapshot(outcome)['executions']))
            except Exception as error:record('cleanup_requires_reconciliation',error=str(error))
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:process.wait(timeout=15)
            except subprocess.TimeoutExpired:process.kill();process.wait();record('controller_forced_stop')
        log.close()

if __name__=='__main__':main()
