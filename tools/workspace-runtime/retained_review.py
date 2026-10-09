"""One explicitly admitted read-only review of acknowledged, ceased work."""
import copy
import hashlib
from pathlib import Path
import re
import time
from common import (Journal,atomic,canonical,digest,locked,read,result_bounds,strings,text)
from recovery import documentary_reference,effective_result
from verification import github,verify


def is_retained_review(admission):
    return admission['dispatch']['assignment'].get('review_retained_work') is not None


def reviewed_spec(admission):
    assignment=admission['dispatch']['assignment'];spec=assignment.get('review_retained_work')
    if not isinstance(spec,dict) or set(spec)!={'source','summary','criteria'}:
        raise ValueError('invalid retained-work review shape')
    source=spec['source']
    if (not isinstance(source,dict) or set(source)!={'execution_id','result_digest'} or
            not isinstance(source['execution_id'],str) or not 1<=len(source['execution_id'])<=200 or
            source['execution_id']==admission['dispatch']['execution_id'] or
            not isinstance(source['result_digest'],str) or not re.fullmatch(r'[0-9a-f]{64}',source['result_digest'])):
        raise ValueError('invalid retained-work source identity')
    result={'summary':spec['summary'],'criteria':spec['criteria'],'deliveries':[],'limitations':[]}
    result_bounds(result)
    wanted={c['id'] for c in assignment['criteria']}
    if {c['id'] for c in spec['criteria']}!=wanted or len(spec['criteria'])!=len(wanted):
        raise ValueError('reviewed claims must cover the new pinned criteria exactly')
    for criterion in spec['criteria']:
        strings(criterion['evidence'],32,required=True)
        if not all(documentary_reference(value) for value in criterion['evidence']):
            raise ValueError('retained-work claims require explicit documentary references')
    return spec


def verification_policy(admission):
    return sorted([{'repository':r['repository'],'canonical_commands':sorted(r['canonical_commands']),
                    'required_checks':sorted(r['required_checks'])}
                   for r in admission['project_profile']['verification']['repositories']],key=lambda r:r['repository'])


def source_snapshot(config,admission):
    spec=reviewed_spec(admission);identity=spec['source']['execution_id']
    source_root=config.executions/hashlib.sha256(identity.encode()).hexdigest()
    canonical(str(source_root));journal=Journal(source_root);source=journal.admission
    if (journal.execution_id!=identity or digest(source['dispatch'])!=source['dispatch_digest'] or
            source['dispatch']['task_id']!=admission['dispatch']['task_id'] or
            source['dispatch']['assignment']['project']!=admission['dispatch']['assignment']['project']):
        raise ValueError('retained work must belong to the same task and exact project snapshot')
    if is_retained_review(source):raise ValueError('retained-work reviews cannot be chained')
    for receipt in (source,admission):
        if (receipt['config_file']!=canonical(config.path,directory=False) or
                receipt['runtime_root']!=canonical(str(config.root)) or
                receipt['project_profile']['host']!=config.projects[receipt['dispatch']['assignment']['project']['id']]['host'] or
                receipt.get('host_id',config.value['host_id'])!=config.value['host_id']):
            raise ValueError('retained-work receipt is outside the admitted host and private state')
    if source['project_profile']['host']!=admission['project_profile']['host']:
        raise ValueError('retained work belongs to another destination host')
    if verification_policy(source)!=verification_policy(admission):
        raise ValueError('retained work requires a compatible verification policy')
    marker=read(source_root/'launch-committed.json');proof=read(source_root/'cessation.json')
    if (proof['kind']!='descendants_reaped' or marker['generation']!=proof['generation'] or
            marker['boundary_id']!=proof['boundary_id'] or
            proof['boundary_id']!=identity+':'+proof['generation'] or
            marker['dispatch_digest']!=source['dispatch_digest']):
        raise ValueError('retained work lacks exact coding-boundary descendant cessation')
    result=effective_result(source_root)
    if result is None or digest(result)!=spec['source']['result_digest']:
        raise ValueError('retained-work effective result is absent or changed')
    result_bounds(result)
    cessation={k:proof[k] for k in ('boundary_id','kind','evidence')}
    acknowledged=read(source_root/'ack.json')['sequence']
    matching=[]
    for event in journal.events():
        body=event['event']
        ceased=(body.get('cessation')==cessation if body['kind']=='stopped' else
                body.get('provenance',{}).get('cessation')==cessation if body['kind']=='recovered_result' else False)
        if ceased and body.get('result')==result and event['sequence']<=acknowledged:
            matching.append(event['sequence'])
    if not matching:
        raise ValueError('retained source result has no durable Store acknowledgement')
    # Logical terminal closure is asserted by this Store-admitted review. The
    # host independently checks acknowledged result identity and physical death.
    records=journal.records()
    manifest={'origin':'host_retained_review','algorithm':'retained-review-v1',
        'source_execution_id':identity,'source_admission_digest':digest(source),
        'source_result_digest':digest(result),'source_record_digest':digest(records),
        'source_cessation_digest':digest(proof),
        'source_event_sequence':max(matching),'dispatch_digest':admission['dispatch_digest'],
        'verification_policy_digest':digest(verification_policy(admission))}
    return source_root,source,result,records,manifest,proof


