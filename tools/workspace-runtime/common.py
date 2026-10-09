"""Private durable state and canonical resource admission for workspace jobs."""
from contextlib import contextmanager
import base64
import binascii
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import time
import tomllib
import unicodedata
from urllib.parse import urlsplit
from safe_git import discover

MAX_MESSAGE = 2 * 1024 * 1024
MAX_EVENTS = 4096
MAX_EVENT_BYTES = 16 * 1024 * 1024
MAX_JOURNAL_BYTES = 64 * 1024 * 1024
MAX_EXCHANGE_BYTES = 1024 * 1024
MAX_EVENT_MESSAGE = 256 * 1024


def text(value, maximum):
    if (not isinstance(value,str) or not value.strip() or len(value)>maximum or
            any(unicodedata.category(c)=='Cc' and c not in '\n\t' for c in value)):
        raise ValueError('empty, invalid or oversized text field')


def strings(value,maximum,*,required=False):
    if not isinstance(value,list) or len(value)>maximum or required and not value:
        raise ValueError('invalid evidence list')
    for item in value:
        text(item,4096)


def result_bounds(value):
    if not isinstance(value,dict) or set(value)!={'summary','criteria','deliveries','limitations'}:
        raise ValueError('invalid structured result shape')
    text(value['summary'],16384);strings(value['limitations'],32)
    if not isinstance(value['criteria'],list) or len(value['criteria'])>64:
        raise ValueError('criterion catalogue exceeds bounds')
    ids=set()
    for criterion in value['criteria']:
        if not isinstance(criterion,dict) or set(criterion)!={'id','satisfied','evidence'} or type(criterion['satisfied']) is not bool:
            raise ValueError('invalid criterion result')
        text(criterion['id'],200);strings(criterion['evidence'],32)
        if criterion['id'] in ids:
            raise ValueError('criterion identity repeated')
        ids.add(criterion['id'])
    if not isinstance(value['deliveries'],list) or len(value['deliveries'])>32:
        raise ValueError('delivery catalogue exceeds bounds')
    for delivery in value['deliveries']:
        if not isinstance(delivery,dict) or set(delivery)!={'repository','pull_request','reviewed_head','merge_revision','tree','checks'}:
            raise ValueError('invalid delivery observation')
        for key,bound in [('repository',256),('pull_request',2048),('reviewed_head',64),('merge_revision',64),('tree',64)]:
            text(delivery[key],bound)
        strings(delivery['checks'],32)


def event_bounds(event):
    if len(encoded(event))>MAX_EVENT_MESSAGE:
        raise ValueError('workspace event exceeds Store bound')
    kind=event['kind']
    if kind=='started':
        text(event['runtime_id'],256);strings(event['instruction_sources'],32,required=True)
    elif kind=='progress':text(event['summary'],4096)
    elif kind=='attention':text(event['reason'],4096)
    elif kind=='question':
        q=event['question'];text(q['id'],200);text(q['prompt'],4096);strings(q['options'],16)
        if q['kind'] not in ('routine','missing_information','new_authority','inconclusive'):
            raise ValueError('invalid task question kind')
    elif kind=='stopped':
        c=event['cessation'];text(c['boundary_id'],256);text(c['evidence'],16384);text(event['reason'],4096)
        if c['kind'] not in ('not_started','descendants_reaped'):
            raise ValueError('unconfirmed cessation')
        if event['result'] is not None:result_bounds(event['result'])
        if event['verification'] is not None:verification_bounds(event['verification'])
    elif kind=='recovered_result':
        if set(event)!={'kind','result','provenance','verification'}:
            raise ValueError('invalid recovered-result event shape')
        result_bounds(event['result']);recovery_provenance_bounds(event['provenance'])
        if event['verification'] is not None:verification_bounds(event['verification'])
    else:
        raise ValueError('unknown workspace event')


def verification_bounds(value):
    if not isinstance(value,dict) or set(value)!={'passed','evidence'} or type(value['passed']) is not bool:
        raise ValueError('invalid trusted verification shape')
    strings(value['evidence'],64)


