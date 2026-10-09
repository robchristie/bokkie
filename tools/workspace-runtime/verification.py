"""Trusted read-only delivery observations, separate from agent result text."""
import json
from pathlib import Path
import re
import shlex
import subprocess
import time
from urllib.parse import urlsplit
from common import encoded, result_bounds


def github(endpoint,*,timeout=30):
    response = subprocess.run(['gh','api','--hostname','github.com',endpoint],
                              stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout)
    if response.returncode:
        raise RuntimeError('GitHub read unavailable')
    if len(response.stdout)>8*1024*1024:
        raise ValueError('GitHub observation exceeds bound')
    return json.loads(response.stdout)


def shell_payload(command):
    try:
        parts=shlex.split(command)
    except ValueError:
        return None
    if len(parts)==3 and Path(parts[0]).name in ('bash','sh','zsh') and parts[1] in ('-lc','-c'):
        return parts[2]
    return None


def review_value(report,label,pattern):
    if not isinstance(report,str):
        return None
    lines=[line[len(label):] for line in report.splitlines() if line.startswith(label)]
    if len(lines)!=1:
        return None
    match=re.fullmatch('[ \\t]*(?:'+pattern+')[ \\t]*',lines[0])
    if match is None:
        return None
    return next(value for value in match.groups() if value is not None)


def review_head(report):
    return review_value(report,'Reviewed head:',r'([0-9a-f]{40})|`([0-9a-f]{40})`')


def review_verdict(report):
    return review_value(report,'Verdict:',r'(PASS|BLOCK)|\*\*(PASS|BLOCK)\*\*')


def review_observation(records, root_thread, head):
    children=set()
    reports={}
    completed=set()
    roles={}
    configured=next((r['value'] for r in records if r['kind']=='reviewer_profile'),None)
    if configured is None or configured['role']!='exact_head_reviewer':
        return {'evidence':None,'blocking':False}
    for record in records:
        if record['kind']=='child_thread_read':
            value=record['value'];thread=value['thread']
            if (thread.get('id')!=value['child_id'] or thread.get('parentThreadId')!=root_thread or
                    thread.get('agentRole')!=configured['role'] or
                    configured.get('model') is not None and thread.get('model')!=configured['model'] or
                    configured.get('reasoning_effort') is not None and thread.get('reasoningEffort')!=configured['reasoning_effort']):
                continue
            roles[thread['id']]=thread['agentRole']
            for turn in thread.get('turns',[]):
                if turn.get('status')!='completed':continue
                key=(thread['id'],turn.get('id'))
                for item in turn.get('items',[]):
                    if item.get('type')=='agentMessage' and item.get('phase')=='final_answer':
                        reports[key]=item.get('text','');completed.add(key)
            continue
        if record['kind']!='protocol_event':
            continue
        message=record['value']
        p=message.get('params',{})
        item=p.get('item',{})
        if message.get('method')=='thread/started':
            thread=p.get('thread',{})
            if thread.get('parentThreadId')==root_thread and thread.get('agentRole')==configured['role']:
                roles[thread['id']]=thread['agentRole']
        if p.get('threadId')==root_thread and item.get('type')=='subAgentActivity':
            if item.get('agentThreadId'):
                children.add(item['agentThreadId'])
        if (message.get('method')=='item/completed' and item.get('type')=='agentMessage' and
                item.get('phase')=='final_answer'):
            reports[(p.get('threadId'),p.get('turnId'))]=item.get('text','')
        if message.get('method')=='turn/completed' and p.get('turn',{}).get('status')=='completed':
            completed.add((p.get('threadId'),p['turn']['id']))
    latest={}
    for key, report in reports.items():
        if (key[0] in children and key[0] in roles and key in completed and
                review_head(report)==head):
            latest[key[0]]=(key,report)
    if any(review_verdict(report)!='PASS' for _,report in latest.values()):
        return {'evidence':None,'blocking':True}
    if latest:
        key,_=next(iter(latest.values()))
        return {'evidence':'Observed independent completed child '+key[0]+' turn '+key[1],'blocking':False}
    return {'evidence':None,'blocking':False}


