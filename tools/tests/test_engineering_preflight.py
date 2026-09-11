"""No-model admission probes and privacy-preserving runtime telemetry."""
import copy
import importlib.util
import json
import os
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('engineering_preflight', Path(__file__).resolve().parents[1] / 'engineering-runtime/preflight.py')
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class PreflightTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace = self.root / 'workspace'
        self.workspace.mkdir()
        (self.workspace / 'source.txt').write_text('initial source')
        self.profile = {'workspace': str(self.workspace), 'codex': str(self.root / 'codex'),
                        'bwrap': str(self.root / 'bwrap'), 'broker': str(self.root / 'broker')}
        for name in ('codex', 'bwrap', 'broker'):
            (self.root / name).write_text(name)
        self.path = self.root / 'profile.json'
        self.path.write_text(json.dumps(self.profile))

    def session(self, profile, role, root):
        return {'effective_settings': {'sandbox': {'writableRoots': [profile['workspace']]},
                                      'instructionSources': []},
                'effective_capabilities': {'worker_scratch': profile.get('worker_scratch')},
                'guidance_identity': 'synthetic-guidance-digest', 'rpc_methods': ['initialize', 'config/read']}

    def run_fake(self):
        parameters = {'profile': copy.deepcopy(self.profile), 'worker': {}, 'supervisor': {}}
        # Source capture is real; only installed app-server and Rust loader are replaced.
        original = subprocess.run
        def run(command, **kwargs):
            if command == [self.profile['codex'], '--version']:
                return subprocess.CompletedProcess(command, 0, b'codex-cli synthetic-test\n')
            return original(command, **kwargs)
        with patch.object(p, '_profile', return_value=parameters), patch.object(p, '_session', side_effect=self.session), patch.object(p.subprocess, 'run', side_effect=run):
            return p.run_preflight(self.path, self.root / 'receipts')

    def test_environment_and_workspace_invalidate_independently(self):
        first = self.run_fake()
        (self.workspace / 'source.txt').write_text('changed source')
        source_changed = self.run_fake()
        self.assertEqual(first['environment_identity'], source_changed['environment_identity'])
        self.assertNotEqual(first['workspace_identity'], source_changed['workspace_identity'])
        (self.root / 'codex').write_text('changed installed executable')
        environment_changed = self.run_fake()
        self.assertNotEqual(source_changed['environment_identity'], environment_changed['environment_identity'])
        self.assertEqual(source_changed['workspace_identity'], environment_changed['workspace_identity'])
        self.assertNotIn('changed source', json.dumps(environment_changed))
        self.assertEqual(environment_changed['model_turns'], 0)
        self.assertEqual(environment_changed, json.loads((self.root / 'receipts/preflight.json').read_text()))

    def test_rejects_receipts_inside_source_before_writing(self):
        with self.assertRaisesRegex(ValueError, 'outside'):
            p.run_preflight(self.path, self.workspace / 'receipts')
        self.assertFalse((self.workspace / 'receipts').exists())

    def test_source_selection_failure_cannot_receive_pass_receipt(self):
        (self.workspace / 'source.txt').unlink()
        (self.workspace / 'alias').symlink_to(self.path)
        with self.assertRaisesRegex(ValueError, 'unsafe_path'):
            self.run_fake()
        self.assertFalse((self.root / 'receipts/preflight.json').exists())

    def test_probe_rejects_zero_tests_even_when_process_succeeds(self):
        result = subprocess.CompletedProcess([], 0, b'test result: ok. 0 passed; 0 failed')
        with patch.object(p, '_run', return_value=(result, {'output_sha256': '0' * 64})):
            with self.assertRaisesRegex(ValueError, 'test missing'):
                p.run_probe('child_review')

    def test_preflight_refuses_turn_start_before_transport(self):
        value = object.__new__(p.PreflightBroker)
        with self.assertRaisesRegex(ValueError, 'cannot start a model turn'):
            value.send({'id': 1, 'method': 'turn/start', 'params': {}})

    def test_raw_account_and_skill_rpc_events_are_not_retained(self):
        value = object.__new__(p.PreflightBroker)
        value.facts = {}
        value.event('rpc_receipt', {'secret': 'account credential'})
        value.event('item/completed', {'text': 'account content'})
        self.assertEqual(value.facts, {})

    def test_token_usage_is_numeric_bounded_and_replay_preserves_observations(self):
        value = object.__new__(p.broker.Broker)
        value.spool = p.broker.Spool(self.root)
        usage = {'threadId': 'child', 'turnId': 'turn', 'secret': 'do not retain',
                 'tokenUsage': {'total': {'inputTokens': 19, 'outputTokens': 4,
                    'cachedInputTokens': 8, 'totalTokens': 23, 'reasoningOutputTokens': 2,
                    'credential': 'do not retain'}, 'last': {'inputTokens': True, 'outputTokens': -1}}}
        notification = {'method': 'thread/tokenUsage/updated', 'params': usage}
        value.observe(notification)
        value.observe(notification)
        events = p.broker.Spool(self.root).events
        self.assertEqual(len(events), 2)
        self.assertEqual(events[0]['value'], events[1]['value'])
        self.assertEqual(events[0]['kind'], 'token_usage')
        self.assertEqual(events[0]['value']['tokenUsage']['total']['inputTokens'], 19)
        self.assertEqual(events[0]['value']['tokenUsage']['last'], {})
        self.assertNotIn('do not retain', json.dumps(events))

    def test_context_measurements_are_bytes_and_do_not_retain_prompts(self):
        value = object.__new__(p.broker.Broker)
        prompt = {'snapshot': {'history': ['synthetic secret']}, 'command_types': 'schema', 'other': 'value'}
        value.manifest = {'prompt': json.dumps(prompt),
                          'thread_params': {'developerInstructions': 'é', 'dynamicTools': [{'name': 'tool'}]}}
        counts = value.context_input_bytes()
        self.assertEqual(counts['developer_instructions'], 2)
        self.assertEqual(counts['command_schema_utf8'], 6)
        self.assertEqual(counts['snapshot_json'], len(p.broker.encoded(prompt['snapshot'])))
        self.assertIsNone(counts['runtime_injected_schema_bytes'])
        self.assertNotIn('synthetic secret', json.dumps(counts))

    def limited_broker(self, limit):
        manifest = {'turn_seconds': 60, 'deadline': 9999999999, 'role': 'supervisor',
                    'execution_id': 'qualification', 'dispatch_key': 'qualification',
                    'workspace': str(self.workspace), 'model': 'gpt-6-astra', 'effort': 'medium',
                    'max_subagents': 2, 'subagent_model': 'gpt-5.6-terra', 'subagent_effort': 'medium',
                    'bwrap': '/usr/bin/bwrap', 'codex': '/no-model', 'thread_params': {}, 'prompt': '{}'}
        p.broker.atomic(self.root / 'dispatch.json', manifest)
        with patch.dict(os.environ, {}, clear=True):
            if limit is not None:
                os.environ['BOKKIE_QUALIFICATION_CONTEXT_LIMIT'] = limit
            value = p.broker.Broker(self.root)
        self.addCleanup(value.selector.close)
        return value

    def test_context_limit_counts_unique_root_children_and_replayed_links(self):
        value = self.limited_broker('3')
        value.thread = 'root'
        value.observe_context('root', 'thread/start response')
        events = [
            {'method': 'thread/started', 'params': {'thread': {'id': 'root'}}},
            {'method': 'turn/started', 'params': {'threadId': 'child-a', 'turn': {'id': 'a'}}},
            {'method': 'item/started', 'params': {'threadId': 'root',
             'item': {'type': 'subAgentActivity', 'agentThreadId': 'child-b'}}}]
        for event in events + events:
            value.observe(event)
        self.assertEqual(value.context_threads, {'root', 'child-a', 'child-b'})
        observed = [e for e in value.spool.events if e['kind'] == 'context_observed']
        self.assertEqual(len(observed), 3)
        replay = self.limited_broker('3')
        replay.observe_context('child-b', 'replayed link')
        self.assertEqual(replay.context_threads, value.context_threads)
        with self.assertRaisesRegex(RuntimeError, 'context limit exceeded'):
            value.observe({'method': 'item/completed', 'params': {'threadId': 'root',
                'item': {'type': 'collabAgentToolCall', 'agentsStates': {'child-c': {'status': 'completed'}}}}})
        evidence = [e['value'] for e in value.spool.events if e['kind'] == 'context_limit'][-1]
        self.assertEqual(evidence['observed_count'], 4)
        self.assertTrue(evidence['exceeded'])
        self.assertEqual(evidence['unreported_children'], 'unknown')

    def test_context_limit_is_optional_and_rejects_invalid_configuration(self):
        value = self.limited_broker(None)
        value.observe_context('root', 'test')
        self.assertEqual(value.context_threads, set())
        self.assertEqual(value.spool.events, [])
        for limit in ('0', '-1', '', '1.5', 'three', ' 3', '３'):
            with self.subTest(limit=limit), self.assertRaisesRegex(ValueError, 'positive integer'):
                self.limited_broker(limit)

    def test_context_overflow_reaps_exact_boundary_and_cannot_relaunch(self):
        value = self.limited_broker('1')
        actual_spawn = subprocess.Popen
        def spawn(*args, **kwargs):
            return actual_spawn([sys.executable, '-c', 'import signal; signal.pause()'], **kwargs)
        def rpc(method, params):
            if method == 'initialize':
                return {}
            if method == 'config/read':
                return {'config': {'agents': {'max_threads': 2, 'max_depth': 1,
                    'default_subagent_model': 'gpt-5.6-terra', 'default_subagent_reasoning_effort': 'medium'},
                    'features': {'apps': False}, 'web_search': 'disabled', 'mcp_servers': {}}}
            if method == 'thread/start':
                return {'thread': {'id': 'root'}, 'cwd': str(self.workspace), 'model': 'gpt-6-astra',
                    'reasoningEffort': 'medium', 'approvalPolicy': 'on-request', 'approvalsReviewer': 'user',
                    'sandbox': {'type': 'readOnly'}, 'instructionSources': []}
            if method == 'skills/list':
                return {'data': []}
            self.assertEqual(method, 'turn/start')
            self.assertEqual(value.context_threads, {'root'})
            value.observe({'method': 'turn/started', 'params': {'threadId': 'child', 'turn': {'id': 'child-turn'}}})
            self.fail('overflow must interrupt the existing run')
        with patch.object(value, 'spawn', side_effect=spawn) as spawned, patch.object(value, 'rpc', side_effect=rpc):
            self.assertEqual(value.run(), 'finished')
            self.assertIsNotNone(value.child.poll())
            self.assertTrue(value.spool.has('boundary_reaped'))
            self.assertTrue(value.spool.has('failure'))
            self.assertEqual(value.run(), 'existing')
            self.assertEqual(spawned.call_count, 1)


if __name__ == '__main__':
    unittest.main()
