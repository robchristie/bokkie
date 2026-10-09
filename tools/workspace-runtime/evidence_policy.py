"""Report-task policy derivation and no-model enforcement probes."""
import hashlib
import json
import os
from pathlib import Path
import socket
import tempfile
import time
import tomllib
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
    directory = root/'agent-state'/'evidence-roles'
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
        overrides['agents.'+name+'.config_file'] = str(codex_home/'evidence-roles'/target.name)
        identities.append({'role': name, 'sha256': hashlib.sha256(raw.encode()).hexdigest(),
                           'model': safe.get('model'), 'effort': safe.get('model_reasoning_effort'),
                           'sandbox': policy(profile, reviewer=True), 'approval_policy': 'never',
                           'approvals_reviewer': 'user'})
    return overrides, identities


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


def mounts(admission, profile, codex_home, root):
    """Start from an empty filesystem so unselected home socket aliases disappear."""
    command = [admission['bwrap'], '--die-with-parent', '--unshare-pid', '--new-session',
               '--proc', '/proc', '--dev', '/dev']
    # Tooling and resolver/CA files are fixed host infrastructure, never candidate cwd.
    for path in ('/usr', '/bin', '/lib', '/lib64', '/etc/ssl', '/etc/resolv.conf', '/etc/hosts',
                 '/etc/passwd', '/etc/group', '/etc/nsswitch.conf', '/etc/ld.so.cache'):
        if Path(path).exists():
            command += ['--ro-bind', path, path]
    executable = str(Path(admission['codex']).resolve())
    command += ['--ro-bind', executable, executable]
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
            'source_copy':False,'source_mutation':False}


def qualify(broker):
    """No turn is started until the actual command sandbox rejects alternate effects."""
    profile = broker.profile
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
             'observations': observations, 'derived_roles': broker.evidence_roles,
             'approval_policy': 'never', 'approvals_reviewer': 'user',
             'apps': False, 'web_search': 'disabled', 'inherited_mcp': False}
    atomic(broker.root/'evidence-policy.json', value, immutable=True)
    broker.journal.record('evidence_policy_qualified', value)
    return value