def recovery_provenance_bounds(value):
    expected={'origin','algorithm','recovered_at','dispatch_digest','admission_digest','result_digest','cessation','sources'}
    if not isinstance(value,dict) or set(value)!=expected:
        raise ValueError('invalid recovery provenance shape')
    if value['origin']!='host_reconciliation' or value['algorithm']!='retained-delivery-v1':
        raise ValueError('unsupported recovery provenance')
    if type(value['recovered_at']) is not int or value['recovered_at']<=0:
        raise ValueError('invalid recovery time')
    for key in ('dispatch_digest','admission_digest','result_digest'):
        if not isinstance(value[key],str) or not re.fullmatch(r'[0-9a-f]{64}',value[key]):
            raise ValueError('invalid recovery digest')
    cessation=value['cessation']
    if not isinstance(cessation,dict) or set(cessation)!={'boundary_id','kind','evidence'} or cessation['kind']!='descendants_reaped':
        raise ValueError('recovery requires descendant cessation')
    text(cessation['boundary_id'],256);text(cessation['evidence'],16384)
    sources=value['sources']
    if not isinstance(sources,list) or not 1<=len(sources)<=16:
        raise ValueError('recovery source catalogue exceeds bounds')
    kinds=set()
    for source in sources:
        if not isinstance(source,dict) or set(source)!={'kind','sha256'}:
            raise ValueError('invalid recovery source descriptor')
        text(source['kind'],64)
        if source['kind'] in kinds or not isinstance(source['sha256'],str) or not re.fullmatch(r'[0-9a-f]{64}',source['sha256']):
            raise ValueError('invalid or repeated recovery source')
        kinds.add(source['kind'])


def pidfd_open(pid):
    if hasattr(os,'pidfd_open'):
        return os.pidfd_open(pid)
    libc=ctypes.CDLL(None,use_errno=True)
    operation=libc.pidfd_open
    operation.argtypes=[ctypes.c_int,ctypes.c_uint]
    operation.restype=ctypes.c_int
    descriptor=operation(pid,0)
    if descriptor<0:
        raise OSError(ctypes.get_errno(),'cannot open parent pidfd')
    return descriptor


def encoded(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False,
                      sort_keys=True, separators=(',', ':')).encode()


