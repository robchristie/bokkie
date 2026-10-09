"""Explicit deterministic recovery of a missing report; never restarts work."""
import hashlib
from pathlib import Path
import re
import time
from urllib.parse import urlsplit
from common import (Journal,MAX_MESSAGE,atomic,canonical,digest,encoded,event_bounds,
                    locked,private_directory,read,recovery_provenance_bounds,result_bounds,strings)
from verification import github,verify,verify_prerequisites


def documentary_reference(value):
    if value.startswith('https://'):
        parsed=urlsplit(value)
        return bool(parsed.hostname and not parsed.username and not parsed.password)
    if value.startswith('runtime:') or value.startswith('sha256:'):
        return bool(value.split(':',1)[1].strip())
    return bool(re.fullmatch(r'[A-Za-z0-9_.@#/-]+',value) and '/' in value and not value.startswith('/'))


def proposal_bounds(proposal,admission):
    if not isinstance(proposal,dict) or set(proposal)!={'execution_id','dispatch_digest','result','criterion_mapping'}:
        raise ValueError('invalid recovery proposal shape')
    if (proposal['execution_id']!=admission['dispatch']['execution_id'] or
            proposal['dispatch_digest']!=admission['dispatch_digest'] or
            admission['dispatch_digest']!=digest(admission['dispatch'])):
        raise ValueError('recovery proposal does not bind the immutable admission')
    result_bounds(proposal['result'])
    wanted={c['id'] for c in admission['dispatch']['assignment']['criteria']}
    result={c['id']:c for c in proposal['result']['criteria']}
    mapping=proposal['criterion_mapping']
    if not isinstance(mapping,list) or len(mapping)!=len(wanted) or set(result)!=wanted:
        raise ValueError('recovery requires explicit mapping of every original criterion')
    seen=set()
    for row in mapping:
        if not isinstance(row,dict) or set(row)!={'id','evidence'} or row['id'] not in wanted or row['id'] in seen:
            raise ValueError('invalid or repeated recovery criterion mapping')
        strings(row['evidence'],32,required=True)
        if row['evidence']!=result[row['id']]['evidence'] or not all(documentary_reference(v) for v in row['evidence']):
            raise ValueError('criterion mapping must match explicit result documentary references')
        if result[row['id']]['satisfied'] is False and not any(v.startswith(row['id']+':') for v in proposal['result']['limitations']):
            raise ValueError('an unsatisfied criterion requires its named limitation')
        seen.add(row['id'])


def recovery_state(config,root,journal):
    admission=journal.admission
    if (canonical(config.path,directory=False)!=admission['config_file'] or
            canonical(str(config.root))!=admission['runtime_root']):
        raise ValueError('recovery is outside the original private configuration or runtime')
    original_project=admission['dispatch']['assignment']['project']['id']
    current=config.projects.get(original_project)
    if current is None or current['host']!=admission['project_profile']['host']:
        raise ValueError('recovery original project host is not currently registered')
    if admission.get('host_id') is not None and admission['host_id']!=config.value['host_id']:
        raise ValueError('recovery host identity differs from the admitted host')
    if (root/'cancel.json').exists():
        raise ValueError('cancelled execution cannot import a recovered result')
    if (root/'result.json').exists():
        raise ValueError('an agent-submitted result already exists')
    marker=read(root/'launch-committed.json');proof=read(root/'cessation.json')
    if (proof['kind']!='descendants_reaped' or marker['generation']!=proof['generation'] or
            marker['boundary_id']!=proof['boundary_id'] or
            marker['boundary_id']!=journal.execution_id+':'+marker['generation'] or
            marker['dispatch_digest']!=admission['dispatch_digest']):
        raise ValueError('recovery lacks exact original-boundary descendant cessation')
    cessation={key:proof[key] for key in ('boundary_id','kind','evidence')}
    events=journal.events()
    if not any(e['event']['kind']=='stopped' and 'result' in e['event'] and
               e['event']['result'] is None and e['event']['cessation']==cessation for e in events):
        raise ValueError('recovery requires an existing null-result stop with the same proof')
    return cessation,proof,events


def projected_observation(endpoint,value):
    if '/check-runs?' in endpoint:
        return {'total_count':value['total_count'],'check_runs':[
            {k:c.get(k) for k in ('id','name','head_sha','started_at','status','conclusion','html_url')}
            for c in value['check_runs']]}
    if '/reviews?' in endpoint:
        return [{k:r[k] for k in ('state','commit_id','html_url')}|{'user':{'login':r['user']['login']}} for r in value]
    if '/git/commits/' in endpoint:return {'tree':{'sha':value['tree']['sha']}}
    return {'merged':value['merged'],'head':{'sha':value['head']['sha']},
            'merge_commit_sha':value['merge_commit_sha'],'user':{'login':value['user']['login']}}


class ReadCache:
    def __init__(self,query,deadline):
        self.query=query;self.deadline=deadline;self.responses={};self.reads=0

    def __call__(self,endpoint):
        if endpoint in self.responses and '/check-runs?' not in endpoint:
            return self.responses[endpoint]
        self.reads+=1
        if self.reads>64 or time.time()>=self.deadline:
            raise ValueError('recovery read budget exhausted')
        value=self.query(endpoint,timeout=min(30,self.deadline-time.time())) if self.query is github else self.query(endpoint)
        self.responses[endpoint]=value
        return value

    def retained(self):
        return [{'endpoint':endpoint,'observation':projected_observation(endpoint,value)} for endpoint,value in self.responses.items()]


