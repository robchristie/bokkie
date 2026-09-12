#!/usr/bin/env python3
"""Runtime qualification without model turns; retain identities, never account content."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location('preflight_broker', Path(__file__).with_name('broker.py'))
broker = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(broker)

# Each probe declares its question and smallest production-path regression set.
# The acceptance condition is every named test actually running and passing.
PROBES = {
    'github_delivery': {
        'question': 'Do fixed delivery scope, CI gates, replay and credential isolation hold without model turns?',
        'python_modules': ['test_github_delivery', 'test_engineering_runtime'],
        'rust': ['github_profile_is_explicit_and_local_identity_is_unchanged',
                 'github_host_branch_commit_and_receipt_replay_use_real_local_git_only',
                 'github_delivery_replays_persisted_result_before_uncertain_commit_readback',
                 'github_tool_refuses_stale_and_unattributed_merge_before_host_call',
                 'store::engineering::tests::github_delivery_requires_trusted_adapter_grant_and_current_execution',
                 'store::engineering::tests::github_delivery_replays_intent_and_result_after_reopen',
                 'store::engineering::tests::github_delivery_pending_intent_blocks_new_workers_and_acceptance',
                 'store::engineering::tests::github_delivery_cancellation_retains_pending_intent_until_reconciled',
                 'store::engineering::tests::github_delivery_merge_needs_supervisor_ceased_workers_and_verified_post_merge']},
    'qualification_driver': {
        'question': 'Do deadline and acceptance guards reject incomplete fixture observations and orphaned dispatch?',
        'python_modules': ['test_qualification_runner'],
        'rust': ['dispatch_limits_honour_package_deadline_and_original_claim']},
    'campaign_admission': {
        'question': 'Do reservation, crash/restart, changed-input and finite allowance gates hold?',
        'python_modules': ['test_qualification_campaign']},
    'config_schema': {
        'question': 'Do installed-schema aliases and task overrides preserve bounded capabilities?',
        'python': ['test_installed_schema_projects_canonical_concurrency_field',
                   'test_effective_capabilities_reject_silent_reductions_and_redact_secrets',
                   'test_mcp_overrides_preserve_inherited_transport_and_never_copy_credentials'],
        'rust': ['profile_rejects_infinite_bounds_and_overlapping_storage']},
    'child_review': {
        'question': 'Does child-journal review require an actual linked successful final turn?',
        'rust': ['actual_child_review_is_discoverable_and_retains_exact_turn_provenance',
                 'child_review_rejects_missing_parent_failed_turn_and_mismatched_final',
                 'child_review_discovery_does_not_grant_root_tools_to_the_child']},
    'submission_binding': {
        'question': 'Are invented validations and submissions against changed sources rejected?',
        'rust': ['validation_rejects_checks_run_against_an_older_source',
                 'invented_validation_and_changed_artefacts_are_rejected',
                 'store::engineering::tests::submission_preflight_allows_distinct_evidence_per_criterion_and_rejects_duplicates']},
    'encoded_paging': {
        'question': 'Do encoded and binary pages retain exact bytes without blocking cessation?',
        'rust': ['large_evidence_pages_preserve_exact_bytes_and_bound_encoded_replies',
                 'binary_evidence_pages_are_lossless_and_explicitly_encoded',
                 'expanded_reply_and_bad_request_do_not_block_later_requests_or_reaping']},
    'source_boundaries': {
        'question': 'Is full selected source captured across journal-size and resource boundaries?',
        'python': ['test_complete_git_source_capture_above_journal_limit_is_exact',
                   'test_source_capture_reports_resource_failure_without_partial_binding',
                   'test_source_observation_rejects_fifo_without_waiting_for_a_writer'],
        'rust': ['source_capture_failures_and_available_bindings_are_exposed_to_tools']},
}


def file_identity(path):
    path = Path(path).resolve(strict=True)
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    return {'path': str(path), 'sha256': digest, 'byte_length': path.stat().st_size}


def runtime_identity():
    paths = ['tools/engineering-runtime/github_delivery.py', 'tools/tests/test_github_delivery.py',
             'instructions/engineering-github-worker.md', 'instructions/engineering-github-supervisor.md',
             'tools/engineering-runtime/broker.py', 'tools/engineering-runtime/preflight.py',
             'src/engineering_runtime.rs', 'src/engineering.rs', 'src/store/engineering.rs',
             'Cargo.lock', 'tools/tests/test_engineering_runtime.py',
             'tools/qualify-engineering.py', 'tools/qualification_campaign.py',
             'tools/qualification_observations.py', 'tools/tests/test_qualification_runner.py',
             'tools/tests/test_qualification_campaign.py', 'tools/tests/test_engineering_preflight.py',
             'tools/tests/test_qualification_observations.py']
    return broker.digest({name: file_identity(ROOT / name)['sha256'] for name in paths})


def _run(command, cwd=ROOT, timeout=600):
    result = subprocess.run(command, cwd=cwd, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    return result, {'command': command, 'exit_code': result.returncode,
                    'output_sha256': hashlib.sha256(result.stdout).hexdigest(),
                    'output_bytes': len(result.stdout),
                    'test_output': result.stdout.decode('utf-8', errors='replace')[-65536:],
                    'output_partial': len(result.stdout) > 65536}


def run_probe(name, profile_path=None):
    """Exercise existing production validators through exact named regression tests."""
    definition = PROBES[name]
    checks = []
    for module in definition.get('python_modules', []):
        command = [sys.executable, '-m', 'unittest', module, '-v']
        result, receipt = _run(command, ROOT / 'tools/tests')
        if result.returncode or b'Ran 0 tests' in result.stdout or b'Ran ' not in result.stdout:
            raise ValueError('probe failed: ' + name + '/' + module)
        checks.append(receipt)
    for test in definition.get('python', []):
        command = [sys.executable, '-m', 'unittest', 'test_engineering_runtime.BrokerTests.' + test, '-v']
        result, receipt = _run(command, ROOT / 'tools/tests')
        if result.returncode or b'Ran 1 test' not in result.stdout:
            raise ValueError('probe failed: ' + name + '/' + test + ' (output sha256 ' + receipt['output_sha256'] + ')')
        checks.append(receipt)
    for test in definition.get('rust', []):
        command = ['cargo', 'test', '--locked', '--lib', test if '::' in test else 'engineering_runtime::tests::' + test,
                   '--', '--exact', '--nocapture']
        result, receipt = _run(command)
        if result.returncode or b'1 passed; 0 failed' not in result.stdout:
            raise ValueError('probe failed or test missing: ' + name + '/' + test + ' (output sha256 ' + receipt['output_sha256'] + ')')
        checks.append(receipt)
    return {'schema_version': 1, 'name': name, 'question': definition['question'],
            'evidence_owner': 'tools/engineering-runtime/preflight.py',
            'exit_condition': 'Every exact named production-path regression ran and passed',
            'runtime_identity': runtime_identity(), 'passed': True, 'checks': checks,
            'model_turns': 0}


class PreflightBroker(broker.Broker):
    """Use production transport, with an explicit no-turn guard and compact events."""
    def __init__(self, root):
        super().__init__(root)
        self.facts = {}
        self.methods = []

    def send(self, value):
        method = value.get('method')
        if method and method not in ('initialize', 'initialized', 'config/read', 'thread/start', 'skills/list'):
            raise ValueError('preflight cannot start a model turn or use another RPC')
        return super().send(value)

    def rpc(self, method, params):
        self.methods.append(method)
        return super().rpc(method, params)

    def event(self, kind, value, terminal=False):
        # Raw RPC responses can carry account configuration or skill content.
        # All protocol data stays transient; only whitelisted projections survive.
        if kind in ('effective_capabilities', 'effective_settings'):
            self.facts[kind] = value

    def observe(self, message):
        if 'id' in message and not message.get('method'):
            self.responses[message['id']] = message
        elif 'id' in message:
            raise ValueError('unexpected server request during no-model preflight')


def _profile(profile_path, receipt_dir):
    # The production Rust loader owns validation and defaulting, including paths,
    # tool allowlists and finite budgets. Validate creates no SQLite database.
    command = ['cargo', 'run', '--quiet', '--locked', '--bin', 'bokkie-engineering', '--',
               '--db', str(receipt_dir / 'unused.sqlite'), '--profile', str(profile_path), 'preflight-parameters']
    result = subprocess.run(command, cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=600)
    if result.returncode:
        raise ValueError('production profile validation failed (diagnostic sha256 ' +
                         hashlib.sha256(result.stderr).hexdigest() + ')')
    return json.loads(result.stdout)


def _session(profile, role, root):
    params = {**profile['_thread_parameters'][role], 'ephemeral': True}
    manifest = {**{k: v for k, v in profile.items() if k != '_thread_parameters'},
                'role': role, 'turn_seconds': 60, 'deadline': int(time.time()) + 60,
                'execution_id': 'no-model-preflight', 'dispatch_key': 'no-model-preflight',
                'thread_params': params, 'prompt': ''}
    root.mkdir(mode=0o700)
    (root / 'replies').mkdir(mode=0o700)
    broker.atomic(root / 'dispatch.json', manifest)
    peer = PreflightBroker(root)
    try:
        # Match the production namespace and worker lock-registry read-only mount;
        # no writer reservation is acquired because no turn can execute.
        broker.workspace_lock_root().mkdir(mode=0o700, parents=True, exist_ok=True)
        environment = peer.environment()
        peer.child = subprocess.Popen(peer.command(), cwd=profile['workspace'], env=environment,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, start_new_session=True)
        for stream in (peer.child.stdin, peer.child.stdout, peer.child.stderr):
            os.set_blocking(stream.fileno(), False)
        peer.selector.register(peer.child.stdout, selectors.EVENT_READ)
        peer.selector.register(peer.child.stderr, selectors.EVENT_READ)
        peer.rpc('initialize', {'clientInfo': {'name': 'bokkie_preflight', 'version': '1'},
                                'capabilities': {'experimentalApi': True}})
        peer.send({'method': 'initialized', 'params': {}})
        config = peer.rpc('config/read', {'cwd': profile['workspace'], 'includeLayers': False})
        peer.verify_capability_config(config['config'])
        started = peer.rpc('thread/start', manifest['thread_params'])
        peer.verify_thread_settings(started)
        identities = [file_identity(path) for path in started.get('instructionSources', [])]
        skills = peer.rpc('skills/list', {'cwds': [profile['workspace']], 'forceReload': True})
        skill_names = []
        for entry in skills.get('data', []):
            if entry.get('errors'):
                raise ValueError('installed skill discovery reported errors')
            skill_names += [skill['name'] for skill in entry.get('skills', []) if skill.get('enabled', True)]
            identities += [file_identity(skill['path']) for skill in entry.get('skills', [])
                           if skill.get('enabled', True)]
        if profile.get('github_delivery') is not None:
            if not any(name.split(':')[-1] == 'land-reviewed-pr' for name in skill_names):
                raise ValueError('Pagefold delivery requires the installed land-reviewed-pr skill')
            peer.facts['enabled_skill_names'] = sorted(skill_names)
        # Relocating an identical fixture must not disguise an unchanged failure.
        for item in identities:
            guidance_path = Path(item['path'])
            if guidance_path.is_relative_to(Path(profile['workspace'])):
                item['path'] = 'workspace/' + str(guidance_path.relative_to(profile['workspace']))
        peer.facts['guidance_identity'] = broker.digest(sorted(identities, key=lambda item: item['path']))
        peer.facts['guidance_count'] = len(identities)
        peer.facts['developer_instructions_sha256'] = hashlib.sha256(params.get('developerInstructions', '').encode()).hexdigest()
        peer.facts['dynamic_tools_schema_sha256'] = broker.digest(params.get('dynamicTools', []))
        peer.facts['rpc_methods'] = peer.methods
        return peer.facts
    finally:
        if peer.child is not None:
            if peer.child.poll() is None:
                os.killpg(peer.child.pid, signal.SIGKILL)
            peer.child.wait(timeout=10)
            for stream in (peer.child.stdin, peer.child.stdout, peer.child.stderr):
                stream.close()
        peer.selector.close()


def run_preflight(profile_path, receipt_dir):
    """Validate a local installed runtime and source selection without model turns."""
    profile_path = Path(profile_path).resolve(strict=True)
    receipt_dir = Path(receipt_dir).resolve()
    # Check storage separation before writing even temporary preflight material.
    raw = broker.read(profile_path)
    workspace = Path(raw['workspace']).resolve(strict=True)
    if receipt_dir.is_relative_to(workspace):
        raise ValueError('preflight receipts must be outside the worker workspace')
    receipt_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    before = file_identity(profile_path)
    parameters = _profile(profile_path, receipt_dir)
    profile = parameters['profile']
    github_result = None
    if profile.get('github_delivery') is not None:
        Path(profile['broker_root']).mkdir(mode=0o700, parents=True, exist_ok=True)
        spec = importlib.util.spec_from_file_location('github_delivery', Path(__file__).with_name('github_delivery.py'))
        github = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(github)
        scope = {k: profile['github_delivery'][k] for k in ('repo', 'base', 'branch')}
        github_result = github.preflight(scope, profile['workspace'])
    profile['_thread_parameters'] = {role: parameters[role] for role in ('supervisor', 'worker')}
    with tempfile.TemporaryDirectory(prefix='preflight-', dir=receipt_dir) as temporary:
        root = Path(temporary)
        sessions = {role: _session(profile, role, root / role) for role in ('supervisor', 'worker')}
        # Same file-selection and budget rules as production command observations.
        capture = object.__new__(broker.Broker)
        capture.manifest = {'workspace': profile['workspace']}
        source = capture.source_snapshot()
    if 'unavailable' in source:
        raise ValueError('workspace source selection unavailable: ' + source['unavailable']['code'])
    if before != file_identity(profile_path):
        raise ValueError('profile changed during preflight')
    # Workspace settings are intentionally absent from the environment component.
    # Guidance identities stay here: local guidance changes affect compatibility.
    environment_sessions = json.loads(json.dumps(sessions))
    for facts in environment_sessions.values():
        facts['effective_settings'].pop('instructionSources', None)
        sandbox = facts['effective_settings'].get('sandbox', {})
        sandbox.pop('writableRoots', None)
        facts['effective_capabilities'].pop('worker_scratch', None)
    version = subprocess.run([profile['codex'], '--version'], stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True).stdout
    if len(version) > 1024:
        raise ValueError('Codex version exceeded bound')
    environment = {'implementation_identity': broker.digest({name: file_identity(Path(__file__).with_name(name))['sha256']
                        for name in ('broker.py', 'preflight.py')}),
                   'codex_version': version.decode('utf-8').strip(),
                   'executables': {key: file_identity(profile[key]) for key in ('codex', 'bwrap', 'broker')},
                   'sessions': environment_sessions}
    if github_result is not None:
        environment['github_delivery'] = github_result
    selection = {'workspace': profile['workspace'], 'worker_scratch': profile.get('worker_scratch'),
                 'source_identity': broker.digest(source), 'capture': source['capture'],
                 'commit': source.get('commit'), 'tree': source.get('tree'), 'clean': source.get('clean')}
    result = {'schema_version': 1, 'passed': True, 'model_turns': 0,
              'profile_sha256': before['sha256'], 'runtime_identity': runtime_identity(), 'environment': environment, 'workspace': selection,
              'environment_identity': broker.digest(environment),
              'workspace_identity': broker.digest(selection)}
    broker.atomic(receipt_dir / 'preflight.json', result)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest='operation', required=True)
    preflight = subparsers.add_parser('preflight')
    preflight.add_argument('--profile', type=Path, required=True)
    preflight.add_argument('--receipt-dir', type=Path, required=True)
    probe = subparsers.add_parser('probe')
    probe.add_argument('name', choices=PROBES)
    args = parser.parse_args()
    result = run_preflight(args.profile, args.receipt_dir) if args.operation == 'preflight' else run_probe(args.name)
    print(json.dumps(result, sort_keys=True))