def digest(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def atomic(path, value, *, immutable=False):
    path = Path(path)
    raw = encoded(value)
    if len(raw) > MAX_MESSAGE:
        raise ValueError('state message exceeds bound')
    if path.exists() and immutable:
        if path.read_bytes() != raw:
            raise ValueError('immutable state conflict')
        return
    temporary = path.with_name(path.name + '.tmp-' + str(os.getpid()))
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if temporary.exists():
            temporary.unlink()


def read(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_MESSAGE:
        raise ValueError('invalid private state file')
    return json.loads(path.read_bytes())


def canonical(value, *, directory=True):
    path = Path(value)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError('path must be existing, absolute and canonical')
    if directory and not path.is_dir():
        raise ValueError('path must be a directory')
    return str(path)


def overlaps(left, right):
    left, right = Path(left), Path(right)
    return left.is_relative_to(right) or right.is_relative_to(left)


def edge_authorization(path):
    """Read an existing private Basic value; never persist its credential bytes."""
    path=Path(canonical(path,directory=False))
    metadata=path.stat()
    if (not path.is_file() or metadata.st_uid!=os.getuid() or
            stat.S_IMODE(metadata.st_mode)!=0o600 or metadata.st_size>4096):
        raise ValueError('edge authorisation file must be an existing private mode-600 regular file')
    try:
        value=path.read_text(encoding='ascii').strip()
        if not re.fullmatch(r'Basic [A-Za-z0-9+/]+={0,2}',value):
            raise ValueError('invalid Basic value')
        decoded=base64.b64decode(value[6:],validate=True)
        if b':' not in decoded:
            raise ValueError('Basic value requires username and password fields')
    except (UnicodeError,binascii.Error,ValueError):
        raise ValueError('edge authorisation file must contain one valid Basic Authorization value') from None
    return value


@contextmanager
def locked(path, *, blocking=True):
    path = Path(path)
    if path.is_symlink():
        raise ValueError('lock must not be a symlink')
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        yield descriptor
    finally:
        os.close(descriptor)


def private_directory(path):
    path = Path(path)
    if path.exists():
        canonical(str(path))
    else:
        canonical(str(path.parent))
        path.mkdir(mode=0o700)
    if path.stat().st_uid != os.getuid() or path.stat().st_mode & 0o077:
        raise ValueError('runtime directory must be private to this account')
    return path


class Config:
    def __init__(self, path):
        self.path = canonical(str(Path(path)), directory=False)
        self.value = read(self.path)
        c = self.value
        if c.get('version') != 1 or not re.fullmatch(r'[A-Za-z0-9_-]{1,80}', c['host_id']):
            raise ValueError('unsupported host configuration')
        origin = urlsplit(c['server_url'])
        if (origin.scheme not in ('http', 'https') or not origin.hostname or
                origin.username or origin.password or origin.path not in ('', '/') or
                origin.query or origin.fragment or
                (origin.scheme == 'http' and origin.hostname not in ('127.0.0.1', 'localhost', '::1'))):
            raise ValueError('server_url must be HTTPS or literal loopback HTTP origin')
        self.root = private_directory(c['runtime_root'])
        self.executions = private_directory(self.root / 'executions')
        # This account-wide registry deliberately does not vary with runtime,
        # database, host profile or controller identity.
        self.registry = Path.home() / '.local/state/bokkie/workspace-resource-locks'
        self.registry.mkdir(mode=0o700, parents=True, exist_ok=True)
        private_directory(self.registry)
        self.token_file = canonical(c['token_file'], directory=False)
        token_stat = Path(self.token_file).stat()
        if token_stat.st_uid != os.getuid() or token_stat.st_mode & 0o077:
            raise ValueError('host token must be private to this account')
        self.token = Path(self.token_file).read_text().strip()
        if not re.fullmatch(r'[0-9A-Fa-f]{64}',self.token):
            raise ValueError('invalid host authentication token')
        self.edge_authorization_file=None
        self.edge_authorization=None
        if c.get('edge_authorization_file') is not None:
            self.edge_authorization_file=canonical(c['edge_authorization_file'],directory=False)
            self.edge_authorization=edge_authorization(self.edge_authorization_file)
        canonical(c['codex'], directory=False)
        canonical(c['bwrap'], directory=False)
        codex_home=Path(os.environ.get('CODEX_HOME',str(Path.home()/'.codex')))
        inherited_bytes=(codex_home/'config.toml').read_bytes()
        inherited=tomllib.loads(inherited_bytes.decode())
        self.projects = {}
        for p in c['projects']:
            if p['id'] in self.projects or not isinstance(p['revision'], int) or p['revision'] < 1:
                raise ValueError('invalid project allowlist')
            canonical(p['workspace'])
            if set(p.get('role',{}))-{'model','model_reasoning_effort'}:
                raise ValueError('role overrides may contain only model tuning')
            p['role']={key:p.get('role',{}).get(key,inherited.get(key))
                       for key in ('model','model_reasoning_effort')}
            if any(not isinstance(v,str) or not v or len(v)>128 for v in p['role'].values()):
                raise ValueError('effective role requires an exact model and effort')
            p['account_config_sha256']=hashlib.sha256(inherited_bytes).hexdigest()
            if type(p.get('max_contexts',4)) is not int or not 1<=p.get('max_contexts',4)<=32:
                raise ValueError('invalid finite context limit')
            actions=p['permitted_actions']
            if not isinstance(actions,list) or not 1<=len(actions)<=32 or len(set(actions))!=len(actions):
                raise ValueError('invalid host permitted action policy')
            for action in actions:text(action,200)
            for key,maximum in [('max_seconds',86400),('max_turns',100),('max_tokens',2000000)]:
                if type(p['limits'][key]) is not int or not 1<=p['limits'][key]<=maximum:
                    raise ValueError('invalid host execution ceiling')
            resources = [canonical(r) for r in p['write_roots'] + p['git_common_dirs']]
            if not resources or len(resources) > 32 or len(resources) != len(set(resources)):
                raise ValueError('invalid writable resource set')
            canonical(p['scratch'])
            if not any(Path(p['scratch']).is_relative_to(Path(r)) for r in resources):
                raise ValueError('scratch must be covered by a writable resource')
            protected_paths=[str(self.root),str(self.registry),self.token_file,self.path]
            if self.edge_authorization_file is not None:
                protected_paths.append(self.edge_authorization_file)
            for protected in protected_paths:
                if any(overlaps(protected, r) for r in resources):
                    raise ValueError('writable resources overlap protected host state')
            if p.get('reviewer'):
                reviewer=p['reviewer']
                reviewer_path=canonical(reviewer['config_file'],directory=False)
                if (reviewer['role']!='exact_head_reviewer' or any(overlaps(reviewer_path,r) for r in resources) or
                        any(Path(reviewer_path).is_relative_to(Path(r)) for r in (str(self.root),str(self.registry)))):
                    raise ValueError('independent reviewer profile must be protected from task writes')
                review_config=tomllib.loads(Path(reviewer_path).read_text())
                if review_config.get('sandbox_mode')!='read-only' or review_config.get('approval_policy')!='never':
                    raise ValueError('independent reviewer requires read-only, non-escalating permissions')
                reviewer['sha256']=hashlib.sha256(Path(reviewer_path).read_bytes()).hexdigest()
                reviewer['model']=review_config.get('model')
                reviewer['reasoning_effort']=review_config.get('model_reasoning_effort')
            for repository in p['verification']['repositories']:
                checkout = canonical(repository['checkout'])
                common = discover(checkout,p['git_common_dirs'],checkout)['common']
                if canonical(common) not in p['git_common_dirs']:
                    raise ValueError('repository Git common directory is not reserved')
                repository['git_common_dir'] = canonical(common)
                if not repository['required_checks'] or not repository['canonical_commands']:
                    raise ValueError('delivery must declare canonical commands and CI checks')
                if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository['repository']):
                    raise ValueError('invalid GitHub repository identity')
            p['resources'] = sorted(resources)
            self.projects[p['id']] = p

    def admit(self, dispatch):
        if not isinstance(dispatch['execution_id'], str) or not 1 <= len(dispatch['execution_id']) <= 200:
            raise ValueError('invalid execution identity')
        root = self.executions / hashlib.sha256(dispatch['execution_id'].encode()).hexdigest()
        if root.exists():
            canonical(str(root))
            saved = read(root / 'admission.json')
            if (saved['dispatch_digest'] != digest(dispatch) or
                    saved['dispatch_digest'] != digest(saved['dispatch'])):
                raise ValueError('execution identity reused with changed payload')
            return root
        project = dispatch['assignment']['project']
        p = self.projects.get(project['id'])
        if (p is None or project['revision'] != p['revision'] or
                project['registration']['host'] != p['host'] or
                project['registration']['workspace'] != p['workspace'] or
                dispatch['profile_revision'] != p['profile_revision']):
            raise ValueError('dispatch does not match the fixed project/profile allowlist')
        limits = dispatch['assignment']['limits']
        for key, maximum in [('max_seconds', 86400), ('max_turns', 100), ('max_tokens', 2000000)]:
            if type(limits[key]) is not int or not 1 <= limits[key] <= min(maximum,p['limits'][key]):
                raise ValueError('invalid finite execution limit')
        actions=dispatch['assignment']['permitted_actions']
        if not isinstance(actions,list) or any(action not in p['permitted_actions'] for action in actions):
            raise ValueError('dispatch permitted actions exceed the host profile')
        if (type(dispatch['admitted_at']) is not int or type(dispatch['deadline_at']) is not int or
                dispatch['deadline_at']<=dispatch['admitted_at'] or
                dispatch['deadline_at']-dispatch['admitted_at']>limits['max_seconds']):
            raise ValueError('invalid controller-pinned admission deadline')
        root.mkdir(mode=0o700)
        (root / 'events').mkdir(mode=0o700)
        (root / 'requests').mkdir(mode=0o700)
        (root / 'answers').mkdir(mode=0o700)
        (root / 'agent-state').mkdir(mode=0o700)
        atomic(root / 'admission.json', {'dispatch':dispatch, 'dispatch_digest':digest(dispatch),
               'project_profile':p, 'admitted_at':dispatch['admitted_at'], 'deadline':dispatch['deadline_at'],
               'codex':self.value['codex'], 'bwrap':self.value['bwrap'],
               'runtime_root':str(self.root), 'registry':str(self.registry),
               'host_id':self.value['host_id'],
               'token_file':self.token_file, 'config_file':self.path,
               'edge_authorization_file':self.value.get('edge_authorization_file')}, immutable=True)
        return root


class Journal:
    def __init__(self, root):
        self.root = Path(root)
        self.admission = read(self.root / 'admission.json')
        self.execution_id = self.admission['dispatch']['execution_id']

    def events(self):
        result, total = [], 0
        for sequence, path in enumerate(sorted((self.root / 'events').glob('*.json')), 1):
            if path.name != f'{sequence:08d}.json':
                raise ValueError('event journal sequence gap')
            event = read(path)
            if event['execution_id'] != self.execution_id or event['sequence'] != sequence:
                raise ValueError('event journal identity mismatch')
            total += path.stat().st_size
            if sequence > MAX_EVENTS or total > MAX_EVENT_BYTES:
                raise ValueError('event journal exceeds bound')
            result.append(event)
        return result

    def event(self, event, *, terminal=False):
        event_bounds(event)
        events = self.events()
        raw = encoded({'execution_id':self.execution_id, 'sequence':len(events)+1,'event':event})
        used = sum(len(encoded(e)) for e in events)
        if len(events) >= MAX_EVENTS - (0 if terminal else 2) or used+len(raw) > MAX_EVENT_BYTES-(0 if terminal else 131072):
            raise ValueError('event journal exhausted')
        atomic(self.root / 'events' / f'{len(events)+1:08d}.json', json.loads(raw), immutable=True)

    def record(self, kind, value):
        path = self.root / 'runtime.jsonl'
        raw = encoded({'kind':kind,'value':value})+b'\n'
        if len(raw) > MAX_MESSAGE or (path.stat().st_size if path.exists() else 0)+len(raw) > MAX_JOURNAL_BYTES:
            raise ValueError('runtime journal exhausted')
        with path.open('ab') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())

    def records(self):
        path = self.root / 'runtime.jsonl'
        if not path.exists():
            return []
        if path.stat().st_size > MAX_JOURNAL_BYTES:
            raise ValueError('runtime journal exceeds bound')
        raw = path.read_bytes()
        if not raw.endswith(b'\n'):
            raise ValueError('torn runtime journal; ownership remains uncertain')
        return [json.loads(line) for line in raw.splitlines()]


