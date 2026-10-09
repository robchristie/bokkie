"""No-model report provenance, policy and independent-review falsification tests."""
import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
from types import SimpleNamespace
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).parent))
from broker import Broker, tools, cli_component, STARTUP_STDERR_BYTES
from common import Config, atomic, checkpoint_bounds, digest, encoded, read, result_bounds
from evidence_report import EvidenceStore, NoRedirect, bounded_process, captured_content, endpoint, github_get, _github_get, selector
from evidence_policy import derived_roles, environment, mounts, policy, closed_mcp_inventory, routing_proof, public_ca_paths, code_mode_host_path, companion_readiness, reviewer_selection_proof, permission_profiles, prepare_task_config, profile_proof, file_feature_proof, turn_policy_proof, retain_turn_policy, FILE_READ_FEATURES, ROOT_PERMISSIONS, REVIEW_PERMISSIONS
from verification import verify


class EvidenceReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.root = self.base/'job';self.root.mkdir()
        for name in ('events','requests','answers','agent-state'):(self.root/name).mkdir()
        self.workspace = self.base/'workspace';self.workspace.mkdir()
        self.scratch = self.base/'scratch';self.scratch.mkdir()
        self.profile = {'id':'project','revision':1,'host':'dev','workspace':str(self.workspace),
            'result_contract':'evidence_report','profile_revision':'report-v1','permitted_actions':['inspect','verify'],
            'write_roots':[str(self.scratch)],'scratch':str(self.scratch),'resources':[str(self.scratch)],
            'read_roots':[str(self.workspace)],'git_common_dirs':[], 'network_access':False,
            'limits':{'max_seconds':120,'max_turns':4,'max_tokens':100000},
            'source_read':{'repositories':['owner/repo'],'gh':'/trusted/gh','config_dir':'/trusted/config',
                           'cwd':'/trusted/cwd','max_requests':4,'max_bytes':100000,'timeout_seconds':5},
            'reviewer':{'role':'evidence_reviewer','model':'review-model','reasoning_effort':'high'}}
        self.dispatch = {'execution_id':'report-job','assignment':{'result_contract':'evidence_report',
            'repository_scope':['owner/repo'],'criteria':[{'id':'receipt','description':'Assess retained receipts'}],
            'limits':self.profile['limits']}}
        self.admission = {'dispatch':self.dispatch,'dispatch_digest':digest(self.dispatch),
            'project_profile':self.profile,'deadline':time.time()+60,'registry':str(self.base/'registry'),
            'codex':str(Path('/usr/bin/true').resolve())}
        atomic(self.root/'admission.json',self.admission)
        self.store = EvidenceStore(self.root,self.admission)
        self.selected = {'kind':'repository_file','repository':'owner/repo','commit':'a'*40,'path':'docs/receipt.md'}
        self.raw = b'Retained coverage and reporting receipt.\n'

    def response(self, raw=None):
        raw = self.raw if raw is None else raw
        return {'type':'file','encoding':'base64','path':self.selected['path'],'size':len(raw),
            'sha':hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),
            'html_url':'https://github.com/owner/repo/blob/'+self.selected['commit']+'/docs/receipt.md',
            'content':base64.b64encode(raw).decode()}

    def captured(self):
        return self.store.capture(self.selected,query=lambda *_:self.response())

    def sealed(self):
        source = self.captured()['source']
        return self.store.seal('The receipt establishes coverage; benefit is inconclusive.',[source['id']])

    def result(self, report):
        return {'summary':'Report complete; subject benefit is inconclusive.',
                'criteria':[{'id':'receipt','satisfied':True,'evidence':['Assessed retained source bytes']}],
                'deliveries':[],'limitations':[],'report':report}

    def policy_record(self, thread_id, turn_id, profiles, home, reviewer=False):
        name=REVIEW_PERMISSIONS if reviewer else ROOT_PERMISSIONS
        entries=[{'path':{'type':'special','value':{'kind':'minimal'}} if key==':minimal' else
                  {'type':'path','path':key},'access':access} for key,access in profiles[name]['filesystem'].items()]
        context={'turn_id':turn_id,'root_turn_id':turn_id,'approval_policy':'never','approvals_reviewer':'user',
            'permission_profile':{'type':'managed','file_system':{'type':'restricted','entries':entries},'network':'restricted'},
            'file_system_sandbox_policy':{'kind':'restricted','entries':entries},
            'sandbox_policy':{'type':'read-only'} if reviewer else {'type':'workspace-write','writable_roots':[str(self.scratch)],
                'network_access':False,'exclude_tmpdir_env_var':True,'exclude_slash_tmp':True},
            'model':'review-model' if reviewer else 'implementation-model','effort':'high' if reviewer else 'medium'}
        metadata={'id':thread_id,'cli_version':'0.160.1'}
        if reviewer:metadata.update(parent_thread_id='root',agent_role='evidence_reviewer',
            source={'subagent':{'thread_spawn':{'parent_thread_id':'root','agent_role':'evidence_reviewer'}}})
        path=self.root/'agent-state'/'sessions'/(thread_id+'.jsonl');path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps({'type':'session_meta','payload':metadata})+'\n'+json.dumps({'type':'turn_context','payload':context})+'\n')
        thread={'id':thread_id,'path':str(home/'sessions'/path.name),'parentThreadId':'root' if reviewer else None,
            'agentRole':'evidence_reviewer' if reviewer else None,'model':context['model'],'reasoningEffort':context['effort'],
            'sandbox':{'type':'workspaceWrite','writableRoots':[str(self.scratch)],'networkAccess':False,
                       'excludeTmpdirEnvVar':True,'excludeSlashTmp':True}}
        return {'kind':'report_turn_policy','value':retain_turn_policy(self.root,thread,turn_id,profiles,home,reviewer)}

    def records(self, report):
        completed_at=self.store.seal_provenance(report['digest'])['completed_at']
        home=self.base/'codex-home';profiles=permission_profiles(self.admission,self.profile,home)
        qualification = {'model_calls':0,'observations':[],'permission_profiles':profiles,'codex_home':str(home)}
        atomic(self.root/'evidence-policy.json',qualification)
        return [
            {'kind':'evidence_policy_qualified','value':qualification},
            {'kind':'reviewer_profile','value':self.profile['reviewer']},
            {'kind':'thread_identity','value':{'thread_id':'root','settings':{'model':'implementation-model',
                'reasoningEffort':'medium','activePermissionProfile':{'id':ROOT_PERMISSIONS,'extends':None},
                'sandbox':{'type':'workspaceWrite','writableRoots':[str(self.scratch)],'networkAccess':False,
                    'excludeTmpdirEnvVar':True,'excludeSlashTmp':True}}}},
            {'kind':'evidence_report_sealed','value':{'digest':report['digest'],'source_manifest_digest':report['source_manifest_digest'],'completed_at':completed_at}},
            {'kind':'protocol_event','value':{'method':'item/started','params':{'threadId':'root','startedAtMs':(completed_at+1)*1000,'item':{'type':'subAgentActivity','kind':'started','agentThreadId':'review-child'}}}},
            {'kind':'protocol_event','value':{'method':'turn/completed','params':{'threadId':'root','turn':{'id':'root-turn','status':'completed'}}}},
            self.policy_record('root','root-turn',profiles,home),
            self.policy_record('review-child','review-turn',profiles,home,True),
            {'kind':'child_thread_read','value':{'child_id':'review-child','include_turns':True,'thread':{
                'id':'review-child','createdAt':completed_at+1,'parentThreadId':'root','agentRole':'evidence_reviewer','model':'review-model',
                'reasoningEffort':'high','turns':[{'id':'review-turn','status':'completed','items':[
                    {'type':'agentMessage','phase':'final_answer','text':'Verdict: PASS\nReviewed report: '+report['digest']+'\nReviewed sources: '+report['source_manifest_digest']}]}]}}}]

    def host_config(self):
        home=self.base/'home';home.mkdir();codex=home/'codex';codex.mkdir()
        (codex/'config.toml').write_text('model="implementation-model"\nmodel_reasoning_effort="xhigh"\n')
        trusted=self.base/'trusted';trusted.mkdir();gh_config=trusted/'gh-config';gh_config.mkdir()
        cwd=trusted/'source-client';cwd.mkdir();reviewer=trusted/'reviewer.toml'
        reviewer.write_text('name="evidence_reviewer"\ndescription="Independent sealed report reviewer"\ndeveloper_instructions="Inspect immutable report and source bytes"\nmodel="review-model"\nmodel_reasoning_effort="high"\nsandbox_mode="read-only"\napproval_policy="never"\napprovals_reviewer="user"\n[sandbox_read_only]\nnetwork_access=false\n')
        token=self.base/'token';token.write_text('0'*64);token.chmod(0o600)
        profile=copy.deepcopy(self.profile)
        profile['source_read'].update(gh=str(Path('/usr/bin/true').resolve()),config_dir=str(gh_config),cwd=str(cwd))
        profile['reviewer']['config_file']=str(reviewer)
        value={'version':1,'host_id':'dev','server_url':'http://127.0.0.1:7744',
            'runtime_root':str(self.base/'state'),'token_file':str(token),'codex':str(Path('/usr/bin/true').resolve()),
            'bwrap':str(Path(shutil.which('bwrap') or '/usr/bin/true').resolve()),'projects':[profile]}
        path=self.base/'host.json';atomic(path,value)
        return path,home,codex,value

    def test_real_report_configuration_closes_writable_scope_and_host_credentials(self):
        path,home,codex,value=self.host_config()
        with patch('common.Path.home',return_value=home),patch.dict(os.environ,{'CODEX_HOME':str(codex)}):
            configured=Config(path)
            self.assertEqual(configured.projects['project']['resources'],[str(self.scratch)])
            for mutation,match in [({'write_roots':[str(self.workspace),str(self.scratch)]},'scratch'),
                ({'network_access':True},'no task network'),({'readonly_mcp_servers':['inherited']},'no task network'),
                ({'read_roots':[str(self.base)]},'exact selected read mount'),
                ({'read_roots':[str(self.workspace),str(codex)]},'protected host state'),
                ({'read_roots':[str(self.workspace),value['projects'][0]['source_read']['config_dir']]},'protected host state')]:
                invalid=copy.deepcopy(value);invalid['projects'][0].update(mutation);atomic(path,invalid)
                with self.subTest(mutation=mutation),self.assertRaisesRegex(ValueError,match):Config(path)

    def test_report_admission_scope_contract_and_saved_budget_remain_immutable(self):
        path,home,codex,value=self.host_config()
        with patch('common.Path.home',return_value=home),patch.dict(os.environ,{'CODEX_HOME':str(codex)}):configured=Config(path)
        dispatch=copy.deepcopy(self.dispatch)
        dispatch.update(task_id='task',obligation_id='obligation',definition_revision=1,profile_revision='report-v1',
                        admitted_at=int(time.time()),deadline_at=int(time.time())+60)
        dispatch['assignment'].update(project={'id':'project','revision':1,'registration':{'host':'dev','workspace':str(self.workspace)}},permitted_actions=['inspect'])
        original=configured.admit(dispatch);saved=read(original/'admission.json')
        configured.projects['project']['limits']['max_tokens']=16000000
        self.assertEqual(read(configured.admit(dispatch)/'admission.json'),saved)
        for mutation in ({'result_contract':'engineering_delivery'}, {'repository_scope':['other/repo']}, {'permitted_actions':['ordinary_code_delivery']}):
            invalid=copy.deepcopy(dispatch);invalid['execution_id']='new-'+digest(mutation);invalid['assignment'].update(mutation)
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):configured.admit(invalid)

    def test_typed_source_interface_excludes_urls_methods_traversal_and_other_repositories(self):
        self.assertEqual(endpoint(selector(self.selected,['owner/repo'])), 'repos/owner/repo/contents/docs/receipt.md?ref='+'a'*40)
        mutations=[{'url':'https://github.com/owner/repo'}, {'method':'POST'}, {'repository':'other/repo'},
                   {'commit':'main'}, {'path':'../receipt'}, {'path':'docs/%2fsecret'}, {'path':'docs//receipt'},
                   {'path':'/absolute'}, {'path':'docs/./receipt'}, {'path':'docs/\\receipt'}, {'kind':'command'}]
        for mutation in mutations:
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):
                selector(self.selected|mutation,['owner/repo'])

    def test_repository_content_requires_regular_selected_entry_and_exact_blob(self):
        for mutation in ({'type':'symlink'}, {'type':'dir'}, {'encoding':'none'}, {'path':'other.md'},
                         {'target':'elsewhere'}, {'submodule_git_url':'remote'}, {'size':2},
                         {'html_url':'https://github.com/other/repo/blob/'+'a'*40+'/docs/receipt.md'}, {'sha':'b'*40}):
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):
                captured_content(self.selected,self.response()|mutation)

    def test_capture_replay_retains_first_mutable_comment_and_all_provenance(self):
        selected={'kind':'issue_comment','repository':'owner/repo','comment_id':17}
        comment={'id':17,'html_url':'https://github.com/owner/repo/pull/3#issuecomment-17',
            'issue_url':'https://api.github.com/repos/owner/repo/issues/3','body':'first observation',
            'user':{'login':'receipt-author'},'created_at':'2026-10-09T00:00:00Z','updated_at':'2026-10-09T01:00:00Z'}
        calls=[]
        def query(*args):calls.append(args);return comment
        first=self.store.capture(selected,query=query)
        comment['body']='later edited observation';comment['updated_at']='2026-10-10T01:00:00Z'
        repeated=self.store.capture(selected,query=query)
        self.assertEqual(first,repeated);self.assertEqual(len(calls),1)
        capsule=read(self.store.store/'captures'/(first['source']['id']+'.json'))
        self.assertEqual(capsule['metadata'],{'author':'receipt-author','created_at':'2026-10-09T00:00:00Z','updated_at':'2026-10-09T01:00:00Z'})
        self.assertEqual(capsule['admission_digest'],digest(self.admission))
        self.assertEqual(capsule['dispatch_digest'],digest(self.dispatch))
        self.assertEqual(capsule['execution_id'],'report-job')
        self.assertEqual(digest(capsule),first['source']['id'])
        self.assertEqual(base64.b64decode(capsule['content_base64']),b'first observation')

    def test_full_capsule_identity_changes_for_same_bytes_in_different_admission(self):
        first=self.captured()['source']
        other=copy.deepcopy(self.admission);other['dispatch']['execution_id']='different-job';other['dispatch_digest']=digest(other['dispatch'])
        (self.base/'other-job').mkdir()
        second=EvidenceStore(self.base/'other-job',other)
        second_id=second.capture(self.selected,query=lambda *_:self.response())['source']['id']
        self.assertNotEqual(first['id'],second_id)

    def test_finite_failed_source_reads_count_and_no_read_is_replayed(self):
        self.store.policy['max_requests']=1
        def failed(*_):raise ValueError('unavailable')
        with self.assertRaisesRegex(ValueError,'unavailable'):self.store.capture(self.selected,query=failed)
        with self.assertRaisesRegex(ValueError,'budget exhausted'):self.captured()

    def test_source_length_and_retained_budget_are_bounded(self):
        self.store.policy['max_bytes']=3
        with self.assertRaisesRegex(ValueError,'byte budget'):self.captured()
        with self.assertRaisesRegex(ValueError,'exceed bound'):
            captured_content(self.selected,self.response(b'x'*(256*1024+1)))

    def test_seal_domains_and_repairs_create_distinct_immutable_report_identities(self):
        report=self.sealed()
        manifest={'format':'evidence-source-manifest-v1','sources':report['sources']}
        body={key:report[key] for key in ('format','markdown','source_manifest_digest')}
        self.assertEqual(report['source_manifest_digest'],digest(manifest));self.assertEqual(report['digest'],digest(body))
        self.assertEqual(self.store.report(report['digest']),report)
        repaired=self.store.seal('A repaired conclusion.',[report['sources'][0]['id']])
        self.assertNotEqual(report['digest'],repaired['digest'])
        self.assertEqual(self.store.report(report['digest']),report)
        with self.assertRaises(ValueError):self.store.seal('x',[report['sources'][0]['id']]*2)
        with self.assertRaises(ValueError):self.store.seal('x',['f'*64])

    def test_report_reseal_preserves_original_immutable_completion_second(self):
        source=self.captured()['source']['id']
        with patch('evidence_report.time.time',return_value=1000.4):report=self.store.seal('Bound report.',[source])
        original=self.store.seal_provenance(report['digest'])
        with patch('evidence_report.time.time',return_value=2000.7):self.store.seal('Bound report.',[source])
        self.assertEqual(self.store.seal_provenance(report['digest']),original)
        self.assertEqual(original['completed_at'],1000)
        self.assertEqual(original['report_digest'],report['digest'])
        self.assertEqual(original['source_manifest_digest'],report['source_manifest_digest'])

    def test_seal_reply_wait_is_bounded_cancellable_and_consumes_no_inference(self):
        broker=Broker(self.root);clock=[1000.8]
        broker.admission['deadline']=1100
        def advance(seconds):clock[0]+=seconds
        with patch('broker.time.time',side_effect=lambda:clock[0]),patch('broker.time.monotonic',side_effect=lambda:clock[0]),patch('broker.time.sleep',side_effect=advance):
            broker.wait_review_window(1000)
        self.assertAlmostEqual(clock[0],1001)
        atomic(self.root/'cancel.json',{'cancel':True})
        with self.assertRaises(InterruptedError):broker.wait_review_window(1000)
        (self.root/'cancel.json').unlink()
        with patch('broker.time.time',return_value=999.0),self.assertRaisesRegex(ValueError,'bounded window'):broker.wait_review_window(1000)
        self.assertEqual(broker.tokens,{})

    def test_report_capsule_and_mirror_tampering_never_qualify(self):
        report=self.sealed();records=self.records(report)
        self.assertTrue(verify(self.admission,self.result(report),records,root=self.root)['passed'])
        identity=report['sources'][0]['id'];path=self.store.store/'captures'/(identity+'.json')
        original=read(path);changed=original|{'metadata':{'author':'forged'}};atomic(path,changed)
        self.assertFalse(verify(self.admission,self.result(report),records,root=self.root)['passed'])
        atomic(path,original)
        mirror=self.store.mirror/'reports'/(report['digest']+'.json');atomic(mirror,report|{'markdown':'tampered'})
        self.assertFalse(verify(self.admission,self.result(report),records,root=self.root)['passed'])

    def test_independent_completed_post_seal_child_is_required_for_both_digests(self):
        report=self.sealed();baseline=self.records(report)
        self.assertTrue(verify(self.admission,self.result(report),baseline,root=self.root)['passed'])
        changes=['parent','role','model','effort','unfinished','root','wrong-report','wrong-sources','quoted','duplicate','pre-seal','no-profile','no-policy','no-root','late-old-child','same-second','no-creation','no-start-time','wrong-start-kind','wrong-seal-time']
        for change in changes:
            records=copy.deepcopy(baseline);thread=records[-1]['value']['thread'];turn=thread['turns'][0];item=turn['items'][0]
            if change=='parent':thread['parentThreadId']='other'
            elif change=='role':thread['agentRole']='worker'
            elif change=='model':thread['model']='implementation-model'
            elif change=='effort':thread['reasoningEffort']='low'
            elif change=='unfinished':turn['status']='inProgress'
            elif change=='root':thread['id']='root';records[-1]['value']['child_id']='root'
            elif change=='wrong-report':item['text']=item['text'].replace(report['digest'],'0'*64)
            elif change=='wrong-sources':item['text']=item['text'].replace(report['source_manifest_digest'],'0'*64)
            elif change=='quoted':item['text']='\n'.join('> '+line for line in item['text'].splitlines())
            elif change=='duplicate':item['text']+='\nVerdict: PASS'
            elif change=='pre-seal':records.insert(0,copy.deepcopy(records[4]))
            elif change=='no-profile':records.pop(1)
            elif change=='no-policy':records.pop(0)
            elif change=='late-old-child':thread['createdAt']=records[3]['value']['completed_at']-1
            elif change=='same-second':thread['createdAt']=records[3]['value']['completed_at']
            elif change=='no-creation':thread.pop('createdAt')
            elif change=='no-start-time':records[4]['value']['params'].pop('startedAtMs')
            elif change=='wrong-start-kind':records[4]['value']['params']['item']['kind']='interacted'
            elif change=='wrong-seal-time':records[3]['value']['completed_at']+=1
            with self.subTest(change=change):
                self.assertFalse(verify(self.admission,self.result(report),records,root=None if change=='no-root' else self.root)['passed'])

    def test_later_block_and_new_report_identity_invalidate_earlier_pass(self):
        report=self.sealed();records=self.records(report)
        thread=records[-1]['value']['thread'];later=copy.deepcopy(thread['turns'][0]);later['id']='repair-turn'
        later['items'][0]['text']=later['items'][0]['text'].replace('PASS','BLOCK');thread['turns'].append(later)
        self.assertFalse(verify(self.admission,self.result(report),records,root=self.root)['passed'])
        repaired=self.store.seal('Repaired report.',[report['sources'][0]['id']])
        self.assertFalse(verify(self.admission,self.result(repaired),self.records(report),root=self.root)['passed'])

    def test_original_completed_root_and_reviewer_turn_policies_are_required(self):
        report=self.sealed();baseline=self.records(report)
        for change in ('root-missing','child-missing','child-unavailable','wrong-turn','context-hash','parent','role','original-mutation'):
            records=copy.deepcopy(baseline)
            if change=='root-missing':records.pop(6)
            elif change=='child-missing':records.pop(7)
            elif change=='child-unavailable':records.insert(7,{'kind':'report_turn_policy_unavailable','value':{'thread_id':'review-child','turn_id':'review-turn'}})
            elif change=='wrong-turn':records[7]['value']['turn_id']='other-turn'
            elif change=='context-hash':records[7]['value']['context_sha256']='0'*64
            elif change=='parent':records[7]['value']['parent_thread_id']='other-root'
            elif change=='role':records[7]['value']['agent_role']='worker'
            elif change=='original-mutation':
                path=self.root/'agent-state'/'sessions'/'review-child.jsonl';raw=path.read_text()
                path.write_text(raw.replace('"network": "restricted"','"network": "enabled"'))
            with self.subTest(change=change):self.assertFalse(verify(self.admission,self.result(report),records,root=self.root)['passed'])
            if change=='original-mutation':path.write_text(raw)

    def test_turn_policy_rejects_broader_reads_network_approvals_and_arg0_aliases(self):
        home=self.base/'codex-home';profiles=permission_profiles(self.admission,self.profile,home)
        record=self.policy_record('review-child','review-turn',profiles,home,True)
        baseline=record['value']['context']
        exception={'path':{'type':'path','path':str(home/'tmp'/'arg0'/'codex-arg0Ab123')},'access':'read'}
        context=copy.deepcopy(baseline);context['permission_profile']['file_system']['entries'].append(exception)
        context['file_system_sandbox_policy']['entries']=copy.deepcopy(context['permission_profile']['file_system']['entries'])
        self.assertEqual(turn_policy_proof(context,profiles,home,True)['runtime_read_exceptions'],[exception['path']['path']])
        for change in ('root-read','scratch-write','network','approval','reviewer','different-projection','arg0-write','arg0-traversal','duplicate'):
            context=copy.deepcopy(baseline);entries=context['permission_profile']['file_system']['entries']
            if change=='root-read':entries.append({'path':{'type':'path','path':'/'},'access':'read'})
            elif change=='scratch-write':next(value for value in entries if value['path'].get('path')==str(self.scratch))['access']='write'
            elif change=='network':context['permission_profile']['network']='enabled'
            elif change=='approval':context['approval_policy']='on-request'
            elif change=='reviewer':context['approvals_reviewer']='auto_review'
            elif change=='different-projection':context['file_system_sandbox_policy']={'kind':'unrestricted'}
            elif change=='arg0-write':entries.append(exception|{'access':'write'})
            elif change=='arg0-traversal':entries.append({'path':{'type':'path','path':str(home/'tmp'/'arg0')+'/codex-arg0Ab/../auth.json'},'access':'read'})
            elif change=='duplicate':entries.append(copy.deepcopy(entries[0]))
            if change!='different-projection':context['file_system_sandbox_policy']['entries']=copy.deepcopy(entries)
            with self.subTest(change=change),self.assertRaises(ValueError):turn_policy_proof(context,profiles,home,True)

    def test_root_turn_projection_binds_supported_metadata_with_scratch_tmpdir(self):
        home=self.base/'codex-home';profiles=permission_profiles(self.admission,self.profile,home)
        record=self.policy_record('root','root-turn',profiles,home)
        context=copy.deepcopy(record['value']['context']);projection=copy.deepcopy(record['value']['root_sandbox'])
        projection['excludeTmpdirEnvVar']=False;context['sandbox_policy']['exclude_tmpdir_env_var']=False
        self.assertEqual(turn_policy_proof(context,profiles,home,root_sandbox=projection)['profile'],ROOT_PERMISSIONS)
        with self.assertRaises(ValueError):turn_policy_proof(context,profiles,home)
        with self.assertRaises(ValueError):turn_policy_proof(context,profiles,home,root_sandbox=record['value']['root_sandbox'])
        projection['writableRoots'].append(str(self.workspace))
        with self.assertRaises(ValueError):turn_policy_proof(context,profiles,home,root_sandbox=projection)

    def test_report_submission_uses_only_host_seal_and_root_checkpoint_is_nonterminal(self):
        report=self.sealed();broker=Broker(self.root);broker.thread='root';sent=[];broker.send=sent.append
        def call(tool,args,thread='root'):
            request={'id':len(sent)+1,'method':'item/tool/call','params':{'threadId':thread,'namespace':'bokkie_workspace','tool':tool,'arguments':args}}
            broker.request(request);return json.loads(sent[-1]['result']['contentItems'][0]['text'])
        checkpoint={'stage':'pilot-assessment','summary':'Receipts assessed','assessment':'inconclusive',
                    'evidence':['No comparable benefit measurement'],'next_action':'Ask whether the evidence gap satisfies the agreed assessment'}
        reply=call('checkpoint',checkpoint);self.assertFalse(reply['terminal']);self.assertIn('remaining_tokens',reply['budget'])
        call('checkpoint',checkpoint,'child');self.assertFalse(sent[-1]['result']['success'])
        call('wait_for_checks',{'repository':'owner/repo','revision':'a'*40});self.assertFalse(sent[-1]['result']['success'])
        forged=self.result(report);call('result',forged);self.assertFalse(sent[-1]['result']['success'])
        args={key:forged[key] for key in ('summary','criteria','limitations')};args['report_id']=report['digest']
        call('result',args);self.assertTrue(sent[-1]['result']['success']);self.assertEqual(broker.result,forged)
        self.assertEqual(read(self.root/'result.json')['report'],report)
        names={tool['name'] for tool in tools(True)[0]['tools']}
        self.assertNotIn('wait_for_checks',names);self.assertIn('capture_source',names)

    def test_checkpoint_bounds_and_result_digest_validation(self):
        with self.assertRaises(ValueError):checkpoint_bounds({'stage':'x'})
        report=self.sealed();result_bounds(self.result(report))
        changed=copy.deepcopy(report);changed['markdown']='changed'
        with self.assertRaises(ValueError):result_bounds(self.result(changed))

    def test_child_profiles_cannot_retain_inherited_network_apps_mcp_or_escalation(self):
        home=self.base/'codex';home.mkdir();agents=home/'agents';agents.mkdir()
        raw='description="Independent evidence reader"\nmodel="review-model"\nmodel_reasoning_effort="high"\ndeveloper_instructions="Keep judgement independent"\nsandbox_mode="danger-full-access"\napproval_policy="on-request"\nweb_search="live"\n[mcp_servers.role_only]\ncommand="/usr/bin/true"\n'
        profile=agents/'danger.toml';profile.write_text(raw)
        self.profile['reviewer']['config_file']=str(profile)
        overrides,identities=derived_roles(self.root,home,{'mcp_servers':{'inherited':{}},'plugins':{'plugin@market':{}}},self.profile)
        self.assertEqual(profile.read_text(),raw)
        for name in ('danger','evidence_reviewer'):
            parsed=tomllib.loads((self.root/'agent-state'/'agents'/(name+'.toml')).read_text())
            self.assertEqual(parsed['model'],'review-model');self.assertEqual(parsed['model_reasoning_effort'],'high')
            self.assertEqual(parsed['sandbox_mode'],'read-only');self.assertEqual(parsed['approval_policy'],'never')
            self.assertFalse(parsed['sandbox_read_only']['network_access']);self.assertEqual(parsed['web_search'],'disabled')
            self.assertFalse(parsed['features']['apps']);self.assertNotIn('mcp_servers',parsed)
            self.assertFalse(parsed['plugins']['plugin@market']['enabled'])
            self.assertIn('agents.'+name+'.config_file',overrides)
        self.assertEqual(len(identities),2)

    @unittest.skipUnless(shutil.which('bwrap') and Path('/usr/bin/python3').exists(), 'requires Bubblewrap and system Python')
    def test_actual_minimal_mounts_protect_sources_mirror_and_mask_host_socket(self):
        # /tmp and /var/tmp are intentionally masked; selected roots use a
        # separate disposable local store so they cannot alias those controls.
        with tempfile.TemporaryDirectory(dir='/dev/shm') as directory:
            base=Path(directory);workspace=base/'workspace';workspace.mkdir()
            scratch=base/'scratch';scratch.mkdir();root=base/'job';root.mkdir()
            (root/'agent-state').mkdir();(root/'evidence-mirror').mkdir()
            home=base/'codex';home.mkdir()
            trust_asset=base/'public-ca.pem';trust_asset.write_text('disposable public trust asset')
            selected=self.profile|{'workspace':str(workspace),'scratch':str(scratch),'read_roots':[str(workspace)]}
            admission={'bwrap':shutil.which('bwrap'),'codex':'/usr/bin/python3'}
            with tempfile.TemporaryDirectory() as control_root,socket.socket(socket.AF_UNIX) as control:
                address=str(Path(control_root)/'socket');control.bind(address);control.listen()
                with socket.socket(socket.AF_UNIX) as positive:positive.connect(address)
                code="""import json,socket,sys
from pathlib import Path
values={}
for key,path in json.loads(sys.argv[1]).items():
 try: Path(path).write_text('probe');values[key]=True
 except OSError: values[key]=False
try:
 with socket.socket(socket.AF_UNIX) as sock:sock.connect(sys.argv[2])
 values['masked_socket']=True
except OSError:values['masked_socket']=False
print(json.dumps(values,sort_keys=True))
"""
                writes={'scratch':str(scratch/'probe'),'product':str(workspace/'probe'),'mirror':'/bokkie-evidence/probe','trust_asset':str(trust_asset)}
                with patch('evidence_policy.public_ca_paths',return_value=[str(trust_asset)]):
                    command=mounts(admission,selected,home,root)+['--chdir',str(workspace),'--','/usr/bin/python3','-c',code,json.dumps(writes),address]
                result=subprocess.run(command,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
                self.assertEqual(result.returncode,0,result.stderr.decode())
                self.assertEqual(json.loads(result.stdout),{'scratch':True,'product':False,'mirror':False,'trust_asset':False,'masked_socket':False})
                self.assertEqual(trust_asset.read_text(),'disposable public trust asset')

    def test_cli_override_components_preserve_existing_mcp_and_plugin_identity(self):
        path,home,codex,value=self.host_config()
        with (codex/'config.toml').open('a') as stream:
            stream.write('[mcp_servers.fixture]\ncommand="/usr/bin/true"\n[plugins."fixture@market"]\nenabled=true\n')
        with patch('common.Path.home',return_value=home),patch.dict(os.environ,{'CODEX_HOME':str(codex)}):
            configured=Config(path)
            self.admission.update(project_profile=configured.projects['project'],codex=value['codex'],bwrap=value['bwrap'])
            atomic(self.root/'admission.json',self.admission)
            command=Broker(self.root).command()
        overrides=[command[index+1] for index,item in enumerate(command) if item=='-c']
        self.assertIn('mcp_servers.fixture.enabled=false',overrides)
        self.assertIn('plugins.fixture@market.enabled=false',overrides)
        self.assertFalse(any('mcp_servers."' in override or 'plugins."' in override for override in overrides))
        for invalid in ('component.with.dot','"quoted"','component\nname'):
            with self.subTest(invalid=invalid),self.assertRaises(ValueError):cli_component(invalid)

    def test_native_codex_companion_is_required_and_mounted_individually_readonly(self):
        package=self.base/'native-package';package.mkdir();native=package/'codex';native.write_text('fixture native executable');native.chmod(0o700)
        with self.assertRaisesRegex(ValueError,'companion'):code_mode_host_path(native)
        companion=package/'codex-code-mode-host';companion.write_text('fixture companion executable');companion.chmod(0o700)
        self.assertEqual(code_mode_host_path(native),str(companion))
        home=self.base/'codex-home';home.mkdir()
        command=mounts({'bwrap':shutil.which('bwrap') or '/usr/bin/true','codex':str(native)},self.profile,home,self.root)
        self.assertTrue(any(command[index:index+3]==['--ro-bind',str(companion),str(companion)] for index in range(len(command))))
        self.assertFalse(any(command[index:index+3]==['--ro-bind',str(package),str(package)] for index in range(len(command))))
        companion.unlink();companion.symlink_to(native)
        with self.assertRaisesRegex(ValueError,'unsupported'):code_mode_host_path(native)

    def test_companion_probe_requires_success_under_both_closed_policies(self):
        native=self.base/'codex';native.write_text('fixture native')
        companion=self.base/'codex-code-mode-host';companion.write_text('fixture companion');companion.chmod(0o700)
        broker=Broker(self.root);broker.admission['codex']=str(native)
        observed={'protocol_version':1,'session_ready':True,'execution_completed':True,'enabled_tools':0,'model_calls':0}
        calls=[]
        def rpc(method,params):
            calls.append((method,params));return {'exitCode':0,'stdout':json.dumps(observed),'stderr':''}
        broker.rpc=rpc;proofs=companion_readiness(broker)
        self.assertEqual(len(proofs),2);self.assertTrue(proofs[1]['reviewer'])
        self.assertEqual(calls[0][1]['permissionProfile'],'bokkie_report_root')
        self.assertEqual(calls[1][1]['permissionProfile'],'bokkie_report_reviewer')
        self.assertEqual(calls[0][1]['command'][-1],str(companion))
        self.assertIn("'enabled_tools':[]",calls[0][1]['command'][2])
        broker.rpc=lambda *_:{'exitCode':1,'stdout':'','stderr':'synthetic failure'}
        with self.assertRaisesRegex(ValueError,'execution failed'):companion_readiness(broker)

    def test_named_reviewer_requires_effective_registration_and_protected_layer(self):
        path,home,codex,value=self.host_config()
        with patch('common.Path.home',return_value=home),patch.dict(os.environ,{'CODEX_HOME':str(codex)}):configured=Config(path)
        profile=configured.projects['project']
        overrides,identities=derived_roles(self.root,codex,{},profile)
        selected=next(item for item in identities if item['role']=='evidence_reviewer')
        entry={'config_file':selected['config_file'],'description':selected['description']}
        effective={'agents':{'enabled':True,'evidence_reviewer':entry}}
        proof=reviewer_selection_proof(effective,profile,self.root,identities)
        self.assertEqual(proof['model'],'review-model');self.assertEqual(proof['reasoning_effort'],'high')
        self.assertEqual(proof['sha256'],selected['sha256'])
        self.assertIn('agents.evidence_reviewer.description',overrides)
        self.assertTrue(selected['config_file'].endswith('/agents/evidence_reviewer.toml'))
        for invalid in ({'agents':{'enabled':False,'evidence_reviewer':entry}},
                        {'agents':{'evidence_reviewer':entry|{'description':None}}},
                        {'agents':{'evidence_reviewer':entry|{'config_file':'/different/role.toml'}}},
                        {'agents':{}}):
            with self.subTest(invalid=invalid),self.assertRaises(ValueError):reviewer_selection_proof(invalid,profile,self.root,identities)
        derived=self.root/'agent-state'/'agents'/'evidence_reviewer.toml'
        derived.chmod(0o600);derived.write_text(derived.read_text().replace('review-model','inherited-default'))
        with self.assertRaisesRegex(ValueError,'protected tuning'):reviewer_selection_proof(effective,profile,self.root,identities)

    def test_seal_reply_selects_agent_type_and_leaves_model_tuning_in_role(self):
        report=self.sealed();broker=Broker(self.root);broker.thread='root';sent=[];broker.send=sent.append
        broker.wait_review_window=lambda _:None
        broker.request({'id':1,'method':'item/tool/call','params':{'threadId':'root','namespace':'bokkie_workspace',
            'tool':'seal_report','arguments':{'markdown':report['markdown'],'source_ids':[source['id'] for source in report['sources']]}}})
        reply=json.loads(sent[-1]['result']['contentItems'][0]['text'])
        self.assertEqual(reply['review']['agent_type'],'evidence_reviewer')
        self.assertEqual(reply['review']['fork_turns'],'none')
        self.assertNotEqual(reply['review']['task_name'],reply['review']['agent_type'])
        self.assertNotIn('model',reply['review']);self.assertNotIn('reasoning_effort',reply['review'])

    def test_private_report_config_removes_legacy_and_ambient_environment_without_global_edits(self):
        home=self.base/'private-codex';home.mkdir()
        inherited={'sandbox_mode':'danger-full-access','sandbox_workspace_write':{'network_access':True},
            'default_permissions':':danger-full-access','shell_environment_policy':{'inherit':'all','set':{'SYNTHETIC_SECRET':'harmless'}},
            'model':'review-model','model_reasoning_effort':'high'}
        original=copy.deepcopy(inherited)
        profiles=permission_profiles({'codex':'/usr/bin/python3'},self.profile,home)
        generated=prepare_task_config(self.root,inherited,profiles,home,str(self.scratch))
        self.assertEqual(inherited,original)
        self.assertNotIn('sandbox_mode',generated);self.assertNotIn('sandbox_workspace_write',generated)
        self.assertEqual(generated['default_permissions'],ROOT_PERMISSIONS)
        self.assertEqual(generated['shell_environment_policy']['inherit'],'none')
        self.assertNotIn('SYNTHETIC_SECRET',generated['shell_environment_policy']['set'])
        self.assertNotIn(str(home/'auth.json'),profiles[ROOT_PERMISSIONS]['filesystem'])
        self.assertNotIn(str(home/'config.toml'),profiles[ROOT_PERMISSIONS]['filesystem'])
        self.assertEqual(profiles[ROOT_PERMISSIONS]['filesystem'][str(self.scratch)],'write')
        self.assertEqual(profiles[REVIEW_PERMISSIONS]['filesystem'][str(self.scratch)],'read')
        self.assertEqual(profiles[ROOT_PERMISSIONS]['filesystem']['/tmp'],'read')
        effective=copy.deepcopy(generated)
        self.assertTrue(profile_proof(effective,profiles)['restricted_reads'])
        effective['permissions'][ROOT_PERMISSIONS]['filesystem']['/']='read'
        with self.assertRaisesRegex(ValueError,'broadens'):profile_proof(effective,profiles)

    def test_native_file_features_require_actual_known_inventory_disabled_values(self):
        inventory={'data':[{'name':name,'enabled':False} for name in FILE_READ_FEATURES],'nextCursor':None}
        self.assertIn('view_image',file_feature_proof(inventory)['disabled_native_features'])
        missing=copy.deepcopy(inventory);missing['data'].pop()
        with self.assertRaises(ValueError):file_feature_proof(missing)
        enabled=copy.deepcopy(inventory);enabled['data'][0]['enabled']=True
        with self.assertRaises(ValueError):file_feature_proof(enabled)
        with self.assertRaises(ValueError):file_feature_proof(inventory|{'nextCursor':'next'})

    def test_no_model_routing_proof_rejects_unbound_origins_without_retaining_account_values(self):
        account={'account':{'type':'chatgpt','email':'synthetic-private-email'},'workspaceRouting':{
            'backendOrigin':'https://backend.example','chatgptAccountId':'synthetic-private-account',
            'accountRoutingOverride':'NO_CONSTRAINT'}}
        proof=routing_proof(account)
        self.assertTrue(proof['workspace_routing_verified']);self.assertEqual(proof['model_calls'],0)
        self.assertNotIn('synthetic-private',json.dumps(proof));self.assertNotIn('backend.example',json.dumps(proof))
        for mutation in ({'backendOrigin':'http://backend.example'},{'backendOrigin':'https://user:password@backend.example'},
            {'backendOrigin':'https://backend.example/path'},{'backendOrigin':'https://backend.example?query'},
            {'chatgptAccountId':''},{'accountRoutingOverride':'unknown'}):
            invalid=copy.deepcopy(account);invalid['workspaceRouting'].update(mutation)
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):routing_proof(invalid)
        with self.assertRaises(ValueError):routing_proof(account|{'workspaceRouting':None})
        with self.assertRaises(ValueError):routing_proof({'account':None})
        self.assertFalse(routing_proof({'account':{'type':'apiKey'}})['workspace_routing_applicable'])

    def test_resolved_platform_ca_files_are_explicit_readonly_assets(self):
        admission={'bwrap':shutil.which('bwrap') or '/usr/bin/true','codex':'/usr/bin/python3'}
        home=self.base/'codex';home.mkdir()
        command=mounts(admission,self.profile,home,self.root)
        for path in public_ca_paths():
            self.assertEqual(str(Path(path).resolve()),path)
            self.assertTrue(Path(path).is_file())
            self.assertTrue(any(command[index:index+3]==['--ro-bind',path,path] for index in range(len(command))))
        self.assertNotIn(['--ro-bind','/etc','/etc'],[command[index:index+3] for index in range(len(command))])
        self.assertNotIn(['--ro-bind',str(Path.home()),str(Path.home())],[command[index:index+3] for index in range(len(command))])

    def test_mcp_inventory_requires_explicitly_disabled_empty_complete_servers(self):
        entry={'runtimeStatus':'disabled','tools':{},'resources':[],'resourceTemplates':[],'serverCapabilities':None}
        proof=closed_mcp_inventory({'data':[entry],'nextCursor':None})
        self.assertEqual(proof['inherited_mcp_servers'],1)
        for mutation in ({'runtimeStatus':'connected'},{'runtimeStatus':None},{'tools':{'tool':{}}},
                         {'resources':[{}]},{'resourceTemplates':[{}]},{'serverCapabilities':{}}):
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):
                closed_mcp_inventory({'data':[entry|mutation],'nextCursor':None})
        with self.assertRaises(ValueError):closed_mcp_inventory({'data':[entry],'nextCursor':'next'})
        with self.assertRaises(ValueError):closed_mcp_inventory({'data':[{'runtimeStatus':'disabled'}],'nextCursor':None})

    def test_startup_stderr_is_bounded_private_and_safe_diagnostic_has_no_raw_values(self):
        broker=Broker(self.root)
        startup=b'Error: invalid transport\nin `mcp_servers.fixture`\nsynthetic-private-value\n'+b'x'*STARTUP_STDERR_BYTES
        later=b'synthetic-runtime-private-value'
        broker.observe_stderr(startup);broker.initialised=True;broker.observe_stderr(later);broker.retain_stderr()
        path=self.root/'startup-stderr.private';diagnostic=read(self.root/'stderr-diagnostic.json')
        self.assertEqual(path.stat().st_mode&0o777,0o600)
        self.assertEqual(path.read_bytes(),startup[:STARTUP_STDERR_BYTES])
        self.assertEqual(diagnostic['bytes'],len(startup)+len(later))
        self.assertEqual(diagnostic['sha256'],hashlib.sha256(startup+later).hexdigest())
        self.assertEqual(diagnostic['startup']['category'],'inherited_mcp_transport')
        self.assertFalse(diagnostic['startup']['capture_complete'])
        self.assertNotIn('synthetic-private-value',json.dumps(diagnostic))
        self.assertNotIn('synthetic-runtime-private-value',json.dumps(diagnostic))
        self.assertNotIn('mcp_servers.fixture',json.dumps(diagnostic))

    def test_forwarding_and_account_source_credentials_are_removed_from_task_environment(self):
        with patch.dict(os.environ,{'SSH_AUTH_SOCK':'/tmp/ssh','DBUS_SESSION_BUS_ADDRESS':'unix:path=/run/bus','DOCKER_HOST':'unix:///tmp/docker','GH_TOKEN':'secret','BOKKIE_SYNTHETIC_SECRET':'harmless'}):
            value=environment(str(self.scratch))
        self.assertFalse(any(key in value for key in ('SSH_AUTH_SOCK','DBUS_SESSION_BUS_ADDRESS','DOCKER_HOST','GH_TOKEN','BOKKIE_SYNTHETIC_SECRET')))
        self.assertEqual(value['TMPDIR'],str(self.scratch))

    def test_trusted_source_process_rejects_overflow_and_enforces_wall_deadline(self):
        environment={'PATH':'/usr/bin:/bin'}
        command=['/usr/bin/python3','-c',"import sys;sys.stdout.write('x'*4096)"]
        with self.assertRaisesRegex(ValueError,'exceeds bound'):
            bounded_process(command,str(self.base),environment,time.monotonic()+2,128)
        began=time.monotonic()
        command=['/usr/bin/python3','-c','import time;time.sleep(10)']
        with self.assertRaisesRegex(ValueError,'timed out'):
            bounded_process(command,str(self.base),environment,began+.1,128)
        self.assertLess(time.monotonic()-began,2)

    def test_trusted_source_helper_bounds_whole_dns_get_process_without_secret_arguments(self):
        with patch('evidence_report.bounded_process',return_value=encoded(self.response())) as process:
            result=github_get(self.profile['source_read'],self.selected,time.monotonic()+5)
        self.assertEqual(result,self.response())
        arguments=process.call_args.args
        self.assertEqual(arguments[1],'/trusted/cwd');self.assertEqual(arguments[0][2],'--source-get')
        request=json.loads(arguments[0][3]);self.assertEqual(request['selected'],self.selected)
        self.assertEqual(request['policy']['config_dir'],'/trusted/config')
        self.assertLessEqual(request['seconds'],5);self.assertNotIn('private-token',json.dumps(arguments[0]))

    def test_source_transport_constructs_fixed_get_without_credential_bytes_in_reply(self):
        class Response:
            status=200;headers={}
            fp=SimpleNamespace(raw=SimpleNamespace(_sock=SimpleNamespace(settimeout=lambda _:None)))
            def __enter__(self):return self
            def __exit__(self,*_):pass
            def geturl(self):return 'https://api.github.com/'+endpoint(self.selected)
            def read1(self,size):
                raw=self.raw[:size];self.raw=self.raw[size:]
                if not self.raw:self.fp=None  # Real HTTPResponse closes on Content-Length exhaustion.
                return raw
        response=Response();response.selected=self.selected;response.raw=encoded(self.response())
        requests=[]
        def open_request(request,timeout):requests.append(request);return response
        with patch('evidence_report.bounded_process',return_value=b'private-token\n') as credential,patch('evidence_report.build_opener',return_value=SimpleNamespace(open=open_request)):
            value=_github_get(self.profile['source_read'],self.selected,time.monotonic()+5)
        self.assertEqual(value,self.response());self.assertNotIn('private-token',json.dumps(value))
        self.assertIsNone(response.fp)
        self.assertEqual(requests[0].get_method(),'GET');self.assertEqual(requests[0].full_url,'https://api.github.com/'+endpoint(self.selected))
        self.assertEqual(credential.call_args.args[0],['/trusted/gh','auth','token','--hostname','github.com'])
        self.assertEqual(credential.call_args.args[1],'/trusted/cwd');self.assertEqual(credential.call_args.args[2]['GH_CONFIG_DIR'],'/trusted/config')
        with self.assertRaisesRegex(ValueError,'redirects'):
            NoRedirect().redirect_request(requests[0],None,302,'redirect',{},'https://other.example/')


if __name__=='__main__':unittest.main()
