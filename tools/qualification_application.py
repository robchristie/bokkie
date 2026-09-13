"""Read-only application closeout against authoritative Store and retained runtime evidence.

This consumes Store acceptance, rather than inventing another acceptance transition.
The evidence JSON selects exact revisions; it cannot supply review/delivery verdicts.
"""
from contextlib import ExitStack, contextmanager
import fcntl
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sqlite3


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_json(path):
    return json.loads(path.read_text())


def blob(brokers, digest):
    require(isinstance(digest, str) and re.fullmatch('[0-9a-f]{64}', digest), 'invalid retained evidence digest')
    data = (brokers / 'blobs' / digest).read_bytes()
    require(hashlib.sha256(data).hexdigest() == digest, 'retained evidence digest mismatch')
    return data


def lock(stack, path):
    # Existing production lock inode only: never create a substitute lock.
    handle = stack.enter_context(path.open('rb'))
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        raise ValueError('controller or broker still owns execution responsibility') from error


def observe_delivery(profile, number):
    """Only the existing host adapter may supply missing legacy delivery facts."""
    path = Path(__file__).parent / 'engineering-runtime/github_delivery.py'
    spec = importlib.util.spec_from_file_location('campaign_delivery_adapter', path)
    adapter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(adapter)
    config = {key: profile['github_delivery'][key] for key in ('repo', 'base', 'branch')}
    result = adapter.execute(config, profile['workspace'], 'status', {'pr': number})
    return {'adapter_sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'operation': 'status',
            'result': result}


def verify_ci(receipt, head):
    require(receipt.get('state') == 'success' and receipt.get('head') == head,
            'delivery lacks successful exact-revision CI')
    run = receipt.get('run') or {}
    require(type(run.get('id')) is int and run['id'] > 0 and
            type(run.get('attempt')) is int and run['attempt'] > 0 and run.get('head') == head and
            run.get('status') == 'completed' and run.get('conclusion') == 'success' and run.get('url') and
            run in receipt.get('runs', []), 'CI run identity or attempt is missing')
    jobs = receipt.get('jobs') or []
    require(bool(jobs) and all(j.get('run_id') == run['id'] and j.get('attempt') == run['attempt'] and
            j.get('head') == head and type(j.get('id')) is int and j['id'] > 0 and j.get('url') for j in jobs),
            'CI jobs differ from exact run/head/attempt')
    required = [j for j in jobs if j.get('name') == 'Fresh-checkout verification']
    require(len(required) == 1 and required[0].get('status') == 'completed' and
            required[0].get('conclusion') == 'success', 'required CI job did not pass')


