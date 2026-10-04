#!/usr/bin/env python3
"""Qualify only a fresh synthetic account on the marked calibration host.

Run without arguments after installing the account-enabled policy rendered by
manage.policy_text for the existing bokkie-calibration release. This tool does
not install policy, contact a model, use a real account, or alter the service.
It creates uniquely labelled disposable Engine resources and prints its evidence
directory beneath /home/rob/bokkie-deployment-calibration.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import uuid


ROOT = Path('/home/rob/bokkie-deployment-calibration')
MARKER = 'Synthetic deployment qualification only\n'
AUTH = '/home/probe/.codex/auth.json'
SYNTHETIC = b'{}\n'
SPEC = importlib.util.spec_from_file_location('account_deployment_manager',
                                             Path(__file__).with_name('manage.py'))
manage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(manage)

PAYLOAD = r'''
import errno, hashlib, json, os, sys
from pathlib import Path
sys.path.insert(0, '/opt')
import qualify
auth = Path('/home/probe/.codex/auth.json')
before = auth.stat()
assert auth.read_bytes() == b'{}\n', 'synthetic content differs'
assert before.st_uid == os.getuid()
assert before.st_gid == os.getgid()
assert before.st_mode & 0o777 == 0o600
assert not auth.with_name('account-sibling-canary').exists(), 'outer account sibling leaked'
assert not auth.with_name('config.toml').exists(), 'unexpected account config'
mounts = [line.split() for line in Path('/proc/self/mountinfo').read_text().splitlines()
          if line.split()[4] == str(auth)]
assert mounts and 'ro' in mounts[-1][5].split(','), 'final account mount is not read-only'
replacement = auth.with_name('synthetic-replacement')
replacement.write_bytes(b'synthetic replacement\n')
def write():
    descriptor = os.open(auth, os.O_WRONLY)
    try:
        os.write(descriptor, b'changed')
    finally:
        os.close(descriptor)
results = {}
for name, operation in {
    'write': write,
    'truncate': lambda: os.truncate(auth, 0),
    'unlink': lambda: auth.unlink(),
    'replace': lambda: os.replace(replacement, auth),
    'rename': lambda: auth.rename(auth.with_name('renamed-auth.json')),
}.items():
    try:
        operation()
    except OSError as error:
        assert error.errno in (errno.EPERM, errno.EACCES, errno.EROFS, errno.EBUSY, errno.EXDEV)
        results[name] = error.errno
    else:
        raise AssertionError(name + ' unexpectedly succeeded')
after = auth.stat()
assert (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino)
assert auth.read_bytes() == b'{}\n', 'synthetic content changed'
# Reuse the qualified hostile namespace/mount/helper syscall contract. These
# calls must return EPERM (clone3 ENOSYS), including mount/umount2/unshare/setns.
denied = qualify.syscalls()
print(json.dumps({'result': 'passed', 'readable': True, 'final_mount_read_only': True,
    'source_inode': [after.st_dev, after.st_ino], 'source_sha256': hashlib.sha256(auth.read_bytes()).hexdigest(),
    'uid': after.st_uid, 'gid': after.st_gid, 'mode': after.st_mode & 0o777,
    'denied_account_operations': results, 'outer_sibling_hidden': True,
    'config_absent': True, 'syscalls': denied, 'model_calls': 0}))
'''

OUTER = r'''
import hashlib, json, os, sys
from pathlib import Path
sys.path.insert(0, '/opt/conversation')
import broker
auth = Path('/home/probe/.codex/auth.json')
assert Path('/proc/self/attr/current').read_text().strip() == sys.argv[1] + ' (enforce)'
assert auth.read_bytes() == b'{}\n', 'synthetic input differs'
assert not auth.with_name('config.toml').exists()
auth.with_name('account-sibling-canary').write_text('synthetic outer sibling\n')
before = auth.stat()
child = broker.spawn({'bwrap': '/usr/bin/bwrap'}, {}, dict(os.environ),
                     payload=[sys.executable, '-c', sys.argv[2]])
try:
    stdout, stderr = child.communicate(timeout=25)
finally:
    broker.terminate(child)
assert child.returncode == 0, ('account payload failed', child.returncode, stderr.decode())
result = json.loads(stdout)
after = auth.stat()
assert (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino)
assert auth.read_bytes() == b'{}\n', 'backing synthetic content changed'
assert result['source_inode'] == [after.st_dev, after.st_ino]
assert auth.with_name('account-sibling-canary').is_file()
print(json.dumps(result))
'''


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run():
    require(len(sys.argv) == 1, 'this synthetic harness takes no paths or account arguments')
    require(ROOT.resolve(strict=True) == ROOT, 'calibration root must not be redirected')
    marker = ROOT / '.synthetic-bokkie-deployment'
    require(not marker.is_symlink() and marker.read_text() == MARKER, 'synthetic marker required')
    config = manage.load(ROOT)
    require(config['name'] == 'bokkie-calibration', 'only the calibration release is allowed')
    require(config['codex_auth'] is None and config['conversation_profile'] is None,
            'base release must have no enabled account or profile')
    require((config['uid'], config['gid']) in ((10001, 10001), (3000, 3000)),
            'only the synthetic baseline or selected Nostromo account identity is allowed')
    info, version = manage.api('GET', '/info'), manage.api('GET', '/version')
    require(version['Version'] == '29.8.1' and version['Arch'] == 'amd64'
            and info['KernelVersion'] == '6.12.73+deb13-amd64'
            and set(info['SecurityOptions']) == {'name=apparmor,profile=default',
                'name=seccomp,profile=builtin', 'name=cgroupns'}, 'unqualified host')
    image = manage.api('GET', '/images/' + config['image'] + '/json')
    require(image['Config']['Labels'].get('org.opencontainers.image.revision') == config['source'],
            'image/source identity mismatch')

    name = 'bokkie-account-' + uuid.uuid4().hex
    evidence = ROOT / name
    evidence.mkdir(mode=0o755)
    auth = evidence / 'synthetic-auth.json'
    descriptor = os.open(auth, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'wb') as stream:
        stream.write(SYNTHETIC)
    original = auth.stat()
    source_hash = digest(auth)

    def record(filename, value):
        (evidence / (filename + '.json')).write_text(json.dumps(value, indent=2) + '\n')

    def remove_container(identifier):
        observed = manage.api('GET', '/containers/' + identifier + '/json')
        require(observed['Config']['Labels'].get('bokkie.account-qualification') == name,
                'refusing to remove a foreign container')
        manage.api('DELETE', '/containers/' + identifier + '?force=true')

    derived = {**config, 'codex_auth': str(auth), 'conversation_profile': {
        'broker': '/opt/conversation/broker.py', 'codex': '/usr/local/bin/codex',
        'bwrap': '/usr/bin/bwrap', 'model': 'synthetic-preflight-only', 'effort': 'medium'}}
    profile = manage.profile_name(derived)
    rendered = manage.policy_text(derived).replace('BOKKIE_PROFILE', profile)
    (evidence / 'apparmor.profile').write_text(rendered)
    record('inputs', {'source': config['source'], 'image': config['image'], 'profile': profile,
        'rendered_apparmor_sha256': hashlib.sha256(rendered.encode()).hexdigest(),
        'policy_sha256': {p.name: digest(p) for p in manage.POLICY.iterdir() if p.is_file()},
        'harness_sha256': digest(Path(__file__)), 'manager_sha256': digest(Path(manage.__file__)),
        'synthetic_source_sha256': source_hash, 'synthetic_source_inode': [original.st_dev, original.st_ino],
        'credentials': 'synthetic only', 'model_calls': 0,
        'daemon': {'version': version['Version'], 'arch': version['Arch'],
                   'kernel': info['KernelVersion'], 'security': info['SecurityOptions']}})
    print(json.dumps({'evidence': str(evidence), 'required_profile': profile}), flush=True)

    helper = container = None
    volume_created = False
    removed = []
    try:
        # A dedicated helper changes ownership of this exact newly created
        # synthetic file only. It has no account, network, host directory or
        # Docker socket mount, and no capability other than CHOWN.
        helper_config = {'Image': config['image'], 'User': '0:0',
            'Entrypoint': ['python3'], 'Cmd': ['-c', f"import os; os.chown('/fixture',{config['uid']},{config['gid']})"],
            'Labels': {'bokkie.account-qualification': name},
            'HostConfig': {'NetworkMode': 'none', 'ReadonlyRootfs': True, 'CapDrop': ['ALL'],
                'CapAdd': ['CHOWN'], 'SecurityOpt': ['no-new-privileges:true'],
                'PidsLimit': 16, 'Memory': 64 * 1024**2, 'NanoCpus': 1000000000,
                'RestartPolicy': {'Name': 'no'},
                'Mounts': [{'Type': 'bind', 'Source': str(auth), 'Target': '/fixture'}]}}
        record('ownership-helper', helper_config)
        helper = manage.api('POST', '/containers/create?name=' + name + '-owner', helper_config)['Id']
        manage.api('POST', '/containers/' + helper + '/start')
        completion = manage.api('POST', '/containers/' + helper + '/wait?condition=not-running')
        require(completion['StatusCode'] == 0, 'synthetic ownership helper failed')
        remove_container(helper)
        removed.append(helper)
        helper = None
        owned = auth.stat()
        require(stat.S_ISREG(owned.st_mode) and (owned.st_dev, owned.st_ino) ==
                (original.st_dev, original.st_ino), 'synthetic source identity changed')
        require((owned.st_uid, owned.st_gid, stat.S_IMODE(owned.st_mode)) == (config['uid'], config['gid'], 0o600),
                'synthetic backing file must be writable by the payload UID')

        expected = manage.runtime(derived, ROOT)
        expected.update(Entrypoint=['python3', '/opt/probe.py'], Cmd=['hold'],
                        Labels={'bokkie.account-qualification': name})
        expected.pop('NetworkingConfig')
        expected.pop('ExposedPorts')
        expected['HostConfig']['NetworkMode'] = 'none'
        expected['HostConfig']['Mounts'] = [
            {'Type': 'volume', 'Source': name, 'Target': '/data'},
            {'Type': 'bind', 'Source': str(auth), 'Target': AUTH, 'ReadOnly': True}]
        record('requested', expected)
        require(manage.api('GET', '/volumes/' + name, missing=True) is None, 'volume already exists')
        manage.api('POST', '/volumes/create', {'Name': name, 'Labels': {'bokkie.account-qualification': name}})
        volume_created = True
        # Docker initially copies the image's /data ownership (UID10001) into a
        # new volume. Match the selected test identity before exercising writes.
        volume_owner = {**helper_config,
            'Cmd': ['-c', f"import os; os.chown('/data',{config['uid']},{config['gid']})"],
            'HostConfig': {**helper_config['HostConfig'], 'Mounts': [
                {'Type': 'volume', 'Source': name, 'Target': '/data'}]}}
        record('volume-ownership-helper', volume_owner)
        helper = manage.api('POST', '/containers/create?name=' + name + '-volume-owner', volume_owner)['Id']
        manage.api('POST', '/containers/' + helper + '/start')
        completion = manage.api('POST', '/containers/' + helper + '/wait?condition=not-running')
        require(completion['StatusCode'] == 0, 'synthetic volume ownership helper failed')
        remove_container(helper)
        removed.append(helper)
        helper = None
        container = manage.api('POST', '/containers/create?name=' + name, expected)['Id']
        manage.api('POST', '/containers/' + container + '/start')
        actual = manage.api('GET', '/containers/' + container + '/json')
        manage.validate_runtime(expected, actual)
        record('effective', actual)

        def execute(probe, arguments):
            result = subprocess.run([*manage.DOCKER, 'exec', container, 'python3', *arguments],
                                    capture_output=True, text=True, timeout=90)
            record(probe, {'exit': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr})
            require(result.returncode == 0, probe + ' failed; see retained evidence')
            require(manage.api('GET', '/containers/' + container + '/json')['State']['Running'],
                    'qualification container stopped')
            return json.loads(result.stdout)

        def set_mode(mode):
            nonlocal helper
            require(mode in (0, 0o600), 'only synthetic unreadable/readable modes are allowed')
            permissions = {**helper_config, 'User': f"{config['uid']}:{config['gid']}",
                'Cmd': ['-c', "import os; os.chmod('/fixture', " + str(mode) + ')'],
                'HostConfig': {**helper_config['HostConfig'], 'CapAdd': []}}
            helper = manage.api('POST', '/containers/create?name=' + name + '-mode', permissions)['Id']
            try:
                manage.api('POST', '/containers/' + helper + '/start')
                completion = manage.api('POST', '/containers/' + helper + '/wait?condition=not-running')
                require(completion['StatusCode'] == 0, 'synthetic permission helper failed')
            finally:
                remove_container(helper)
                removed.append(helper)
                helper = None

        try:
            set_mode(0)
            execute('unreadable-account-negative', ['-c', '''import json,os
from pathlib import Path
account=Path('/home/probe/.codex/auth.json')
assert account.is_file() and not os.access(account,os.R_OK)
try:
    account.read_bytes()
except PermissionError:
    pass
else:
    raise AssertionError('unreadable synthetic account was readable')
print(json.dumps({'result':'passed','is_file':True,'readable':False,'model_calls':0}))
'''])
        finally:
            set_mode(0o600)
        account = execute('account-integrity-before', ['-c', OUTER, profile, PAYLOAD])
        require(account['source_inode'] == [original.st_dev, original.st_ino]
                and account['source_sha256'] == source_hash, 'mounted source differs')
        for probe in ('boundary', 'lifecycle', 'preflight'):
            observed = execute(probe, ['/opt/probe.py', probe])
            if probe == 'preflight':
                require(observed['exit_code'] == 0
                        and json.loads(observed['stdout'])['model_calls'] == 0,
                        'zero-model preflight failed')
            else:
                require(observed['result'] == 'passed' and observed['model_calls'] == 0,
                        probe + ' did not qualify')
        account = execute('account-integrity-after', ['-c', OUTER, profile, PAYLOAD])
        final = auth.stat()
        require((final.st_dev, final.st_ino) == (original.st_dev, original.st_ino)
                and account['source_inode'] == [final.st_dev, final.st_ino]
                and account['source_sha256'] == source_hash, 'backing source changed')
        record('result', {'result': 'passed', 'image': config['image'], 'source': config['source'],
                         'profile': profile, 'synthetic_source_content_and_inode_unchanged': True,
                         'credentials': 'synthetic only', 'model_calls': 0})
    finally:
        try:
            if helper:
                remove_container(helper)
                removed.append(helper)
            if container:
                remove_container(container)
                removed.append(container)
            if volume_created:
                volume = manage.api('GET', '/volumes/' + name)
                require(volume['Labels'].get('bokkie.account-qualification') == name,
                        'refusing to remove a foreign volume')
                manage.api('DELETE', '/volumes/' + name)
                removed.append(name)
            auth.unlink()
            removed.append('synthetic-auth.json')
        finally:
            record('cleanup', {'removed': removed})


if __name__ == '__main__':
    run()
