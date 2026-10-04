"""Offline filter generation and kernel enforcement; no Bubblewrap or model turn."""
import ctypes
import errno
import fcntl
import json
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch

import payload_filter


def evaluate(program, architecture, number, flags=0):
    """Run exported classic BPF against seccomp_data, without making a syscall."""
    data = struct.pack('=iI7Q', number, architecture, 0, flags, 0, 0, 0, 0, 0)
    instructions = list(struct.iter_unpack('=HBBI', program))
    accumulator, position = 0, 0
    for _ in instructions:
        code, yes, no, value = instructions[position]
        if code == 0x20:  # BPF_LD | BPF_W | BPF_ABS
            accumulator, = struct.unpack_from('=I', data, value)
        elif code == 0x54:  # BPF_ALU | BPF_AND | BPF_K
            accumulator &= value
        elif code == 0x06:  # BPF_RET | BPF_K
            return value
        elif code == 0x05:  # BPF_JMP | BPF_JA
            position += value
        elif code in (0x15, 0x25, 0x35, 0x45):
            condition = {0x15: accumulator == value, 0x25: accumulator > value,
                         0x35: accumulator >= value, 0x45: bool(accumulator & value)}[code]
            position += yes if condition else no
        else:
            raise AssertionError('unsupported generated BPF instruction: ' + hex(code))
        position += 1
    raise AssertionError('BPF did not return an action')