def observed_review(records,root_thread,head):
    return review_observation(records,root_thread,head)['evidence']


def checks_at(repository, revision, required, query):
    page=query(f'repos/{repository}/commits/{revision}/check-runs?per_page=100')
    if page['total_count']>100:
        raise ValueError('check catalogue exceeds the bounded single page')
    checks=page['check_runs']
    observations=[]
    for name in required:
        candidates=[c for c in checks if c['name']==name and c['head_sha']==revision]
        if not candidates:
            raise ValueError('required check missing at '+revision+': '+name)
        latest=max(candidates,key=lambda c:(c.get('started_at') or '',c['id']))
        if latest['status']!='completed' or latest['conclusion']!='success':
            raise ValueError('required check has not passed at '+revision+': '+name)
        observations.append(latest['html_url'])
    return observations


def bounded_query(admission,query,deadline):
    if query is not github:return query
    end=min(admission['deadline'] if deadline is None else deadline,time.time()+120)
    count=0
    def read(endpoint):
        nonlocal count
        count+=1
        if count>64 or time.time()>=end:
            raise ValueError('Trusted verification budget exhausted; retain delivery for reconciliation')
        return github(endpoint,timeout=min(30,end-time.time()))
    return read


def observe_delivery(admission,result,records,query,evidence):
    if result is None:
        raise ValueError('No structured workspace result was submitted')
    result_bounds(result)
    wanted={c['id'] for c in admission['dispatch']['assignment']['criteria']}
    criteria=result['criteria']
    if (len(criteria)!=len(wanted) or {c['id'] for c in criteria}!=wanted or
            any(not c['evidence'] for c in criteria)):
        raise ValueError('Result does not evidence every completion criterion')
    if not result['deliveries']:
        raise ValueError('No attributable engineering delivery was supplied')
    if len(result['deliveries'])>8:
        raise ValueError('Delivery catalogue exceeds trusted verification bound')
    repositories={r['repository']:r for r in admission['project_profile']['verification']['repositories']}
    thread_record=next((r for r in records if r['kind']=='thread_identity'),None)
    root_thread=thread_record['value']['thread_id'] if thread_record else None
    commands={}
    for record in records:
        if record['kind']=='command_observation':
            value=record['value']
            commands.setdefault(value['item']['id'],{})[value['phase']]=value
    for delivery in result['deliveries']:
        repository=delivery['repository']
        if repository not in repositories:
            raise ValueError('Delivery repository is not in the fixed profile')
        policy=repositories[repository]
        head,merge,tree=(delivery[k] for k in ('reviewed_head','merge_revision','tree'))
        if any(not re.fullmatch(r'[0-9a-f]{40}',v) for v in (head,merge,tree)):
            raise ValueError('Delivery revisions require complete Git identities')
        pr=urlsplit(delivery['pull_request'])
        match=re.fullmatch('/'+re.escape(repository)+r'/pull/([1-9][0-9]*)',pr.path)
        if pr.scheme!='https' or pr.netloc!='github.com' or pr.query or pr.fragment or not match:
            raise ValueError('Delivery PR identity does not match the repository')
        prefix=f'repos/{repository}/pulls/{match.group(1)}'
        pull=query(prefix)
        if not pull['merged'] or pull['head']['sha']!=head or pull['merge_commit_sha']!=merge:
            raise ValueError('GitHub PR does not bind the reviewed candidate and merge revision')
        head_tree=query(f'repos/{repository}/git/commits/{head}')['tree']['sha']
        merge_tree=query(f'repos/{repository}/git/commits/{merge}')['tree']['sha']
        if head_tree!=tree or merge_tree!=tree:
            raise ValueError('Reviewed and merged trees differ from the supplied identity')
        evidence.append(delivery['pull_request']+' candidate '+head+' merge '+merge+' tree '+tree)
        reviews=query(prefix+'/reviews?per_page=100')
        if len(reviews)>=100:
            raise ValueError('review catalogue requires a bounded additional page')
        current={}
        for review in reviews:
            current[review['user']['login']]=review
        if any(r['state']=='CHANGES_REQUESTED' and r['commit_id']==head for r in current.values()):
            raise ValueError('Unresolved GitHub change request remains at the reviewed head')
        observation=review_observation(records,root_thread,head)
        if observation['blocking']:
            raise ValueError('Qualified retained review blocks the exact candidate head')
        review_evidence=observation['evidence']
        approved=next((r for r in current.values() if r['state']=='APPROVED' and
                       r['commit_id']==head and r['user']['login']!=pull['user']['login']),None)
        if approved:
            review_evidence=approved['html_url']
        if review_evidence is None:
            raise ValueError('Independent exact-head review has no attributable receipt')
        evidence.append(review_evidence)
        for command in policy['canonical_commands']:
            matching=[]
            for observation in commands.values():
                start,finish=observation.get('started'),observation.get('completed')
                if (start and finish and finish['item'].get('exitCode')==0 and
                        shell_payload(finish['item']['command'])==command and
                        all(v['source'].get('repository')==repository and
                            v['source'].get('head')==head and v['source'].get('tree')==tree and
                            v['source'].get('clean') is True for v in (start,finish))):
                    matching.append(finish['item']['id'])
            if not matching:
                raise ValueError('Canonical command lacks successful exact-candidate observation: '+command)
            evidence.append('Observed canonical command '+command+' at '+head+' item '+matching[-1])
    return repositories

