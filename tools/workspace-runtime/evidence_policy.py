"""Report-task policy derivation and no-model enforcement probes."""
import hashlib
import json
import os
from pathlib import Path
import socket
import tempfile
import time
import tomllib
from urllib.parse import urlsplit
from common import atomic

MIRROR = '/bokkie-evidence'
FORWARDING = ('SSH_AUTH_SOCK', 'SSH_AGENT_PID', 'DBUS_SESSION_BUS_ADDRESS',
              'DBUS_SYSTEM_BUS_ADDRESS', 'DOCKER_HOST', 'DOCKER_CONTEXT',
              'CONTAINER_HOST', 'XDG_RUNTIME_DIR', 'DISPLAY', 'WAYLAND_DISPLAY',
              'GH_TOKEN','GITHUB_TOKEN','GH_ENTERPRISE_TOKEN','GITHUB_ENTERPRISE_TOKEN',
              'HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','http_proxy','https_proxy','all_proxy')
MASKS = ('/run', '/var/run', '/tmp', '/var/tmp')
CONTROL_SLOTS = ('.git','.agents','.codex','.aws','.azure','.config','.kube','.ssh','.docker','.gnupg')


def environment(scratch):
    value = {key: val for key, val in os.environ.items() if key not in FORWARDING}
    value['TMPDIR'] = scratch
    return value


def policy(profile, reviewer=False):
    if reviewer:
        return {'type': 'readOnly', 'networkAccess': False}
    return {'type': 'workspaceWrite', 'writableRoots': [profile['scratch']], 'networkAccess': False,
            'excludeSlashTmp': True, 'excludeTmpdirEnvVar': True}


def derived_roles(root, codex_home, inherited, profile):
    """All configured children inherit a closed policy; named tuning is retained."""
    root = Path(root)
    directory = root/'agent-state'/'agents'
    directory.mkdir(mode=0o700, exist_ok=True)
    selected = {}
    agents_dir = codex_home/'agents'
    if agents_dir.is_dir():
        for path in agents_dir.glob('*.toml'):
            selected[path.stem] = path
    for name, value in inherited.get('agents', {}).items():
        if isinstance(value, dict) and value.get('config_file'):
            path = Path(value['config_file'])
            selected[name] = path if path.is_absolute() else codex_home/path
    reviewer = profile['reviewer']
    selected[reviewer['role']] = Path(reviewer['config_file'])
    overrides, identities = {}, []
    for name, path in sorted(selected.items()):
        if not name or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-' for c in name):
            raise ValueError('unsupported inherited child role name')
        value = tomllib.loads(path.read_text())
        safe = {key: value[key] for key in ('name', 'description', 'model', 'model_reasoning_effort', 'developer_instructions') if key in value}
        safe['name']=name
        safe.update(sandbox_mode='read-only', approval_policy='never', approvals_reviewer='user', web_search='disabled')
        raw = '\n'.join(key+' = '+json.dumps(val, ensure_ascii=False) for key, val in safe.items())+'\n'
        raw += '\n[sandbox_read_only]\nnetwork_access = false\n\n[features]\napps = false\n'
        # Drop role-local MCP configuration. The root's inherited servers are
        # already disabled; transport-free role tables are invalid standalone
        # configuration in the qualified CLI and may make the role unavailable.
        for plugin in inherited.get('plugins', {}):
            raw += '\n[plugins.'+json.dumps(plugin)+']\nenabled = false\n'
        target = directory/(name+'.toml')
        if target.exists() and target.read_text() != raw:
            raise ValueError('derived child policy changed after admission')
        if not target.exists():
            with target.open('x') as stream:
                stream.write(raw)
            target.chmod(0o400)
        parsed = tomllib.loads(raw)
        if parsed['sandbox_mode'] != 'read-only' or parsed['sandbox_read_only']['network_access'] is not False:
            raise ValueError('derived child policy is not closed')
        description=safe.get('description')
        if not isinstance(description,str) or not description.strip():
            raise ValueError('derived named role requires selection guidance')
        overrides['agents.'+name+'.config_file'] = str(codex_home/'agents'/target.name)
        overrides['agents.'+name+'.description'] = description
        identities.append({'role': name, 'config_file':str(codex_home/'agents'/target.name),
                           'description':description, 'sha256': hashlib.sha256(raw.encode()).hexdigest(),
                           'model': safe.get('model'), 'effort': safe.get('model_reasoning_effort'),
                           'sandbox': policy(profile, reviewer=True), 'approval_policy': 'never',
                           'approvals_reviewer': 'user'})
    return overrides, identities