class PayloadFilterTests(unittest.TestCase):
    def test_generated_filter_denies_constructor_operations_and_other_abis(self):
        library = payload_filter._library()
        native = library.seccomp_arch_native()
        with payload_filter.payload_filter_fd() as descriptor:
            program = os.read(descriptor, 32768)
        # List expectations independently so accidentally removing a production
        # rule fails this behavioural check instead of shrinking the test too.
        denied = ('mount', 'umount2', 'pivot_root', 'chroot', 'unshare', 'setns', 'fsopen',
                  'fsconfig', 'fsmount', 'fspick', 'open_tree', 'move_mount',
                  'mount_setattr', 'ptrace', 'process_vm_readv', 'process_vm_writev',
                  'pidfd_getfd')
        for name in denied:
            number = library.seccomp_syscall_resolve_name(name.encode())
            self.assertGreaterEqual(number, 0, name)
            self.assertEqual(evaluate(program, native, number), 0x00050000 | errno.EPERM, name)
        clone = library.seccomp_syscall_resolve_name(b'clone')
        for flag in (0x00020000, 0x02000000, 0x04000000, 0x08000000,
                     0x10000000, 0x20000000, 0x40000000):
            for extra in (0, signal.SIGCHLD, 0x100):
                self.assertEqual(evaluate(program, native, clone, flag | extra),
                                 0x00050000 | errno.EPERM)
        for flags in (signal.SIGCHLD, 0x10f00, 0x1200011):
            self.assertEqual(evaluate(program, native, clone, flags), 0x7fff0000)
        clone3 = library.seccomp_syscall_resolve_name(b'clone3')
        self.assertEqual(evaluate(program, native, clone3), 0x00050000 | errno.ENOSYS)
        for name in ('read', 'write', 'execve', 'fork', 'vfork', 'wait4', 'futex'):
            number = library.seccomp_syscall_resolve_name(name.encode())
            self.assertEqual(evaluate(program, native, number), 0x7fff0000, name)
        compatibility = library.seccomp_arch_resolve_name(b'x86')
        self.assertEqual(evaluate(program, compatibility, clone), 0x80000000)
        # x32 shares the x86-64 audit architecture but marks syscall numbers.
        self.assertEqual(evaluate(program, native, clone | 0x40000000), 0x80000000)

    def test_descriptor_is_sealed_and_closed(self):
        with payload_filter.payload_filter_fd() as descriptor:
            self.assertEqual(os.lseek(descriptor, 0, os.SEEK_CUR), 0)
            self.assertFalse(os.get_inheritable(descriptor))
            self.assertEqual(fcntl.fcntl(descriptor, 1034), 0x000f)  # F_GET_SEALS
            with self.assertRaises(OSError):
                os.write(descriptor, b'corrupt')
        with self.assertRaises(OSError):
            os.fstat(descriptor)

    def test_missing_or_incompatible_library_fails_closed(self):
        for result in (OSError('missing'), object()):
            options = {'side_effect': result} if isinstance(result, Exception) else {'return_value': result}
            with self.subTest(result=result), patch.object(payload_filter.ctypes, 'CDLL', **options):
                with self.assertRaisesRegex(ValueError, 'compatible libseccomp'):
                    with payload_filter.payload_filter_fd():
                        self.fail('must not yield a descriptor')

    def test_unsupported_native_abi_fails_before_filter_creation(self):
        library = Mock(wraps=payload_filter._library())
        library.seccomp_arch_native.return_value = 0
        with patch.object(payload_filter, '_library', return_value=library):
            with self.assertRaisesRegex(ValueError, 'qualified Linux x86-64'):
                with payload_filter.payload_filter_fd():
                    self.fail('must not yield a descriptor')
        library.seccomp_init.assert_not_called()

    def test_unknown_and_pseudo_syscalls_fail_closed_and_release_context(self):
        for missing in (-1, -10100):
            library = Mock(wraps=payload_filter._library())
            original = library.seccomp_syscall_resolve_name
            library.seccomp_syscall_resolve_name = Mock(
                side_effect=lambda name: missing if name == b'mount_setattr' else original(name))
            with self.subTest(missing=missing), patch.object(payload_filter, '_library', return_value=library):
                with self.assertRaisesRegex(ValueError, 'cannot resolve mount_setattr'):
                    with payload_filter.payload_filter_fd():
                        self.fail('must not yield a descriptor')
            library.seccomp_release.assert_called_once()
            library.seccomp_export_bpf.assert_not_called()

    def test_library_operation_failures_never_yield_partial_filter(self):
        for operation in ('seccomp_attr_set', 'seccomp_rule_add_exact_array', 'seccomp_export_bpf'):
            library = Mock(wraps=payload_filter._library())
            getattr(library, operation).return_value = -errno.EINVAL
            with self.subTest(operation=operation), patch.object(payload_filter, '_library', return_value=library):
                with self.assertRaisesRegex(ValueError, 'payload syscall filter failed'):
                    with payload_filter.payload_filter_fd():
                        self.fail('must not yield a descriptor')
            library.seccomp_release.assert_called_once()

    def test_kernel_enforcement_preserves_processes_and_threads(self):
        # Install the generated bytes only in this disposable subprocess. Invalid
        # arguments prevent host mount, namespace or process-memory mutation even
        # if the filter regresses. BPF evaluation above proves each rule separately.
        source = r'''
import ctypes, errno, json, os, signal, subprocess, sys, threading
import payload_filter

class Instruction(ctypes.Structure):
    _fields_ = [('code', ctypes.c_ushort), ('jt', ctypes.c_ubyte),
                ('jf', ctypes.c_ubyte), ('k', ctypes.c_uint)]
class Program(ctypes.Structure):
    _fields_ = [('length', ctypes.c_ushort), ('filter', ctypes.POINTER(Instruction))]

library = payload_filter._library()
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
with payload_filter.payload_filter_fd() as descriptor:
    raw = os.read(descriptor, 32768)
instructions = (Instruction * (len(raw) // ctypes.sizeof(Instruction))).from_buffer_copy(raw)
program = Program(len(instructions), instructions)
assert libc.prctl(38, 1, 0, 0, 0) == 0, ctypes.get_errno()  # PR_SET_NO_NEW_PRIVS
assert libc.prctl(22, 2, ctypes.byref(program), 0, 0) == 0, ctypes.get_errno()  # PR_SET_SECCOMP

def denied(name, arguments, expected=errno.EPERM):
    number = library.seccomp_syscall_resolve_name(name.encode())
    ctypes.set_errno(0)
    result = libc.syscall(ctypes.c_long(number), *(ctypes.c_long(value) for value in arguments))
    assert result == -1 and ctypes.get_errno() == expected, (name, result, ctypes.get_errno())

denied('unshare', [0])
denied('setns', [-1, 0])
denied('mount', [0, 0, 0, 0, 0])
denied('umount2', [0, 0])
denied('pivot_root', [0, 0])
denied('chroot', [0])
denied('clone3', [0, 0], errno.ENOSYS)
for flag in payload_filter.NAMESPACE_FLAGS.values():
    denied('clone', [flag | 0x10000, 0, 0, 0, 0])  # CLONE_THREAD without SIGHAND/VM is invalid
denied('ptrace', [-1, -1, 0, 0])
denied('process_vm_readv', [-1, 0, 0, 0, 0, 0])
denied('process_vm_writev', [-1, 0, 0, 0, 0, 0])
denied('pidfd_getfd', [-1, -1, 0])

threads = []
thread = threading.Thread(target=lambda: threads.append('thread'))
thread.start(); thread.join()
assert threads == ['thread']
pid = os.fork()
if pid == 0:
    os._exit(17)
assert os.waitpid(pid, 0)[1] == 17 << 8
child = subprocess.run([sys.executable, '-c', 'print("exec")'], check=True, capture_output=True, text=True)
assert child.stdout.strip() == 'exec'
print(json.dumps({'fork': True, 'thread': True, 'exec': True, 'denied': True}))
'''
        result = subprocess.run([sys.executable, '-c', source],
                                cwd=Path(__file__).parent, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {'fork': True, 'thread': True, 'exec': True, 'denied': True})


if __name__ == '__main__':
    unittest.main()
