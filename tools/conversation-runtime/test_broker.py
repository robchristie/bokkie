"""Offline peers exercise the production broker protocol and containment checks."""
import importlib.util
import json
from pathlib import Path
import sys
import subprocess
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
    def run_tool_peer(self, scenario='selection', discuss=False, preflight=False):
        profile = {'model': 'fixture-model', 'effort': 'medium', 'timeout_seconds': 1,
                   'max_context_bytes': 4096, 'max_output_bytes': 1024}
        specs = [{'type': 'function', 'name': name, 'description': 'Select a proposal',
                  'inputSchema': {'type': 'object'}, 'deferLoading': False}
                 for name in (['bokkie_discuss', 'bokkie_lookup'] if discuss else ['bokkie_lookup'])]
        source = '''import sys,json,time
config=CONFIG
started=STARTED
scenario=SCENARIO
specs=SPECS
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line); method=r.get('method')
 assert method, 'broker must never answer tool or approval requests'
 if method=='initialized': continue
 result={}
 if method=='initialize': result={'userAgent':'bokkie_conversation/0.155.1 (fixture)'}
 if method=='config/read': result={'config':config}
 if method=='thread/start':
  assert r['params']['dynamicTools']==specs and r['params']['environments']==[]
  result=started
 if method!='turn/start':
  send({'id':r['id'],'result':result}); continue
 assert 'outputSchema' not in r['params']
 if scenario!='early': send({'id':r['id'],'result':{'turn':{'id':'turn-1'}}})
 send({'method':'turn/started','params':{'threadId':'thread-1','turn':{'id':'turn-1'}}})
 if scenario in ('final','oversized_final'):
  text='{"tool":"bokkie_propose","arguments":{"action":"pause"}}'
  if scenario=='oversized_final': text='x'*1024
  send({'method':'item/completed','params':{'threadId':'thread-1','turnId':'turn-1','item':{'type':'agentMessage','text':text,'phase':'final_answer'}}})
  send({'method':'turn/completed','params':{'threadId':'thread-1','turn':{'id':'turn-1','status':'completed'}}})
  continue
 params={'threadId':'thread-1','turnId':'turn-1','callId':'call-1','tool':'bokkie_lookup','arguments':{'query':'morning'},'namespace':None}
 if scenario=='thread': params['threadId']='other'
 if scenario=='turn': params['turnId']='other'
 if scenario=='missing_call': params.pop('callId')
 if scenario=='forbidden_name': params['tool']='bokkie_propose'
 if scenario=='namespace': params['namespace']='host'
 if scenario=='malformed': params['arguments']='{"query":"morning"}'
 if scenario=='oversized': params['arguments']={'query':'x'*1024}
 if scenario=='nonfinite': params['arguments']={'query':float('nan')}
 if scenario in ('item','item_mismatch','multiple_items','builtin','completed_item'):
  item={'type':'dynamicToolCall','id':'call-1','tool':'bokkie_lookup','arguments':{'query':'morning'},'status':'inProgress'}
  if scenario=='builtin': item['type']='commandExecution'
  if scenario=='item_mismatch': item['id']='other-call'
  event={'method':'item/completed' if scenario=='completed_item' else 'item/started','params':{'threadId':'thread-1','turnId':'turn-1','item':item}}
  send(event)
  if scenario=='multiple_items': send(event)
 method='item/commandExecution/requestApproval' if scenario=='approval' else 'item/tool/call'
 send({'id':r['id'],'method':method,'params':params})
 if scenario=='multiple_requests': send({'id':100,'method':method,'params':dict(params,callId='call-2')})
 # Stay alive until containment teardown. A tool response would resume inference.
 for extra in sys.stdin: raise RuntimeError('unexpected response: '+extra)
'''.replace('CONFIG', repr(config())).replace('STARTED', repr(started())).replace('SCENARIO', repr(scenario)).replace('SPECS', repr(specs))
        children = []
        original_popen, original_send = subprocess.Popen, broker.Peer.send

        def launch(*args, **kwargs):
            child = original_popen(*args, **kwargs)
            children.append(child)
            return child

        with patch.object(broker, 'configuration', return_value={}), patch.object(broker, 'command', return_value=[sys.executable, '-u', '-c', source]), patch.object(broker.subprocess, 'Popen', side_effect=launch), patch.object(broker.Peer, 'send', autospec=True, side_effect=original_send) as sent:
            try:
                return broker.run({'profile': profile, 'context': {'message': 'hello'}, 'tools': specs,
                                   'preflight': preflight})
            finally:
                self.assertEqual(len(children), 1)
                self.assertIsNotNone(children[0].poll())
                self.assertTrue(all('method' in call.args[1] for call in sent.call_args_list))
                self.assertEqual(sum(call.args[1].get('method') == 'turn/start' for call in sent.call_args_list),
                                 0 if preflight else 1)

    def test_tool_preflight_registers_catalogue_without_model_turn(self):
        result = self.run_tool_peer(preflight=True)
        self.assertEqual(result['model_calls'], 0)
        self.assertEqual(result['offered_tools'], ['bokkie_lookup'])

    def test_tool_proposal_stops_without_tool_response_or_second_turn(self):
        for scenario in ('selection', 'item', 'early', 'multiple_requests'):
            with self.subTest(scenario=scenario):
                self.assertEqual(self.run_tool_peer(scenario),
                                 {'tool': 'bokkie_lookup', 'arguments': {'query': 'morning'}})

    def test_tool_requests_fail_closed(self):
        for scenario in ('thread', 'turn', 'missing_call', 'forbidden_name', 'namespace',
                         'malformed', 'oversized', 'nonfinite', 'item_mismatch',
                         'multiple_items', 'builtin', 'completed_item', 'approval'):
            with self.subTest(scenario=scenario), self.assertRaises(ValueError):
                self.run_tool_peer(scenario)

    def test_plain_final_is_only_offered_discussion_data(self):
        result = self.run_tool_peer('final', discuss=True)
        self.assertEqual(result['tool'], 'bokkie_discuss')
        self.assertEqual(result['arguments']['reason'], 'answer')
        self.assertIn('bokkie_propose', result['arguments']['message'])
        with self.assertRaisesRegex(ValueError, 'required proposal'):
            self.run_tool_peer('final')
        with self.assertRaisesRegex(ValueError, 'exceeded bound'):
            self.run_tool_peer('oversized_final', discuss=True)

    def test_tool_surface_is_closed_and_bounded(self):
        valid = {'type': 'function', 'name': 'bokkie_lookup', 'description': 'Lookup',
                 'inputSchema': {'type': 'object'}}
        self.assertEqual(broker.offered_tools([valid]), {'bokkie_lookup'})
        for specs in ([], [valid, valid], [dict(valid, type='namespace')],
                      [dict(valid, name='shell')], [dict(valid, deferLoading=True)],
                      [dict(valid, inputSchema={'type': 'string'})],
                      [dict(valid, unexpected=True)], [dict(valid, description='x'*32768)]):
            with self.subTest(specs=str(specs)[:100]), self.assertRaises(ValueError):
                broker.offered_tools(specs)

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
