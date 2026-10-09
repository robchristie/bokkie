#!/usr/bin/env python3
"""One detached, immutable workspace assignment; no engineering supervisor."""
import hashlib
import json
import os
from pathlib import Path
import selectors
import select
import signal
import subprocess
import sys
import time
import tomllib
import uuid
from common import (Journal, Reservations, atomic, canonical, digest, encoded,
                    locked, read, pidfd_open, result_bounds, event_bounds, MAX_MESSAGE)
from verification import verify

NAMESPACE='bokkie_workspace'


def tool(name, description, properties, required):
    return {'type':'function','name':name,'description':description,'deferLoading':False,
            'inputSchema':{'type':'object','properties':properties,'required':required,
                           'additionalProperties':False}}


def tools():
    string={'type':'string'}
    strings={'type':'array','items':string}
    criterion={'type':'object','properties':{'id':string,'satisfied':{'type':'boolean'},'evidence':strings},
               'required':['id','satisfied','evidence'],'additionalProperties':False}
    delivery={'type':'object','properties':{k:string for k in ('repository','pull_request','reviewed_head','merge_revision','tree')},
              'required':['repository','pull_request','reviewed_head','merge_revision','tree','checks'],
              'additionalProperties':False}
    delivery['properties']['checks']=strings
    return [{'type':'namespace','name':NAMESPACE,
             'description':'Durable task progress, questions and untrusted structured results.',
             'tools':[
                 tool('progress','Retain a meaningful progress update.',{'summary':string},['summary']),
                 tool('question','Wait for a durable Bokkie answer; ask only when facts, agreed decisions or authority require it.',
                      {'id':string,'kind':{'type':'string','enum':['routine','missing_information','new_authority','inconclusive']},
                       'prompt':string,'options':strings},['id','kind','prompt','options']),
                 tool('result','Submit the complete structured outcome after ordinary workspace delivery. Submission does not establish acceptance.',
                      {'summary':string,'criteria':{'type':'array','items':criterion},
                       'deliveries':{'type':'array','items':delivery},'limitations':strings},
                      ['summary','criteria','deliveries','limitations'])]}]


def source_observation(profile,cwd):
    try:
        canonical(cwd)
        if not any(Path(cwd).is_relative_to(Path(root)) for root in profile['write_roots']):
            return {'available':False,'reason':'command cwd is outside declared product roots'}
        def git(*args):
            value=subprocess.check_output(['git','--no-lazy-fetch','-C',cwd,*args],
                  stderr=subprocess.DEVNULL,timeout=5,env=dict(os.environ,GIT_OPTIONAL_LOCKS='0'))
            if len(value)>MAX_MESSAGE:
                raise ValueError('source observation exceeds bound')
            return value.decode().strip()
        common=git('rev-parse','--path-format=absolute','--git-common-dir')
        repository=next(r for r in profile['verification']['repositories'] if r['git_common_dir']==common)
        return {'available':True,'cwd':cwd,'repository':repository['repository'],
                'head':git('rev-parse','HEAD'),'tree':git('rev-parse','HEAD^{tree}'),
                'clean':not git('status','--porcelain=v1','--untracked-files=all')}
    except (OSError,ValueError,StopIteration,subprocess.SubprocessError):
        return {'available':False,'reason':'source identity unavailable'}