class Reservations:
    def __init__(self, registry):
        self.root = Path(registry)

    def acquire(self, execution_id, generation, resources):
        key = hashlib.sha256(execution_id.encode()).hexdigest()
        marker = self.root / (key+'.json')
        with locked(self.root / 'registry.lock'):
            if marker.exists():
                raise RuntimeError('existing reservation requires verified cessation')
            for path in self.root.glob('*.json'):
                other = read(path)
                if other['released']:
                    continue
                if any(overlaps(a,b) for a in resources for b in other['resources']):
                    raise BlockingIOError('writable resources overlap an active or uncertain execution')
            atomic(marker, {'execution_id':execution_id,'generation':generation,
                   'resources':resources,'released':False}, immutable=True)
        return marker

    def release(self, execution_id, generation, cessation):
        if cessation['kind'] not in ('not_started','descendants_reaped'):
            raise ValueError('cessation is unconfirmed')
        path = self.root / (hashlib.sha256(execution_id.encode()).hexdigest()+'.json')
        with locked(self.root / 'registry.lock'):
            marker = read(path)
            if marker['generation'] != generation:
                raise ValueError('reservation generation mismatch')
            if marker['released']:
                return
            marker.update(released=True, cessation=cessation)
            atomic(path,marker)


def control(root, value):
    journal = Journal(root)
    if value['execution_id'] != journal.execution_id:
        raise ValueError('control execution mismatch')
    if value['cancel']:
        atomic(Path(root)/'cancel.json', {'cancel':True}, immutable=True)
    for answer in value['answers']:
        question_id = answer['question_id']
        if not isinstance(question_id,str) or not 1 <= len(question_id) <= 256:
            raise ValueError('invalid answer question identity')
        key = hashlib.sha256(question_id.encode()).hexdigest()
        atomic(Path(root)/'answers'/(key+'.json'),answer,immutable=True)