def verify_acceptance(state, expected, brokers, profile):
    revision = state['contract_revision']
    require(state['id'] == expected['outcome_id'] and revision == expected['contract_revision'] and
            state['state_revision'] == expected['state_revision'], 'Store outcome or revision differs from requested closure')
    acceptance = state.get('acceptance')
    require(isinstance(acceptance, dict) and acceptance['contract_revision'] == revision,
            'exact Store acceptance is missing or stale')
    contracts = [c for c in state['contracts'] if c['revision'] == revision]
    require(len(contracts) == 1, 'exact accepted contract is missing')
    criteria = {c['id'] for c in contracts[0]['contract']['criteria']}
    require(bool(criteria), 'application contract has no acceptance criteria')
    executions = {e['id']: e for e in state['executions']}
    require(acceptance['assessor_execution_id'] in executions and
            executions[acceptance['assessor_execution_id']]['role'] == 'supervisor', 'acceptance assessor is not retained')
    assessments = {a['id']: a for a in state['assessments']}
    submissions = {s['id']: s for s in state['submissions']}
    covered, accepted_artefacts, reviews = set(), [], []
    require(bool(acceptance['assessment_ids']), 'acceptance has no assessments')
    for identity in acceptance['assessment_ids']:
        assessment = assessments[identity]
        verdict = assessment['input']
        require(assessment['contract_revision'] == revision and verdict['verdict'] == 'accept' and
                not verdict['unmet_criteria'], 'accepted assessment is stale or unsuccessful')
        submission = submissions[verdict['submission_id']]
        require(submission['contract_revision'] == revision and submission['digest'] == verdict['submission_digest'],
                'accepted result binding differs from assessment')
        require(submission['execution_id'] in executions and
                executions[submission['execution_id']]['role'] == 'worker', 'result worker is not retained')
        artefacts = submission['input']['artefacts']
        accepted_artefacts.extend(a for a in artefacts if a not in accepted_artefacts)
        for evidence in submission['input']['evidence']:
            require(evidence['exit_code'] == 0 and evidence['artefact'] in artefacts,
                    'accepted criterion has unsuccessful or unbound validation')
            blob(brokers, evidence['command_digest'])
            blob(brokers, evidence['output_digest'])
            covered.add(evidence['criterion_id'])
        reviews.append((verdict['review'], artefacts))
    require(criteria <= covered, 'accepted results do not cover the exact contract')
    reviews.append((acceptance['review'], accepted_artefacts))
    for review, covered_artefacts in reviews:
        digest = review['evidence_digest']
        report = json.loads(blob(brokers, digest))
        require(read_json(brokers / 'reviews' / (digest + '.json')) == review,
                'independent review registration is missing or differs')
        require(review['reviewer_identity'] and review['reviewer_identity'] not in executions,
                'review lacks a separate retained reviewer identity')
        require(report['verdict'] == 'pass' and report['artefacts'] == review['artefacts'] and
                all(a in review['artefacts'] for a in covered_artefacts),
                'independent review does not pass for every accepted artefact')
    deliveries = []
    for operation in state['delivery_operations']:
        require(operation.get('evidence_digest'), 'delivery operation remains unresolved')
        result = json.loads(blob(brokers, operation['evidence_digest']))
        if operation['operation'] != 'merge' or operation['contract_revision'] != revision:
            continue
        request = json.loads(operation['arguments_json'])
        require(operation['post_merge_verified'] is True and result.get('merged') is True and
                result.get('post_merge_verified') is True and result.get('merge_commit'),
                'delivery lacks retained successful post-merge verification')
        require(request['arguments']['head'] == result['head'] and request['arguments']['pr'] == result['pr'] and
                any(a['kind'] == 'git' and a['commit'] == result['head'] for a in accepted_artefacts),
                'delivery head differs from accepted and reviewed result')
        require(any(request['review'] == review for review, _ in reviews),
                'delivery review differs from accepted review')
        scope = profile.get('github_delivery') or {}
        require(scope.get('authority'), 'retained profile has no delivery authority')
        supplemental = None
        keys = {'head_tree', 'merge_tree', 'tree_equal', 'pre_merge_ci', 'post_merge_ci'}
        if not keys.intersection(result):
            supplemental = observe_delivery(profile, result['pr'])
            observed = supplemental['result']
            require(all(observed.get(k) == result.get(k) for k in ('repo', 'base', 'branch', 'pr', 'head', 'merge_commit')),
                    'supplemental delivery observation differs from retained delivery')
        else:
            observed = result
        require(observed.get('merged') is True and observed.get('post_merge_verified') is True and
                all(observed.get(k) == scope.get(k) for k in ('repo', 'base', 'branch')),
                'delivery differs from authorised repository/base/branch')
        require(observed.get('tree_equal') is True and observed.get('head_tree') and
                observed['head_tree'] == observed.get('merge_tree') and any(
                    a['kind'] == 'git' and a['commit'] == observed['head'] and
                    a['tree'] == observed['head_tree'] and a['repository'] == profile['workspace']
                    for a in accepted_artefacts), 'reviewed, accepted and landed trees differ')
        verify_ci(observed.get('pre_merge_ci') or {}, observed['head'])
        verify_ci(observed.get('post_merge_ci') or {}, observed['merge_commit'])
        deliveries.append({'operation_id': operation['id'], 'evidence_digest': operation['evidence_digest'],
                           'head': result['head'], 'merge_commit': result['merge_commit'],
                           'repository': result['repo'], 'pr': result['pr'],
                           'head_tree': observed['head_tree'], 'merge_tree': observed['merge_tree'],
                           'pre_merge_ci': observed['pre_merge_ci'], 'post_merge_ci': observed['post_merge_ci'],
                           'supplemental_observation': supplemental})
    require(bool(deliveries), 'application delivery has no verified merge')
    return {'outcome_id': state['id'], 'state_revision': state['state_revision'],
            'contract_revision': revision, 'acceptance': acceptance,
            'submission_ids': [assessments[a]['input']['submission_id'] for a in acceptance['assessment_ids']],
            'deliveries': deliveries}


