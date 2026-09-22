"""Offline peers exercise the production broker protocol and containment checks."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('conversation_broker', Path(__file__).with_name('broker.py'))
broker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(broker)


def config():
    return {'features': {key: False for key in broker.DISABLED}, 'web_search': 'disabled',
            'skills': {'include_instructions': False}, 'project_doc_max_bytes': 0,
            'notify': [], 'mcp_servers': {}}


def started():
    return {'thread': {'id': 'thread-1', 'environments': [], 'ephemeral': True},
            'model': 'fixture-model', 'reasoningEffort': 'medium',
            'approvalPolicy': 'never', 'approvalsReviewer': 'user',
            'sandbox': {'type': 'readOnly', 'networkAccess': False}, 'instructionSources': []}


class BrokerTests(unittest.TestCase):
    def run_peer(self, scenario='success', preflight=False):
        profile = {'model': 'fixture-model', 'effort': 'medium', 'timeout_seconds': 1,
                   'max_context_bytes': 4096, 'max_output_bytes': 1024}
        source = '''import sys,json,time
config=CONFIG
started=STARTED
scenario=SCENARIO
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line); method=r.get('method')
 if method=='initialized': continue
 result={}
 if method=='initialize': result={'userAgent':'bokkie_conversation/0.155.1 (fixture)'}
 if method=='config/read': result={'config':config}
 if method=='thread/start':
  assert r['params']['environments']==[] and r['params']['dynamicTools']==[]
  assert r['params']['ephemeral'] is True
  result=started
 if method=='turn/start':
  assert r['params']['outputSchema']['type']=='object'
  if scenario=='timeout': time.sleep(10)
  result={'turn':{'id':'turn-1'}}
 send({'id':r['id'],'result':result})
 if method=='turn/start':
  tid='other-thread' if scenario=='identity' else 'thread-1'
  if scenario=='request': send({'id':99,'method':'item/tool/call','params':{}}); continue
  send({'method':'turn/started','params':{'threadId':tid,'turn':{'id':'turn-1'}}})
  kind='commandExecution' if scenario=='tool' else 'agentMessage'
  text='not JSON' if scenario=='malformed' else json.dumps({'operation':'reply','text':'hello'})
  if scenario=='oversized': text='x'*1025
  send({'method':'item/completed','params':{'threadId':tid,'turnId':'turn-1','item':{'type':kind,'text':text,'phase':'final_answer'}}})
  send({'method':'turn/completed','params':{'threadId':tid,'turn':{'id':'turn-1','status':'completed'}}})
'''.replace('CONFIG', repr(config())).replace('STARTED', repr(started())).replace('SCENARIO', repr(scenario))
        with patch.object(broker, 'configuration', return_value={}), patch.object(broker, 'command', return_value=[sys.executable, '-u', '-c', source]):
            return broker.run({'profile': profile, 'context': {'message': 'hello'},
                               'output_schema': {'type': 'object'}, 'preflight': preflight})

    def test_fresh_request_and_structured_output(self):
        self.assertEqual(self.run_peer(), {'operation': 'reply', 'text': 'hello'})
        self.assertEqual(self.run_peer(), {'operation': 'reply', 'text': 'hello'})

    def test_no_model_preflight(self):
        self.assertEqual(self.run_peer(preflight=True)['model_calls'], 0)

    def test_forbidden_tools_identity_and_output_fail_closed(self):
        for scenario in ('request', 'tool', 'identity', 'malformed', 'oversized', 'timeout'):
            with self.subTest(scenario=scenario), self.assertRaises(ValueError):
                self.run_peer(scenario)

    def test_effective_config_cannot_broaden(self):
        for mutate in [lambda c: c['features'].update(shell_tool=True),
                       lambda c: c.update(mcp_servers={'unexpected': {}}),
                       lambda c: c['skills'].update(include_instructions=True)]:
            value = config(); mutate(value)
            with self.assertRaises(ValueError): broker.verify_config(value)

    def test_thread_cannot_gain_environment_or_guidance(self):
        for mutate in [lambda t: t['thread'].update(environments=[{'environmentId':'host'}]),
                       lambda t: t.update(instructionSources=['/host/AGENTS.md']),
                       lambda t: t['sandbox'].update(networkAccess=True),
                       lambda t: t.update(model='different')]:
            value = started(); mutate(value)
            with self.assertRaises(ValueError): broker.verify_thread(value, {'model':'fixture-model','effort':'medium'})

    def test_boundary_read_only_with_private_state(self):
        args = broker.command({'bwrap':'/usr/bin/bwrap','codex':'/usr/bin/codex'}, {})
        self.assertEqual(args[args.index('--ro-bind')+1:args.index('--ro-bind')+3], ['/', '/'])
        self.assertIn('--unshare-pid', args)
        self.assertIn('--tmpfs', args)
        self.assertIn('/tmp/conversation', args)
        self.assertNotIn('--dev-bind', args)


if __name__ == '__main__':
    unittest.main()
