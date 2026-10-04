"""Hostile, credential-free qualification of the actual conversation launcher.

Run only inside the disposable, network-off container. No sensitive proc content
is read and no kernel setting or helper memory is written.
"""
from contextlib import contextmanager
import ctypes
import errno
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import threading
import time
from unittest.mock import patch

sys.path.insert(0, '/opt/conversation')
import broker

PROFILE = {'bwrap': '/usr/bin/bwrap', 'codex': '/usr/local/bin/codex'}
SELF = '/opt/qualify.py'
STATUS_KEYS = ('CapEff', 'CapPrm', 'CapBnd', 'NoNewPrivs', 'Seccomp', 'Seccomp_filters')


def emit(value):
    print(json.dumps(value, sort_keys=True), flush=True)


def context(pid='self'):
    root = Path('/proc') / str(pid)
    status = dict(line.split(':', 1) for line in (root / 'status').read_text().splitlines())
    return {'status': {k: status[k].strip() for k in STATUS_KEYS},
            'namespaces': {k: os.readlink(root / 'ns' / k) for k in ('user', 'mnt', 'pid')},
            'uid_map': (root / 'uid_map').read_text(), 'gid_map': (root / 'gid_map').read_text(),
            'apparmor': (root / 'attr/current').read_text().strip(),
            'mounts': (root / 'mountinfo').read_text().splitlines()}


def open_errno(path, flags):
    try:
        fd = os.open(path, flags | os.O_CLOEXEC)
    except OSError as error:
        return error.errno
    os.close(fd)
    return 0


def proc_checks():
    # These opens do not read kernel contents or write settings.
    prefixes = ('', '/proc/self/root', '/proc/1/root', '/proc/1/task/1/root')
    inaccessible = {prefix + path: open_errno(prefix + path, os.O_RDONLY)
                    for prefix in prefixes for path in (
                        '/proc/kcore', '/proc/keys', '/proc/interrupts', '/proc/timer_list')}
    readonly = {prefix + path: open_errno(prefix + path, os.O_WRONLY)
                for prefix in prefixes for path in (
                    '/proc/sys/kernel/shmmax', '/proc/sys/net/ipv4/ip_forward', '/proc/sysrq-trigger')}
    assert all(v in (errno.EACCES, errno.EPERM, errno.ENOENT) for v in inaccessible.values()), inaccessible
    assert all(v in (errno.EACCES, errno.EPERM, errno.EROFS) for v in readonly.values()), readonly
    return {'masked_replacements': inaccessible, 'readonly_replacements': readonly}


def syscalls():
    library = ctypes.CDLL('libseccomp.so.2')
    library.seccomp_syscall_resolve_name.argtypes = [ctypes.c_char_p]
    library.seccomp_syscall_resolve_name.restype = ctypes.c_int
    libc = ctypes.CDLL(None, use_errno=True)
    libc.syscall.restype = ctypes.c_long
    null = ctypes.c_void_p()
    text = lambda value: ctypes.c_char_p(value.encode())
    cases = {
        'mount': (text('none'), text('/data'), null, ctypes.c_ulong(0x1021), null),
        'umount2': (text('/data'), 2), 'pivot_root': (text('/tmp'), text('/tmp')),
        'unshare': (0x10000000,), 'setns': (-1, 0), 'chroot': (text('/tmp'),),
        'fsopen': (text('tmpfs'), 0), 'fsconfig': (-1, 0, null, null, 0),
        'fsmount': (-1, 0, 0), 'fspick': (-1, text('/'), 0),
        'open_tree': (-1, text('/'), 0), 'move_mount': (-1, text('/'), -1, text('/'), 0),
        'mount_setattr': (-1, text('/'), 0, null, 0),
        'ptrace': (16, os.getpid(), null, null),
        'process_vm_readv': (1, null, 0, null, 0, 0),
        'process_vm_writev': (1, null, 0, null, 0, 0), 'pidfd_getfd': (-1, 0, 0),
        'clone': (0x30020011, null, null, null, null), 'clone3': (null, 0),
    }
    results = {}
    for name, args in cases.items():
        number = library.seccomp_syscall_resolve_name(name.encode())
        assert number >= 0, name
        ctypes.set_errno(0)
        result = libc.syscall(ctypes.c_long(number), *args)
        if name == 'clone' and result == 0:
            os._exit(91)
        results[name] = {'result': result, 'errno': ctypes.get_errno()}
        expected = errno.ENOSYS if name == 'clone3' else errno.EPERM
        assert result == -1 and results[name]['errno'] == expected, (name, results[name])
    return results


