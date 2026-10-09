"""Deterministic host recovery and trust tests; never call a model/account."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).parent))
from common import (Config, Journal, Reservations, atomic, canonical, control,
                    digest, edge_authorization, encoded, locked, read, pidfd_open)
from broker import Broker, source_observation, stopped
from verification import verify
from urllib.error import HTTPError
from worker import Worker


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base=Path(self.temp.name).resolve()
        self.registry=self.base/'registry';self.registry.mkdir()
        self.product=self.base/'product';self.product.mkdir()
        self.scratch=self.base/'scratch';self.scratch.mkdir()
        self.root=self.base/'job';self.root.mkdir()
        for name in ('events','requests','answers','agent-state'):
            (self.root/name).mkdir()
        self.profile={'id':'project','revision':1,'host':'dev','workspace':str(self.product),
              'profile_revision':'v1','write_roots':[str(self.product),str(self.scratch)],
              'permitted_actions':['ordinary_code_delivery'],'limits':{'max_seconds':120,'max_turns':4,'max_tokens':100000},
              'git_common_dirs':[],'resources':[str(self.product),str(self.scratch)],
              'scratch':str(self.scratch),'verification':{'repositories':[]}}
        self.dispatch={'execution_id':'job','task_id':'task','obligation_id':'obligation',
             'definition_revision':1,'profile_revision':'v1','admitted_at':int(time.time()),
             'deadline_at':int(time.time())+60,'assignment':{
                 'project':{'id':'project','revision':1,'registration':{'host':'dev','workspace':str(self.product)}},
                 'criteria':[{'id':'criterion','description':'Delivered ordinary change'}],
                 'permitted_actions':['ordinary_code_delivery'],
                 'limits':{'max_seconds':60,'max_turns':4,'max_tokens':100000}}}
        self.admission={'dispatch':self.dispatch,'dispatch_digest':digest(self.dispatch),
              'project_profile':self.profile,'admitted_at':time.time(),'deadline':time.time()+60,
              'registry':str(self.registry),'codex':'/no-model','bwrap':'/no-boundary',
              'runtime_root':str(self.base/'state'),'token_file':str(self.base/'token'),
              'config_file':str(self.base/'config')}
        atomic(self.root/'admission.json',self.admission)

    def config(self):
        config=Config.__new__(Config)
        config.projects={'project':self.profile}
        config.executions=self.base/'executions';config.executions.mkdir()
        config.root=self.base
        config.registry=self.registry
        config.value={'codex':'/no-model','bwrap':'/no-boundary','host_id':'dev'}
        config.token_file=str(self.base/'token')
        config.edge_authorization=None
        config.path=str(self.base/'config')
        return config

    def test_duplicate_dispatch_and_changed_payload_cannot_retarget(self):
        config=self.config()
        first=config.admit(self.dispatch)
        deadline=read(first/'admission.json')['deadline']
        self.assertEqual(config.admit(self.dispatch),first)
        self.assertEqual(read(first/'admission.json')['deadline'],deadline)
        changed=json.loads(json.dumps(self.dispatch))
        changed['assignment']['limits']['max_seconds']=61
        with self.assertRaisesRegex(ValueError,'changed payload'):
            config.admit(changed)
        changed=json.loads(json.dumps(self.dispatch))
        changed['assignment']['project']['registration']['workspace']=str(self.scratch)
        with self.assertRaisesRegex(ValueError,'allowlist'):
            config.admit(changed)

    def test_canonical_paths_reject_symlink_aliases(self):
        alias=self.base/'alias';alias.symlink_to(self.product,target_is_directory=True)
        with self.assertRaisesRegex(ValueError,'canonical'):
            canonical(str(alias))

    def test_dispatch_cannot_exceed_host_actions_or_finite_ceilings(self):
        config=self.config()
        invalid=json.loads(json.dumps(self.dispatch));invalid['assignment']['permitted_actions']=['deployment']
        with self.assertRaisesRegex(ValueError,'permitted actions'):config.admit(invalid)
        invalid=json.loads(json.dumps(self.dispatch));invalid['assignment']['limits']['max_tokens']=100001
        with self.assertRaisesRegex(ValueError,'finite execution'):config.admit(invalid)

    def test_redirects_cannot_forward_host_authorisation(self):
        received=[]
        class Target(BaseHTTPRequestHandler):
            def do_GET(self):
                received.append(self.headers.get('Authorization'));self.send_response(200);self.end_headers()
            def log_message(self,*_args):pass
        target=ThreadingHTTPServer(('127.0.0.1',0),Target)
        class Redirect(BaseHTTPRequestHandler):
            def do_POST(self):
                self.send_response(302)
                self.send_header('Location',f'http://127.0.0.1:{target.server_port}/other-origin')
                self.end_headers()
            def log_message(self,*_args):pass
        redirect=ThreadingHTTPServer(('127.0.0.1',0),Redirect)
        threads=[threading.Thread(target=s.serve_forever,daemon=True) for s in (target,redirect)]
        for thread in threads:thread.start()
        try:
            config=self.config();config.token='0'*64
            config.edge_authorization='Basic dXNlcjpwYXNz'
            config.value['server_url']=f'http://127.0.0.1:{redirect.server_port}'
            with self.assertRaises(HTTPError):Worker(config).request({'events':[],'heartbeats':[]})
            self.assertEqual(received,[])
        finally:
            for server in (target,redirect):server.shutdown();server.server_close()
            for thread in threads:thread.join(timeout=2)

    def test_host_token_and_optional_existing_edge_basic_have_separate_headers(self):
        observed=[]
        class Peer(BaseHTTPRequestHandler):
            def do_POST(self):
                observed.append({'host_token':self.headers.get('X-Bokkie-Host-Token'),
                                 'edge':self.headers.get('Authorization')})
                self.rfile.read(int(self.headers['Content-Length']))
                self.send_response(200);self.end_headers()
                self.wfile.write(b'{"acknowledgements":[],"dispatches":[],"controls":[]}')
            def log_message(self,*_args):pass
        server=ThreadingHTTPServer(('127.0.0.1',0),Peer)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        try:
            config=self.config();config.token='0'*64
            config.value['server_url']=f'http://127.0.0.1:{server.server_port}'
            worker=Worker(config);worker.request({'events':[],'heartbeats':[]})
            credential=self.base/'edge-basic';credential.write_text('Basic dXNlcjpwYXNz\n');credential.chmod(0o600)
            config.edge_authorization=edge_authorization(str(credential))
            worker.request({'events':[],'heartbeats':[]})
            self.assertEqual(observed,[{'host_token':'0'*64,'edge':None},
                                      {'host_token':'0'*64,'edge':'Basic dXNlcjpwYXNz'}])
        finally:
            server.shutdown();server.server_close();thread.join(timeout=2)

    def test_existing_edge_credential_validation_and_no_admission_copy(self):
        credential=self.base/'edge-basic';credential.write_text('Basic dXNlcjpwYXNz\n');credential.chmod(0o600)
        self.assertEqual(edge_authorization(str(credential)),'Basic dXNlcjpwYXNz')
        alias=self.base/'edge-alias';alias.symlink_to(credential)
        with self.assertRaises(ValueError):edge_authorization(str(alias))
        credential.chmod(0o644)
        with self.assertRaisesRegex(ValueError,'mode-600'):edge_authorization(str(credential))
        credential.chmod(0o600);credential.write_text('Bearer unsafe-value')
        with self.assertRaisesRegex(ValueError,'Basic Authorization'):edge_authorization(str(credential))
        credential.write_text('Basic dXNlcjpwYXNz\nAuthorization: injected')
        with self.assertRaisesRegex(ValueError,'Basic Authorization'):edge_authorization(str(credential))
        credential.write_text('Basic dXNlcjpwYXNz')
        config=self.config();config.value['edge_authorization_file']=str(credential)
        config.edge_authorization=edge_authorization(str(credential))
        root=config.admit(self.dispatch)
        manifest=read(root/'admission.json')
        self.assertEqual(manifest['edge_authorization_file'],str(credential))
        self.assertNotIn(config.edge_authorization,(root/'admission.json').read_text())

    def test_oversized_model_result_is_retained_without_an_invalid_outbox_event(self):
        broker=Broker(self.root);broker.thread='root';broker.owner=type('Peer',(),{'stdin':io.BytesIO()})()
        broker.send=lambda value:broker.owner.stdin.write(encoded(value)+b'\n')
        args={'summary':'x'*16385,'criteria':[],'deliveries':[],'limitations':[]}
        request={'id':9,'method':'item/tool/call','params':{'threadId':'root','turnId':'turn','callId':'result',
                  'namespace':'bokkie_workspace','tool':'result','arguments':args}}
        broker.request(request)
        self.assertEqual(Journal(self.root).events(),[])
        self.assertEqual(len(list((self.root/'requests').glob('*.rejected-result.json'))),1)
        self.assertFalse(json.loads(broker.owner.stdin.getvalue())['result']['success'])

    def test_parent_child_and_git_common_reservations_survive_owner_loss(self):
        locks=Reservations(self.registry)
        child=self.product/'worktree';child.mkdir()
        locks.acquire('one','generation-one',[str(self.product)])
        with self.assertRaises(BlockingIOError):
            locks.acquire('two','generation-two',[str(child)])
        # Process/OS-lock absence supplies no release receipt.
        with self.assertRaises(BlockingIOError):
            locks.acquire('three','generation-three',[str(self.product)])
        with self.assertRaises(ValueError):
            locks.release('one','generation-one',{'kind':'connection_closed'})
        locks.release('one','generation-one',{'kind':'descendants_reaped'})
        git=self.base/'git-common';git.mkdir()
        locks.acquire('four','generation-four',[str(child),str(git)])
        with self.assertRaises(BlockingIOError):
            locks.acquire('five','generation-five',[str(self.scratch),str(git)])
        self.assertEqual(len(list(self.registry.glob('*.json'))),2)

    def test_answers_are_immutable_and_cancel_is_monotonic(self):
        first={'execution_id':'job','cancel':True,'answers':[{'question_id':'q','text':'retained'}]}
        control(self.root,first);control(self.root,first)
        control(self.root,{'execution_id':'job','cancel':False,'answers':[]})
        self.assertTrue(read(self.root/'cancel.json')['cancel'])
        with self.assertRaisesRegex(ValueError,'immutable'):
            control(self.root,{'execution_id':'job','cancel':False,'answers':[{'question_id':'q','text':'changed'}]})

    def test_committed_launch_after_lost_ack_never_spawns_again(self):
        atomic(self.root/'launch-committed.json',{'generation':'old','boundary_id':'job:old'})
        with patch('broker.subprocess.Popen') as spawn:
            Broker(self.root).run();Broker(self.root).run()
            spawn.assert_not_called()

    def test_expired_admission_cannot_start_a_payload(self):
        admission=read(self.root/'admission.json');admission['deadline']=time.time()-1
        atomic(self.root/'admission.json',admission)
        with patch('broker.subprocess.Popen') as spawn:
            Broker(self.root).run();spawn.assert_not_called()
        self.assertEqual(read(self.root/'cessation.json')['kind'],'not_started')

    def test_broker_loss_keeps_reservation_and_projects_uncertainty(self):
        Reservations(self.registry).acquire('job','old',self.profile['resources'])
        atomic(self.root/'launch-committed.json',{'generation':'old','boundary_id':'job:old'})
        config=self.config()
        worker=Worker(config)
        worker.reconcile(self.root);worker.reconcile(self.root)
        events=Journal(self.root).events()
        self.assertEqual(len(events),1)
        self.assertEqual(events[0]['event']['kind'],'attention')
        marker=next(self.registry.glob('*.json'))
        self.assertFalse(read(marker)['released'])

    def test_durable_answer_is_delivered_once_and_child_cannot_report(self):
        broker=Broker(self.root)
        broker.thread='root';broker.owner=type('Peer',(),{'stdin':io.BytesIO()})()
        broker.send=lambda value:broker.owner.stdin.write(encoded(value)+b'\n')
        request={'id':7,'method':'item/tool/call','params':{'threadId':'root','turnId':'turn',
            'callId':'call','namespace':'bokkie_workspace','tool':'question',
            'arguments':{'id':'q','kind':'routine','prompt':'Which?','options':['one','two']}}}
        broker.request(request);self.assertEqual(len(broker.pending),1)
        control(self.root,{'execution_id':'job','cancel':False,'answers':[{'question_id':'q','text':'one'}]})
        broker.deliver_answers();broker.deliver_answers();broker.request(request)
        self.assertEqual(len(broker.owner.stdin.getvalue().splitlines()),1)
        child=json.loads(json.dumps(request));child['id']=8;child['params']['threadId']='child'
        broker.request(child)
        reply=json.loads(broker.owner.stdin.getvalue().splitlines()[-1])
        self.assertFalse(reply['result']['success'])
        self.assertEqual(len(Journal(self.root).events()),1)

    def test_event_gap_and_changed_immutable_event_fail_closed(self):
        journal=Journal(self.root)
        journal.event({'kind':'progress','summary':'one'})
        journal.event({'kind':'progress','summary':'two'})
        (self.root/'events/00000001.json').unlink()
        with self.assertRaisesRegex(ValueError,'gap'):
            journal.events()

    def test_outbox_reconnect_replays_unacknowledged_events(self):
        config=self.config()
        execution_root=config.admit(self.dispatch)
        journal=Journal(execution_root)
        journal.event({'kind':'progress','summary':'retained'})
        first=Worker(config)
        payloads=[]
        def lost(payload):
            payloads.append(payload)
            raise OSError('lost acknowledgement')
        with patch.object(first,'request',side_effect=lost):
            with self.assertRaises(OSError):first.exchange()
        second=Worker(config)
        def ack(payload):
            payloads.append(payload)
            return {'acknowledgements':[{'execution_id':'job','sequence':1}],'dispatches':[],'controls':[]}
        with patch.object(second,'request',side_effect=ack):second.exchange()
        self.assertEqual(payloads[0]['events'],payloads[1]['events'])
        self.assertEqual(read(execution_root/'ack.json')['sequence'],1)

    def test_stopped_reconciliation_preserves_result_without_model_restart(self):
        proof={'generation':'old','boundary_id':'job:old','kind':'descendants_reaped','evidence':'trusted ECHILD'}
        atomic(self.root/'launch-committed.json',{'generation':'old','boundary_id':'job:old'})
        atomic(self.root/'cessation.json',proof)
        result={'summary':'delivered','criteria':[],'deliveries':[],'limitations':[]}
        atomic(self.root/'result.json',result)
        with patch('broker.verify',return_value={'passed':False,'evidence':['missing review']}):
            stopped(self.root,'pending proof')
        with patch('broker.verify',return_value={'passed':True,'evidence':['acquired review']}),patch('broker.subprocess.Popen') as spawn:
            stopped(self.root,'reconciled',reverify=True)
            spawn.assert_not_called()
        events=Journal(self.root).events()
        self.assertEqual(len(events),2)
        self.assertEqual(events[0]['event']['result'],events[1]['event']['result'])
        self.assertEqual(events[0]['event']['cessation'],events[1]['event']['cessation'])
        self.assertTrue(events[1]['event']['verification']['passed'])

    def test_outside_reaper_cancels_sets_id_descendant_and_records_echild(self):
        atomic(self.root/'launch-committed.json',{'generation':'actual-helper','boundary_id':'job:actual-helper'})
        parent_fd=pidfd_open(os.getpid())
        sentinel=self.root/'delayed-write'
        payload='import os,time; from pathlib import Path; p=os.fork(); (os.setsid(),print("ready",flush=True),time.sleep(.5),Path('+repr(str(sentinel))+').write_text("late")) if p==0 else time.sleep(10)'
        owner=subprocess.Popen([sys.executable,str(Path(__file__).with_name('reaper.py')),str(self.root),str(parent_fd),
                 sys.executable,'-c',payload],pass_fds=(parent_fd,),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        os.close(parent_fd)
        self.addCleanup(lambda: owner.terminate() if owner.poll() is None else None)
        self.assertEqual(owner.stdout.readline().strip(),b'ready')
        owner.terminate();owner.wait(timeout=5)
        owner.stdout.close();owner.stderr.close()
        proof=read(self.root/'cessation.json')
        self.assertEqual(proof['kind'],'descendants_reaped')
        self.assertIn('ECHILD',proof['evidence'])
        time.sleep(.55)
        self.assertFalse(sentinel.exists())


class VerificationTests(unittest.TestCase):
    def setUp(self):
        self.head='a'*40;self.merge='b'*40;self.tree='c'*40
        self.admission={'dispatch':{'assignment':{'criteria':[{'id':'c'}]}},
                'project_profile':{'verification':{'repositories':[{'repository':'owner/repo',
                'required_checks':['Canonical CI'],'canonical_commands':['tools/check.sh']}]}}}
        self.result={'summary':'done','criteria':[{'id':'c','satisfied':True,'evidence':['delivery']}],
            'deliveries':[{'repository':'owner/repo','pull_request':'https://github.com/owner/repo/pull/1',
                          'reviewed_head':self.head,'merge_revision':self.merge,'tree':self.tree,'checks':['untrusted text']}],
            'limitations':[]}
        source={'repository':'owner/repo','head':self.head,'tree':self.tree,'clean':True}
        self.records=[{'kind':'thread_identity','value':{'thread_id':'root'}},
            {'kind':'reviewer_profile','value':{'role':'exact_head_reviewer','sha256':'configured-readonly-digest'}},
            *[{'kind':'command_observation','value':{'phase':phase,'item':{'id':'cmd','command':"/bin/bash -lc 'tools/check.sh'",'exitCode':0},'source':source}} for phase in ('started','completed')]]
        self.review=[{'kind':'protocol_event','value':{'method':'thread/started','params':{'thread':{'id':'reviewer','parentThreadId':'root','agentRole':'exact_head_reviewer'}}}},
             {'kind':'protocol_event','value':{'method':'item/started','params':{'threadId':'root','item':{'type':'subAgentActivity','agentThreadId':'reviewer'}}}},
             {'kind':'protocol_event','value':{'method':'item/completed','params':{'threadId':'reviewer','turnId':'review-turn','item':{'type':'agentMessage','phase':'final_answer','text':'Verdict: PASS\nReviewed head: '+self.head}}}},
             {'kind':'protocol_event','value':{'method':'turn/completed','params':{'threadId':'reviewer','turn':{'id':'review-turn','status':'completed'}}}}]

    def query(self,endpoint):
        if endpoint.endswith('/pulls/1'):
            return {'merged':True,'head':{'sha':self.head},'merge_commit_sha':self.merge,'user':{'login':'author'}}
        if '/git/commits/' in endpoint:return {'tree':{'sha':self.tree}}
        if '/reviews?' in endpoint:return []
        revision=self.head if self.head in endpoint else self.merge
        return {'total_count':1,'check_runs':[{'name':'Canonical CI','head_sha':revision,'id':1,'started_at':'2026-10-09',
                       'status':'completed','conclusion':'success','html_url':'https://github.com/owner/repo/actions/runs/1'}]}

    def test_agent_text_and_self_comment_do_not_establish_acceptance(self):
        self.assertFalse(verify(self.admission,self.result,self.records,query=self.query)['passed'])
        root_report=json.loads(json.dumps(self.review));root_report[2]['value']['params']['threadId']='root'
        self.assertFalse(verify(self.admission,self.result,self.records+root_report,query=self.query)['passed'])

    def test_actual_child_review_canonical_command_and_both_ci_revisions_required(self):
        self.assertTrue(verify(self.admission,self.result,self.records+self.review,query=self.query)['passed'])
        def missing_merge(endpoint):
            if self.merge in endpoint and '/check-runs?' in endpoint:return {'total_count':0,'check_runs':[]}
            return self.query(endpoint)
        self.assertFalse(verify(self.admission,self.result,self.records+self.review,query=missing_merge)['passed'])
        self.records[-1]['value']['source']={**self.records[-1]['value']['source'],'clean':False}
        self.assertFalse(verify(self.admission,self.result,self.records+self.review,query=self.query)['passed'])

    def test_missing_or_incomplete_independent_review_rejected(self):
        self.assertFalse(verify(self.admission,self.result,self.records+self.review[:-1],query=self.query)['passed'])
        self.review[2]['value']['params']['item']['text']='Verdict: PASS\nReviewed head: '+'d'*40
        self.assertFalse(verify(self.admission,self.result,self.records+self.review,query=self.query)['passed'])

    def test_implementation_child_cannot_supply_independent_review(self):
        self.review[0]['value']['params']['thread']['agentRole']='worker'
        self.assertFalse(verify(self.admission,self.result,self.records+self.review,query=self.query)['passed'])


if __name__=='__main__':
    unittest.main()
