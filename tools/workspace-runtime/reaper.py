#!/usr/bin/env python3
"""Trusted outside owner: adopt/kill/reap every payload descendant."""
import ctypes
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time
from common import atomic, read


def reap():
    children = Path(f'/proc/self/task/{os.getpid()}/children')
    while True:
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return
            if pid == 0:
                break
        for value in children.read_text().split():
            try:
                os.kill(int(value), signal.SIGKILL)
            except ProcessLookupError:
                pass
        time.sleep(0.01)


def supervise(root, parent_fd, command):
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    libc = ctypes.CDLL(None,use_errno=True)
    if libc.prctl(36,1,0,0,0) != 0:
        raise OSError(ctypes.get_errno(),'cannot establish workspace subreaper')
    manifest = read(root/'launch-committed.json')
    child = None
    try:
        if stopping or select.select([parent_fd],[],[],0)[0]:
            return
        child = subprocess.Popen(command)
        atomic(root/'boundary-started.json',{'generation':manifest['generation'],
               'pid':child.pid,'owner_pid':os.getpid()},immutable=True)
        while not stopping and not select.select([parent_fd],[],[],0.05)[0]:
            if child.poll() is not None:
                break
    finally:
        reap()
        proof = {'generation':manifest['generation'],'boundary_id':manifest['boundary_id'],
                 'kind':'descendants_reaped' if child else 'not_started',
                 'evidence':'Trusted outside subreaper observed waitpid ECHILD',
                 'owner_pid':os.getpid()}
        atomic(root/'cessation.json',proof,immutable=True)
        os.close(parent_fd)


if __name__ == '__main__':
    try:
        supervise(Path(sys.argv[1]),int(sys.argv[2]),sys.argv[3:])
    except Exception:
        print('workspace cleanup owner failed; ownership remains uncertain',file=sys.stderr)
        sys.exit(125)
