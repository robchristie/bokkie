#!/usr/bin/env python3
"""Bounded public Cargo preparation; no builds, model turns or global mutations."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import resource
import re
import stat
import signal
import subprocess
import time
import tomllib

POLYORAMA = 'git+https://github.com/robchristie/polyorama.git?rev='


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def sha(path):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError('dependency identity input must be a regular file: ' + str(path))
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def read_input(path):
    """Read only bounded, regular root inputs without following aliases."""
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError('dependency input must be a regular non-symlink file: ' + str(path))
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError('dependency input must be a regular file: ' + str(path))
        raw = stream.read(2 * 1024**2 + 1)
    if len(raw) > 2 * 1024**2:
        raise ValueError('dependency input exceeds bound: ' + str(path))
    return raw.decode('utf-8')


def storage_paths(manifest):
    """Keep bounded dependency material and growing build output in disjoint roots."""
    workspace = Path(manifest['workspace']).resolve(strict=True)
    storage = Path(manifest['dependency_preparation']['storage'])
    build = storage.with_name(storage.name + '-build')
    for label, path in (('dependency storage', storage), ('build storage', build)):
        if path.is_symlink() or path.resolve() != path or not path.is_relative_to(workspace) or path == workspace:
            raise ValueError(label + ' must be canonical and strictly inside workspace')
        for parent in (path, *path.parents):
            if parent.exists() and not parent.is_dir():
                raise ValueError(label + ' must use real directories')
            if parent == workspace:
                break
    if build.is_relative_to(storage) or storage.is_relative_to(build):
        raise ValueError('dependency and build storage must not overlap')
    return storage, build


def configuration(manifest):
    config = manifest.get('dependency_preparation')
    if config is None:
        return None
    workspace = Path(manifest['workspace']).resolve(strict=True)
    inputs = {name: read_input(workspace / name) for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')}
    storage, build = storage_paths(manifest)
    for parent in [workspace, *workspace.parents]:
        for name in ('config', 'config.toml'):
            path = parent / '.cargo' / name
            if path.exists() or path.is_symlink():
                raise ValueError('dependency preparation does not support ancestor Cargo configuration: ' + str(path))
    if storage.exists():
        storage_entries(storage, config['max_bytes'])
    for label, path in (('dependency storage', storage), ('build storage', build)):
        ignored = subprocess.run(['git', '-C', str(workspace), 'check-ignore', '--quiet', str(path)],
                                 env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'}, timeout=10)
        if ignored.returncode:
            raise ValueError(label + ' must be Git ignored')
    for key, maximum in [('timeout_seconds', 1800), ('max_bytes', 10 * 1024**3)]:
        if type(config[key]) is not int or not 1 <= config[key] <= maximum:
            raise ValueError('dependency preparation requires finite ' + key)
    for path in (storage / 'cargo/config', storage / 'cargo/config.toml', storage / 'cargo/credentials', storage / 'cargo/credentials.toml', storage / 'home/.cargo/config', storage / 'home/.cargo/config.toml'):
        if path.exists() or path.is_symlink():
            raise ValueError('dependency storage contains unsupported Cargo configuration or credentials')
    cargo = Path(config['cargo'])
    if not cargo.is_absolute() or cargo.resolve(strict=True) != cargo:
        raise ValueError('dependency cargo must be the canonical installed toolchain executable')
    toolchain = tomllib.loads(inputs['rust-toolchain.toml'])['toolchain']['channel']
    if not all(part.isdigit() for part in toolchain.split('.')) or len(toolchain.split('.')) != 3:
        raise ValueError('dependency toolchain must be an exact installed version')
    result = subprocess.run([str(cargo), '--version'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
    if result.returncode or not result.stdout.startswith(('cargo ' + toolchain + ' ').encode()):
        raise ValueError('dependency executable differs from pinned toolchain')
    package = tomllib.loads(inputs['Cargo.toml'])
    if any(key in package for key in ('workspace', 'patch', 'replace')) or 'workspace' in package.get('package', {}):
        raise ValueError('dependency preparation supports one unpatched root package')
    def inspect(value):
        if isinstance(value, dict):
            if 'git' in value and (value['git'] != 'https://github.com/robchristie/polyorama.git' or len(value.get('rev', '')) != 40 or any(c not in '0123456789abcdef' for c in value.get('rev', ''))):
                raise ValueError('dependency manifest contains an unsupported public source')
            if 'path' in value or 'registry' in value:
                raise ValueError('dependency path and alternate registry inputs are unsupported')
            for child in value.values():
                inspect(child)
        elif isinstance(value, list):
            for child in value:
                inspect(child)
    for key, value in package.items():
        if 'dependencies' in key or key == 'target':
            inspect(value)
    for package in tomllib.loads(inputs['Cargo.lock']).get('package', []):
        source = package.get('source')
        if source is None:
            continue
        if source == 'registry+https://github.com/rust-lang/crates.io-index':
            continue
        if source.startswith(POLYORAMA):
            rev, separator, commit = source[len(POLYORAMA):].partition('#')
            if separator and len(rev) == 40 and rev == commit and all(c in '0123456789abcdef' for c in rev):
                continue
        raise ValueError('dependency lock contains an unsupported public source')
    return config


def environment(manifest):
    config = manifest.get('dependency_preparation')
    if config is None:
        return {}
    storage, build = storage_paths(manifest)
    cargo = Path(config['cargo'])
    return {'CARGO_HOME': str(storage / 'cargo'), 'CARGO_TARGET_DIR': str(build),
            'RUSTC': str(cargo.with_name('rustc')), 'RUSTDOC': str(cargo.with_name('rustdoc')),
            'RUSTUP_TOOLCHAIN': tomllib.loads(read_input(Path(manifest['workspace']) / 'rust-toolchain.toml'))['toolchain']['channel'],
            'CARGO_NET_OFFLINE': 'true'}


def binding(peer):
    manifest = peer.manifest
    config = configuration(manifest)
    workspace = Path(manifest['workspace'])
    return {'schema_version': 1, 'workspace': str(workspace), 'configuration': config,
            'inputs': {name: sha(workspace / name) for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')},
            'tools': {name: sha(Path(config['cargo']).with_name(name)) for name in ('cargo', 'rustc', 'rustdoc')},
            'implementation': sha(Path(__file__)),
            'broker_implementation': sha(Path(__file__).with_name('broker.py')),
            'environment_identity': peer.command_environment_identity(include_material=False),
            'network_access': manifest.get('worker_network_access', False),
            'worker_scratch': manifest.get('worker_scratch'),
            'github_boundary': peer.github_boundary() if manifest.get('github_delivery') is not None else [],
            'bwrap': sha(Path(manifest['bwrap']))}


def storage_entries(root, maximum):
    """Reject special nodes before following paths or opening any material."""
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise ValueError('dependency storage root must be a real directory: ' + str(root))
    entries = []
    total = 0
    visited = 0
    for path in root.rglob('*'):
        visited += 1
        if visited > 100000:
            raise ValueError('dependency storage entry bound exceeded')
        mode = path.lstat().st_mode
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode):
            raise ValueError('dependency storage contains a symlink or non-regular file: ' + str(path))
        total += path.stat().st_size
        entries.append(path)
        if total > maximum or len(entries) > 100000:
            raise ValueError('dependency storage bound exceeded')
    return entries


def inventory(root, maximum):
    entries = {}
    total = 0
    for path in sorted(storage_entries(root, maximum)):
        if path.name in ('.global-cache', '.package-cache', '.package-cache-mutate'):
            continue
        total += path.stat().st_size
        entries[str(path.relative_to(root))] = sha(path)
    return {'bytes': total, 'files': len(entries), 'sha256': digest(entries)}


def diagnostic_tail(raw):
    # Only a bounded command diagnostic survives; URL userinfo/query values and
    # common credential assignments are removed even for unexpected tool output.
    text = raw.decode('utf-8', errors='replace')
    text = re.sub(r'(https?://)[^/\s@]+@', r'\1[redacted]@', text)
    text = re.sub(r'(https?://[^\s?#]+)[?#][^\s]+', r'\1?[redacted]', text)
    text = re.sub(r'(?i)authorization[ :=]+[^\n]+', 'authorization: [redacted]', text)
    text = re.sub(r'(?i)(token|password|secret|authorization)([ :=]+)[^\s]+', r'\1\2[redacted]', text)
    text = ''.join(c for c in text if c in '\n\t' or c.isprintable())
    return text.encode('utf-8')[-2048:].decode('utf-8', errors='ignore')


def command(peer, operation, online=False):
    m = peer.manifest
    storage = Path(m['dependency_preparation']['storage'])
    args = [m['bwrap'], '--die-with-parent', '--unshare-pid', '--new-session',
            '--ro-bind', '/', '/', '--bind', str(storage), str(storage),
            '--proc', '/proc', '--dev', '/dev', '--chdir', m['workspace']]
    # Preparation can write only isolated material. Readiness uses the worker's
    # workspace write boundary, still preserving source manifests and Git state.
    if operation == 'metadata':
        args += ['--bind', m['workspace'], m['workspace']]
    if operation == 'locate-project' or (not online and not m.get('worker_network_access', False)):
        args += ['--unshare-net']
    if m.get('github_delivery') is not None:
        args += peer.github_boundary()
    args += ['--', m['dependency_preparation']['cargo'], operation, '--locked',
             '--manifest-path', str(Path(m['workspace']) / 'Cargo.toml')]
    if operation == 'locate-project':
        args += ['--workspace', '--offline', '--message-format', 'plain']
    if operation == 'metadata':
        args += ['--offline', '--format-version', '1']
    return args


def execute(peer, operation, online=False):
    m = peer.manifest
    config = m['dependency_preparation']
    storage = Path(config['storage'])
    env = {'PATH': str(Path(config['cargo']).parent) + ':/usr/bin:/bin',
           'HOME': str(storage / 'home'), 'TMPDIR': str(storage / 'tmp'),
           'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
           **environment(m)}
    if not online and operation != 'locate-project':
        env = peer.environment()
    if online:
        env['CARGO_NET_OFFLINE'] = 'false'
    storage_entries(storage, config['max_bytes'])
    args = command(peer, operation, online)
    process = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True,
                               preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_FSIZE, (config['max_bytes'], config['max_bytes'])))
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    output = hashlib.sha256()
    count = 0
    tail = b''
    allowance = min(config['timeout_seconds'], max(0, m.get('deadline', time.time() + config['timeout_seconds']) - time.time()))
    deadline = time.monotonic() + allowance
    try:
        while selector.get_map():
            if time.monotonic() >= deadline:
                raise ValueError('dependency command deadline exceeded')
            # Enforce allocation during execution, including failed fetches.
            storage_entries(storage, config['max_bytes'])
            for key, _ in selector.select(0.1):
                chunk = os.read(key.fd, 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                output.update(chunk)
                tail = (tail + chunk)[-8192:]
                count += len(chunk)
                if count > 32 * 1024**2:
                    raise ValueError('dependency command output bound exceeded')
        code = process.wait(timeout=max(0.1, deadline - time.monotonic()))
        if code:
            raise ValueError('dependency ' + operation + ' failed; output sha256=' + output.hexdigest() + '; cause: ' + diagnostic_tail(tail))
        if operation == 'locate-project':
            if count > 8192 or tail.decode('utf-8').strip() != str(Path(m['workspace']) / 'Cargo.toml'):
                raise ValueError('dependency Cargo workspace resolves outside the registered root manifest')
        return {'command': args, 'exit_code': code, 'output_sha256': output.hexdigest(), 'output_bytes': count}
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        process.stdout.close()
        selector.close()


def ready(peer, prepare=False):
    config = configuration(peer.manifest)
    if config is None:
        return None
    storage = Path(config['storage'])
    if prepare:
        storage.mkdir(parents=True, exist_ok=True, mode=0o700)
    if not storage.is_dir():
        raise ValueError('dependency preparation missing; run prepare-dependencies before model execution')
    for name in ('preparation.lock', 'ready.json', 'ready.tmp'):
        if (storage / name).is_symlink():
            raise ValueError('dependency receipt path cannot be a symlink')
    with (storage / 'preparation.lock').open('a+b') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        expected = binding(peer)
        receipt = storage / 'ready.json'
        if receipt.exists() and receipt.stat().st_size > 1024**2:
            raise ValueError('dependency receipt exceeds bound')
        cached = json.loads(receipt.read_text()) if receipt.exists() else None
        material = storage / 'cargo'
        valid = cached is not None and cached.get('binding') == expected and material.is_dir() and cached.get('material') == inventory(material, config['max_bytes'])
        if not valid and not prepare:
            raise ValueError('dependency preparation stale or incomplete; run prepare-dependencies')
        receipt.unlink(missing_ok=True)
        for name in ('cargo', 'home', 'tmp'):
            (storage / name).mkdir(exist_ok=True, mode=0o700)
        checks = [execute(peer, 'locate-project')]
        if not valid:
            checks.append(execute(peer, 'fetch', online=True))
        checks.append(execute(peer, 'metadata'))
        if binding(peer) != expected:
            raise ValueError('dependency inputs changed during preparation')
        result = {'binding': expected, 'material': inventory(material, config['max_bytes']),
                  'checks': checks, 'reused': valid, 'model_turns': 0, 'passed': True}
        temporary = receipt.with_suffix('.tmp')
        temporary.write_text(json.dumps(result, sort_keys=True) + '\n')
        temporary.replace(receipt)
        return result