def inner():
    state = context()
    init = context(1)
    aliases = ['/data/boundary-canary', '/proc/self/root/data/boundary-canary',
               '/proc/1/root/data/boundary-canary', '/proc/1/task/1/root/data/boundary-canary']
    fd = os.open('/data', os.O_RDONLY | os.O_DIRECTORY)
    try:
        aliases.append(f'/proc/self/fd/{fd}/boundary-canary')
        writes = {p: open_errno(p, os.O_WRONLY) for p in aliases}
    finally:
        os.close(fd)
    assert set(writes.values()) == {errno.EROFS}, writes
    memory = {p: open_errno(p, os.O_RDWR) for p in (
        '/proc/1/mem', '/proc/1/task/1/mem', '/proc/1/root/proc/1/mem',
        '/proc/self/root/proc/1/task/1/mem')}
    assert all(v in (errno.EACCES, errno.EPERM) for v in memory.values()), memory
    helper_fds = {}
    for directory in ('/proc/1/fd', '/proc/1/task/1/fd'):
        for item in Path(directory).iterdir():
            # Opening an eventfd or pipe must not recover the helper's handles.
            error = open_errno(item, os.O_RDWR | os.O_NONBLOCK)
            shared_stdio = (item.name in ('0', '1', '2') and
                            os.stat(item).st_ino == os.fstat(int(item.name)).st_ino and
                            os.stat(item).st_dev == os.fstat(int(item.name)).st_dev)
            helper_fds[str(item)] = {'errno': error, 'shared_stdio': shared_stdio}
    assert helper_fds and all(v['errno'] in (errno.EACCES, errno.EPERM) or v['shared_stdio']
                              for v in helper_fds.values()), helper_fds
    fds = {}
    for item in Path('/proc/self/fd').iterdir():
        try:
            fds[item.name] = os.readlink(item)
        except FileNotFoundError:
            pass
    assert not any('memfd:' in value for value in fds.values()), fds
    assert not Path('/home/probe/.codex/canary').exists()
    assert not Path('/tmp/outer-canary').exists()
    Path('/tmp/private-write').write_text('private')
    denied = syscalls()
    # An ordinary fork/exec inherits confinement; ordinary threads remain usable.
    fork = subprocess.run([sys.executable, SELF, 'syscalls'], capture_output=True, text=True, timeout=5)
    assert fork.returncode == 0, fork.stderr
    threaded = []
    thread = threading.Thread(target=lambda: threaded.append(True))
    thread.start(); thread.join(timeout=5)
    assert threaded == [True]
    nested = subprocess.run([PROFILE['bwrap'], '--unshare-user', '--ro-bind', '/', '/', '--', '/bin/true'],
                            capture_output=True, text=True, timeout=5)
    assert nested.returncode != 0 and any(message in nested.stderr for message in (
        'Operation not permitted', 'No permissions to create new namespace')), nested
    processes = {p.name: (p / 'comm').read_text().strip() for p in Path('/proc').iterdir() if p.name.isdigit()}
    # At observation time only bwrap's PID1 and this payload remain.
    assert set(processes) == {'1', str(os.getpid())}, processes
    for current in (state, init):
        assert all(int(current['status'][k], 16) == 0 for k in ('CapEff', 'CapPrm', 'CapBnd')), current
        assert current['status']['NoNewPrivs'] == '1' and current['status']['Seccomp'] == '2'
        assert current['apparmor'].endswith(' (enforce)')
    emit({'context': state, 'init': init, 'writes': writes, 'helper_memory': memory,
          'helper_fds': helper_fds, 'payload_fds': fds, 'syscalls': denied,
          'fork_exec_inheritance': json.loads(fork.stdout), 'thread': True,
          'nested_bwrap': nested.stderr.strip(), 'processes': processes, 'proc': proc_checks()})


