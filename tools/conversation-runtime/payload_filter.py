"""Generate the fixed post-construction syscall boundary for Bubblewrap.

This revocation filter is layered over the container's default-deny policy; it
does not grant syscalls missing from an already installed filter. Only the
qualified Linux x86-64 native ABI is supported. libseccomp owns syscall numbers
and the generated architecture guards, including rejection of x86 and x32.
"""
from contextlib import contextmanager
import ctypes
import errno
import fcntl
import os
import sys


# Stable libseccomp API values, not architecture-specific syscall numbers.
_ALLOW = 0x7fff0000
_KILL_PROCESS = 0x80000000
_ERRNO = 0x00050000
_ACT_BADARCH = 2
_MASKED_EQ = 7
# Linux UAPI constants are independent of Python's build-time header support.
_F_ADD_SEALS = 1033
_ALL_SEALS = 0x0001 | 0x0002 | 0x0004 | 0x0008  # SEAL, SHRINK, GROW, WRITE

DENIED_SYSCALLS = (
    'mount', 'umount2', 'pivot_root', 'chroot', 'unshare', 'setns',
    'fsopen', 'fsconfig', 'fsmount', 'fspick', 'open_tree', 'move_mount',
    'mount_setattr', 'ptrace', 'process_vm_readv', 'process_vm_writev', 'pidfd_getfd',
)

# Linux UAPI clone flags. Each comparison rejects the bit even when combined
# with ordinary process/thread flags. CLONE_NEWTIME is available via unshare or
# clone3, both already denied; on legacy clone its low bit is in CSIGNAL.
NAMESPACE_FLAGS = {
    'CLONE_NEWNS': 0x00020000,
    'CLONE_NEWCGROUP': 0x02000000,
    'CLONE_NEWUTS': 0x04000000,
    'CLONE_NEWIPC': 0x08000000,
    'CLONE_NEWUSER': 0x10000000,
    'CLONE_NEWPID': 0x20000000,
    'CLONE_NEWNET': 0x40000000,
}


class _Comparison(ctypes.Structure):
    _fields_ = [('arg', ctypes.c_uint), ('op', ctypes.c_int),
                ('datum_a', ctypes.c_uint64), ('datum_b', ctypes.c_uint64)]


def _library():
    try:
        library = ctypes.CDLL('libseccomp.so.2')
        signatures = {
            'seccomp_arch_native': ([], ctypes.c_uint32),
            'seccomp_arch_resolve_name': ([ctypes.c_char_p], ctypes.c_uint32),
            'seccomp_init': ([ctypes.c_uint32], ctypes.c_void_p),
            'seccomp_release': ([ctypes.c_void_p], None),
            'seccomp_attr_set': ([ctypes.c_void_p, ctypes.c_int, ctypes.c_uint32], ctypes.c_int),
            'seccomp_syscall_resolve_name': ([ctypes.c_char_p], ctypes.c_int),
            'seccomp_rule_add_exact_array': ([ctypes.c_void_p, ctypes.c_uint32,
                                             ctypes.c_int, ctypes.c_uint,
                                             ctypes.POINTER(_Comparison)], ctypes.c_int),
            'seccomp_export_bpf': ([ctypes.c_void_p, ctypes.c_int], ctypes.c_int),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(library, name)
            function.argtypes, function.restype = arguments, result
        return library
    except (OSError, AttributeError) as error:
        raise ValueError('payload syscall filter requires compatible libseccomp.so.2') from error


def _check(result, operation):
    if result != 0:
        raise ValueError('payload syscall filter failed to ' + operation)


def _syscall(library, name):
    number = library.seccomp_syscall_resolve_name(name.encode('ascii'))
    # Negative pseudo-syscalls are unsuitable for the qualified native ABI too.
    # Never silently omit a restriction when a library cannot resolve it.
    if number < 0:
        raise ValueError('payload syscall filter cannot resolve ' + name)
    return number


def _memfd():
    # Some Python distributions omit os.memfd_create despite libc/kernel support.
    # Resolve the libc function by name instead of encoding a syscall number.
    try:
        function = ctypes.CDLL(None, use_errno=True).memfd_create
    except AttributeError as error:
        raise ValueError('payload syscall filter requires memfd_create') from error
    function.argtypes, function.restype = [ctypes.c_char_p, ctypes.c_uint], ctypes.c_int
    descriptor = function(b'bokkie-payload-filter', 0x0001 | 0x0002)  # CLOEXEC | ALLOW_SEALING
    if descriptor < 0:
        raise ValueError('payload syscall filter could not create its descriptor')
    return descriptor


@contextmanager
def payload_filter_fd():
    """Yield a sealed, rewound BPF descriptor and close it on every exit path."""
    library = _library()
    architecture = library.seccomp_arch_resolve_name(b'x86_64')
    if (sys.platform != 'linux' or ctypes.sizeof(ctypes.c_void_p) != 8 or
            architecture == 0 or library.seccomp_arch_native() != architecture):
        raise ValueError('payload syscall filter requires the qualified Linux x86-64 native ABI')
    # seccomp_init includes only the native architecture. Do not add compatibility
    # ABIs: libseccomp's bad-architecture branch also excludes x32 syscall numbers.
    context = library.seccomp_init(_ALLOW)
    if not context:
        raise ValueError('payload syscall filter could not allocate its context')
    descriptor = None
    try:
        _check(library.seccomp_attr_set(context, _ACT_BADARCH, _KILL_PROCESS),
               'reject unsupported ABIs')
        for name in DENIED_SYSCALLS:
            _check(library.seccomp_rule_add_exact_array(
                context, _ERRNO | errno.EPERM, _syscall(library, name), 0, None),
                'restrict ' + name)
        clone = _syscall(library, 'clone')
        for flag in NAMESPACE_FLAGS.values():
            comparison = _Comparison(0, _MASKED_EQ, flag, flag)
            _check(library.seccomp_rule_add_exact_array(
                context, _ERRNO | errno.EPERM, clone, 1, ctypes.byref(comparison)),
                'restrict namespace clone')
        # clone3 puts flags behind a pointer, which classic seccomp cannot inspect.
        # ENOSYS permits libc's ordinary process/thread fallback to legacy clone.
        _check(library.seccomp_rule_add_exact_array(
            context, _ERRNO | errno.ENOSYS, _syscall(library, 'clone3'), 0, None),
            'restrict clone3')
        descriptor = _memfd()
        _check(library.seccomp_export_bpf(context, descriptor), 'export BPF')
        size = os.fstat(descriptor).st_size
        if not size or size % 8 or size > 4096 * 8:
            raise ValueError('payload syscall filter exported invalid BPF')
        fcntl.fcntl(descriptor, _F_ADD_SEALS, _ALL_SEALS)
        os.lseek(descriptor, 0, os.SEEK_SET)
        yield descriptor
    finally:
        if descriptor is not None:
            os.close(descriptor)
        library.seccomp_release(context)