def reviewer_selection_proof(config, profile, root, identities):
    reviewer=profile['reviewer'];name=reviewer['role']
    registered=config.get('agents')
    if not isinstance(registered,dict) or registered.get('enabled') is False:
        raise ValueError('effective named reviewer catalogue is unavailable')
    entry=registered.get(name)
    selected=next((value for value in identities if value['role']==name),None)
    if (not isinstance(entry,dict) or selected is None or
            entry.get('config_file')!=selected['config_file'] or
            entry.get('description')!=selected['description'] or not entry['description'].strip()):
        raise ValueError('effective named reviewer registration is unavailable or changed')
    path=Path(root)/'agent-state'/'agents'/(name+'.toml')
    raw=path.read_bytes();value=tomllib.loads(raw.decode())
    if (hashlib.sha256(raw).hexdigest()!=selected['sha256'] or value.get('name')!=name or
            value.get('description')!=selected['description'] or
            not isinstance(value.get('developer_instructions'),str) or not value['developer_instructions'].strip() or
            value.get('model')!=reviewer['model'] or value.get('model_reasoning_effort')!=reviewer['reasoning_effort'] or
            value.get('sandbox_mode')!='read-only' or value.get('approval_policy')!='never' or
            value.get('approvals_reviewer')!='user' or value.get('sandbox_read_only',{}).get('network_access') is not False or
            value.get('web_search')!='disabled' or value.get('features',{}).get('apps') is not False):
        raise ValueError('named reviewer layer differs from its protected tuning or policy')
    return {'role':name,'config_file':selected['config_file'],'sha256':selected['sha256'],
            'model':reviewer['model'],'reasoning_effort':reviewer['reasoning_effort'],
            'registered_description':True,'standard_agent_discovery':True,'model_calls':0}


def routing_proof(value):
    if not isinstance(value,dict) or not isinstance(value.get('account'),dict):
        raise ValueError('No-model account discovery has no usable account')
    account_type=value['account'].get('type')
    if account_type=='chatgpt':
        routing=value.get('workspaceRouting')
        if not isinstance(routing,dict):
            raise ValueError('No-model ChatGPT workspace routing is unavailable')
        origin=urlsplit(routing.get('backendOrigin',''))
        if (origin.scheme!='https' or not origin.hostname or origin.username or origin.password or
                origin.path not in ('','/') or origin.query or origin.fragment or
                not isinstance(routing.get('chatgptAccountId'),str) or not routing['chatgptAccountId'] or
                routing.get('accountRoutingOverride') not in ('NO_CONSTRAINT','us','us_cr')):
            raise ValueError('No-model ChatGPT workspace routing is invalid')
        return {'account_type':'chatgpt','workspace_routing_verified':True,'model_calls':0}
    if account_type=='apiKey':
        return {'account_type':'apiKey','workspace_routing_verified':False,'workspace_routing_applicable':False,'model_calls':0}
    raise ValueError('No-model account discovery returned an unsupported account')


def closed_mcp_inventory(inventory):
    if (not isinstance(inventory,dict) or inventory.get('nextCursor') is not None or
            not isinstance(inventory.get('data'),list) or len(inventory['data'])>100):
        raise ValueError('effective report MCP inventory is incomplete')
    facts=[]
    for entry in inventory['data']:
        if (not isinstance(entry,dict) or 'serverCapabilities' not in entry or entry.get('runtimeStatus')!='disabled' or
                entry.get('tools')!={} or entry.get('resources')!=[] or entry.get('resourceTemplates')!=[] or
                entry.get('serverCapabilities') is not None):
            raise ValueError('effective report exposes inherited MCP servers')
        facts.append({'runtime_status':'disabled','tools':0,'resources':0,'resource_templates':0})
    return {'inherited_mcp_servers':len(facts),'servers':facts,'additional_page':False}


def public_ca_paths():
    # Preserve the existing platform trust store, including distro symlinks
    # which resolve outside /etc/ssl. Never mount a broad /etc or home tree.
    selected=[]
    for name in ('/etc/ssl/cert.pem','/etc/ssl/certs/ca-certificates.crt','/etc/pki/tls/certs/ca-bundle.crt'):
        path=Path(name)
        if not path.exists():continue
        resolved=path.resolve(strict=True)
        if not resolved.is_file() or not any(resolved.is_relative_to(Path(root)) for root in
                ('/etc/ssl','/etc/ca-certificates','/etc/pki','/usr/share')):
            raise ValueError('unsupported platform public certificate store')
        if str(resolved) not in selected:selected.append(str(resolved))
    return selected