def boundary():
    assert os.getuid() == 10001
    Path('/home/probe/.codex/canary').write_text('synthetic account canary')
    Path('/tmp/outer-canary').write_text('outer temporary directory')
    Path('/data/boundary-canary').write_text('outer writable state')
    outer = context()
    outer_proc = proc_checks()
    # A valid FD with invalid BPF must not be treated as optional hardening.
    @contextmanager
    def malformed_filter():
        import payload_filter
        descriptor = payload_filter._memfd()
        try:
            os.write(descriptor, bytes(8))
            os.lseek(descriptor, 0, os.SEEK_SET)
            yield descriptor
        finally:
            os.close(descriptor)
    with patch.object(broker, 'payload_filter_fd', malformed_filter):
        invalid = broker.spawn(PROFILE, {}, dict(os.environ),
                               payload=['/bin/echo', 'UNEXPECTED_PAYLOAD'])
        invalid_out, invalid_err = invalid.communicate(timeout=5)
        assert invalid.returncode != 0 and b'UNEXPECTED_PAYLOAD' not in invalid_out
        assert b'seccomp' in invalid_err.lower(), invalid_err
    child = broker.spawn(PROFILE, {}, dict(os.environ), payload=[sys.executable, SELF, 'inner'])
    stdout, stderr = child.communicate(timeout=20)
    assert child.returncode == 0, (child.returncode, stdout, stderr)
    observed = json.loads(stdout)
    for who in ('context', 'init'):
        assert all(observed[who]['namespaces'][k] != outer['namespaces'][k] for k in ('user', 'mnt', 'pid'))
        assert int(observed[who]['status']['Seccomp_filters']) > int(outer['status']['Seccomp_filters'])
    assert not Path('/tmp/private-write').exists()
    assert Path('/data/boundary-canary').read_text() == 'outer writable state'
    versions = {name: subprocess.check_output(command, text=True).strip() for name, command in {
        'bwrap': ['bwrap', '--version'], 'codex': ['codex', '--version'],
        'python': [sys.executable, '--version'], 'node': ['node', '--version'],
        'libseccomp': ['dpkg-query', '-W', '-f=${Version}', 'libseccomp2']}.items()}
    emit({'result': 'passed', 'versions': versions, 'outer': outer, 'outer_proc': outer_proc, 'inner': observed,
          'bad_filter_rejected': invalid_err.decode().strip(),
          'credentials': False, 'model_calls': 0})


def process_id(pid):
    try:
        stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return {'pid': int(pid), 'start': stat[19], 'state': stat[0]}
    except FileNotFoundError:
        return None


def tagged_processes(token):
    found = []
    for p in Path('/proc').iterdir():
        if p.name.isdigit():
            try:
                if token.encode() in (p / 'cmdline').read_bytes().split(b'\0'):
                    identity = process_id(p.name)
                    if identity:
                        found.append(identity)
            except (FileNotFoundError, ProcessLookupError):
                pass
    return found


def wait_gone(identities):
    deadline = time.monotonic() + 5
    while True:
        remaining = []
        for expected in identities:
            current = process_id(expected['pid'])
            if current and current['start'] == expected['start']:
                remaining.append(current)
        if not remaining:
            return
        assert time.monotonic() < deadline, ('surviving processes', remaining)
        time.sleep(0.02)