@contextmanager
def verify_application(campaign, attempts, evidence):
    require(isinstance(evidence, dict) and isinstance(evidence.get('attempts'), list),
            'closure requires exact attempt/outcome/revision selectors')
    selectors = evidence['attempts']
    require(len(selectors) == len(attempts) and
            {x['attempt_id'] for x in selectors} == {a['id'] for a in attempts},
            'closure must account for every attempt exactly once')
    with ExitStack() as stack:
        receipts, roots = [], set()
        for attempt in attempts:
            expected = next(s for s in selectors if s['attempt_id'] == attempt['id'])
            saved = json.loads(attempt['evidence'] or '{}')
            require(saved.get('root'), 'attempt has no retained runtime root')
            root = Path(saved['root']).resolve()
            require(root not in roots, 'attempts cannot share an ambiguous runtime root')
            roots.add(root)
            binding = read_json(root / 'campaign-binding.json')
            require(binding == {'campaign': campaign.campaign_id, 'registry': str(campaign.path.resolve()),
                                'root': str(root), 'attempt_id': attempt['id']}, 'runtime campaign binding differs')
            profile = read_json(root / 'profile.json')
            brokers = Path(profile['broker_root']).resolve()
            require(brokers == root / 'brokers', 'broker evidence is outside the retained runtime root')
            lock(stack, brokers / 'controller.lock')
            database = root / 'engineering.sqlite'
            connection = sqlite3.connect(database.as_uri() + '?mode=ro', uri=True)
            stack.callback(connection.close)
            connection.execute('BEGIN')
            require(connection.execute('SELECT COUNT(*) FROM engineering_writers').fetchone()[0] == 0,
                    'Store retains writer responsibility')
            require(connection.execute("SELECT COUNT(*) FROM obligations WHERE state NOT IN ('completed','cancelled') OR lease_token IS NOT NULL").fetchone()[0] == 0,
                    'Store retains non-terminal obligations or leases')
            rows = connection.execute('SELECT o.id,o.state_revision,v.snapshot_json,b.state FROM engineering_outcomes o '
                'JOIN engineering_versions v ON v.outcome_id=o.id AND v.revision=o.state_revision '
                'JOIN obligations b ON b.id=o.root_obligation_id').fetchall()
            require(bool(rows) and len(rows) == connection.execute('SELECT COUNT(*) FROM engineering_outcomes').fetchone()[0],
                    'retained Store has missing outcome snapshots')
            states = []
            for identity, revision, raw, root_state in rows:
                state = json.loads(raw)
                require(state['id'] == identity and state['state_revision'] == revision,
                        'Store snapshot binding differs')
                require(all(e['cessation_verified'] is True for e in state['executions']),
                        'Store execution cessation remains unresolved')
                for execution in state['executions']:
                    lock(stack, brokers / execution['id'] / 'owner.lock')
                for operation in state['delivery_operations']:
                    require(operation.get('evidence_digest'), 'Store retains unresolved delivery responsibility')
                    blob(brokers, operation['evidence_digest'])
                state['observed_root_state'] = root_state
                states.append(state)
            if attempt['passed']:
                require(attempt['stage'] == 'application_dogfood' and saved.get('outcome_id') == expected['outcome_id'],
                        'passing application attempt has no matching retained outcome')
                matching = [s for s in states if s['id'] == expected['outcome_id']]
                require(len(matching) == 1 and matching[0]['observed_root_state'] == 'completed',
                        'application outcome is not completed')
                receipt = verify_acceptance(matching[0], expected, brokers, profile)
                require(saved.get('acceptance') == matching[0]['acceptance'],
                        'attempt acceptance differs from exact Store acceptance')
            else:
                receipt = {'result': 'reconciled_failure'}
            receipts.append({'attempt_id': attempt['id'], 'root': str(root),
                'database': str(database), 'snapshots_sha256': hashlib.sha256(
                    json.dumps(states, sort_keys=True).encode()).hexdigest(),
                'controller_and_broker_locks_verified': True, **receipt})
        yield {'policy': 'store_application_acceptance_v1', 'attempts': receipts,
               'qualification_claim': 'application_delivery_only'}