def verify_prerequisites(admission,result,records,*,query=github,deadline=None):
    """Validate delivery identity/review/canonical evidence; this is not acceptance."""
    evidence=[]
    try:
        observe_delivery(admission,result,records,bounded_query(admission,query,deadline),evidence)
        return {'prerequisites_verified':True,'evidence':evidence}
    except (ValueError,RuntimeError,KeyError,TypeError,OSError,subprocess.TimeoutExpired) as error:
        return {'prerequisites_verified':False,'evidence':evidence+[str(error)[:1024]]}


def verify(admission, result, records, *, query=github, deadline=None, root=None):
    evidence=[]
    try:
        if admission['dispatch']['assignment'].get('result_contract','engineering_delivery')=='evidence_report':
            return {'passed':True,'evidence':verify_report(admission,result,records,root)}
        if result is None or any(c['satisfied'] is not True for c in result['criteria']):
            raise ValueError('Result does not satisfy every completion criterion')
        if result['limitations']:
            raise ValueError('Result retains unresolved limitations')
        query=bounded_query(admission,query,deadline)
        repositories=observe_delivery(admission,result,records,query,evidence)
        for delivery in result['deliveries']:
            policy=repositories[delivery['repository']]
            evidence.extend(checks_at(delivery['repository'],delivery['reviewed_head'],policy['required_checks'],query))
            evidence.extend(checks_at(delivery['repository'],delivery['merge_revision'],policy['required_checks'],query))
        if len(encoded(evidence))>32768:
            raise ValueError('verification evidence exceeds bound')
        return {'passed':True,'evidence':evidence}
    except (ValueError,RuntimeError,KeyError,TypeError,OSError,subprocess.TimeoutExpired) as error:
        # Retain the delivered result and the observations already acquired.
        # Missing proof never restarts the underlying workspace assignment.
        return {'passed':False,'evidence':evidence+[str(error)[:1024]]}


