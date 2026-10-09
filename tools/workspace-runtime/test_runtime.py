"""Deterministic host recovery and trust tests; never call a model/account."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).parent))
from common import (Config, Journal, Reservations, atomic, canonical, control,
                    digest, edge_authorization, encoded, locked, read, pidfd_open)
from broker import Broker, source_observation, stopped
from check_wait import MAX_READS
from verification import observed_review, verify
from recovery import effective_result,recover_result
from retained_review import run_retained_review
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
        with self.assertRaisesRegex(ValueError,'changed payload'):
            config.admit(changed)

    def test_profile_promotion_replays_the_old_immutable_admission(self):
        config=self.config();root=config.admit(self.dispatch)
        original=(root/'admission.json').read_bytes()
        promoted=json.loads(json.dumps(self.profile));promoted['revision']=2
        promoted['profile_revision']='v2';promoted['workspace']=str(self.scratch)
        promoted['role']={'model':'new-role','model_reasoning_effort':'different'}
        config.projects={'project':promoted}
        self.assertEqual(config.admit(self.dispatch),root)
        self.assertEqual((root/'admission.json').read_bytes(),original)
        changed=json.loads(json.dumps(self.dispatch));changed['profile_revision']='v2'
        with self.assertRaisesRegex(ValueError,'changed payload'):config.admit(changed)
        new=json.loads(json.dumps(changed));new['execution_id']='new-job'
        new['assignment']['project']['revision']=2
        new['assignment']['project']['registration']['workspace']=str(self.scratch)
        self.assertEqual(read(config.admit(new)/'admission.json')['project_profile']['profile_revision'],'v2')

    def waiting_broker(self):
        broker=Broker(self.root);broker.thread='root'
        broker.profile['verification']['repositories']=[{'repository':'owner/repo','required_checks':['Canonical CI']}]
        broker.sent=[];broker.send=broker.sent.append
        request={'id':20,'method':'item/tool/call','params':{'threadId':'root','turnId':'turn','callId':'ci-wait',
            'namespace':'bokkie_workspace','tool':'wait_for_checks','arguments':{'repository':'owner/repo','revision':'a'*40}}}
        broker.request(request)
        return broker,request

    def check_response(self,broker,status='queued',conclusion=None,*,exit_code=0,missing=False):
        with patch('broker.shutil.which',return_value='/usr/bin/gh'):broker.poll_helpers()
        call=broker.sent[-1]
        self.assertEqual(call['method'],'command/exec')
        self.assertEqual(call['params']['sandboxPolicy']['type'],'readOnly')
        self.assertLessEqual(call['params']['timeoutMs'],20000)
        page={'total_count':0,'check_runs':[]} if missing else {'total_count':1,'check_runs':[
            {'id':1,'name':'Canonical CI','head_sha':'a'*40,'status':status,'conclusion':conclusion,
             'started_at':None,'html_url':'https://github.com/owner/repo/actions/runs/1'}]}
        broker.observe({'id':call['id'],'result':{'exitCode':exit_code,'stdout':json.dumps(page),'stderr':''}})

    def test_ci_wait_holds_unchanged_queued_reply_then_returns_passed_facts(self):
        broker,request=self.waiting_broker();self.check_response(broker)
        self.assertEqual(len(broker.check_waits),1)
        self.assertFalse(any(m.get('id')==request['id'] and 'result' in m for m in broker.sent))
        entry=next(iter(broker.check_waits.values()));entry['next_read']=0
        self.check_response(broker)
        self.assertEqual(len(Journal(self.root).events()),1)
        entry['next_read']=0;self.check_response(broker,'completed','success')
        self.assertEqual(broker.check_waits,{})
        reply=broker.sent[-1];value=json.loads(reply['result']['contentItems'][0]['text'])
        self.assertEqual(value['state'],'passed')
        self.assertFalse(any(r['kind']=='command_observation' for r in Journal(self.root).records()))
        before=len(broker.sent);broker.request(request);self.assertEqual(len(broker.sent),before)

    def test_ci_wait_returns_failed_unavailable_and_never_missing_success(self):
        for scenario in ('failed','unavailable','missing'):
            with self.subTest(scenario=scenario):
                broker,request=self.waiting_broker()
                if scenario=='failed':self.check_response(broker,'completed','failure')
                elif scenario=='unavailable':self.check_response(broker,exit_code=1)
                else:self.check_response(broker,missing=True)
                if scenario=='missing':
                    self.assertTrue(broker.check_waits)
                    entry=next(iter(broker.check_waits.values()));entry['reads']=MAX_READS;entry['next_read']=0
                    broker.poll_helpers()
                value=json.loads(broker.sent[-1]['result']['contentItems'][0]['text'])
                self.assertEqual(value['state'],'failed' if scenario=='failed' else 'unavailable')
                # Give each independent synthetic request a different identity.
                self.root=self.base/('job-'+scenario);self.root.mkdir()
                for name in ('events','requests','answers','agent-state'):(self.root/name).mkdir()
                atomic(self.root/'admission.json',self.admission)

    def test_ci_wait_cancellation_retains_terminal_wait_without_reply(self):
        broker,request=self.waiting_broker();self.check_response(broker)
        atomic(self.root/'cancel.json',{'cancel':True})
        with self.assertRaises(InterruptedError):broker.pump()
        self.assertEqual(broker.check_waits,{})
        finished=[r for r in Journal(self.root).records() if r['kind']=='check_wait_finished']
        self.assertEqual(finished[-1]['value']['state'],'cancelled')
        self.assertFalse(any(m.get('id')==request['id'] and 'result' in m for m in broker.sent))

    def test_ci_wait_is_root_only_and_declared_repository_only(self):
        broker,request=self.waiting_broker()
        child=json.loads(json.dumps(request));child['id']=21;child['params']['threadId']='child'
        broker.request(child);self.assertFalse(broker.sent[-1]['result']['success'])
        foreign=json.loads(json.dumps(request));foreign['id']=22
        foreign['params']['arguments']['repository']='other/repo'
        broker.request(foreign);self.assertFalse(broker.sent[-1]['result']['success'])
        self.assertEqual(len(broker.check_waits),1)

    def test_child_read_collects_actual_metadata_and_history_as_separate_records(self):
        broker=Broker(self.root);broker.thread='root';broker.profile['reviewer']={'role':'exact_head_reviewer'}
        broker.sent=[];broker.send=broker.sent.append
        broker.observe({'method':'item/completed','params':{'threadId':'root','turnId':'turn',
            'item':{'type':'subAgentActivity','agentThreadId':'child','agentPath':'/root/review','kind':'started','id':'activity'}}})
        broker.poll_helpers();call=broker.sent[-1]
        self.assertEqual(call['params'],{'threadId':'child','includeTurns':False})
        thread={'id':'child','parentThreadId':'root','agentRole':'exact_head_reviewer','turns':[]}
        broker.observe({'id':call['id'],'result':{'thread':thread}})
        broker.child_reads['child']['next_read']=0;broker.poll_helpers();call=broker.sent[-1]
        self.assertTrue(call['params']['includeTurns'])
        thread={**thread,'turns':[{'id':'child-turn','status':'completed','items':[
             {'type':'agentMessage','phase':'final_answer','text':'Verdict: PASS\nReviewed head: '+'a'*40}]}]}
        broker.observe({'id':call['id'],'result':{'thread':thread}})
        self.assertTrue(broker.child_reads['child']['done'])
        records=Journal(self.root).records()
        self.assertEqual(len([r for r in records if r['kind']=='child_thread_read']),2)
        self.assertEqual(len([r for r in records if r['kind']=='protocol_event']),1)

    def test_oversized_child_snapshot_is_discarded_with_unavailable_proof_and_preserved_result(self):
        from common import MAX_MESSAGE
        broker=Broker(self.root);broker.thread='root'
        broker.child_reads['child']={'next_read':0,'reads':1,'inflight':True,'include_turns':True,'done':False,'last_digest':None}
        broker.helper_requests[42]={'kind':'child_read','child_id':'child','include_turns':True}
        result={'summary':'retained','criteria':[],'deliveries':[],'limitations':[]}
        atomic(self.root/'result.json',result)
        broker.consume_stdout(b'{"id":42,"result":'+b'x'*MAX_MESSAGE)
        broker.consume_stdout(b'more}\n{"id":99,"result":{}}\n')
        self.assertTrue(broker.child_reads['child']['done'])
        self.assertEqual(read(self.root/'result.json'),result)
        self.assertEqual(broker.responses[99]['result'],{})
        row=Journal(self.root).records()[-1]
        self.assertEqual(row['kind'],'child_read_unavailable')
        self.assertGreater(row['value']['error']['bytes'],MAX_MESSAGE)

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

    def test_same_exchange_cancellation_is_durable_before_dispatch_launch(self):
        config=self.config();worker=Worker(config)
        response={'acknowledgements':[],'dispatches':[self.dispatch],
                  'controls':[{'execution_id':'job','cancel':True,'answers':[]}]}
        seen=[]
        def launch_after_control(root):
            self.assertTrue(read(root/'cancel.json')['cancel']);seen.append(root)
            # Simulate a lost launch acknowledgement with committed identity.
            atomic(root/'launch-committed.json',{'generation':'lost-ack'},immutable=True)
        with patch.object(worker,'request',return_value=response),patch('worker.launch',side_effect=launch_after_control):
            worker.exchange();worker.exchange()
        self.assertEqual(len(seen),1)

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

    def test_supported_child_read_requires_actual_link_role_completed_turn_and_final_answer(self):
        activity=self.review[1]
        thread={'id':'reviewer','parentThreadId':'root','agentRole':'exact_head_reviewer',
            'model':'review-model','reasoningEffort':'high','turns':[{'id':'read-turn','status':'completed','items':[
            {'type':'agentMessage','phase':'final_answer','text':'Verdict: PASS\nReviewed head: '+self.head}]}]}
        record={'kind':'child_thread_read','value':{'child_id':'reviewer','include_turns':True,'thread':thread}}
        self.assertTrue(verify(self.admission,self.result,self.records+[activity,record],query=self.query)['passed'])
        for field,value in [('parentThreadId',None),('agentRole',None),('agentRole','worker')]:
            bad=json.loads(json.dumps(record));bad['value']['thread'][field]=value
            self.assertFalse(verify(self.admission,self.result,self.records+[activity,bad],query=self.query)['passed'])
        bad=json.loads(json.dumps(record));bad['value']['thread']['turns'][0]['status']='inProgress'
        self.assertFalse(verify(self.admission,self.result,self.records+[activity,bad],query=self.query)['passed'])
        bad=json.loads(json.dumps(record));bad['value']['thread']['turns'][0]['items'][0]['phase']='commentary'
        self.assertFalse(verify(self.admission,self.result,self.records+[activity,bad],query=self.query)['passed'])

    def test_later_completed_block_for_same_head_does_not_reuse_earlier_pass(self):
        thread={'id':'reviewer','parentThreadId':'root','agentRole':'exact_head_reviewer','turns':[
          {'id':'first','status':'completed','items':[{'type':'agentMessage','phase':'final_answer','text':'Verdict: PASS\nReviewed head: '+self.head}]},
          {'id':'second','status':'completed','items':[{'type':'agentMessage','phase':'final_answer','text':'Verdict: BLOCK\nReviewed head: '+self.head}]}]}
        record={'kind':'child_thread_read','value':{'child_id':'reviewer','include_turns':True,'thread':thread}}
        self.assertFalse(verify(self.admission,self.result,self.records+[self.review[1],record],query=self.query)['passed'])

    def markdown_review_records(self,report):
        root='01a1204c-6de7-7632-8e83-c5cecc4da8a4'
        child='01a1204e-82ef-7f02-9af7-00c893353835'
        turn='01a1204e-8322-7de1-b400-fde8f6816280'
        records=[{'kind':'reviewer_profile','value':{'role':'exact_head_reviewer',
                 'sha256':'protected-profile-fixture','model':'gpt-6-astra','reasoning_effort':'high'}},
          {'kind':'protocol_event','value':{'method':'item/completed','params':{'threadId':root,
             'item':{'id':'activity','type':'subAgentActivity','agentThreadId':child,'agentPath':'/root/review','kind':'started'}}}},
          {'kind':'child_thread_read','value':{'child_id':child,'include_turns':True,'thread':{
             'id':child,'parentThreadId':root,'agentRole':'exact_head_reviewer','model':'gpt-6-astra',
             'reasoningEffort':'high','status':{'type':'idle'},'ephemeral':False,'historyMode':'legacy',
             'turns':[{'id':turn,'status':'completed','items':[{'id':'report','type':'agentMessage',
                        'phase':'final_answer','text':report}]}]}}}]
        return root,records

    def test_actual_markdown_review_shape_and_plain_format_are_attributable(self):
        head='1dd3d6251cb51b9879690960fbd8824e993a6db1'
        for verdict,revision in [('PASS',head),('**PASS**',head),('PASS','`'+head+'`'),('**PASS**','`'+head+'`')]:
            root,records=self.markdown_review_records('Verdict: '+verdict+'\nReviewed head: '+revision+'  \nBlocking findings: None.')
            self.assertIsNotNone(observed_review(records,root,head))
        for field,value in [('parentThreadId','another-root'),('agentRole','worker'),('model','another-model'),('reasoningEffort','xhigh')]:
            root,records=self.markdown_review_records('Verdict: **PASS**\nReviewed head: `'+head+'`')
            records[-1]['value']['thread'][field]=value
            self.assertIsNone(observed_review(records,root,head))
        root,records=self.markdown_review_records('Verdict: **PASS**\nReviewed head: `'+head+'`')
        records[-1]['value']['thread']['turns'][0]['status']='inProgress'
        self.assertIsNone(observed_review(records,root,head))

    def test_review_wrappers_must_be_single_matched_and_anchored(self):
        head='1dd3d6251cb51b9879690960fbd8824e993a6db1'
        invalid=[
          'Verdict: **PASS*\nReviewed head: `'+head+'`',
          'Verdict: *PASS**\nReviewed head: `'+head+'`',
          'Verdict: ***PASS***\nReviewed head: `'+head+'`',
          'Verdict: `PASS`\nReviewed head: `'+head+'`',
          'Verdict: **PASS** extra\nReviewed head: `'+head+'`',
          'A report said Verdict: **PASS**\nReviewed head: `'+head+'`',
          'Verdict: **PASS**\nQuoted Reviewed head: `'+head+'`',
          'Verdict: **PASS**\nReviewed head: ``'+head+'``',
          'Verdict: **PASS**\nReviewed head: `'+head,
          'Verdict: **PASS**\nReviewed head: '+head+'`',
          'Verdict: **PASS**\nReviewed head: **'+head+'**',
          'Verdict: **PASS**\nReviewed head: `'+head+'` extra',
          'Verdict: PASS\nVerdict: **PASS**\nReviewed head: `'+head+'`',
          'Verdict: PASS\nVerdict: **BLOCK*\nReviewed head: `'+head+'`',
          'Verdict: PASS\nReviewed head: '+head+'\nReviewed head: `'+head+'`',
          'Verdict: PASS\nReviewed head: '+head+'\nReviewed head: `'+head,
        ]
        for report in invalid:
            with self.subTest(report=report):
                root,records=self.markdown_review_records(report)
                self.assertIsNone(observed_review(records,root,head))

    def test_later_markdown_block_preserves_the_same_head_review_hold(self):
        head='1dd3d6251cb51b9879690960fbd8824e993a6db1'
        root,records=self.markdown_review_records('Verdict: **PASS**\nReviewed head: `'+head+'`')
        thread=records[-1]['value']['thread']
        thread['turns'].append({'id':'later-turn','status':'completed','items':[{'type':'agentMessage',
            'phase':'final_answer','text':'Verdict: **BLOCK**\nReviewed head: `'+head+'`'}]})
        self.assertIsNone(observed_review(records,root,head))

    def approved_query(self,endpoint):
        if '/reviews?' in endpoint:return [{'state':'APPROVED','commit_id':self.head,
             'user':{'login':'independent-reviewer'},'html_url':'https://github.com/owner/repo/pull/1#review'}]
        return self.query(endpoint)

    def test_external_approval_cannot_override_qualified_retained_block_or_malformed_verdict(self):
        for verdict in ('BLOCK','**BLOCK**','**PASS*'):
            records=json.loads(json.dumps(self.review))
            records[2]['value']['params']['item']['text']='Verdict: '+verdict+'\nReviewed head: '+self.head
            self.assertFalse(verify(self.admission,self.result,self.records+records,query=self.approved_query)['passed'])
        self.assertTrue(verify(self.admission,self.result,self.records,query=self.approved_query)['passed'])


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        VerificationTests.setUp(self)
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup)
        self.base=Path(temp.name).resolve();state=self.base/'runtime';state.mkdir(mode=0o700)
        executions=state/'executions';executions.mkdir(mode=0o700)
        self.root=executions/hashlib.sha256(b'job').hexdigest();self.root.mkdir(mode=0o700)
        for name in ('events','requests','answers','agent-state'):(self.root/name).mkdir(mode=0o700)
        self.config_file=self.base/'worker.json';self.config_file.write_text('{}');self.config_file.chmod(0o600)
        registry=self.base/'registry';registry.mkdir(mode=0o700)
        self.config=SimpleNamespace(path=str(self.config_file),root=state,executions=executions,
            value={'host_id':'development'},projects={'project':{'host':'LV426'}})
        self.admission['dispatch']={'execution_id':'job','task_id':'task','obligation_id':'obligation','definition_revision':2,
           'assignment':{'project':{'id':'project'},'criteria':[{'id':'c','description':'Deliver the agreed documentation'}]}}
        self.admission.update(dispatch_digest=digest(self.admission['dispatch']),config_file=str(self.config_file),
            runtime_root=str(state),registry=str(registry),deadline=time.time()-1)
        self.admission['project_profile']['host']='LV426'
        atomic(self.root/'admission.json',self.admission)
        self.proof={'generation':'old','boundary_id':'job:old','kind':'descendants_reaped','evidence':'trusted ECHILD'}
        self.marker={'generation':'old','boundary_id':'job:old','dispatch_digest':self.admission['dispatch_digest']}
        atomic(self.root/'launch-committed.json',self.marker);atomic(self.root/'cessation.json',self.proof)
        self.ceased={k:self.proof[k] for k in ('boundary_id','kind','evidence')}
        Journal(self.root).event({'kind':'stopped','cessation':self.ceased,'result':None,'verification':None,'reason':'Observed token budget exhausted'},terminal=True)
        self.original_stop=Journal(self.root).events()[0]
        self.original_records=self.records+self.review
        self.write_records(self.original_records)
        self.result['criteria'][0]['evidence']=['https://github.com/owner/repo/pull/1','runtime:command:cmd']
        self.proposal={'execution_id':'job','dispatch_digest':self.admission['dispatch_digest'],'result':self.result,
            'criterion_mapping':[{'id':'c','evidence':list(self.result['criteria'][0]['evidence'])}]}
        self.evidence=self.base/'proposal.json';atomic(self.evidence,self.proposal)
        self.ci_pass=False;self.queries=[]

    def write_records(self,records):
        path=self.root/'runtime.jsonl'
        if path.exists():path.unlink()
        for record in records:Journal(self.root).record(record['kind'],record['value'])

    def query(self,endpoint):
        self.queries.append(endpoint)
        value=VerificationTests.query(self,endpoint)
        if '/check-runs?' in endpoint and self.merge in endpoint and not self.ci_pass:
            value['check_runs'][0].update(status='in_progress',conclusion=None)
        return value

    def recover(self):
        return recover_result(self.config,'job',self.evidence,query=self.query)

    def test_budget_stop_recovery_waits_for_ci_then_reverifies_without_model_or_launch(self):
        before=(self.root/'runtime.jsonl').read_bytes()
        with patch('broker.launch') as launch,patch('broker.subprocess.Popen') as spawn:
            capsule=self.recover();launch.assert_not_called();spawn.assert_not_called()
        events=Journal(self.root).events()
        self.assertEqual(events[0],self.original_stop)
        self.assertEqual(events[1]['event']['kind'],'recovered_result')
        self.assertFalse(events[1]['event']['verification']['passed'])
        self.assertEqual(capsule['provenance']['origin'],'host_reconciliation')
        self.assertEqual(capsule['provenance']['result_digest'],digest(self.result))
        self.assertFalse((self.root/'result.json').exists())
        self.assertEqual((self.root/'runtime.jsonl').read_bytes(),before)
        self.assertEqual(self.queries.count('repos/owner/repo/pulls/1'),1)
        self.ci_pass=True
        with patch('broker.verify',side_effect=lambda a,r,records,**kw:verify(a,r,records,query=self.query,**kw)):
            stopped(self.root,'Rechecked actual CI',reverify=True)
        final=Journal(self.root).events()[-1]['event']
        self.assertEqual(final['kind'],'stopped');self.assertTrue(final['verification']['passed'])
        self.assertEqual(final['result'],capsule['result']);self.assertEqual(final['cessation'],self.ceased)

    def test_explicit_partial_criterion_is_imported_truthfully_and_never_accepted(self):
        self.proposal['result']['criteria'][0]['satisfied']=False
        self.proposal['result']['limitations']=['c: Required result tool was not called before budget cessation']
        atomic(self.evidence,self.proposal);self.ci_pass=True
        capsule=self.recover()
        self.assertFalse(capsule['result']['criteria'][0]['satisfied'])
        self.assertEqual(capsule['result']['limitations'],self.proposal['result']['limitations'])
        self.assertFalse(Journal(self.root).events()[-1]['event']['verification']['passed'])

    def test_capsule_crash_retry_and_lost_ack_do_not_duplicate_or_change_recovery_time(self):
        with patch('recovery.queue_recovery',side_effect=OSError('crash before event publication')):
            with self.assertRaises(OSError):self.recover()
        capsule=read(self.root/'recovered-result.json');before_queries=len(self.queries)
        self.assertIsNone(effective_result(self.root))
        with patch('recovery.time.time',return_value=capsule['provenance']['recovered_at']+1000):
            self.assertEqual(self.recover(),capsule)
        self.assertEqual(len(self.queries),before_queries)
        count=len(Journal(self.root).events());self.assertEqual(self.recover(),capsule)
        self.assertEqual(len(Journal(self.root).events()),count)

    def test_crash_before_capsule_publication_can_retry_with_fresh_actual_ci(self):
        from recovery import atomic as real_atomic
        def fail_capsule(path,value,**kwargs):
            if Path(path).name=='recovered-result.json':raise OSError('crash before capsule')
            return real_atomic(path,value,**kwargs)
        with patch('recovery.atomic',side_effect=fail_capsule):
            with self.assertRaises(OSError):self.recover()
        self.assertFalse((self.root/'recovered-result.json').exists())
        self.ci_pass=True;self.recover()
        self.assertTrue(Journal(self.root).events()[-1]['event']['verification']['passed'])

    def test_wrong_admission_mapping_or_existing_result_cannot_import(self):
        invalid=json.loads(json.dumps(self.proposal));invalid['dispatch_digest']='0'*64
        atomic(self.evidence,invalid)
        with self.assertRaisesRegex(ValueError,'immutable admission'):self.recover()
        invalid=json.loads(json.dumps(self.proposal));invalid['criterion_mapping'][0]['evidence']=['https://github.com/other/repo/pull/2']
        atomic(self.evidence,invalid)
        with self.assertRaisesRegex(ValueError,'documentary references'):self.recover()
        invalid=json.loads(json.dumps(self.proposal));invalid['result']['criteria'][0]['satisfied']=False
        atomic(self.evidence,invalid)
        with self.assertRaisesRegex(ValueError,'named limitation'):self.recover()
        atomic(self.evidence,self.proposal);atomic(self.root/'result.json',self.result)
        with self.assertRaisesRegex(ValueError,'agent-submitted'):self.recover()
        self.assertFalse((self.root/'recovered-result.json').exists());self.assertEqual(self.queries,[])

    def test_wrong_original_configuration_runtime_destination_or_future_host_rejected(self):
        original=self.config.path;other=self.base/'other.json';other.write_text('{}')
        self.config.path=str(other)
        with self.assertRaisesRegex(ValueError,'original private'):self.recover()
        self.config.path=original;other_root=self.base/'other-runtime';other_root.mkdir()
        self.config.root=other_root
        with self.assertRaisesRegex(ValueError,'original private'):self.recover()
        self.config.root=Path(self.admission['runtime_root']);self.config.projects['project']['host']='other-host'
        with self.assertRaisesRegex(ValueError,'project host'):self.recover()
        self.config.projects['project']['host']='LV426'
        self.admission['host_id']='different-route';atomic(self.root/'admission.json',self.admission)
        with self.assertRaisesRegex(ValueError,'host identity'):self.recover()
        self.assertFalse((self.root/'recovered-result.json').exists());self.assertEqual(self.queries,[])

    def test_cancellation_and_mismatched_or_non_descendant_proof_rejected(self):
        atomic(self.root/'cancel.json',{'cancel':True})
        with self.assertRaisesRegex(ValueError,'cancelled'):self.recover()
        (self.root/'cancel.json').unlink()
        for value in ({**self.proof,'generation':'wrong'},{**self.proof,'kind':'not_started'},
                      {**self.proof,'boundary_id':'other-boundary'}):
            atomic(self.root/'cessation.json',value)
            with self.assertRaisesRegex(ValueError,'descendant cessation'):self.recover()
        self.assertFalse((self.root/'recovered-result.json').exists());self.assertEqual(self.queries,[])

    def test_missing_local_canonical_or_independent_review_prerequisite_prevents_import(self):
        records=json.loads(json.dumps(self.original_records))
        records[3]['value']['source']['clean']=False;self.write_records(records)
        with self.assertRaisesRegex(ValueError,'prerequisites'):self.recover()
        self.write_records(self.records)
        with self.assertRaisesRegex(ValueError,'prerequisites'):self.recover()
        self.assertFalse((self.root/'recovered-result.json').exists())

    def test_changed_retry_proposal_cannot_replace_the_import(self):
        self.recover();self.proposal['result']['summary']='replacement';atomic(self.evidence,self.proposal)
        with self.assertRaisesRegex(ValueError,'immutable capsule'):self.recover()
        self.assertEqual(read(self.root/'recovered-result.json')['result']['summary'],'done')

    def test_capsule_read_is_bounded(self):
        from common import MAX_MESSAGE
        (self.root/'recovered-result.json').write_bytes(b'x'*(MAX_MESSAGE+1))
        with self.assertRaisesRegex(ValueError,'private state'):effective_result(self.root)


class RetainedReviewTests(unittest.TestCase):
    write_records=RecoveryTests.write_records
    query=RecoveryTests.query

    def setUp(self):
        RecoveryTests.setUp(self)
        self.proposal['result']['criteria'][0]['satisfied']=False
        self.proposal['result']['limitations']=['c: Original required tool was not called']
        atomic(self.evidence,self.proposal);self.ci_pass=True
        recover_result(self.config,'job',self.evidence,query=self.query)
        self.source_root=self.root;self.source_result=effective_result(self.source_root)
        atomic(self.source_root/'ack.json',{'sequence':len(Journal(self.source_root).events())})
        self.queries=[]
        self.review_root=self.new_review('review')

    def new_review(self,name,mutate=None):
        admission=json.loads(json.dumps(self.admission));dispatch=admission['dispatch']
        dispatch['execution_id']=name;dispatch['definition_revision']=3
        dispatch['assignment']['criteria']=[{'id':'review','description':'Review the acknowledged retained delivery'}]
        dispatch['assignment']['review_retained_work']={'source':{'execution_id':'job','result_digest':digest(self.source_result)},
             'summary':'Reviewed retained delivery under the new acceptance definition',
             'criteria':[{'id':'review','satisfied':True,'evidence':['https://github.com/owner/repo/pull/1','runtime:source:job']}]}
        admission['host_id']='development';admission['deadline']=time.time()+120
        if mutate:mutate(admission)
        admission['dispatch_digest']=digest(dispatch)
        root=self.config.executions/hashlib.sha256(name.encode()).hexdigest();root.mkdir(mode=0o700)
        for child in ('events','requests','answers','agent-state'):(root/child).mkdir(mode=0o700)
        atomic(root/'admission.json',admission)
        return root

    def test_readonly_review_uses_acknowledged_source_without_coding_or_protocol_copy(self):
        before=(self.source_root/'runtime.jsonl').read_bytes()
        with patch('broker.launch') as launch,patch('broker.subprocess.Popen') as spawn,patch('broker.Reservations.acquire') as reserve:
            run_retained_review(self.config,self.review_root,query=self.query)
            launch.assert_not_called();spawn.assert_not_called();reserve.assert_not_called()
        event=Journal(self.review_root).events()[-1]['event']
        self.assertEqual(event['cessation']['kind'],'not_started');self.assertTrue(event['verification']['passed'])
        self.assertEqual(event['result']['deliveries'],self.source_result['deliveries'])
        self.assertEqual(event['result']['criteria'][0]['id'],'review')
        self.assertEqual(event['result']['limitations'],[])
        self.assertFalse(self.source_result['criteria'][0]['satisfied'])
        self.assertEqual(effective_result(self.source_root),self.source_result)
        self.assertEqual((self.source_root/'runtime.jsonl').read_bytes(),before)
        self.assertFalse((self.review_root/'runtime.jsonl').exists())
        self.assertFalse((self.review_root/'launch-committed.json').exists())
        manifest=read(self.review_root/'retained-review.json')
        self.assertEqual(manifest['origin'],'host_retained_review')
        self.assertEqual(manifest['source_record_digest'],digest(Journal(self.source_root).records()))

    def test_retained_review_missing_ack_digest_task_project_host_or_policy_stops_unaccepted(self):
        cases={
          'digest':lambda a:a['dispatch']['assignment']['review_retained_work']['source'].update(result_digest='0'*64),
          'task':lambda a:a['dispatch'].update(task_id='other-task'),
          'project':lambda a:a['dispatch']['assignment'].update(project={'id':'other-project'}),
          'host':lambda a:a.update(host_id='other-host'),
          'policy':lambda a:a['project_profile']['verification']['repositories'][0].update(required_checks=['Different CI']),
          'criteria':lambda a:a['dispatch']['assignment']['review_retained_work']['criteria'][0].update(id='other-criterion'),
        }
        for name,mutate in cases.items():
            root=self.new_review('bad-'+name,mutate)
            run_retained_review(self.config,root,query=self.query)
            event=Journal(root).events()[-1]['event']
            self.assertFalse(event['verification']['passed']);self.assertIsNone(event['result'])
        atomic(self.source_root/'ack.json',{'sequence':0})
        root=self.new_review('unacknowledged');run_retained_review(self.config,root,query=self.query)
        self.assertIsNone(Journal(root).events()[-1]['event']['result'])
        self.assertEqual(self.queries,[])

    def test_retained_reviews_cannot_chain_or_use_unconfirmed_descendant_proof(self):
        source=read(self.source_root/'admission.json')
        source['dispatch']['assignment']['review_retained_work']={'source':{},'summary':'chain','criteria':[]}
        source['dispatch_digest']=digest(source['dispatch']);atomic(self.source_root/'admission.json',source)
        run_retained_review(self.config,self.review_root,query=self.query)
        self.assertIsNone(Journal(self.review_root).events()[-1]['event']['result'])
        atomic(self.source_root/'admission.json',self.admission)
        atomic(self.source_root/'cessation.json',{**self.proof,'kind':'not_started'})
        root=self.new_review('wrong-proof');run_retained_review(self.config,root,query=self.query)
        self.assertIsNone(Journal(root).events()[-1]['event']['result']);self.assertEqual(self.queries,[])

    def test_retained_review_cancellation_fences_before_and_after_reads(self):
        atomic(self.review_root/'cancel.json',{'cancel':True})
        run_retained_review(self.config,self.review_root,query=self.query)
        self.assertEqual(self.queries,[])
        root=self.new_review('cancel-during-read')
        def cancel_after_read(endpoint):
            value=self.query(endpoint);atomic(root/'cancel.json',{'cancel':True});return value
        run_retained_review(self.config,root,query=cancel_after_read)
        self.assertEqual(len(self.queries),1)
        self.assertFalse(Journal(root).events()[-1]['event']['verification']['passed'])
        self.assertEqual(read(root/'cessation.json')['kind'],'not_started')

    def test_pending_ci_retains_review_and_reverify_reads_original_attributed_records(self):
        self.ci_pass=False;run_retained_review(self.config,self.review_root,query=self.query)
        first=Journal(self.review_root).events()[-1]['event'];self.assertFalse(first['verification']['passed'])
        self.ci_pass=True
        with patch('broker.verify',side_effect=lambda a,r,records,**kw:verify(a,r,records,query=self.query,**kw)):
            stopped(self.review_root,'Rechecked CI without another job',reverify=True)
        final=Journal(self.review_root).events()[-1]['event']
        self.assertTrue(final['verification']['passed']);self.assertEqual(final['result'],first['result'])
        self.assertEqual(final['cessation'],first['cessation'])
        count=len(Journal(self.review_root).events());run_retained_review(self.config,self.review_root,query=self.query)
        self.assertEqual(len(Journal(self.review_root).events()),count)

    def test_changed_source_records_prevent_later_reverification(self):
        run_retained_review(self.config,self.review_root,query=self.query)
        Journal(self.source_root).record('unexpected_host_change',{'value':'changed'})
        stopped(self.review_root,'Recheck changed evidence',reverify=True)
        self.assertFalse(Journal(self.review_root).events()[-1]['event']['verification']['passed'])

    def test_worker_branches_before_launch_and_persists_cancellation_first(self):
        self.config.admit=lambda dispatch:self.review_root
        worker=Worker(self.config);dispatch=Journal(self.review_root).admission['dispatch']
        response={'acknowledgements':[],'dispatches':[dispatch],
             'controls':[{'execution_id':dispatch['execution_id'],'cancel':True,'answers':[]}]}
        with (patch.object(worker,'request',return_value=response),patch('worker.launch') as launch,
              patch('worker.run_retained_review',side_effect=lambda c,r:run_retained_review(c,r,query=self.query))):
            worker.exchange();launch.assert_not_called()
        self.assertEqual(self.queries,[])
        self.assertTrue(read(self.review_root/'cancel.json')['cancel'])
        self.assertEqual(read(self.review_root/'cessation.json')['kind'],'not_started')


class SafeGitTransportTests(unittest.TestCase):
    def test_failed_confined_helper_retains_bounded_diagnostic(self):
        import safe_git
        pipe=safe_git.Pipe(['/usr/bin/python3','-I','-c',
            'import sys;sys.stderr.write("fixture namespace setup failure\\n");sys.exit(1)'],
            {'PATH':'/usr/bin:/bin','LANG':'C'},[],time.monotonic()+3)
        try:
            with self.assertRaisesRegex(ValueError,'helper stderr:.*fixture namespace setup failure'):
                pipe.take(delimiter=b'\n')
        finally:pipe.close()

    def test_helper_diagnostic_overflow_fails_without_pipe_backpressure(self):
        import safe_git
        pipe=safe_git.Pipe(['/usr/bin/python3','-I','-c',
            'import sys;sys.stderr.write("X"*65536);sys.stderr.flush()'],
            {'PATH':'/usr/bin:/bin','LANG':'C'},[],time.monotonic()+3)
        try:
            with self.assertRaisesRegex(ValueError,'diagnostic exceeds bound'):
                pipe.take(delimiter=b'\n')
            self.assertEqual(len(pipe.diagnostic),safe_git.MAX_DIAGNOSTIC)
        finally:pipe.close()


class SafeGitEarlyTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.base=Path(self.temp.name).resolve();self.repo=self.base/'repo';self.repo.mkdir()
        self.private=self.base/'private';self.private.mkdir(mode=0o700)
        self.environment={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C','GIT_CONFIG_NOSYSTEM':'1',
                          'GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_SYSTEM':'/dev/null','GIT_TERMINAL_PROMPT':'0'}
        self.git('init','--quiet')
        (self.repo/'source.txt').write_text('committed source\n')
        (self.repo/'.gitignore').write_text('/generated/\n')
        self.git('add','source.txt','.gitignore')
        self.git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--quiet','-m','Fixture source')
        self.common=str((self.repo/'.git').resolve())
        self.profile={'write_roots':[str(self.repo)],'git_common_dirs':[self.common],
            'verification':{'repositories':[{'repository':'fixture/repo','checkout':str(self.repo),'git_common_dir':self.common}]}}

    def git(self,*args,cwd=None):
        return subprocess.check_output(['/usr/bin/git','-C',str(cwd or self.repo),*args],env=self.environment,
            stderr=subprocess.PIPE,timeout=5).decode().strip()

    def observe(self,cwd=None):
        return source_observation(self.profile,str(cwd or self.repo),self.private,full=True)

    def test_fsmonitor_command_is_never_executed_by_host_observation(self):
        marker=self.base/'marker';sentinel=self.base/'host-sentinel';sentinel.write_text('private fixture data')
        helper=self.base/'fsmonitor';helper.write_text('#!/bin/sh\ncat '+str(sentinel)+' > '+str(marker)+'\n')
        helper.chmod(0o700);self.git('config','core.fsmonitor',str(helper))
        observation=self.observe()
        self.assertTrue(observation['available'],observation);self.assertTrue(observation['clean'])
        self.assertFalse(marker.exists())

    def test_assume_unchanged_cannot_hide_dirty_tracked_source(self):
        self.git('update-index','--assume-unchanged','source.txt')
        (self.repo/'source.txt').write_text('changed while index flag claims unchanged\n')
        observation=self.observe()
        self.assertTrue(observation['available'],observation);self.assertFalse(observation['clean'])

    def test_real_linked_worktree_has_the_same_clean_commit_identity(self):
        linked=self.base/'linked';self.git('worktree','add','--quiet','-b','fixture-linked',str(linked))
        self.profile['write_roots'].append(str(linked))
        observation=self.observe(linked)
        self.assertTrue(observation['available'],observation);self.assertTrue(observation['clean'])
        self.assertEqual(observation['head'],self.git('rev-parse','HEAD'))
        self.assertEqual(observation['tree'],self.git('rev-parse','HEAD^{tree}'))

    def commit_fixture(self,*paths):
        self.git('add',*paths)
        self.git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--quiet','-m','Fixture policy')

    def test_clean_and_process_filters_never_execute_or_hide_changed_bytes(self):
        (self.repo/'.gitattributes').write_text('source.txt filter=malicious\n')
        self.commit_fixture('.gitattributes')
        marker=self.base/'filter-marker';sentinel=self.base/'filter-sentinel';sentinel.write_text('private fixture')
        helper=self.base/'filter';helper.write_text('#!/bin/sh\ncat '+str(sentinel)+' > '+str(marker)+'\nprintf "committed source\\n"\n')
        helper.chmod(0o700)
        self.git('config','filter.malicious.clean',str(helper))
        self.git('config','filter.malicious.process',str(helper))
        self.git('config','filter.malicious.required','true')
        (self.repo/'source.txt').write_text('different raw source\n')
        observation=self.observe()
        self.assertTrue(observation['available']);self.assertFalse(observation['clean']);self.assertFalse(marker.exists())

    def test_skip_worktree_and_poisoned_index_do_not_change_raw_tree_evidence(self):
        self.git('update-index','--skip-worktree','source.txt')
        (self.repo/'source.txt').write_text('dirty despite skip-worktree\n')
        self.assertFalse(self.observe()['clean'])
        (self.repo/'source.txt').write_text('committed source\n')
        # The task index is neither executed nor used as the candidate identity.
        (self.repo/'.git/index').write_bytes(b'not a Git index')
        self.assertTrue(self.observe()['clean'])

    def test_only_verified_committed_ignore_policy_hides_generated_output(self):
        generated=self.repo/'generated';generated.mkdir();(generated/'large-output').write_text('generated bytes')
        self.assertTrue(self.observe()['clean'])
        (self.repo/'unexpected-source.rs').write_text('unexpected source')
        (self.repo/'.git/info/exclude').write_text('*\n')
        global_ignore=self.base/'global-ignore';global_ignore.write_text('*\n')
        self.git('config','core.excludesFile',str(global_ignore))
        (self.repo/'.gitignore').write_text('*\n')
        self.assertFalse(self.observe()['clean'])
        (self.repo/'.gitignore').write_text('/generated/\n')
        self.assertFalse(self.observe()['clean'])

    def test_config_includes_and_inherited_git_overrides_are_not_consumed(self):
        marker=self.base/'include-marker';sentinel=self.base/'include-sentinel';sentinel.write_text('private fixture')
        helper=self.base/'included-hook';helper.write_text('#!/bin/sh\ncat '+str(sentinel)+' > '+str(marker)+'\n');helper.chmod(0o700)
        included=self.base/'included-config';included.write_text('[core]\nfsmonitor = '+str(helper)+'\n')
        self.git('config','include.path',str(included))
        with patch.dict(os.environ,{'GIT_CONFIG_GLOBAL':str(included),'GIT_CONFIG_COUNT':'1',
                    'GIT_CONFIG_KEY_0':'core.fsmonitor','GIT_CONFIG_VALUE_0':str(helper),'GIT_DIR':str(self.base/'not-repo')}):
            observation=self.observe()
        self.assertTrue(observation['clean']);self.assertFalse(marker.exists())

    def test_packed_reference_is_resolved_as_data_and_aliased_head_is_unavailable(self):
        self.git('pack-refs','--all')
        self.assertTrue(self.observe()['clean'])
        sentinel=self.base/'head-sentinel';sentinel.write_text('not a source reference')
        head=self.repo/'.git/HEAD';head.unlink();head.symlink_to(sentinel)
        self.assertFalse(self.observe()['available'])

    def test_alternates_reftable_and_unregistered_backlinks_fail_closed(self):
        alternate=self.repo/'.git/objects/info/alternates';alternate.write_text(str(self.base)+'\n')
        self.assertFalse(self.observe()['available']);alternate.unlink()
        reftable=self.repo/'.git/reftable';reftable.mkdir()
        self.assertFalse(self.observe()['available']);reftable.rmdir()
        linked=self.base/'linked';self.git('worktree','add','--quiet','-b','fixture-bad-link',str(linked))
        self.profile['write_roots'].append(str(linked))
        gitdir=Path((linked/'.git').read_text().strip()[8:]);(gitdir/'gitdir').write_text(str(self.repo/'.git')+'\n')
        self.assertFalse(self.observe(linked)['available'])

    def test_symlink_target_text_is_compared_without_following_host_sentinel(self):
        sentinel=self.base/'symlink-sentinel';sentinel.write_text('private fixture bytes')
        link=self.repo/'link';link.symlink_to(sentinel)
        self.commit_fixture('link')
        self.assertTrue(self.observe()['clean'])
        link.unlink();link.symlink_to(self.base/'different-target')
        self.assertFalse(self.observe()['clean'])

    def test_source_path_replacement_and_limits_never_publish_partial_clean_proof(self):
        import safe_git
        original=safe_git.os.read;done=False
        def change_during_read(descriptor,count):
            nonlocal done
            target=os.readlink('/proc/self/fd/'+str(descriptor))
            if target==str(self.repo/'source.txt') and not done:
                done=True;(self.repo/'source.txt').write_text('mutated during read\n')
            return original(descriptor,count)
        with patch('safe_git.os.read',side_effect=change_during_read):
            self.assertFalse(self.observe()['available'])
        with patch('safe_git.MAX_FILES',0):self.assertFalse(self.observe()['available'])
        with patch('safe_git.MAX_SECONDS',0):self.assertFalse(self.observe()['available'])

    def test_truncated_or_identity_mismatched_object_is_unavailable(self):
        import zlib
        tree=self.git('rev-parse','HEAD^{tree}')
        path=self.repo/'.git/objects'/tree[:2]/tree[2:]
        path.chmod(0o600)
        path.write_bytes(zlib.compress(b'tree 0\0'))
        self.assertFalse(self.observe()['available'])

    def test_metadata_tier_never_claims_clean_and_leaves_fsmonitor_unexecuted(self):
        (self.repo/'source.txt').write_text('dirty')
        observation=source_observation(self.profile,str(self.repo),self.private,full=False)
        self.assertTrue(observation['available']);self.assertIsNone(observation['clean'])
        self.assertEqual(observation['capture'],'verified_metadata')

    def test_unsupported_fd_binding_reports_unavailable_without_clean_evidence(self):
        helper=self.base/'unsupported-bwrap';helper.write_text('#!/bin/sh\nprintf "usage: fixture bubblewrap\\n"\n')
        helper.chmod(0o700)
        observation=source_observation(self.profile,str(self.repo),self.private,str(helper),full=True)
        self.assertFalse(observation['available']);self.assertNotIn('clean',observation)
        self.assertIn('FD-consuming',observation['detail'])


if __name__=='__main__':
    unittest.main()
