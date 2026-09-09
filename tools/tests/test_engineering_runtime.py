"""Deterministic protocol tests. Fake peers only; never invoke a model/account."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('engineering_broker', Path(__file__).resolve().parents[1] / 'engineering-runtime' / 'broker.py')
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)


class BrokerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / 'replies').mkdir()
        b.atomic(self.root / 'dispatch.json', {
            'execution_id': 'execution', 'dispatch_key': 'dispatch', 'turn_seconds': 600,
            'deadline': 9999999999, 'role': 'worker', 'workspace': str(self.root),
            'model': 'gpt-6-astra', 'effort': 'medium', 'max_subagents': 2,
            'subagent_model': 'gpt-5.6-terra', 'subagent_effort': 'medium',
            'bwrap': '/usr/bin/bwrap', 'codex': '/no-model-executable',
            'thread_params': {}, 'prompt': 'Fake protocol test only'})
        self.addCleanup(self.temp.cleanup)
        locks = tempfile.TemporaryDirectory()
        self.addCleanup(locks.cleanup)
        self.locks = Path(locks.name) / 'locks'
        patched = patch.object(b, 'workspace_lock_root', return_value=self.locks)
        patched.start()
        self.addCleanup(patched.stop)

    def broker(self):
        value = b.Broker(self.root)
        self.addCleanup(value.selector.close)
        return value

    def other_broker(self, name):
        root = self.root / name
        root.mkdir()
        (root / 'replies').mkdir()
        manifest = b.read(self.root / 'dispatch.json')
        manifest['execution_id'] = name
        b.atomic(root / 'dispatch.json', manifest)
        broker = b.Broker(root)
        self.addCleanup(broker.selector.close)
        return broker

    def test_distinct_spools_cannot_launch_same_workspace_until_reaped(self):
        first = self.broker()
        contender = self.other_broker('different-profile-and-database')
        child = None
        original_spawn = subprocess.Popen

        def spawn(*args, **kwargs):
            nonlocal child
            with patch.object(contender, 'spawn') as second_spawn:
                self.assertEqual(contender.run(), 'finished')
                second_spawn.assert_not_called()
            self.assertTrue(contender.spool.has('not_started'))
            self.assertFalse(contender.spool.has('boundary_started'))
            child = original_spawn([sys.executable, '-c', 'pass'], **kwargs)
            actual_wait = child.wait

            def wait(*args, **kwargs):
                # Cancellation and protocol loss have happened; the lock still
                # prevents a replacement until the actual child is waited on.
                with self.assertRaises(BlockingIOError):
                    b.WorkspaceWriter(self.root, {'generation': 'replacement'})
                return actual_wait(*args, **kwargs)
            child.wait = wait
            return child

        first.spawn = spawn
        first.run()
        self.assertTrue(first.spool.has('boundary_reaped'))
        successor = b.WorkspaceWriter(self.root, {'generation': 'after-reap'})
        successor.release_after_cessation()
        self.assertIsNotNone(child.returncode)

    def test_broker_loss_and_cancel_leave_uncertainty_on_same_inode(self):
        owner = b.WorkspaceWriter(self.root, {'generation': 'lost-owner'})
        inode = os.fstat(owner.fd).st_ino
        b.atomic(self.root / 'cancel.json', {'reason': 'parent gone'})
        owner.close()  # OS lock release on broker death is not a reap receipt.
        alias = self.root / 'alias'
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, 'lacks verified cessation'):
            b.WorkspaceWriter(alias, {'generation': 'replacement'})
        self.assertEqual(next(self.locks.iterdir()).stat().st_ino, inode)
        self.assertFalse(self.broker().spool.has('boundary_reaped'))

    def test_supervisor_does_not_take_workspace_writer_lock(self):
        broker = self.broker()
        broker.manifest['role'] = 'supervisor'
        with patch.object(b, 'WorkspaceWriter') as writer, patch.object(broker, 'spawn', side_effect=OSError('fake spawn failure')):
            broker.run()
            writer.assert_not_called()
        self.assertTrue(broker.spool.has('not_started'))

    def test_duplicate_dispatch_and_lost_launch_ack_never_spawn(self):
        broker = self.broker()
        broker.event('launch_committed', {'generation': 'old'})
        with patch.object(broker, 'spawn') as spawn:
            self.assertEqual(broker.run(), 'existing')
            spawn.assert_not_called()
        # A fresh broker after owner death has the same refusal, independent of PID.
        replacement = self.broker()
        with patch.object(replacement, 'spawn') as spawn:
            self.assertEqual(replacement.run(), 'existing')
            spawn.assert_not_called()

    def test_lost_turn_start_ack_does_not_replay(self):
        broker = self.broker()
        broker.child = type('Child', (), {'stdin': io.BytesIO()})()
        broker.send = lambda value: broker.child.stdin.write(b.encoded(value) + b'\n')
        broker.event('launch_committed', {'generation': 'old'})
        with patch.object(broker, 'pump', side_effect=EOFError('lost ack')):
            with self.assertRaises(EOFError):
                broker.rpc('turn/start', {'threadId': 'thread', 'input': []})
        sends = broker.child.stdin.getvalue().decode().splitlines()
        self.assertEqual(len(sends), 1)
        self.assertTrue(b.Spool(self.root).has('rpc_intent'))
        with patch.object(self.broker(), 'spawn') as spawn:
            self.assertEqual(self.broker().run(), 'existing')
            spawn.assert_not_called()

    def test_request_identity_and_response_are_durable_across_client_disconnect(self):
        broker = self.broker()
        broker.thread = 'thread'
        broker.turn = 'turn'
        broker.child = type('Child', (), {'stdin': io.BytesIO()})()
        broker.send = lambda value: broker.child.stdin.write(b.encoded(value) + b'\n')
        request = {'id': 7, 'method': 'item/tool/requestUserInput', 'params': {
            'threadId': 'thread', 'turnId': 'turn', 'itemId': 'item',
            'questions': [{'id': 'colour', 'question': 'Which colour?', 'options': []}]}}
        broker.observe(request)
        broker.observe(request)
        self.assertEqual(len(broker.pending), 1)
        key = broker.request_key(request)
        # No controller has to remain connected. A later controller observes the
        # exact persisted request and supplies the previously saved Store answer.
        retained = b.Spool(self.root).events
        self.assertEqual(retained[-1]['value']['key'], key)
        answer = {'id': 7, 'result': {'answers': {'colour': {'answers': ['teal']}}}}
        b.atomic(self.root / 'replies' / (key + '.json'), answer)
        broker.deliver()
        broker.deliver()
        self.assertEqual(len(broker.child.stdin.getvalue().splitlines()), 1)
        self.assertTrue(b.Spool(self.root).has('response_intent'))
        self.assertTrue(b.Spool(self.root).has('response_written'))
        other = self.broker()
        self.assertNotEqual(key, other.request_key(request))

    def test_approval_declines_without_session_or_prefix_grants(self):
        broker = self.broker()
        request = {'id': 2, 'method': 'item/commandExecution/requestApproval',
                   'params': {'command': 'arbitrary shell', 'proposedExecpolicyAmendment': ['sh']}}
        broker.observe(request)
        reply = b.read(self.root / 'replies' / (broker.request_key(request) + '.json'))
        self.assertEqual(reply, {'id': 2, 'result': {'decision': 'decline'}})
        self.assertTrue(b.Spool(self.root).has('request'))

    def test_only_literal_pwd_can_receive_a_single_request_approval(self):
        broker = self.broker()
        broker.manifest['allow_single_pwd_approval'] = True
        request = {'id': 20, 'method': 'item/commandExecution/requestApproval', 'params': {'command': 'pwd'}}
        broker.observe(request)
        reply = b.read(self.root / 'replies' / (broker.request_key(request) + '.json'))
        self.assertEqual(reply['result'], {'decision': 'accept'})
        request = {'id': 21, 'method': 'item/commandExecution/requestApproval', 'params': {'command': 'pwd; arbitrary-command'}}
        broker.observe(request)
        reply = b.read(self.root / 'replies' / (broker.request_key(request) + '.json'))
        self.assertEqual(reply['result'], {'decision': 'decline'})

    def test_cancellation_is_not_cessation(self):
        broker = self.broker()
        b.atomic(self.root / 'cancel.json', {'reason': 'lease expired'})
        with self.assertRaises(InterruptedError):
            broker.pump()
        self.assertFalse(b.Spool(self.root).has('boundary_reaped'))

    def test_deadline_and_spool_are_finite(self):
        broker = self.broker()
        broker.deadline = 0
        with self.assertRaises(TimeoutError):
            broker.pump()
        broker.spool.size = b.MAX_SPOOL - b.RESERVE
        with self.assertRaises(ValueError):
            broker.event('progress', {'value': 'x'})
        broker.event('failure', {'message': 'budget exhausted'}, terminal=True)

    def test_torn_spool_never_restarts(self):
        (self.root / 'events.jsonl').write_bytes(b'{"sequence":1')
        with self.assertRaises(json.JSONDecodeError):
            self.broker()

    def test_real_fake_peer_completion_then_reconnect_read(self):
        # Real OS pipe/process handling with a deterministic fake JSON-RPC peer.
        # This does not qualify Bubblewrap, Codex or authentic engineering work.
        peer = self.root / 'fake_peer.py'
        peer.write_text('''import json,sys,os
for line in sys.stdin:
 q=json.loads(line); m=q.get('method'); rid=q.get('id')
 if rid is None: continue
 if m=='initialize': r={}
 elif m=='config/read': r={'config':{'agents':{'max_threads':2,'max_depth':1,'default_subagent_model':'gpt-5.6-terra','default_subagent_reasoning_effort':'medium'},'features':{'apps':False},'web_search':'disabled','mcp_servers':{}}}
 elif m=='skills/list': r={'data':[]}
 elif m=='thread/start': r={'cwd':os.getcwd(),'thread':{'id':'thread'},'model':'gpt-6-astra','reasoningEffort':'medium','approvalPolicy':'on-request','approvalsReviewer':'user','sandbox':{'type':'workspaceWrite','networkAccess':False,'excludeSlashTmp':True,'excludeTmpdirEnvVar':True,'writableRoots':[]},'instructionSources':[]}
 elif m=='turn/start': r={'turn':{'id':'turn'}}
 print(json.dumps({'id':rid,'result':r}),flush=True)
 if m=='turn/start':
  print(json.dumps({'method':'item/completed','params':{'threadId':'thread','turnId':'turn','item':{'type':'agentMessage','text':'submission only','id':'final'}}}),flush=True)
  print(json.dumps({'method':'turn/completed','params':{'threadId':'thread','turn':{'id':'turn','status':'completed'}}}),flush=True)
''')
        broker = self.broker()
        broker.spawn = lambda *args, **kwargs: subprocess.Popen([sys.executable, str(peer)], **kwargs)
        self.assertEqual(broker.run(), 'finished')
        reconnected = b.Spool(self.root)
        self.assertTrue(reconnected.has('turn/completed'))
        self.assertTrue(reconnected.has('boundary_reaped'))
        self.assertFalse(reconnected.has('outcome_success'))
        self.assertEqual(broker.child.poll(), broker.child.returncode)

    def test_stalled_protocol_writer_observes_deadline(self):
        broker = self.broker()
        reader, writer = os.pipe()
        self.addCleanup(os.close, reader)
        stream = os.fdopen(writer, 'wb', buffering=0)
        self.addCleanup(stream.close)
        os.set_blocking(writer, False)
        broker.child = type('Child', (), {'stdin': stream})()
        broker.clock = lambda: 1000
        broker.deadline = 999
        with self.assertRaises(TimeoutError):
            broker.send({'method': 'turn/start', 'params': {}})

    def test_effective_capabilities_reject_silent_reductions_and_redact_secrets(self):
        broker = self.broker()
        config = {'agents': {'max_threads': 2, 'max_depth': 1,
                             'default_subagent_model': 'gpt-5.6-terra',
                             'default_subagent_reasoning_effort': 'medium'},
                  'features': {'apps': False}, 'web_search': 'disabled',
                  'mcp_servers': {'openaiDeveloperDocs': {'enabled': True, 'env': {'TOKEN': 'secret'}}}}
        broker.manifest['readonly_mcp_servers'] = ['openaiDeveloperDocs']
        broker.verify_capability_config(config)
        self.assertNotIn('secret', json.dumps(broker.spool.events))
        config['mcp_servers']['openaiDeveloperDocs']['enabled'] = False
        with self.assertRaisesRegex(ValueError, 'differs from task profile'):
            broker.verify_capability_config(config)
        config['mcp_servers']['openaiDeveloperDocs']['enabled'] = True
        config['agents']['max_threads'] = 1
        with self.assertRaisesRegex(ValueError, 'differs from task profile'):
            broker.verify_capability_config(config)

    def test_source_observation_rejects_fifo_without_waiting_for_a_writer(self):
        broker = self.broker()
        workspace = Path(broker.manifest['workspace'])
        os.mkfifo(workspace / 'untracked-pipe')
        self.assertIn('unavailable', broker.source_snapshot())

    def test_mcp_overrides_preserve_inherited_transport_and_never_copy_credentials(self):
        config_home = self.root / 'account'
        config_home.mkdir()
        (config_home / 'config.toml').write_text('[mcp_servers.openaiDeveloperDocs]\nurl="https://docs.example.invalid/mcp"\nbearer_token_env_var="SECRET_TOKEN"\n')
        with patch.dict(os.environ, {'CODEX_HOME': str(config_home)}):
            config = self.broker().configuration()
        self.assertNotIn('mcp_servers', config)
        self.assertFalse(config['mcp_servers.openaiDeveloperDocs.enabled'])
        self.assertNotIn('SECRET_TOKEN', json.dumps(config))
        self.assertNotIn('docs.example.invalid', json.dumps(config))

    def test_failed_start_retains_bounded_classified_stderr_without_secrets(self):
        broker = self.broker()
        script = 'import sys; sys.stderr.write("invalid transport config SECRET_TOKEN=do-not-retain\\n"); sys.exit(2)'
        broker.spawn = lambda *args, **kwargs: subprocess.Popen([sys.executable, '-c', script], **kwargs)
        broker.run()
        diagnostic = next(event['value'] for event in broker.spool.events if event['kind'] == 'stderr_diagnostic')
        self.assertIn('invalid_mcp_transport_configuration', diagnostic['classes'])
        self.assertNotIn('do-not-retain', json.dumps(broker.spool.events))
        self.assertLessEqual(diagnostic['classified_prefix_bytes'], 8192)
        self.assertTrue(broker.spool.has('boundary_reaped'))
        self.assertFalse(broker.spool.has('turn_identity'))

    def test_command_started_and_completed_retain_actual_source_changes(self):
        workspace = self.root / 'source'
        workspace.mkdir()
        path = workspace / 'reader.txt'
        path.write_text('before')
        broker = self.broker()
        broker.manifest['workspace'] = str(workspace)
        for method in ['item/started', 'item/completed']:
            broker.observe({'method': method, 'params': {'threadId': 't', 'turnId': 'turn',
                            'item': {'id': 'check', 'type': 'commandExecution', 'command': 'check'}}})
            path.write_text('after')
        retained = [event['value']['source'] for event in broker.spool.events if event['kind'] == 'command_source']
        self.assertEqual(len(retained), 2)
        self.assertNotEqual(retained[0], retained[1])
        self.assertTrue(broker.spool.has('item/started'))

    def test_profile_contains_required_pid_namespace_and_client_reviewer(self):
        command = self.broker().command()
        for arg in ['--die-with-parent', '--unshare-pid', '--new-session', '--proc', 'approvals_reviewer="user"']:
            self.assertIn(arg, command)
        self.assertIn('sandbox_workspace_write.exclude_slash_tmp=true', command)
        self.assertNotIn('danger-full-access', command)
        self.assertIn('--ro-bind', command)
        self.assertIn(str(self.locks), command)


if __name__ == '__main__':
    unittest.main()