def report_review_observation(records, root_thread, report, sealed_index, configured):
    """Only actual post-seal reviewer descendants' completed final turns qualify."""
    if (configured.get('role') != 'evidence_reviewer' or not configured.get('model') or
            not configured.get('reasoning_effort')):
        return None
    children = {}
    snapshots = {}
    for index, record in enumerate(records):
        value = record['value']
        if record['kind'] == 'protocol_event':
            params = value.get('params', {})
            item = params.get('item', {})
            if (params.get('threadId') == root_thread and item.get('type') == 'subAgentActivity' and
                    item.get('agentThreadId') and item['agentThreadId'] != root_thread):
                children.setdefault(item['agentThreadId'],index)
        elif record['kind'] == 'child_thread_read':
            thread = value.get('thread', {})
            if (value.get('include_turns') is True and thread.get('id') == value.get('child_id') and
                    thread.get('parentThreadId') == root_thread and thread.get('agentRole') == configured['role'] and
                    thread.get('model') == configured['model'] and
                    thread.get('reasoningEffort') == configured['reasoning_effort']):
                snapshots[thread['id']] = thread
    latest = {}
    for child, first_index in children.items():
        if first_index<=sealed_index:continue
        for turn in snapshots.get(child, {}).get('turns', []):
            if turn.get('status') != 'completed' or not turn.get('id'):
                continue
            answers = [item.get('text', '') for item in turn.get('items', []) if
                       item.get('type') == 'agentMessage' and item.get('phase') == 'final_answer']
            if len(answers) != 1:
                continue
            answer = answers[0]
            report_id = review_value(answer, 'Reviewed report:', r'([0-9a-f]{64})|`([0-9a-f]{64})`')
            sources_id = review_value(answer, 'Reviewed sources:', r'([0-9a-f]{64})|`([0-9a-f]{64})`')
            if report_id == report['digest'] and sources_id == report['source_manifest_digest']:
                latest[child] = (turn['id'], review_verdict(answer))
    if any(verdict != 'PASS' for _, verdict in latest.values()):
        raise ValueError('Independent completed child blocks the sealed report')
    if latest:
        child, (turn, _) = next(iter(latest.items()))
        return 'Observed independent completed evidence reviewer '+child+' turn '+turn
    return None


def verify_report(admission, result, records, root):
    from evidence_report import EvidenceStore
    result_bounds(result)
    assignment = admission['dispatch']['assignment']
    wanted = {criterion['id'] for criterion in assignment['criteria']}
    if (result['deliveries'] or result.get('report') is None or
            len(result['criteria']) != len(wanted) or {c['id'] for c in result['criteria']} != wanted or
            any(c['satisfied'] is not True or not c['evidence'] for c in result['criteria']) or result['limitations']):
        raise ValueError('Evidence report does not satisfy the admitted completion criteria')
    if root is None:
        raise ValueError('Host-owned sealed evidence store is unavailable')
    if not records:
        raise ValueError('Report has no attributable host observations')
    policy = next((r['value'] for r in records if r['kind'] == 'evidence_policy_qualified'), None)
    from common import read
    if policy is None or read(Path(root)/'evidence-policy.json') != policy or policy.get('model_calls') != 0:
        raise ValueError('No attributable no-model report policy qualification')
    report = EvidenceStore(root, admission, create=False).report(result['report']['digest'])
    if report != result['report']:
        raise ValueError('Submitted report differs from the immutable host seal')
    sealed_index = next((index for index, record in enumerate(records) if
                         record['kind'] == 'evidence_report_sealed' and record['value'].get('digest') == report['digest'] and
                         record['value'].get('source_manifest_digest') == report['source_manifest_digest']), None)
    if sealed_index is None:
        raise ValueError('Sealed report has no runtime seal observation')
    thread = next((r['value']['thread_id'] for r in records if r['kind'] == 'thread_identity'), None)
    configured = next((r['value'] for r in records if r['kind'] == 'reviewer_profile'), {})
    if configured != admission['project_profile'].get('reviewer'):
        raise ValueError('Independent reviewer does not match admitted profile')
    observation = report_review_observation(records, thread, report, sealed_index, configured)
    if observation is None:
        raise ValueError('No independent completed child review of both sealed report and sources')
    return ['Host recomputed sealed report '+report['digest'],
            'Host recomputed captured source manifest '+report['source_manifest_digest'], observation]