def code_mode_host_path(codex):
    native=Path(codex).resolve(strict=True)
    if native.name!='codex':
        return None  # Synthetic filesystem fixtures and the separate JS layout.
    companion=native.with_name('codex-code-mode-host')
    if companion.is_symlink() or not companion.is_file() or not os.access(companion,os.X_OK):
        raise ValueError('qualified native Codex code-mode companion is missing or unsupported')
    return str(companion)


def companion_readiness(broker):
    companion=code_mode_host_path(broker.admission['codex'])
    if companion is None:
        raise ValueError('report companion readiness requires the qualified native Codex layout')
    code='''import json,os,select,struct,subprocess,sys,time
binary=sys.argv[1];deadline=time.monotonic()+5
process=subprocess.Popen([binary,'--listen','stdio://'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
def exact(count):
 raw=bytearray()
 while len(raw)<count:
  remaining=deadline-time.monotonic()
  if remaining<=0 or not select.select([process.stdout],[],[],remaining)[0]:raise ValueError('companion readiness timed out')
  chunk=os.read(process.stdout.fileno(),count-len(raw))
  if not chunk:raise ValueError('companion readiness ended early')
  raw.extend(chunk)
 return bytes(raw)
def receive():
 size=struct.unpack('<I',exact(4))[0]
 if size>4096:raise ValueError('companion readiness frame exceeds bound')
 return json.loads(exact(size))
def rpc(value):
 raw=json.dumps(value,separators=(',',':')).encode()
 process.stdin.write(struct.pack('<I',len(raw))+raw);process.stdin.flush()
 return receive()
try:
 ready=rpc({'type':'connection/hello','supportedVersions':[1],'requiredCapabilities':[],'optionalCapabilities':[]})
 if ready!={'type':'connection/ready','selectedVersion':1,'capabilities':[]}:raise ValueError('unsupported companion readiness protocol')
 opened=rpc({'type':'operation/request','id':1,'request':{'method':'session/open','sessionId':'bokkie-readiness'}})
 if opened.get('type')!='operation/response' or opened.get('id')!=1 or opened.get('result',{}).get('value')!={'type':'session/ready','sessionId':'bokkie-readiness'} or opened['result'].get('status')!='ok':raise ValueError('companion session not ready')
 started=rpc({'type':'operation/request','id':2,'request':{'method':'session/execute','sessionId':'bokkie-readiness','request':{
  'tool_call_id':'bokkie-readiness','enabled_tools':[],
  'source':'const probe = 1 + 1; if (probe !== 2) throw new Error("readiness failed");',
  'yield_time_ms':1000,'max_output_tokens':128}}})
 if started.get('type')!='operation/response' or started.get('id')!=2 or started.get('result',{}).get('status')!='ok' or started['result'].get('value',{}).get('type')!='execution/started':raise ValueError('companion execution did not start')
 finished=receive()
 if finished.get('type')!='execute/initialResponse' or finished.get('id')!=2 or finished.get('result',{}).get('status')!='ok':raise ValueError('companion execution response is invalid')
 result=finished['result']['value']['Result']
 if 'error_text' not in result or result['error_text'] is not None or result.get('content_items')!=[] or result.get('cell_id')!=started['result']['value'].get('cellId'):raise ValueError('companion execution failed')
 process.stdin.close();process.wait(timeout=max(.01,deadline-time.monotonic()))
 if process.returncode!=0:raise ValueError('companion readiness failed on shutdown')
 print(json.dumps({'protocol_version':1,'session_ready':True,'execution_completed':True,'enabled_tools':0,'model_calls':0}))
finally:
 if process.poll() is None:process.kill()
 process.wait()
'''
    proofs=[]
    for reviewer in (False,True):
        response=broker.rpc('command/exec',{'command':['/usr/bin/python3','-c',code,companion],
            'cwd':broker.profile['workspace'],'timeoutMs':6000,'outputBytesCap':4096,
            'sandboxPolicy':policy(broker.profile,reviewer)})
        if response.get('exitCode')!=0:
            raise ValueError('no-model code-mode companion execution failed')
        observed=json.loads(response['stdout'])
        wanted={'protocol_version':1,'session_ready':True,'execution_completed':True,'enabled_tools':0,'model_calls':0}
        if observed!=wanted:raise ValueError('no-model code-mode companion readiness is unverified')
        proofs.append({'reviewer':reviewer,**observed})
    fingerprint=hashlib.sha256()
    with Path(companion).open('rb') as stream:
        while chunk:=stream.read(65536):
            if time.time()>=broker.admission['deadline']:raise ValueError('companion identity observation deadline exhausted')
            fingerprint.update(chunk)
    broker.journal.record('evidence_companion_readiness',{'path':companion,'sha256':fingerprint.hexdigest(),'proofs':proofs})
    return proofs