def verification_inputs(root):
    root=Path(root);journal=Journal(root);admission=journal.admission
    manifest=read(root/'retained-review.json')
    if manifest['origin']!='host_retained_review' or manifest['algorithm']!='retained-review-v1':
        raise ValueError('invalid retained review attribution')
    spec=reviewed_spec(admission)
    if manifest['source_execution_id']!=spec['source']['execution_id'] or manifest['dispatch_digest']!=admission['dispatch_digest']:
        raise ValueError('retained review attribution does not bind the dispatch')
    source_root=Path(admission['runtime_root'])/'executions'/hashlib.sha256(manifest['source_execution_id'].encode()).hexdigest()
    canonical(str(source_root));source_journal=Journal(source_root)
    if (digest(source_journal.admission)!=manifest['source_admission_digest'] or
            digest(source_journal.records())!=manifest['source_record_digest'] or
            effective_result(source_root) is None or digest(effective_result(source_root))!=manifest['source_result_digest'] or
            verification_policy(source_journal.admission)!=verification_policy(admission)):
        raise ValueError('retained source evidence changed after admission')
    if digest(read(source_root/'cessation.json'))!=manifest['source_cessation_digest']:
        raise ValueError('retained source cessation proof changed')
    result=read(root/'result.json')
    source_result=effective_result(source_root)
    if (digest(result)!=manifest['result_digest'] or result['summary']!=spec['summary'] or
            result['criteria']!=spec['criteria'] or result['deliveries']!=source_result['deliveries'] or result['limitations']):
        raise ValueError('retained review result differs from the pinned claims and source deliveries')
    if read(source_root/'ack.json')['sequence']<manifest['source_event_sequence']:
        raise ValueError('retained source acknowledgement changed')
    return source_journal.records()


def cancellation_fence(root,admission):
    if (root/'cancel.json').exists():raise InterruptedError('Retained review cancellation requested')
    if time.time()>=admission['deadline']:raise TimeoutError('Retained review admission deadline exhausted')


def run_retained_review(config,root,*,query=github):
    root=Path(root)
    with locked(root/'broker.lock',blocking=False):
        journal=Journal(root);admission=journal.admission
        if any(e['event']['kind']=='stopped' for e in journal.events()):return
        reason='Retained workspace delivery reviewed without launching a coding boundary'
        try:
            cancellation_fence(root,admission)
            source_root,source,source_result,records,manifest,source_proof=source_snapshot(config,admission)
            spec=reviewed_spec(admission)
            result={'summary':spec['summary'],'criteria':copy.deepcopy(spec['criteria']),
                    'deliveries':copy.deepcopy(source_result['deliveries']),'limitations':[]}
            result_bounds(result);manifest['result_digest']=digest(result)
            atomic(root/'retained-review.json',manifest,immutable=True)
            atomic(root/'result.json',result,immutable=True)
            proof={'generation':'retained-review','boundary_id':journal.execution_id+':retained-review',
                'kind':'not_started','evidence':'Coding boundary was not started; inspected ceased source '+
                source['dispatch']['execution_id']+' boundary '+source_proof['boundary_id']}
            atomic(root/'cessation.json',proof,immutable=True)
            cancellation_fence(root,admission)
            read_deadline=min(admission['deadline'],time.time()+120);reads=0
            def fenced_query(endpoint):
                nonlocal reads
                cancellation_fence(root,admission)
                reads+=1
                if reads>64 or time.time()>=read_deadline:raise ValueError('Retained review read budget exhausted')
                value=query(endpoint,timeout=min(30,read_deadline-time.time())) if query is github else query(endpoint)
                cancellation_fence(root,admission)
                return value
            verification=verify(admission,result,records,query=fenced_query,deadline=min(admission['deadline'],time.time()+120))
            cancellation_fence(root,admission)
            event={'kind':'stopped','cessation':{k:proof[k] for k in ('boundary_id','kind','evidence')},
                   'result':result,'verification':verification,'reason':reason}
            journal.event(event,terminal=True)
        except (ValueError,KeyError,TypeError,OSError,InterruptedError,TimeoutError) as error:
            reason=type(error).__name__+': '+str(error)[:1024]
            atomic(root/'failure.json',{'reason':reason})
            if not (root/'cessation.json').exists():
                atomic(root/'cessation.json',{'generation':'retained-review','boundary_id':journal.execution_id+':retained-review',
                    'kind':'not_started','evidence':'No coding boundary or writer reservation was started for the declared retained review'},immutable=True)
            proof=read(root/'cessation.json')
            result=read(root/'result.json') if (root/'result.json').exists() else None
            journal.event({'kind':'stopped','cessation':{k:proof[k] for k in ('boundary_id','kind','evidence')},
                          'result':result,'verification':{'passed':False,'evidence':[reason]},'reason':reason},terminal=True)
