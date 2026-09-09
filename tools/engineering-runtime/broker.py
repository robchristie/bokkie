#!/usr/bin/env python3
"""Private, detached, one-execution Codex broker. No obligation lifecycle here.

The controller reads fsynced events and atomically supplies exact request replies.
A committed launch is never replayed, including after owner death. Only the
original parent can attest wait/reaping of the Bubblewrap boundary.
"""
import argparse
import fcntl
import hashlib
import json
import os
import pwd
import stat
from pathlib import Path
import selectors
import select
import signal
import subprocess
import sys
import time
import tomllib
import uuid

MAX_MESSAGE = 2 * 1024 * 1024
MAX_SPOOL = 16 * 1024 * 1024
MAX_EVENTS = 2048
RESERVE = 8192


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def digest(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic(path, value):
    data = encoded(value)
    if len(data) > MAX_MESSAGE:
        raise ValueError('bounded file exceeded')
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def read(path):
    with path.open('rb') as stream:
        data = stream.read(MAX_MESSAGE + 1)
    if len(data) > MAX_MESSAGE:
        raise ValueError('bounded input exceeded')
    return json.loads(data)


class Spool:
    def __init__(self, root):
        self.root = root
        self.path = root / 'events.jsonl'
        self.events = []
        if self.path.exists():
            if self.path.stat().st_size > MAX_SPOOL:
                raise ValueError('spool bound exceeded')
            with self.path.open('rb') as stream:
                for line in stream:
                    # Torn tails remain uncertain. Never truncate and replay a start.
                    event = json.loads(line)
                    if event['sequence'] != len(self.events) + 1:
                        raise ValueError('spool sequence gap')
                    self.events.append(event)
        self.size = self.path.stat().st_size if self.path.exists() else 0

    def append(self, kind, value, terminal=False):
        event = {'sequence': len(self.events) + 1, 'kind': kind, 'value': value}
        data = encoded(event) + b'\n'
        cap = MAX_SPOOL if terminal else MAX_SPOOL - RESERVE
        if len(data) > MAX_MESSAGE or self.size + len(data) > cap or (not terminal and len(self.events) >= MAX_EVENTS - 4):
            raise ValueError('event spool exhausted; stop and reconcile')
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        with os.fdopen(fd, 'ab') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        sync_dir(self.root)
        self.events.append(event)
        self.size += len(data)
        return event

    def has(self, kind):
        return any(e['kind'] == kind for e in self.events)


def workspace_lock_root():
    # Independent of profile, spool, database, HOME and CODEX_HOME overrides.
    return Path(pwd.getpwuid(os.getuid()).pw_dir) / '.local/state/bokkie/workspace-locks'


class WorkspaceWriter:
    """Stable inode plus durable uncertainty marker; never unlink lock files."""
    def __init__(self, workspace, owner):
        self.fd = None
        self.root = workspace_lock_root()
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        canonical = Path(workspace).resolve(strict=True)
        if self.root.resolve() != self.root or self.root.is_relative_to(canonical):
            raise ValueError('workspace lock storage must be canonical and outside workspace')
        metadata = self.root.stat()
        if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
            raise ValueError('workspace lock storage must be private to this user')
        path = self.root / (hashlib.sha256(os.fsencode(canonical)).hexdigest() + '.lock')
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            metadata = os.fstat(fd)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_nlink != 1:
                raise ValueError('invalid stable workspace lock inode')
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            previous = os.read(fd, 4097)
            if previous and json.loads(previous) != {}:
                raise RuntimeError('prior workspace owner lacks verified cessation; reconcile before reuse')
            self.fd = fd
            self.write({'workspace': str(canonical), **owner})
            sync_dir(self.root)
        except BaseException:
            self.fd = None
            os.close(fd)
            raise

    def write(self, value):
        data = encoded(value)
        os.lseek(self.fd, 0, os.SEEK_SET)
        if os.write(self.fd, data) != len(data):
            raise OSError('short workspace ownership write')
        os.ftruncate(self.fd, len(data))
        os.fsync(self.fd)

    def release_after_cessation(self):
        # Only call after a durable reap receipt or known pre-spawn failure.
        self.write({})
        self.close()

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


class Broker:
    def __init__(self, root, spawn=None, monotonic=time.monotonic):
        self.root = root
        self.spool = Spool(root)
        self.manifest = read(root / 'dispatch.json')
        self.generation = uuid.uuid4().hex
        self.child = None
        self.writer = None
        self.spawn = spawn or subprocess.Popen
        self.clock = monotonic
        self.serial = 0
        self.pending = {}
        self.responses = {}
        self.thread = None
        self.turn = None
        self.completed = False
        self.buffer = b''
        self.stderr_prefix = bytearray()
        self.stderr_bytes = 0
        self.stderr_hash = hashlib.sha256()
        if not 0 < self.manifest['turn_seconds'] <= 2700:
            raise ValueError('turn bound exceeded')
        self.deadline = self.clock() + self.manifest['turn_seconds']
        self.selector = selectors.DefaultSelector()
        self.stop = False

    def event(self, kind, value, terminal=False):
        return self.spool.append(kind, value, terminal)

    def send(self, value):
        data = encoded(value) + b'\n'
        if len(data) > MAX_MESSAGE:
            raise ValueError('protocol request exceeds bound')
        remaining = memoryview(data)
        fd = self.child.stdin.fileno()
        while remaining:
            if self.stop or (self.root / 'cancel.json').exists():
                raise InterruptedError('cancelled during protocol write')
            if self.clock() >= self.deadline or time.time() >= self.manifest['deadline']:
                raise TimeoutError('protocol write deadline exhausted')
            try:
                written = os.write(fd, remaining)
                if written == 0:
                    raise EOFError('protocol write closed')
                remaining = remaining[written:]
            except BlockingIOError:
                select.select([], [fd], [], 0.1)

    def rpc(self, method, params):
        self.serial += 1
        request_id = 'bokkie-' + str(self.serial)
        # Receipt is durable BEFORE the external send, including turn/start.
        self.event('rpc_intent', {'id': request_id, 'method': method, 'params_digest': digest(params)})
        self.send({'id': request_id, 'method': method, 'params': params})
        while request_id not in self.responses:
            self.pump()
        response = self.responses.pop(request_id)
        # Config may contain inherited credentials. Retain only the task-owned
        # capability projection, never raw account configuration or its layers.
        retained = response
        if method == 'config/read' and 'result' in response:
            retained = {'id': response['id'], 'result': self.capability_config(response['result'].get('config', {}))}
        self.event('rpc_receipt', {'id': request_id, 'method': method, 'response': retained})
        if 'error' in response:
            raise RuntimeError(str(response['error']))
        return response['result']

    def request_key(self, message):
        p = message.get('params', {})
        return digest([self.manifest['execution_id'], self.generation,
                       p.get('threadId', self.thread), p.get('turnId', self.turn),
                       p.get('itemId'), message['id']])

    def source_snapshot(self):
        workspace = Path(self.manifest['workspace'])
        try:
            git = ['/usr/bin/git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
                   '-c', 'core.hooksPath=/dev/null', '-C', str(workspace)]
            def query(args):
                result = subprocess.run(git + args, stdin=subprocess.DEVNULL,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5)
                if result.returncode or len(result.stdout) > MAX_MESSAGE:
                    raise ValueError('source metadata unavailable')
                return result.stdout
            try:
                paths = query(['ls-files', '-z', '--cached', '--others', '--exclude-standard']).split(b'\0')
                paths = [os.fsdecode(path) for path in paths if path]
                commit = query(['rev-parse', 'HEAD']).decode().strip()
                tree = query(['rev-parse', 'HEAD^{tree}']).decode().strip()
                clean = not query(['status', '--porcelain', '--untracked-files=normal'])
            except ValueError:
                paths = []
                for parent, directories, names in os.walk(workspace, followlinks=False):
                    directories[:] = [name for name in directories if name != '.git']
                    paths += [str((Path(parent) / name).relative_to(workspace)) for name in names]
                    if len(paths) > 2048:
                        raise ValueError('source file count exceeded')
                commit = tree = None
                clean = False
            if len(paths) > 2048:
                raise ValueError('source file count exceeded')
            files = {}
            total = 0
            for relative in sorted(set(paths)):
                path = workspace / relative
                if path.is_symlink() or not path.resolve().is_relative_to(workspace.resolve()):
                    raise ValueError('source symlink cannot be attested')
                size = path.stat().st_size
                total += size
                if size > MAX_MESSAGE or total > MAX_SPOOL:
                    raise ValueError('source byte bound exceeded')
                raw = path.read_bytes()
                if len(raw) != size:
                    raise ValueError('source changed while being observed')
                files[relative] = {'sha256': hashlib.sha256(raw).hexdigest(), 'byte_length': size}
            return {'files': files, 'commit': commit, 'tree': tree, 'clean': clean}
        except (OSError, ValueError, subprocess.TimeoutExpired):
            return {'unavailable': 'bounded source identity could not be established'}

    def observe(self, message):
        method = message.get('method')
        if 'id' in message and not method:
            self.responses[message['id']] = message
        elif method and 'id' in message:
            key = self.request_key(message)
            if key in self.pending:
                if self.pending[key] != message:
                    raise ValueError('request identity conflict')
                return
            self.event('request', {'key': key, 'message': message})
            self.pending[key] = message
            if method in ('item/commandExecution/requestApproval', 'item/fileChange/requestApproval'):
                # No shell heuristics, policy amendments, prefix or session grants.
                p = message.get('params', {})
                routine = (self.manifest.get('allow_single_pwd_approval', False) and
                           self.manifest['role'] == 'worker' and p.get('command') == 'pwd' and
                           not p.get('proposedExecpolicyAmendment') and
                           not p.get('proposedNetworkPolicyAmendments') and
                           not p.get('additionalPermissions'))
                decision = 'accept' if routine else 'decline'
                self.event('approval_decision', {'key': key, 'decision': decision,
                           'scope': 'this request only', 'policy': 'literal-pwd-v1'})
                reply = {'id': message['id'], 'result': {'decision': decision}}
                atomic(self.root / 'replies' / (key + '.json'), reply)
            elif method == 'item/permissions/requestApproval':
                atomic(self.root / 'replies' / (key + '.json'),
                       {'id': message['id'], 'result': {'permissions': {}, 'scope': 'turn'}})
            elif method not in ('item/tool/call', 'item/tool/requestUserInput'):
                atomic(self.root / 'replies' / (key + '.json'),
                       {'id': message['id'], 'error': {'code': -32601, 'message': 'Unsupported escalation; consult Bokkie'}})
        elif method in ('thread/started', 'turn/started', 'turn/completed', 'item/started', 'item/completed', 'thread/status/changed', 'error'):
            params = message.get('params', {})
            if method in ('item/started', 'item/completed') and params.get('item', {}).get('type') == 'commandExecution':
                self.event('command_source', {'phase': method, 'item_id': params['item']['id'],
                           'thread_id': params.get('threadId'), 'turn_id': params.get('turnId'),
                           'source': self.source_snapshot()})
            self.event(method, params)
            if method == 'turn/started' and params.get('threadId') == self.thread:
                self.turn = params['turn']['id']
            if method == 'turn/completed' and params.get('threadId') == self.thread:
                if self.turn and params['turn']['id'] != self.turn:
                    raise ValueError('unexpected root turn identity')
                self.turn = params['turn']['id']
                self.completed = True

    def deliver(self):
        for key, message in list(self.pending.items()):
            path = self.root / 'replies' / (key + '.json')
            if not path.exists():
                continue
            reply = read(path)
            if reply.get('id') != message['id']:
                raise ValueError('response request ID mismatch')
            self.event('response_intent', {'key': key, 'digest': digest(reply)})
            self.send(reply)
            # Transport write receipt, NOT a claim of protocol-level acceptance.
            self.event('response_written', {'key': key, 'digest': digest(reply)})
            del self.pending[key]

    def read_stderr(self):
        try:
            chunk = os.read(self.child.stderr.fileno(), 65536)
        except BlockingIOError:
            return None
        if not chunk:
            return False
        self.stderr_bytes += len(chunk)
        self.stderr_hash.update(chunk)
        self.stderr_prefix.extend(chunk[:max(0, 8192 - len(self.stderr_prefix))])
        return True

    def stderr_diagnostic(self):
        # Stderr can contain URLs, environment values or tokens. Classify known
        # failure signatures without retaining any untrusted literal text.
        prefix = bytes(self.stderr_prefix).lower()
        classes = []
        for needles, label in [
            ((b'transport', b'untagged enum mcp'), 'invalid_mcp_transport_configuration'),
            ((b'config',), 'configuration_error'),
            ((b'permission denied', b'operation not permitted'), 'os_permission_denied'),
            ((b'bwrap:', b'bubblewrap'), 'containment_startup_error'),
            ((b'not found', b'no such file'), 'required_path_unavailable'),
        ]:
            if any(needle in prefix for needle in needles):
                classes.append(label)
        return {'classes': classes or ['unclassified_stderr'], 'byte_count': self.stderr_bytes,
                'sha256': self.stderr_hash.hexdigest(), 'classified_prefix_bytes': len(self.stderr_prefix),
                'raw_text_retained': False}

    def pump(self):
        if self.stop or (self.root / 'cancel.json').exists():
            raise InterruptedError('cancellation requested; cessation still required')
        if self.clock() >= self.deadline or time.time() >= self.manifest['deadline']:
            raise TimeoutError('finite execution deadline exhausted')
        self.deliver()
        ready = self.selector.select(0.1)
        for key, _ in ready:
            if key.fileobj is self.child.stderr:
                if self.read_stderr() is False:
                    self.selector.unregister(key.fileobj)
                continue
            chunk = os.read(key.fd, 65536)
            if not chunk:
                raise EOFError('app-server transport lost; start will not be replayed')
            self.buffer += chunk
            while b'\n' in self.buffer:
                line, self.buffer = self.buffer.split(b'\n', 1)
                if len(line) > MAX_MESSAGE:
                    raise ValueError('protocol frame exceeds bound')
                self.observe(json.loads(line))
            if len(self.buffer) > MAX_MESSAGE:
                raise ValueError('unterminated protocol frame exceeds bound')

    def configuration(self):
        m = self.manifest
        mode = 'read-only' if m['role'] == 'supervisor' else 'workspace-write'
        config = {
            'approvals_reviewer': 'user', 'approval_policy': 'on-request',
            'model': m['model'], 'model_reasoning_effort': m['effort'],
            'sandbox_mode': mode, 'sandbox_workspace_write.writable_roots': [m['workspace']],
            'sandbox_workspace_write.network_access': m.get('worker_network_access', False) if m['role'] == 'worker' else False,
            'sandbox_workspace_write.exclude_slash_tmp': True,
            'sandbox_workspace_write.exclude_tmpdir_env_var': True,
            'agents.max_threads': m['max_subagents'], 'agents.max_depth': 1,
            'agents.default_subagent_model': m['subagent_model'],
            'agents.default_subagent_reasoning_effort': m['subagent_effort'],
            'features.apps': False, 'web_search': 'disabled',
        }
        # Override only enabled: replacing the MCP table destroys inherited
        # transport configuration. Read only server names, never credentials.
        config_home = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex')))
        configs = [config_home / 'config.toml', Path('/etc/codex/config.toml')]
        configs += [p / '.codex' / 'config.toml' for p in [Path(m['workspace']), *Path(m['workspace']).parents]]
        for path in configs:
            if path.is_file():
                with path.open('rb') as stream:
                    names = tomllib.load(stream).get('mcp_servers', {}).keys()
                for name in names:
                    config['mcp_servers.' + json.dumps(name) + '.enabled'] = name in m.get('readonly_mcp_servers', [])
        return config

    @staticmethod
    def capability_config(config):
        return {
            'agents': {key: config.get('agents', {}).get(key) for key in
                       ('max_threads', 'max_depth', 'default_subagent_model', 'default_subagent_reasoning_effort')},
            'apps': config.get('features', {}).get('apps'),
            'web_search': config.get('web_search'),
            'mcp_servers': {name: {'enabled': value.get('enabled', True)}
                            for name, value in config.get('mcp_servers', {}).items()},
        }

    def verify_capability_config(self, config):
        m = self.manifest
        actual = self.capability_config(config)
        expected_agents = {'max_threads': m['max_subagents'], 'max_depth': 1,
                           'default_subagent_model': m['subagent_model'],
                           'default_subagent_reasoning_effort': m['subagent_effort']}
        enabled = sorted(name for name, value in actual['mcp_servers'].items() if value['enabled'])
        if (actual['agents'] != expected_agents or actual['apps'] is not False or
                actual['web_search'] != 'disabled' or enabled != sorted(m.get('readonly_mcp_servers', []))):
            raise ValueError('effective tools or subagent configuration differs from task profile')
        self.event('effective_capabilities', {
            **actual, 'dynamic_tools': [tool['name'] for tool in m['thread_params'].get('dynamicTools', [])],
            'worker_scratch': m.get('worker_scratch') if m['role'] == 'worker' else None,
        })

    def command(self):
        m = self.manifest
        config = self.configuration()
        # Preserve normal personal/repository guidance and skill loading. These
        # are process overrides, never writes to the account configuration.
        args = [m['bwrap'], '--die-with-parent', '--unshare-pid', '--new-session',
                '--dev-bind', '/', '/', '--proc', '/proc', '--chdir', m['workspace'],
                ]
        if m['role'] == 'worker':
            # The worker cannot alter/delete lock inodes or uncertainty markers,
            # even if a narrowly approved command escapes the inner Codex sandbox.
            lock_root = str(workspace_lock_root())
            args += ['--ro-bind', lock_root, lock_root]
        args += ['--', m['codex'], 'app-server', '--listen', 'stdio://']
        for key, value in config.items():
            if isinstance(value, dict):
                value = '{}'
            else:
                value = json.dumps(value)
            args += ['-c', key + '=' + value]
        return args

    def run(self):
        if self.spool.has('launch_committed'):
            return 'existing'  # Never resurrect a writer after broker death.
        m = self.manifest
        self.event('launch_committed', {'generation': self.generation,
                   'execution_id': m['execution_id'], 'dispatch_key': m['dispatch_key'],
                   'broker_pid': os.getpid(), 'manifest_digest': digest(m),
                   'command': self.command()})
        try:
            if m['role'] == 'worker':
                self.writer = WorkspaceWriter(m['workspace'], {
                    'execution_id': m['execution_id'], 'generation': self.generation,
                    'spool': str(self.root.resolve())})
                self.event('workspace_reserved', {'workspace': str(Path(m['workspace']).resolve()),
                           'generation': self.generation})
            environment = dict(os.environ)
            if m.get('worker_scratch') and m['role'] == 'worker':
                environment['TMPDIR'] = m['worker_scratch']
            self.child = self.spawn(self.command(), cwd=m['workspace'], env=environment, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    start_new_session=True)
            os.set_blocking(self.child.stdin.fileno(), False)
            os.set_blocking(self.child.stdout.fileno(), False)
            os.set_blocking(self.child.stderr.fileno(), False)
            self.event('boundary_started', {'pid': self.child.pid, 'generation': self.generation})
            self.selector.register(self.child.stdout, selectors.EVENT_READ)
            self.selector.register(self.child.stderr, selectors.EVENT_READ)
            self.rpc('initialize', {'clientInfo': {'name': 'bokkie_engineering', 'version': '1'},
                                   'capabilities': {'experimentalApi': True}})
            self.send({'method': 'initialized', 'params': {}})
            effective_config = self.rpc('config/read', {'cwd': m['workspace'], 'includeLayers': False})
            self.verify_capability_config(effective_config['config'])
            started = self.rpc('thread/start', m['thread_params'])
            effective = {key: started.get(key) for key in ('model', 'reasoningEffort', 'approvalPolicy',
                          'approvalsReviewer', 'sandbox', 'instructionSources')}
            self.event('effective_settings', effective)
            if (started.get('approvalsReviewer') != 'user' or
                started.get('approvalPolicy') != 'on-request' or started.get('model') != m['model'] or
                started.get('reasoningEffort') != m['effort']):
                raise ValueError('effective Codex profile differs from authorised profile')
            expected_mode = 'readOnly' if m['role'] == 'supervisor' else 'workspaceWrite'
            if started.get('sandbox', {}).get('type') != expected_mode:
                raise ValueError('effective sandbox differs')
            if m['role'] == 'worker':
                sandbox = started['sandbox']
                if sandbox.get('networkAccess') != m.get('worker_network_access', False) or not sandbox.get('excludeSlashTmp') or not sandbox.get('excludeTmpdirEnvVar'):
                    raise ValueError('effective worker sandbox broadens authority')
            roots = started.get('sandbox', {}).get('writableRoots', [])
            if any(Path(p).resolve() != Path(m['workspace']).resolve() for p in roots):
                raise ValueError('effective writable roots broaden workspace authority')
            if Path(started.get('cwd', '')).resolve() != Path(m['workspace']).resolve():
                raise ValueError('effective cwd differs from registered workspace')
            sources = []
            for path in started.get('instructionSources', []):
                raw = Path(path).read_bytes()
                if len(raw) > MAX_MESSAGE:
                    raise ValueError('guidance identity input exceeded bound')
                sources.append({'path': path, 'sha256': hashlib.sha256(raw).hexdigest()})
            skills = self.rpc('skills/list', {'cwds': [m['workspace']], 'forceReload': True})
            for entry in skills.get('data', []):
                for skill in entry.get('skills', []):
                    if skill.get('enabled', True):
                        path = skill['path']
                        raw = Path(path).read_bytes()
                        if len(raw) > MAX_MESSAGE:
                            raise ValueError('skill identity input exceeded bound')
                        sources.append({'path': path, 'sha256': hashlib.sha256(raw).hexdigest()})
            self.event('guidance_identities', sources)
            self.thread = started['thread']['id']
            self.event('thread_identity', {'thread_id': self.thread})
            result = self.rpc('turn/start', {'threadId': self.thread, 'model': m['model'],
                              'effort': m['effort'], 'input': [{'type': 'text', 'text': m['prompt']}]})
            self.turn = result['turn']['id']
            self.event('turn_identity', {'thread_id': self.thread, 'turn_id': self.turn})
            while not self.completed:
                self.pump()
        except Exception as error:
            self.event('failure', {'type': type(error).__name__, 'message': str(error)[:2048]}, terminal=True)
        finally:
            if self.child is not None:
                # This exact child was spawned by us. Never kill a PID recovered
                # from disk: identity reuse and broker death require reconciliation.
                if self.child.poll() is None:
                    os.killpg(self.child.pid, signal.SIGKILL)
                code = self.child.wait(timeout=10)
                # The exact child is gone; drain its bounded pipe remainder.
                while self.read_stderr():
                    pass
                if self.stderr_bytes:
                    self.event('stderr_diagnostic', self.stderr_diagnostic(), terminal=True)
                self.child.stderr.close()
                self.child.stdin.close()
                self.child.stdout.close()
                self.event('boundary_reaped', {'generation': self.generation, 'pid': self.child.pid,
                           'exit_code': code, 'boundary': m['execution_id'] + ':' + self.generation}, terminal=True)
            else:
                # Spawn failure proves no namespace was created by this call.
                # A broker crash in the spawn/record gap cannot reach this path.
                self.event('not_started', {'generation': self.generation,
                           'reason': 'This broker did not spawn a boundary'}, terminal=True)
            if self.writer is not None:
                self.writer.release_after_cessation()
            self.selector.close()
        return 'finished'


def serve(root):
    lock = (root / 'owner.lock').open('a+b')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return
    broker = Broker(root)
    signal.signal(signal.SIGTERM, lambda *_: setattr(broker, 'stop', True))
    signal.signal(signal.SIGINT, lambda *_: setattr(broker, 'stop', True))
    broker.run()


def launch(root):
    # The owner takes its own lock and commits its start before spawning Codex.
    # Multiple launchers can produce short-lived brokers, never duplicate writers.
    subprocess.Popen([sys.executable, str(Path(__file__).resolve()), 'serve', str(root)],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True, close_fds=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['launch', 'serve', 'status'])
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    root = args.directory.resolve(strict=True)
    if args.operation == 'launch':
        launch(root)
    elif args.operation == 'serve':
        serve(root)
    else:
        lock = (root / 'owner.lock').open('a+b')
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            active = False
        except BlockingIOError:
            active = True
        spool = Spool(root)
        print(json.dumps({'active': active, 'launched': spool.has('launch_committed'),
                          'reaped': spool.has('boundary_reaped'), 'events': len(spool.events)}))