def source_classes(records,proposal,proof,external):
    return {
        'proposal':proposal,'cessation':proof,
        'runtime_commands':[r for r in records if r['kind']=='command_observation'],
        'runtime_review':[r for r in records if r['kind'] in ('thread_identity','reviewer_profile','child_thread_read') or
                         r['kind']=='protocol_event' and (r['value'].get('method') in ('thread/started','turn/completed') or
                         r['value'].get('params',{}).get('item',{}).get('type') in ('subAgentActivity','agentMessage'))],
        'runtime_checks':[r for r in records if r['kind'] in ('check_wait_observation','check_wait_finished')],
        'github_observations':external,
    }


def effective_result(root):
    root=Path(root)
    if (root/'result.json').exists():
        if (root/'recovered-result.json').exists():raise ValueError('agent and recovered results conflict')
        return read(root/'result.json')
    capsule_path=root/'recovered-result.json'
    if not capsule_path.exists():return None
    capsule=read(capsule_path)
    if set(capsule)!={'result','provenance'}:raise ValueError('invalid recovered result capsule')
    result_bounds(capsule['result']);recovery_provenance_bounds(capsule['provenance'])
    if digest(capsule['result'])!=capsule['provenance']['result_digest']:
        raise ValueError('recovered result digest mismatch')
    if not any(e['event']['kind']=='recovered_result' and e['event']['result']==capsule['result'] and
               e['event']['provenance']==capsule['provenance'] for e in Journal(root).events()):
        return None  # An unfinished explicit import is not an automatic result.
    return capsule['result']


def queue_recovery(journal,capsule,verification):
    event={'kind':'recovered_result',**capsule,'verification':verification}
    for existing in journal.events():
        if existing['event']['kind']=='recovered_result':
            if existing['event']!=event:raise ValueError('recovered event conflicts with the immutable capsule')
            return
    journal.event(event,terminal=True)


def recover_result(config,execution_id,evidence_file,*,query=github):
    root=config.executions/hashlib.sha256(execution_id.encode()).hexdigest()
    proposal=read(canonical(str(evidence_file),directory=False))
    with locked(root/'broker.lock',blocking=False):
        journal=Journal(root)
        if journal.execution_id!=execution_id:raise ValueError('recovery execution identity mismatch')
        proposal_bounds(proposal,journal.admission)
        cessation,proof,events=recovery_state(config,root,journal)
        capsule_path=root/'recovered-result.json'
        if capsule_path.exists():
            capsule=read(capsule_path);provenance=capsule['provenance']
            recovery_provenance_bounds(provenance)
            sources={s['kind']:s['sha256'] for s in provenance['sources']}
            current_classes=source_classes(journal.records(),proposal,proof,[])
            if (capsule['result']!=proposal['result'] or sources.get('proposal')!=digest(proposal) or
                    provenance['dispatch_digest']!=journal.admission['dispatch_digest'] or
                    provenance['admission_digest']!=digest(journal.admission) or provenance['cessation']!=cessation or
                    provenance['result_digest']!=digest(capsule['result']) or
                    any(sources.get(kind)!=digest(value) for kind,value in current_classes.items() if kind!='github_observations')):
                raise ValueError('recovery retry conflicts with the immutable capsule')
            verification_digest=sources.get('initial_verification')
            if verification_digest is None:raise ValueError('recovery verification source is missing')
            verification=read(root/'recovery-sources'/(verification_digest+'.json'))
            if digest(verification)!=verification_digest:raise ValueError('recovery verification source digest mismatch')
            queue_recovery(journal,capsule,verification)
            return capsule
        if any(e['event']['kind']=='recovered_result' or e['event']['kind']=='stopped' and e['event']['result'] is not None for e in events):
            raise ValueError('a result or recovery event already exists')
        records=journal.records();cache=ReadCache(query,time.time()+120)
        prerequisites=verify_prerequisites(journal.admission,proposal['result'],records,query=cache,deadline=cache.deadline)
        if prerequisites['prerequisites_verified'] is not True:
            raise ValueError('recovery delivery prerequisites are unverified: '+prerequisites['evidence'][-1])
        verification=verify(journal.admission,proposal['result'],records,query=cache,deadline=cache.deadline)
        classes=source_classes(records,proposal,proof,cache.retained())
        classes['initial_verification']=verification
        source_dir=private_directory(root/'recovery-sources')
        # Existing runtime classes remain in their authoritative journal; only
        # new proposal and actual projected external observations need artefacts.
        for kind in ('proposal','github_observations','initial_verification'):
            atomic(source_dir/(digest(classes[kind])+'.json'),classes[kind],immutable=True)
        provenance={'origin':'host_reconciliation','algorithm':'retained-delivery-v1',
            'recovered_at':int(time.time()),'dispatch_digest':journal.admission['dispatch_digest'],
            'admission_digest':digest(journal.admission),'result_digest':digest(proposal['result']),
            'cessation':cessation,'sources':[{'kind':kind,'sha256':digest(value)} for kind,value in classes.items()]}
        capsule={'result':proposal['result'],'provenance':provenance}
        event_bounds({'kind':'recovered_result',**capsule,'verification':verification})
        recovery_state(config,root,journal)  # Fence cancellation arriving during reads.
        atomic(capsule_path,capsule,immutable=True)
        recovery_state(config,root,journal)
        queue_recovery(journal,capsule,verification)
        return capsule
