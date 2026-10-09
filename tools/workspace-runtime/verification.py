"""Trusted read-only delivery observations, separate from agent result text."""
import json
from pathlib import Path
import re
import shlex
import subprocess
import time
from urllib.parse import urlsplit
from common import encoded


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


def observed_review(records, root_thread, head):
    children=set()
    reports={}
    completed=set()
    roles={}
    configured=next((r['value'] for r in records if r['kind']=='reviewer_profile'),None)
    if configured is None or configured['role']!='exact_head_reviewer':
        return None
    for record in records:
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
    for key, report in reports.items():
        if (key[0] in children and key[0] in roles and key in completed and
                re.findall(r'^Verdict:[ \t]*(PASS|BLOCK)[ \t]*$',report,re.M)==['PASS'] and
                re.findall(r'^Reviewed head:[ \t]*([0-9a-f]{40})[ \t]*$',report,re.M)==[head]):
            return 'Observed independent completed child '+key[0]+' turn '+key[1]
    return None


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


def verify(admission, result, records, *, query=github, deadline=None):
    evidence=[]
    try:
        if query is github:
            end=min(admission['deadline'] if deadline is None else deadline,time.time()+120)
            count=0
            def bounded_query(endpoint):
                nonlocal count
                count+=1
                if count>64 or time.time()>=end:
                    raise ValueError('Trusted verification budget exhausted; retain delivery for reconciliation')
                return github(endpoint,timeout=min(30,end-time.time()))
            query=bounded_query
        if result is None:
            raise ValueError('No structured workspace result was submitted')
        wanted={c['id'] for c in admission['dispatch']['assignment']['criteria']}
        criteria=result['criteria']
        if (len(criteria)!=len(wanted) or {c['id'] for c in criteria}!=wanted or
                any(c['satisfied'] is not True or not c['evidence'] for c in criteria)):
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
            review_evidence=observed_review(records,root_thread,head)
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
            evidence.extend(checks_at(repository,head,policy['required_checks'],query))
            evidence.extend(checks_at(repository,merge,policy['required_checks'],query))
        if len(encoded(evidence))>32768:
            raise ValueError('verification evidence exceeds bound')
        return {'passed':True,'evidence':evidence}
    except (ValueError,RuntimeError,KeyError,TypeError,subprocess.TimeoutExpired) as error:
        # Retain the delivered result and the observations already acquired.
        # Missing proof never restarts the underlying workspace assignment.
        return {'passed':False,'evidence':evidence+[str(error)[:1024]]}