def lifecycle_payload(token):
    # Detach and close stdio: pipe EOF and process-group kills cannot pass alone.
    daemon = subprocess.Popen([sys.executable, SELF, 'daemon', token], start_new_session=True,
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    emit({'ready': True, 'daemon_inner_pid': daemon.pid})
    sys.stdin.buffer.read(1)


def launch_parent(token, barrier=None):
    # Inject a trusted wrapper only in this probe process. Production launch
    # still forwards exactly its sealed filter, never an arbitrary control FD.
    if barrier is not None:
        original = broker._command
        def blocked(*args, **kwargs):
            return [sys.executable, SELF, 'constructor-wrapper', barrier,
                    *original(*args, **kwargs)]
        broker._command = blocked
    child = broker.spawn(PROFILE, {}, dict(os.environ),
                         payload=[sys.executable, SELF, 'lifecycle-payload', token])
    if barrier is None:
        ready_line(child.stdout)
    emit({'supervisor': child.pid})
    # Leave pipes open while the test kills this actual launch parent.
    while True:
        time.sleep(1)


def constructor_wrapper(barrier, command):
    descriptor = os.open(barrier, os.O_RDONLY)
    os.set_inheritable(descriptor, True)
    os.execv(command[0], [command[0], '--block-fd', str(descriptor), *command[1:]])


def blocked_constructor(supervisor):
    deadline = time.monotonic() + 10
    while True:
        monitors = Path(f'/proc/{supervisor}/task/{supervisor}/children').read_text().split()
        for monitor in monitors:
            children = Path(f'/proc/{monitor}/task/{monitor}/children').read_text().split()
            for leader in children:
                root = Path('/proc') / leader
                waiting = (root / 'wchan').read_text().strip()
                mapping = (root / 'uid_map').read_text().split()
                if (waiting in ('pipe_read', 'anon_pipe_read') and
                        (root / 'root/tmp/conversation').is_dir() and
                        mapping == ['10001', '10001', '1']):
                    return [process_id(monitor), process_id(leader)]
        assert time.monotonic() < deadline, 'constructor did not reach post-remap barrier'
        time.sleep(0.01)


def ready_line(pipe):
    assert select.select([pipe], [], [], 10)[0], 'readiness timeout'
    line = pipe.readline()
    assert line, 'launcher exited before readiness'
    return json.loads(line)


def lifecycle():
    results = []
    for mode in ('normal', 'cancel', 'parent-death', 'startup-death'):
        token = 'bokkie-lifetime-' + mode + '-' + str(os.getpid())
        barrier = Path('/tmp') / (token + '.fifo')
        held = None
        parent = child = None
        try:
            if mode in ('normal', 'cancel'):
                child = broker.spawn(PROFILE, {}, dict(os.environ),
                                     payload=[sys.executable, SELF, 'lifecycle-payload', token])
                ready_line(child.stdout)
                identities = tagged_processes(token)
                assert len(identities) >= 4, identities  # supervisor, monitor, payload, daemon
                if mode == 'normal':
                    child.stdin.write(b'x'); child.stdin.flush()
                else:
                    broker.terminate(child)  # Actual broker cancellation and wait.
                stdout, stderr = child.communicate(timeout=5)
                assert child.returncode == (0 if mode == 'normal' else 125), (stdout, stderr)
            else:
                command = [sys.executable, SELF, 'launch-parent', token]
                if mode == 'startup-death':
                    os.mkfifo(barrier)
                    held = os.open(barrier, os.O_RDWR | os.O_NONBLOCK)
                    command.append(str(barrier))
                parent = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                supervisor = ready_line(parent.stdout)['supervisor']
                blocked = blocked_constructor(supervisor) if held is not None else []
                identities = tagged_processes(token)
                for observed in blocked + [process_id(supervisor)]:
                    if observed and observed not in identities:
                        identities.append(observed)
                parent.kill(); parent.communicate(timeout=5)
            # Check before releasing the constructor barrier: the leader must
            # be reaped, with no chance to run its payload after parent death.
            wait_gone(identities)
            assert tagged_processes(token) == []
            result = {'mode': mode, 'identities': identities, 'all_reaped': True}
            if held is not None:
                result['constructor_barrier'] = 'post-mount and UID remap, before do_init'
                result['reaped_before_barrier_release'] = True
            results.append(result)
        finally:
            if held is not None:
                os.close(held)
                barrier.unlink()
            if child is not None:
                broker.terminate(child)
                for stream in (child.stdin, child.stdout, child.stderr):
                    stream.close()
            if parent is not None:
                if parent.poll() is None:
                    parent.kill(); parent.wait(timeout=5)
                for stream in (parent.stdout, parent.stderr):
                    stream.close()
    emit({'result': 'passed', 'cases': results, 'container_alive_during_checks': True, 'model_calls': 0})


if __name__ == '__main__':
    mode = sys.argv[1]
    if mode == 'inner': inner()
    elif mode == 'boundary': boundary()
    elif mode == 'syscalls': emit(syscalls())
    elif mode == 'lifecycle': lifecycle()
    elif mode == 'lifecycle-payload': lifecycle_payload(sys.argv[2])
    elif mode == 'launch-parent': launch_parent(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
    elif mode == 'constructor-wrapper': constructor_wrapper(sys.argv[2], sys.argv[3:])
    elif mode == 'daemon':
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        while True: time.sleep(1)
    else: raise ValueError('unknown probe')
