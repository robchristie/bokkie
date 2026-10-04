"""Credential-free constructor and detached-descendant lifetime regressions."""
import ctypes
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import broker
import supervisor

SELF = str(Path(__file__).resolve())


def ready(stream):
    if not select.select([stream], [], [], 5)[0]:
        raise AssertionError('fixture readiness timed out')
    return json.loads(stream.readline())


def identity(pid):
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
    except FileNotFoundError:
        return None


def constructor(mode, gate):
    # Emulate monitor -> namespace leader -> detached daemon. The startup
    # leader has no PDEATHSIG, as in bwrap's pre-do_init construction window.
    leader = os.fork()
    if leader:
        os.waitpid(leader, 0)
        return
    if mode == 'startup':
        with open(gate, 'rb', buffering=0) as stream:
            print(json.dumps({'leader': os.getpid()}), flush=True)
            stream.read(1)
        Path(gate + '.executed').touch()
    else:
        ready_read, ready_write = os.pipe()
        daemon = os.fork()
        if not daemon:
            os.close(ready_read)
            os.setsid()
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            for fd in (0, 1, 2):
                os.close(fd)
            os.write(ready_write, b'1')
            os.close(ready_write)
            while True:
                signal.pause()
        os.close(ready_write)
        try:
            assert select.select([ready_read], [], [], 5)[0], 'daemon readiness timeout'
            assert os.read(ready_read, 1) == b'1', 'daemon exited before installing signal handlers'
        finally:
            os.close(ready_read)
        print(json.dumps({'leader': os.getpid(), 'daemon': daemon}), flush=True)
        sys.stdin.buffer.read(1)
    os._exit(0)


def launch(mode, gate):
    command = [sys.executable, SELF, 'constructor', mode, gate]
    with patch.object(broker, '_command', return_value=command):
        return broker.spawn({}, {}, dict(os.environ))


class SupervisorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Reap orphaned supervisors ourselves rather than relying on host init.
        libc = ctypes.CDLL(None)
        previous = ctypes.c_int()
        assert libc.prctl(37, ctypes.byref(previous), 0, 0, 0) == 0
        cls.previous_subreaper = previous.value
        supervisor.become_subreaper()

    @classmethod
    def tearDownClass(cls):
        assert ctypes.CDLL(None).prctl(36, cls.previous_subreaper, 0, 0, 0) == 0

    def assert_gone(self, processes):
        self.assertTrue(all(identity(pid) != start for pid, start in processes.items()), processes)

    def test_normal_and_cancellation_reap_detached_descendants(self):
        for cancel in (False, True):
            with self.subTest(cancel=cancel):
                child = launch('ready', '-')
                try:
                    descendants = ready(child.stdout)
                    observed = {pid: identity(pid) for pid in descendants.values()}
                    if cancel:
                        broker.terminate(child)
                    else:
                        child.stdin.write(b'x'); child.stdin.flush()
                    child.communicate(timeout=5)
                    self.assertEqual(child.returncode, 125 if cancel else 0)
                    self.assert_gone(observed)
                finally:
                    broker.terminate(child)
                    for stream in (child.stdin, child.stdout, child.stderr):
                        stream.close()

    def test_parent_death_during_ready_and_blocked_constructor(self):
        for mode in ('ready', 'startup', 'group-death'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                gate = str(Path(directory) / 'gate')
                os.mkfifo(gate)
                held = os.open(gate, os.O_RDWR | os.O_NONBLOCK)
                parent = subprocess.Popen([sys.executable, SELF, 'parent', mode, gate],
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                          start_new_session=True)
                helper = None
                try:
                    observed = ready(parent.stdout)
                    helper = observed.pop('supervisor')
                    identities = {pid: identity(pid) for pid in observed.values()}
                    if mode == 'group-death':
                        os.killpg(parent.pid, signal.SIGKILL)
                    else:
                        parent.kill()
                    parent.communicate(timeout=5)
                    deadline = time.monotonic() + 5
                    while True:
                        reaped, _ = os.waitpid(helper, os.WNOHANG)
                        if reaped:
                            helper = None
                            break
                        self.assertLess(time.monotonic(), deadline, 'supervisor teardown timed out')
                        time.sleep(0.01)
                    self.assert_gone(identities)
                    self.assertFalse(Path(gate + '.executed').exists())
                    # The constructor must already be gone before release.
                    os.write(held, b'x')
                finally:
                    os.close(held)
                    if parent.poll() is None:
                        parent.kill(); parent.wait()
                    for stream in (parent.stdout, parent.stderr):
                        stream.close()
                    if helper:
                        os.kill(helper, signal.SIGTERM)
                        os.waitpid(helper, 0)

    def test_dead_parent_prevents_constructor_launch(self):
        parent = subprocess.Popen([sys.executable, '-c', 'pass'])
        parent_fd = broker.pidfd_open(parent.pid)
        parent.wait(timeout=5)
        with broker.payload_filter_fd() as descriptor:
            duplicate = os.dup(descriptor)
            with patch.object(supervisor.subprocess, 'Popen') as popen, \
                    patch.object(supervisor.signal, 'signal'):
                self.assertEqual(supervisor.supervise(parent_fd, duplicate, ['/invalid']), 125)
                popen.assert_not_called()

    def test_cleanup_timeout_retains_supervisor_owner(self):
        child = unittest.mock.Mock()
        child.poll.return_value = None
        child.wait.side_effect = subprocess.TimeoutExpired('supervisor', 5)
        with self.assertRaises(subprocess.TimeoutExpired):
            broker.terminate(child)
        child.terminate.assert_called_once()
        child.kill.assert_not_called()


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == 'constructor':
        constructor(sys.argv[2], sys.argv[3])
    elif len(sys.argv) > 1 and sys.argv[1] == 'parent':
        child = launch(sys.argv[2], sys.argv[3])
        print(json.dumps({'supervisor': child.pid, **ready(child.stdout)}), flush=True)
        while True:
            signal.pause()
    else:
        unittest.main()