class Broker:
    def __init__(self,root):
        self.root=Path(root)
        self.journal=Journal(root)
        self.admission=self.journal.admission
        self.dispatch=self.admission['dispatch']
        self.profile=self.admission['project_profile']
        self.generation=str(uuid.uuid4())
        self.owner=None
        self.thread=None
        self.turn=None
        self.responses={}
        self.pending={}
        self.counter=0
        self.buffer=b''
        self.selector=selectors.DefaultSelector()
        self.result=None
        self.completed=None
        self.root_turns=set()
        self.contexts=set()
        self.tokens={}
        self.stderr_hash=hashlib.sha256()
        self.stderr_bytes=0
        self.reserved=False
        self.stopping=False

    def command(self):
        a,p=self.admission,self.profile
        codex_home=Path(os.environ.get('CODEX_HOME',str(Path.home()/'.codex'))).resolve()
        inherited=tomllib.loads((codex_home/'config.toml').read_text())
        overrides={'sandbox_mode':'workspace-write','approval_policy':'on-request',
                   'approvals_reviewer':'auto_review',
                   'sandbox_workspace_write.writable_roots':p['resources'],
                   'sandbox_workspace_write.network_access':p.get('network_access',True),
                   'sandbox_workspace_write.exclude_slash_tmp':True,
                   'sandbox_workspace_write.exclude_tmpdir_env_var':True,
                   'features.apps':False,'features.code_mode.enabled':False,
                   'features.code_mode.direct_only_tool_namespaces':[NAMESPACE],
                   'agents.max_threads':p.get('max_contexts',4)}
        # Model tuning belongs to this host-local role profile. Omission keeps
        # the account's configured model/effort and inherited named role files.
        overrides.update(p.get('role',{}))
        if p.get('reviewer'):
            reviewer=p['reviewer']
            if hashlib.sha256(Path(reviewer['config_file']).read_bytes()).hexdigest()!=reviewer['sha256']:
                raise ValueError('independent reviewer profile changed after admission')
            overrides['agents.'+reviewer['role']+'.config_file']=reviewer['config_file']
        for name in inherited.get('mcp_servers',{}):
            if not name or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in name):
                raise ValueError('unsupported inherited MCP name')
            overrides['mcp_servers.'+name+'.enabled']=name in p.get('readonly_mcp_servers',[])
        command=[a['bwrap'],'--die-with-parent','--unshare-pid','--new-session',
                 '--ro-bind','/','/','--proc','/proc','--dev','/dev']
        for resource in p['resources']:
            command+=['--bind',resource,resource]
        runtime_source=str(Path(__file__).resolve().parent)
        command+=['--ro-bind',runtime_source,runtime_source]
        # Keep account configuration/auth/guidance in their existing store,
        # mounted read-only. Runtime writes use private per-execution state.
        command+=['--bind',str(self.root/'agent-state'),str(codex_home)]
        mutable=('sessions','logs','log','shell_snapshots','tmp','session_index.jsonl',
                 'installation_id','thread-writer-locks','tui-thread-reference-capabilities')
        for entry in sorted(codex_home.iterdir()):
            if entry.name in mutable or '.sqlite' in entry.name:
                continue
            if entry.is_file() or entry.is_dir():
                command+=['--ro-bind',str(entry),str(entry)]
        # The payload cannot read/mutate the host outbox, answers, reservations
        # or dedicated host token. Sources resolve in Bubblewrap's host view.
        for directory in (a['runtime_root'],a['registry']):
            command+=['--tmpfs',directory,'--remount-ro',directory]
        hidden_files=[a['token_file'],a['config_file']]
        if a.get('edge_authorization_file') is not None:
            hidden_files.append(a['edge_authorization_file'])
        for path in hidden_files:
            if not any(Path(path).is_relative_to(Path(r)) for r in (a['runtime_root'],a['registry'])):
                command+=['--ro-bind','/dev/null',path]
        command+=['--chdir',p['workspace'],'--',a['codex'],'app-server','--listen','stdio://']
        for key,value in overrides.items():
            command+=['-c',key+'='+json.dumps(value)]
        return command

    def send(self,message):
        raw=encoded(message)+b'\n'
        if len(raw)>MAX_MESSAGE:
            raise ValueError('outgoing protocol frame exceeds bound')
        remaining=memoryview(raw)
        descriptor=self.owner.stdin.fileno()
        while remaining:
            if self.stopping or (self.root/'cancel.json').exists():
                raise InterruptedError('Cancellation requested during protocol write')
            if time.time()>=self.admission['deadline']:
                raise TimeoutError('Admitted execution deadline exhausted during protocol write')
            try:
                written=os.write(descriptor,remaining)
                if written<=0:
                    raise EOFError('App-server protocol write ended')
                remaining=remaining[written:]
            except BlockingIOError:
                select.select([],[descriptor],[],.25)

    def rpc(self,method,params):
        self.counter+=1
        call_id=self.counter
        self.journal.record('rpc_intent',{'id':call_id,'method':method,'params':params})
        self.send({'id':call_id,'method':method,'params':params})
        while call_id not in self.responses:
            self.pump()
        response=self.responses.pop(call_id)
        if 'error' in response:
            raise RuntimeError('app-server '+method+' failed')
        return response['result']

    def pump(self):
        if self.stopping or (self.root/'cancel.json').exists():
            raise InterruptedError('Cancellation requested')
        if time.time()>=self.admission['deadline']:
            raise TimeoutError('Admitted execution deadline exhausted')
        self.deliver_answers()
        for key,_ in self.selector.select(.25):
            chunk=os.read(key.fd,65536)
            if key.fileobj is self.owner.stderr:
                if chunk:
                    self.stderr_bytes+=len(chunk)
                    self.stderr_hash.update(chunk)
                else:
                    self.selector.unregister(key.fileobj)
                continue
            if not chunk:
                raise EOFError('App-server connection lost; launch is not replayed')
            self.buffer+=chunk
            while b'\n' in self.buffer:
                raw,self.buffer=self.buffer.split(b'\n',1)
                if len(raw)>MAX_MESSAGE:
                    raise ValueError('incoming protocol frame exceeds bound')
                self.observe(json.loads(raw))
            if len(self.buffer)>MAX_MESSAGE:
                raise ValueError('unterminated protocol frame exceeds bound')

    def observe(self,message):
        method=message.get('method')
        if 'id' in message and method is None:
            self.responses[message['id']]=message
            return
        if 'id' in message:
            self.request(message)
            return
        p=message.get('params',{})
        if method=='thread/tokenUsage/updated':
            total=p['tokenUsage']['total']['totalTokens']
            if type(total) is not int or total<0:
                raise ValueError('invalid token accounting')
            self.tokens[p['threadId']]=max(total,self.tokens.get(p['threadId'],0))
            self.journal.record('token_usage',p)
            if sum(self.tokens.values())>self.dispatch['assignment']['limits']['max_tokens']:
                raise RuntimeError('Observed token budget exhausted, including cached input')
        if method in ('item/started','item/completed','thread/started','turn/started','turn/completed'):
            self.journal.record('protocol_event',message)
            item=p.get('item',{})
            if method=='thread/started':
                self.contexts.add(p['thread']['id'])
                if len(self.contexts)>self.profile.get('max_contexts',4):
                    raise RuntimeError('Observed context budget exhausted')
            if item.get('type')=='commandExecution':
                self.journal.record('command_observation',{'phase':'started' if method=='item/started' else 'completed',
                    'item':item,'source':source_observation(self.profile,item.get('cwd',''))})
            if method=='turn/started':
                self.root_turns.add((p['threadId'],p['turn']['id']))
                if len(self.root_turns)>self.dispatch['assignment']['limits']['max_turns']:
                    raise RuntimeError('Observed turn budget exhausted')
            if method=='turn/completed' and p.get('threadId')==self.thread:
                self.completed=p['turn']

    def reply(self,key,request,value,*,success=True):
        response={'id':request['id'],'result':{'success':success,
                  'contentItems':[{'type':'inputText','text':json.dumps(value)}]}}
        atomic(self.root/'requests'/(key+'.reply.json'),response,immutable=True)
        # Persist the write intent first. Failure after this point stops the
        # namespace; a lost tool acknowledgement is never blindly redelivered.
        atomic(self.root/'requests'/(key+'.sent.json'),{'digest':digest(response)},immutable=True)
        self.send(response)

    def request(self,request):
        method=request['method']
        if method!='item/tool/call':
            # auto_review owns ordinary escalation. Anything routed back to the
            # host is declined, with no prefix/session/policy amendment grant.
            self.journal.event({'kind':'attention','reason':'Unsupported runtime approval or tool request: '+method})
            if method in ('item/commandExecution/requestApproval','item/fileChange/requestApproval'):
                self.send({'id':request['id'],'result':{'decision':'decline'}})
            elif method=='item/permissions/requestApproval':
                self.send({'id':request['id'],'result':{'permissions':{},'scope':'turn'}})
            else:
                self.send({'id':request['id'],'error':{'code':-32601,'message':'Use the task-scoped question tool'}})
            return
        p=request['params']
        key=digest({'generation':self.generation,'request':request})
        if (self.root/'requests'/(key+'.sent.json')).exists():
            return
        atomic(self.root/'requests'/(key+'.request.json'),request,immutable=True)
        if p['threadId']!=self.thread or p.get('namespace')!=NAMESPACE:
            self.reply(key,request,{'error':'Only the assignment root can report task state'},success=False)
            return
        args=p['arguments']
        if not isinstance(args,dict):
            raise ValueError('task tool arguments must be an object')
        if p['tool']=='progress':
            if set(args)!={'summary'} or not isinstance(args['summary'],str) or len(args['summary'].encode())>8192:
                raise ValueError('invalid progress')
            self.journal.event({'kind':'progress','summary':args['summary']})
            self.reply(key,request,{'retained':True})
        elif p['tool']=='question':
            if (set(args)!={'id','kind','prompt','options'} or
                    args['kind'] not in ('routine','missing_information','new_authority','inconclusive') or
                    not isinstance(args['id'],str) or not 1<=len(args['id'])<=256 or
                    not isinstance(args['prompt'],str) or not isinstance(args['options'],list) or
                    any(not isinstance(v,str) for v in args['options']) or len(encoded(args))>16384):
                raise ValueError('invalid task question')
            question_key=hashlib.sha256(args['id'].encode()).hexdigest()
            question_path=self.root/'requests'/(question_key+'.question.json')
            if not question_path.exists():
                atomic(question_path,args,immutable=True)
                self.journal.event({'kind':'question','question':args})
            elif read(question_path)!=args:
                raise ValueError('question identity reused with changed payload')
            self.pending[key]=request
            self.deliver_answers()
        elif p['tool']=='result':
            try:
                result_bounds(args)
                if len(encoded(args))>65536:
                    raise ValueError('structured result exceeds retained submission bound')
            except ValueError as error:
                atomic(self.root/'requests'/(digest(args)+'.rejected-result.json'),args,immutable=True)
                self.reply(key,request,{'error':str(error),'retained':'private rejected result; acceptance remains pending'},success=False)
                return
            atomic(self.root/'result.json',args,immutable=True)
            self.result=args
            self.reply(key,request,{'retained':True,'acceptance':'pending trusted delivery verification'})
        else:
            self.reply(key,request,{'error':'Unoffered task tool'},success=False)

    def deliver_answers(self):
        for key,request in list(self.pending.items()):
            question_id=request['params']['arguments']['id']
            answer_path=self.root/'answers'/(hashlib.sha256(question_id.encode()).hexdigest()+'.json')
            if answer_path.exists():
                answer=read(answer_path)
                if answer['question_id']!=question_id:
                    raise ValueError('answer identity mismatch')
                self.reply(key,request,{'answer':answer})
                del self.pending[key]

    def run(self,*,preflight=False):
        if (self.root/'launch-committed.json').exists():
            return
        reason='Workspace runtime ended before a structured result'
        reservations=Reservations(self.admission['registry'])
        try:
            reservations.acquire(self.journal.execution_id,self.generation,self.profile['resources'])
            self.reserved=True
            if (self.root/'cancel.json').exists():
                raise InterruptedError('Cancellation requested before runtime launch')
            if time.time()>=self.admission['deadline']:
                raise TimeoutError('Controller-pinned deadline expired before runtime launch')
            command=self.command()
            marker={'generation':self.generation,'boundary_id':self.journal.execution_id+':'+self.generation,
                    'dispatch_digest':self.admission['dispatch_digest']}
            atomic(self.root/'launch-committed.json',marker,immutable=True)
            self.journal.record('launch_committed',marker)
            parent_fd=pidfd_open(os.getpid())
            try:
                environment=dict(os.environ,TMPDIR=self.profile['scratch'])
                self.owner=subprocess.Popen([sys.executable,str(Path(__file__).with_name('reaper.py')),
                    str(self.root),str(parent_fd),*command],pass_fds=(parent_fd,),
                    cwd=self.profile['workspace'],env=environment,
                    stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
            finally:
                os.close(parent_fd)
            self.selector.register(self.owner.stdout,selectors.EVENT_READ)
            self.selector.register(self.owner.stderr,selectors.EVENT_READ)
            os.set_blocking(self.owner.stdin.fileno(),False)
            initial=self.rpc('initialize',{'clientInfo':{'name':'bokkie_workspace','title':'Bokkie workspace execution','version':'1'},
                         'capabilities':{'experimentalApi':True}})
            if '/0.160.1 ' not in initial['userAgent']:
                raise ValueError('Workspace runtime requires qualified Codex 0.160.1')
            self.send({'method':'initialized','params':{}})
            config=self.rpc('config/read',{'cwd':self.profile['workspace'],'includeLayers':False})['config']
            role=self.profile.get('role',{})
            if (role.get('model',config['model'])!=config['model'] or
                    role.get('model_reasoning_effort',config['model_reasoning_effort'])!=config['model_reasoning_effort']):
                raise ValueError('effective role differs from profile')
            expected={'model':config['model'],'effort':config['model_reasoning_effort']}
            self.journal.record('effective_role',expected)
            if self.profile.get('reviewer'):
                self.journal.record('reviewer_profile',self.profile['reviewer'])
            developer='Execute the supplied immutable Bokkie assignment through the normal selected workspace. Follow personal and workspace guidance, then affected product guidance; the workspace owns planning, implementation, independent review, verification, CI and ordinary authorised delivery. There is no Bokkie engineering supervisor. Use bokkie_workspace.progress for meaningful updates, question only for missing information, inconclusive decisions or new authority, and result for a complete structured result. Only the assignment root may call these tools. Runtime fields and profile limits cannot be edited. Source-only delivery does not grant deployment or new credentials/access-policy changes. Model result submission leaves trusted acceptance pending. Preserve completed work if proof is unavailable.'
            started=self.rpc('thread/start',{'cwd':self.profile['workspace'],'ephemeral':True,
                  'serviceName':'bokkie_workspace','developerInstructions':developer,'dynamicTools':tools()})
            self.thread=started['thread']['id']
            sources=started['instructionSources']
            required=self.profile.get('required_instruction_sources',[str(Path(self.profile['workspace'])/'AGENTS.md')])
            if (not all(path in sources for path in required) or
                    started['model']!=expected['model'] or started['reasoningEffort']!=expected['effort'] or
                    started['cwd']!=self.profile['workspace'] or started['sandbox']['type']!='workspaceWrite' or
                    started['approvalPolicy']!='on-request' or started['approvalsReviewer']!='auto_review' or
                    set(started['sandbox'].get('writableRoots',[]))!=set(self.profile['resources']) or
                    started['sandbox'].get('networkAccess')!=self.profile.get('network_access',True) or
                    not started['sandbox'].get('excludeSlashTmp') or not started['sandbox'].get('excludeTmpdirEnvVar') or
                    not any(e['environmentId']=='local' for e in started['thread']['environments'])):
                raise ValueError('effective workspace route or permissions differ from profile')
            self.journal.record('thread_identity',{'thread_id':self.thread,'settings':{k:started[k] for k in ('model','reasoningEffort','cwd','sandbox','instructionSources','approvalsReviewer')}})
            self.journal.record('guidance_identities',[{'path':path,'sha256':hashlib.sha256(Path(path).read_bytes()).hexdigest()} for path in sources])
            skills=self.rpc('skills/list',{'cwds':[self.profile['workspace']],'forceReload':True})
            self.journal.record('enabled_skills',[{'name':skill['name'],'path':skill['path'],
                'sha256':hashlib.sha256(Path(skill['path']).read_bytes()).hexdigest()}
                for entry in skills['data'] for skill in entry['skills'] if skill.get('enabled',True)])
            self.journal.event({'kind':'started','runtime_id':self.thread,'instruction_sources':sources})
            if preflight:
                reason='No-model workspace preflight completed'
                atomic(self.root/'preflight.json',{'model_calls':0,'thread_id':self.thread,
                       'effective_role':expected,'instruction_sources':sources,
                       'sandbox':started['sandbox'],'environments':started['thread']['environments']},immutable=True)
                return
            prompt=json.dumps({'assignment':self.dispatch['assignment'],
                    'host_profile':{'workspace_entry':self.profile['workspace'],'write_roots':self.profile['write_roots'],
                                    'scratch':self.profile['scratch'],
                                    'verification':self.profile['verification']},
                    'instruction':'Complete the saved outcome and its criteria within permitted actions and decision rules. Read the workspace map and affected product guidance before modifying it. Run each declared canonical command as a separate exact shell command on the clean reviewed candidate so its observed item can be attributed. Report progress, ask required questions, then submit attributable delivered results.'})
            self.turn=self.rpc('turn/start',{'threadId':self.thread,'input':[{'type':'text','text':prompt}]})['turn']['id']
            while self.completed is None:
                self.pump()
            reason='Workspace turn '+self.completed['status']
            if self.result is None:
                reason+=' without a structured result; retained delivery requires reconciliation'
        except Exception as error:
            reason=type(error).__name__+': '+str(error)[:1024]
            atomic(self.root/'failure.json',{'type':type(error).__name__,'reason':reason})
        finally:
            if self.owner is not None:
                if self.owner.poll() is None:
                    self.owner.terminate()
                try:
                    self.owner.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    pass  # Never kill the trusted owner and presume cessation.
            elif not (self.root/'launch-committed.json').exists() or self.reserved:
                atomic(self.root/'cessation.json',{'generation':self.generation,
                       'boundary_id':self.journal.execution_id+':'+self.generation,'kind':'not_started',
                       'evidence':'This broker spawned no cleanup owner or payload'},immutable=True)
            self.selector.close()
            atomic(self.root/'stderr-diagnostic.json',{'bytes':self.stderr_bytes,'sha256':self.stderr_hash.hexdigest()})
            stopped(self.root,reason)


def stopped(root,reason,*,reverify=False):
    root=Path(root)
    journal=Journal(root)
    if not (root/'cessation.json').exists():
        if not journal.events() or journal.events()[-1]['event'].get('kind')!='attention':
            journal.event({'kind':'attention','reason':'Cleanup owner has no cessation receipt; writable resources remain reserved'},terminal=True)
        return
    proof=read(root/'cessation.json')
    marker=read(root/'launch-committed.json') if (root/'launch-committed.json').exists() else None
    if marker and (proof['generation']!=marker['generation'] or proof['boundary_id']!=marker['boundary_id']):
        raise ValueError('cessation receipt does not match the immutable boundary')
    cessation={k:proof[k] for k in ('boundary_id','kind','evidence')}
    registry=Reservations(journal.admission['registry'])
    reservation=registry.root/(hashlib.sha256(journal.execution_id.encode()).hexdigest()+'.json')
    if reservation.exists() and read(reservation)['generation']!=proof['generation']:
        journal.event({'kind':'attention','reason':'Reservation generation has no matching cessation proof'},terminal=True)
        return
    result=read(root/'result.json') if (root/'result.json').exists() else None
    events=journal.events()
    if not reverify and any(e['event']['kind']=='stopped' for e in events):
        return
    verification=None
    if result is not None:
        try:
            records=journal.records()
            verification=verify(journal.admission,result,records,
                                deadline=time.time()+120 if reverify else None)
        except ValueError:
            verification={'passed':False,'evidence':['Runtime evidence is unavailable; retained delivery needs reconciliation']}
    event={'kind':'stopped','cessation':cessation,'result':result,'verification':verification,'reason':reason}
    try:
        event_bounds(event)
    except ValueError:
        # Preserve oversized/unusable result locally; emit a transport-safe
        # cessation receipt which explicitly leaves acceptance incomplete.
        event={'kind':'stopped','cessation':cessation,'result':None,
               'verification':{'passed':False,'evidence':['Retained result or verification exceeds Store bounds; inspect private runtime state']},
               'reason':'Workspace ceased; retained delivery needs bounded reconciliation'}
    if not events or events[-1]['event']!=event:
        journal.event(event,terminal=True)
    if reservation.exists():
        saved=read(reservation)
        if not saved['released']:
            registry.release(journal.execution_id,proof['generation'],cessation)


def serve(root):
    with locked(Path(root)/'broker.lock',blocking=False):
        broker=Broker(root)
        def stop(_signal,_frame):
            broker.stopping=True
        signal.signal(signal.SIGTERM,stop)
        signal.signal(signal.SIGINT,stop)
        broker.run()


def launch(root):
    log=open(Path(root)/'broker-diagnostic.log','ab',buffering=0)
    try:
        return subprocess.Popen([sys.executable,str(Path(__file__).resolve()),'serve',str(root)],
                   stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True,close_fds=True).pid
    finally:
        log.close()


if __name__=='__main__':
    if sys.argv[1]=='serve':
        serve(sys.argv[2])
    elif sys.argv[1]=='launch':
        print(json.dumps({'broker_pid':launch(sys.argv[2])}))