def mounts(admission, profile, codex_home, root):
    """Start from an empty filesystem so unselected home socket aliases disappear."""
    command = [admission['bwrap'], '--die-with-parent', '--unshare-pid', '--new-session',
               '--proc', '/proc', '--dev', '/dev']
    # Tooling and resolver/CA files are fixed host infrastructure, never candidate cwd.
    for path in ('/usr', '/bin', '/lib', '/lib64', '/etc/ssl', '/etc/resolv.conf', '/etc/hosts',
                 '/etc/passwd', '/etc/group', '/etc/nsswitch.conf', '/etc/ld.so.cache'):
        if Path(path).exists():
            command += ['--ro-bind', path, path]
    for path in public_ca_paths():
        command+=['--ro-bind',path,path]
    executable = str(Path(admission['codex']).resolve())
    command += ['--ro-bind', executable, executable]
    companion=code_mode_host_path(admission['codex'])
    if companion is not None:command+=['--ro-bind',companion,companion]
    # A JS Codex launcher needs the installed package and its Node environment.
    if executable.endswith('/bin/codex.js'):
        package = Path(executable).parent.parent
        node = package.parents[3]/'bin'/'node'
        if not node.is_file():
            raise ValueError('Codex JS launcher has no fixed Node runtime')
        command += ['--ro-bind', str(package), str(package), '--ro-bind', str(node), str(node)]
    for path in profile['read_roots']:
        if path!=profile['workspace']:
            command += ['--ro-bind', path, path]
    # A shallow mount view supplies absent Codex control mount points without
    # creating them in the source repository or copying its contents.
    workspace=Path(profile['workspace'])
    command+=['--tmpfs',str(workspace)]
    present=set()
    entries=sorted(workspace.iterdir())
    if len(entries)>1024:raise ValueError('workspace mount view exceeds its bounded entry catalogue')
    for entry in entries:
        present.add(entry.name)
        if entry.is_symlink():
            command+=['--symlink',os.readlink(entry),str(entry)]
        elif entry.is_file() or entry.is_dir():
            command+=['--ro-bind',str(entry),str(entry)]
        else:
            raise ValueError('unsupported workspace control or special filesystem entry')
    for name in CONTROL_SLOTS:
        if name not in present:command+=['--dir',str(workspace/name)]
    command+=['--remount-ro',str(workspace)]
    command += ['--bind', str(Path(root)/'agent-state'), str(codex_home)]
    for name in ('config.toml', 'auth.json', 'AGENTS.md', 'instructions.md', 'rules', 'skills'):
        path = codex_home/name
        if path.exists():
            command += ['--ro-bind', str(path), str(path)]
    # Clear the conventional control roots, including any selected ancestor mount.
    for path in MASKS:
        command += ['--tmpfs', path]
        if path!='/tmp':command += ['--remount-ro', path]
    command += ['--bind', profile['scratch'], profile['scratch'],
                '--ro-bind', str(Path(root)/'evidence-mirror'), MIRROR]
    return command


def mount_view(profile, deadline=None):
    workspace=Path(profile['workspace'])
    entries=[]
    hashed_bytes=0
    for entry in sorted(workspace.iterdir()):
        value={'path':str(entry),'kind':'symlink' if entry.is_symlink() else 'directory' if entry.is_dir() else 'file'}
        if entry.is_symlink():value['target']=os.readlink(entry)
        elif entry.is_file():
            fingerprint=hashlib.sha256()
            with entry.open('rb') as stream:
                while chunk:=stream.read(65536):
                    hashed_bytes+=len(chunk)
                    if hashed_bytes>16*1024*1024 or deadline is not None and time.time()>=deadline:
                        raise ValueError('workspace mount-view hash budget exhausted')
                    fingerprint.update(chunk)
            value['sha256']=fingerprint.hexdigest()
        entries.append(value)
    return {'format':'evidence-readonly-mount-view-v1','workspace_entry':profile['workspace'],
            'read_roots':profile['read_roots'],'entries':entries,
            'empty_control_slots':[name for name in CONTROL_SLOTS if not (workspace/name).exists()],
            'source_copy':False,'source_mutation':False,
            'public_tls_assets':[{'path':path,'sha256':hashlib.sha256(Path(path).read_bytes()).hexdigest()} for path in public_ca_paths()]}


