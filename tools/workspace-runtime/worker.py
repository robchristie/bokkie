#!/usr/bin/env python3
"""Outward authenticated exchange; restart reconnects immutable detached jobs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time
import uuid
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import HTTPRedirectHandler, Request, build_opener
from common import Config, Journal, MAX_MESSAGE, MAX_EXCHANGE_BYTES, atomic, control, encoded, locked, private_directory, read
from broker import Broker, launch, stopped


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self,request,fp,code,message,headers,new_url):
        return None


class Worker:
    def __init__(self,config):
        self.config=config
        self.stopping=False

    def roots(self):
        roots=sorted(self.config.executions.iterdir())
        if len(roots)>128:
            raise ValueError('execution catalogue exceeds host bound')
        for root in roots:
            if root.is_symlink() or not root.is_dir():
                raise ValueError('invalid execution catalogue entry')
        return roots

    def owned(self,root):
        try:
            with locked(root/'broker.lock',blocking=False):
                return False
        except BlockingIOError:
            return True

    def reconcile(self,root):
        if self.owned(root):
            return
        with locked(root/'broker.lock',blocking=False):
            if (root/'cessation.json').exists():
                stopped(root,'Recovered trusted cessation receipt after broker exit')
            elif (root/'launch-committed.json').exists():
                journal=Journal(root)
                events=journal.events()
                if not events or events[-1]['event']['kind']!='attention':
                    journal.event({'kind':'attention','reason':'Committed broker is unavailable without descendant cessation proof; reservation retained'},terminal=True)

    def request(self,payload):
        raw=encoded(payload)
        if len(raw)>MAX_EXCHANGE_BYTES:
            raise ValueError('exchange request exceeds bound')
        url=self.config.value['server_url'].rstrip('/')+'/workspace-hosts/'+quote(self.config.value['host_id'],safe='')+'/exchange'
        headers={'X-Bokkie-Host-Token':self.config.token,
                 'Content-Type':'application/json','Accept':'application/json'}
        if self.config.edge_authorization is not None:
            headers['Authorization']=self.config.edge_authorization
        request=Request(url,data=raw,method='POST',headers=headers)
        try:
            response=build_opener(NoRedirect()).open(request,timeout=20)
        except HTTPError as error:
            error.close()
            raise
        with response:
            raw=response.read(MAX_MESSAGE+1)
        if len(raw)>MAX_MESSAGE:
            raise ValueError('exchange response exceeds bound')
        value=json.loads(raw)
        if not isinstance(value,dict) or set(value)!={'acknowledgements','dispatches','controls'}:
            raise ValueError('invalid exchange response')
        for key in value:
            if not isinstance(value[key],list) or len(value[key])>128:
                raise ValueError('exchange catalogue exceeds bound')
        return value

    def exchange(self):
        events=[]
        heartbeats=[]
        by_id={}
        for root in self.roots():
            journal=Journal(root)
            by_id[journal.execution_id]=root
            self.reconcile(root)
            retained=journal.events()
            ack=read(root/'ack.json')['sequence'] if (root/'ack.json').exists() else 0
            for event in retained:
                if event['sequence']>ack and len(events)<64:
                    candidate=events+[event]
                    if len(encoded({'events':candidate,'heartbeats':heartbeats}))>MAX_EXCHANGE_BYTES-65536:
                        break
                    events=candidate
            if self.owned(root) and len(heartbeats)<100 and not any(e['event']['kind']=='stopped' for e in retained):
                heartbeats.append(journal.execution_id)
        response=self.request({'events':events,'heartbeats':heartbeats})
        # Acknowledgements are durable before the next exchange. Lost replies
        # replay exactly the same sequenced payloads; they cannot relaunch jobs.
        for acknowledgement in response['acknowledgements']:
            root=by_id.get(acknowledgement['execution_id'])
            if root is None:
                raise ValueError('acknowledgement names an unknown execution')
            last=len(Journal(root).events())
            previous=read(root/'ack.json')['sequence'] if (root/'ack.json').exists() else 0
            sequence=acknowledgement['sequence']
            if type(sequence) is not int or not previous<=sequence<=last:
                raise ValueError('invalid acknowledgement sequence')
            atomic(root/'ack.json',{'sequence':sequence})
        for dispatch in response['dispatches']:
            root=self.config.admit(dispatch)
            by_id[dispatch['execution_id']]=root
            if not (root/'launch-committed.json').exists() and not (root/'cessation.json').exists():
                launch(root)
        for value in response['controls']:
            root=by_id.get(value['execution_id'])
            if root is None:
                raise ValueError('control names an unknown execution')
            control(root,value)
        return {'events_sent':len(events),'dispatches_observed':len(response['dispatches']),
                'controls_observed':len(response['controls'])}

    def run(self,once=False):
        while not self.stopping:
            try:
                result=self.exchange()
                atomic(self.config.root/'worker-status.json',{'state':'connected',**result})
            except (URLError,HTTPError,OSError,ValueError) as error:
                # No raw network diagnostics, endpoint headers or token appear
                # in status. Brokers continue under their admitted deadlines.
                atomic(self.config.root/'worker-status.json',{'state':'disconnected','error_type':type(error).__name__})
                if once:
                    raise
            if once:
                return
            end=time.monotonic()+self.config.value.get('poll_seconds',2)
            while not self.stopping and time.monotonic()<end:
                time.sleep(max(0,min(.25,end-time.monotonic())))


def status(config):
    executions=[]
    for root in Worker(config).roots():
        journal=Journal(root)
        events=journal.events()
        executions.append({'execution_id':journal.execution_id,'events':len(events),
             'last_event':events[-1]['event'] if events else None,
             'launch_committed':(root/'launch-committed.json').exists(),
             'cessation_recorded':(root/'cessation.json').exists()})
    return {'host_id':config.value['host_id'],'runtime_root':str(config.root),'executions':executions}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config',required=True)
    sub=parser.add_subparsers(dest='operation',required=True)
    run=sub.add_parser('run');run.add_argument('--once',action='store_true')
    sub.add_parser('status')
    sub.add_parser('allowlist')
    preflight=sub.add_parser('preflight');preflight.add_argument('project_id')
    cancel=sub.add_parser('cancel');cancel.add_argument('execution_id')
    verify=sub.add_parser('verify');verify.add_argument('execution_id')
    args=parser.parse_args()
    config=Config(args.config)
    if args.operation=='allowlist':
        print(json.dumps({'host_id':config.value['host_id'],'projects':list(config.projects.values())},indent=2))
    elif args.operation=='status':
        print(json.dumps(status(config),indent=2))
    elif args.operation=='preflight':
        p=config.projects[args.project_id]
        config.executions=private_directory(config.root/'preflights')
        dispatch={'execution_id':'preflight-'+str(uuid.uuid4()),'task_id':'preflight','obligation_id':'preflight',
            'definition_revision':1,'profile_revision':p['profile_revision'],
            'admitted_at':int(time.time()),'deadline_at':int(time.time())+min(60,p['limits']['max_seconds']),'assignment':{
            'project':{'id':p['id'],'revision':p['revision'],'registration':{'host':p['host'],'workspace':p['workspace']}},
            'brief':{'outcome':'No-model runtime observation'},'criteria':[],'permitted_actions':[],
            'decision_rules':'No model turn','limits':{'max_seconds':min(60,p['limits']['max_seconds']),'max_turns':1,'max_tokens':1}}}
        root=config.admit(dispatch)
        with locked(root/'broker.lock',blocking=False):
            Broker(root).run(preflight=True)
        if not (root/'preflight.json').exists():
            raise RuntimeError('workspace preflight failed; inspect private failure.json')
        print(json.dumps({'evidence_root':str(root),**read(root/'preflight.json')},indent=2))
    elif args.operation in ('cancel','verify'):
        root=config.executions/hashlib.sha256(args.execution_id.encode()).hexdigest()
        if Journal(root).execution_id!=args.execution_id:
            raise ValueError('execution identity mismatch')
        if args.operation=='cancel':
            control(root,{'execution_id':args.execution_id,'cancel':True,'answers':[]})
            print(json.dumps({'execution_id':args.execution_id,'cancellation_requested':True}))
        else:
            with locked(root/'broker.lock',blocking=False):
                stopped(root,'Reconciled retained delivery evidence without restarting the workspace',reverify=True)
            print(json.dumps(status(config),indent=2))
    else:
        worker=Worker(config)
        def stop(_signal,_frame):
            worker.stopping=True
        signal.signal(signal.SIGTERM,stop)
        signal.signal(signal.SIGINT,stop)
        with locked(config.root/'worker.lock',blocking=False):
            worker.run(args.once)


if __name__=='__main__':
    main()
