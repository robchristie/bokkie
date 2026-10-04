#!/usr/bin/env python3
"""Trusted per-invocation subreaper; never runs inside the payload namespace."""
import ctypes
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time


def become_subreaper():
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'cannot establish conversation subreaper')


def reap_owned_children():
    """Kill each adoption generation and return only after waitpid reports ECHILD.

    Children cannot reuse their PIDs before this single-threaded owner reaps
    them. Killing the original monitor alone misses Bubblewrap's constructor
    window, and process groups miss setsid/double-fork descendants.
    """
    children_file = Path(f'/proc/self/task/{os.getpid()}/children')
    while True:
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return
            if pid == 0:
                break
        for value in children_file.read_text().split():
            try:
                os.kill(int(value), signal.SIGKILL)
            except ProcessLookupError:
                pass
        # Reparenting and namespace teardown may finish asynchronously. Do not
        # abandon live children on a timeout; the broker bounds its own wait.
        time.sleep(0.01)


def supervise(parent_fd, filter_fd, command):
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    become_subreaper()
    child = None
    try:
        # The broker opened this pidfd before Popen: no parent-PID reuse race,
        # including death before this interpreter starts.
        if stopping or select.select([parent_fd], [], [], 0)[0]:
            return 125
        child = subprocess.Popen(command, pass_fds=(filter_fd,))
        os.close(filter_fd)
        filter_fd = None
        while not stopping:
            if select.select([parent_fd], [], [], 0.05)[0]:
                break
            result = child.poll()
            if result is not None:
                return result if result >= 0 else 128 - result
        return 125
    finally:
        # Also covers failed exec and Python exceptions after launch. SIGKILL of
        # this trusted supervisor itself is outside this userspace guarantee.
        reap_owned_children()
        if child is not None:
            child.poll()
        if filter_fd is not None:
            os.close(filter_fd)
        os.close(parent_fd)


if __name__ == '__main__':
    try:
        result = supervise(int(sys.argv[1]), int(sys.argv[2]), sys.argv[3:])
    except Exception:
        # Do not echo argv, model diagnostics, environment or account material.
        print('conversation supervisor failed', file=sys.stderr)
        result = 125
    sys.exit(result)