def qualify(broker):
    """No turn is started until the actual command sandbox rejects alternate effects."""
    profile = broker.profile
    readiness=companion_readiness(broker)
    scratch = Path(profile['scratch'])
    marker = 'bokkie-policy-'+broker.generation
    scratch_file = scratch/marker
    product_file = Path(profile['workspace'])/marker
    git_roots = profile.get('git_common_dirs', [])
    git_file = str(Path(git_roots[0])/marker) if git_roots else str(product_file)
    # Both controls are reachable from the host; their denial is then observed
    # through the very app-server whose task turn would execute.
    with tempfile.TemporaryDirectory(prefix='bokkie-control-') as directory:
        unix_path = str(Path(directory)/'control.sock')
        with socket.socket(socket.AF_UNIX) as unix, socket.socket(socket.AF_UNIX) as visible_unix, socket.socket() as tcp:
            unix.bind(unix_path); unix.listen()
            visible_path=str(scratch/(marker+'.sock'))
            visible_unix.bind(visible_path);visible_unix.listen()
            tcp.bind(('127.0.0.1', 0)); tcp.listen()
            with socket.socket(socket.AF_UNIX) as positive:
                positive.connect(unix_path)
            with socket.socket(socket.AF_UNIX) as positive:
                positive.connect(visible_path)
            with socket.socket() as positive:
                positive.connect(tcp.getsockname())
            code = '''import json,os,socket,sys
from pathlib import Path
paths=json.loads(sys.argv[1]); result={}
for key,path in paths['writes'].items():
 try:
  with open(path,'x') as stream: stream.write('probe')
  result[key]=True
 except OSError: result[key]=False
for key,address in [('unix',paths['unix']),('visible_unix',paths['visible_unix']),('tcp',('127.0.0.1',paths['port']))]:
 try:
  with socket.socket(socket.AF_UNIX if key in ('unix','visible_unix') else socket.AF_INET) as sock:
   sock.settimeout(.5);sock.connect(address)
  result[key]=True
 except OSError: result[key]=False
result['forwarding']=any(os.environ.get(key) for key in paths['forwarding'])
print(json.dumps(result,sort_keys=True))
'''
            paths = {'writes': {'scratch': str(scratch_file), 'product': str(product_file), 'git': git_file,
                               'mirror': MIRROR+'/'+marker,'tmp':'/tmp/'+marker}, 'unix': unix_path,
                     'visible_unix':visible_path, 'port': tcp.getsockname()[1], 'forwarding': FORWARDING}
            observations = []
            for reviewer in (False, True):
                value = broker.rpc('command/exec', {'command': ['/usr/bin/python3', '-c', code, json.dumps(paths)],
                    'cwd': profile['workspace'], 'timeoutMs': 5000, 'outputBytesCap': 4096,
                    'sandboxPolicy': policy(profile, reviewer)})
                broker.journal.record('evidence_policy_probe',{'reviewer':reviewer,'exit_code':value.get('exitCode'),
                    'stdout':value.get('stdout','')[:4096],'stderr':value.get('stderr','')[:4096]})
                if value.get('exitCode') != 0:
                    raise ValueError('no-model report policy command did not execute')
                observed = json.loads(value['stdout'])
                wanted = {'scratch': not reviewer, 'product': False, 'git': False, 'mirror': False,
                          'tmp':False, 'unix': False, 'visible_unix':False, 'tcp': False, 'forwarding': False}
                if observed != wanted:
                    raise ValueError('no-model evidence policy failed: '+json.dumps(observed, sort_keys=True))
                observations.append({'reviewer': reviewer, 'sandbox': policy(profile, reviewer), 'observed': observed})
                if scratch_file.exists():
                    scratch_file.unlink()
            Path(visible_path).unlink()
            if product_file.exists() or Path(git_file).exists():
                raise ValueError('report policy probe unexpectedly wrote a selected source')
    value = {'model_calls': 0, 'controls': {'host_unix_connected': True, 'host_visible_unix_connected':True, 'host_tcp_connected': True},
             'observations': observations, 'derived_roles': broker.evidence_roles, 'companion_readiness':readiness,
             'approval_policy': 'never', 'approvals_reviewer': 'user',
             'apps': False, 'web_search': 'disabled', 'inherited_mcp': False}
    atomic(broker.root/'evidence-policy.json', value, immutable=True)
    broker.journal.record('evidence_policy_qualified', value)
    return value
