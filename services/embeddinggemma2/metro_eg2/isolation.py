"""Kernel-level isolation for the untrusted-input decoder process (Linux only, fail closed).

``apply(workdir)`` is called by the decoder worker as its *first* action, while it is still
single-threaded and before any decoder library (FFmpeg, pdfium, libpng/libjpeg, numpy) is
imported. Everything it sets is inherited by threads created later and cannot be undone:

1. ``PR_SET_DUMPABLE=0`` and ``PR_SET_NO_NEW_PRIVS=1``.
2. **Landlock** (ABI >= 3 required; this module refuses to run on less):
   * filesystem: read/execute only beneath the Python runtime directories, the ``metro_eg2``
     package directory and system library directories; read/write only beneath the worker's own
     private temporary directory; ``/dev/null`` and ``/dev/urandom``. Everything else, including
     ``/proc`` (so other processes' ``environ``/``mem``), home directories, mounted secrets and
     sibling job directories, is denied by the kernel.
   * network (ABI >= 4): TCP bind/connect denied.
   * scopes (ABI >= 6): signals and abstract UNIX sockets outside the domain denied.
3. **seccomp-BPF** (installed with ``SECCOMP_FILTER_FLAG_TSYNC``): wrong architecture or the
   x32 ABI kills the process. ``EPERM`` for sockets of every family, ptrace,
   ``process_vm_*``, ``kcmp``, pidfd handle/signal syscalls, ``execve``/``execveat``,
   ``fork``/``vfork``, ``clone`` without ``CLONE_THREAD`` (``clone3`` -> ``ENOSYS`` so libc
   falls back to ``clone``), signals to any pid other than itself, mount/namespace/keyring/
   bpf/perf/userfaultfd/io_uring/module/kexec syscalls, and chmod/chown/utime/xattr changes
   (which Landlock does not cover).

This is a deny-list seccomp filter layered on an allow-list Landlock ruleset. It is not a
general sandbox for arbitrary code: syscalls added to future kernels are allowed unless listed.
"""

from __future__ import annotations

import ctypes
import errno
import os
import platform
import struct
import sys

MIN_LANDLOCK_ABI = 3


class IsolationUnavailable(RuntimeError):
    """The kernel or runtime cannot provide the required isolation. Never decode then."""


_libc = ctypes.CDLL(None, use_errno=True)
_libc.syscall.restype = ctypes.c_long
_libc.prctl.restype = ctypes.c_int

PR_SET_DUMPABLE = 4
PR_SET_NO_NEW_PRIVS = 38
PR_GET_NO_NEW_PRIVS = 39

# (x86_64, generic/aarch64); None = not present on that architecture.
_SYS = {
    "landlock_create_ruleset": (444, 444),
    "landlock_add_rule": (445, 445),
    "landlock_restrict_self": (446, 446),
    "seccomp": (317, 277),
    "clone": (56, 220),
    "clone3": (435, 435),
    "kill": (62, 129),
    "tgkill": (234, 131),
    "rt_sigqueueinfo": (129, 138),
    "rt_tgsigqueueinfo": (297, 240),
}
_DENY = {
    # networking: every socket family (the worker talks to its parent over stdin/stdout pipes)
    "socket": (41, 198), "socketpair": (53, 199), "connect": (42, 203), "bind": (49, 200),
    "listen": (50, 201), "accept": (43, 202), "accept4": (288, 242), "sendto": (44, 206),
    "sendmsg": (46, 211), "sendmmsg": (307, 269), "recvfrom": (45, 207), "recvmsg": (47, 212),
    "recvmmsg": (299, 243),
    # other processes
    "ptrace": (101, 117), "process_vm_readv": (310, 270), "process_vm_writev": (311, 271),
    "kcmp": (312, 272), "pidfd_open": (434, 434), "pidfd_getfd": (438, 438),
    "pidfd_send_signal": (424, 424), "tkill": (200, 130),
    # new programs / processes
    "execve": (59, 221), "execveat": (322, 281), "fork": (57, None), "vfork": (58, None),
    # mounts, namespaces, kernel facilities
    "mount": (165, 40), "umount2": (166, 39), "pivot_root": (155, 41), "chroot": (161, 51),
    "unshare": (272, 97), "setns": (308, 268), "move_mount": (429, 429), "open_tree": (428, 428),
    "fsopen": (430, 430), "fsmount": (432, 432), "fspick": (433, 433), "mount_setattr": (442, 442),
    "bpf": (321, 280), "perf_event_open": (298, 241), "userfaultfd": (323, 282),
    "keyctl": (250, 219), "add_key": (248, 217), "request_key": (249, 218),
    "init_module": (175, 105), "finit_module": (313, 273), "delete_module": (176, 106),
    "kexec_load": (246, 104), "kexec_file_load": (320, 294), "reboot": (169, 142),
    "swapon": (167, 224), "swapoff": (168, 225), "open_by_handle_at": (304, 265),
    "name_to_handle_at": (303, 264), "io_uring_setup": (425, 425), "io_uring_enter": (426, 426),
    "io_uring_register": (427, 427), "personality": (135, 92), "acct": (163, 89),
    "quotactl": (179, 60), "syslog": (103, 116), "fanotify_init": (300, 262),
    "mknod": (133, None), "mknodat": (259, 33), "iopl": (172, None), "ioperm": (173, None),
    # metadata changes Landlock does not mediate
    "chmod": (90, None), "fchmod": (91, 52), "fchmodat": (268, 53), "fchmodat2": (452, 452),
    "chown": (92, None), "fchown": (93, 55), "lchown": (94, None), "fchownat": (260, 54),
    "utime": (132, None), "utimes": (235, None), "utimensat": (280, 88), "futimesat": (261, None),
    "setxattr": (188, 5), "lsetxattr": (189, 6), "fsetxattr": (190, 7), "removexattr": (197, 14),
    "lremovexattr": (198, 15), "fremovexattr": (199, 16), "setxattrat": (463, 463),
    "removexattrat": (464, 464), "file_setattr": (469, 469),
}

_ARCH = {"x86_64": (0, 0xC000003E), "amd64": (0, 0xC000003E), "aarch64": (1, 0xC00000B7), "arm64": (1, 0xC00000B7)}


def _arch():
    m = platform.machine().lower()
    if m not in _ARCH or sys.byteorder != "little":
        raise IsolationUnavailable(f"unsupported architecture {m!r} (x86_64 and aarch64 only)")
    return _ARCH[m]


def _nr(name: str, idx: int) -> int:
    n = _SYS[name][idx]
    if n is None:
        raise IsolationUnavailable(f"no syscall {name}")
    return n


def _sys(nr: int, *args) -> int:
    ctypes.set_errno(0)
    res = _libc.syscall(ctypes.c_long(nr), *args)
    return int(res)


def _prctl(option: int, arg: int) -> None:
    if _libc.prctl(option, ctypes.c_ulong(arg), ctypes.c_ulong(0), ctypes.c_ulong(0), ctypes.c_ulong(0)) != 0:
        e = ctypes.get_errno()
        raise IsolationUnavailable(f"prctl({option}) failed: {os.strerror(e)}")


def set_nondumpable() -> None:
    """Make this process non-dumpable: same-uid processes cannot ptrace it or read /proc/<pid>/mem|environ."""
    _prctl(PR_SET_DUMPABLE, 0)


# ----------------------------------------------------------------------------- Landlock

_FS_EXECUTE = 1 << 0
_FS_WRITE_FILE = 1 << 1
_FS_READ_FILE = 1 << 2
_FS_READ_DIR = 1 << 3
_FS_TRUNCATE = 1 << 14
_FS_IOCTL_DEV = 1 << 15
_FILE_RIGHTS = _FS_EXECUTE | _FS_WRITE_FILE | _FS_READ_FILE | _FS_TRUNCATE | _FS_IOCTL_DEV
_NET_BIND_TCP, _NET_CONNECT_TCP = 1 << 0, 1 << 1
_SCOPE_ABSTRACT_UNIX, _SCOPE_SIGNAL = 1 << 0, 1 << 1


def landlock_abi() -> int:
    v = _sys(_nr("landlock_create_ruleset", _arch()[0]), None, ctypes.c_size_t(0), ctypes.c_uint32(1))
    if v < 0:
        return 0
    return v


def _fs_handled(abi: int) -> int:
    bits = (1 << 13) - 1  # ABI 1
    if abi >= 2:
        bits |= 1 << 13  # REFER
    if abi >= 3:
        bits |= _FS_TRUNCATE
    if abi >= 5:
        bits |= _FS_IOCTL_DEV
    return bits


def runtime_read_paths() -> list[str]:
    """Read-only directories the worker needs: the interpreter, its packages, system libraries."""
    here = os.path.dirname(os.path.realpath(__file__))
    cands = {sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix, here}
    cands |= {"/usr/lib", "/usr/lib64", "/lib", "/lib64", "/usr/local/lib", "/etc/ld.so.cache"}
    out = []
    for p in sorted(cands):
        rp = os.path.realpath(p)
        if os.path.exists(rp):
            out.append(rp)
    # Drop entries already covered by a parent entry.
    dirs = [p for p in out if os.path.isdir(p)]
    return [p for p in out if not any(p != d and p.startswith(d.rstrip("/") + "/") for d in dirs)]


def _landlock(workdir: str, read_paths: list[str]) -> int:
    idx = _arch()[0]
    abi = landlock_abi()
    if abi < MIN_LANDLOCK_ABI:
        raise IsolationUnavailable(f"Landlock ABI {abi} < required {MIN_LANDLOCK_ABI} (Linux >= 6.2 with landlock in the LSM list)")
    fs = _fs_handled(abi)
    net = (_NET_BIND_TCP | _NET_CONNECT_TCP) if abi >= 4 else 0
    scoped = (_SCOPE_ABSTRACT_UNIX | _SCOPE_SIGNAL) if abi >= 6 else 0
    if abi >= 6:
        attr = struct.pack("<QQQ", fs, net, scoped)
    elif abi >= 4:
        attr = struct.pack("<QQ", fs, net)
    else:
        attr = struct.pack("<Q", fs)
    buf = ctypes.create_string_buffer(attr, len(attr))
    rfd = _sys(_nr("landlock_create_ruleset", idx), buf, ctypes.c_size_t(len(attr)), ctypes.c_uint32(0))
    if rfd < 0:
        raise IsolationUnavailable(f"landlock_create_ruleset: {os.strerror(ctypes.get_errno())}")
    try:
        read_rights = _FS_EXECUTE | _FS_READ_FILE | _FS_READ_DIR
        rules: list[tuple[str, int]] = [(p, read_rights) for p in read_paths]
        rules.append((workdir, fs))
        rules.append(("/dev/null", _FS_READ_FILE | _FS_WRITE_FILE))
        rules.append(("/dev/urandom", _FS_READ_FILE))
        for path, rights in rules:
            try:
                pfd = os.open(path, os.O_PATH | os.O_CLOEXEC)
            except FileNotFoundError:
                continue
            try:
                if not os.path.isdir(path):
                    rights &= _FILE_RIGHTS
                rights &= fs
                rule = ctypes.create_string_buffer(struct.pack("<Qi", rights, pfd), 12)
                if _sys(_nr("landlock_add_rule", idx), ctypes.c_int(rfd), ctypes.c_int(1), rule, ctypes.c_uint32(0)) != 0:
                    raise IsolationUnavailable(f"landlock_add_rule({path}): {os.strerror(ctypes.get_errno())}")
            finally:
                os.close(pfd)
        if _sys(_nr("landlock_restrict_self", idx), ctypes.c_int(rfd), ctypes.c_uint32(0)) != 0:
            raise IsolationUnavailable(f"landlock_restrict_self: {os.strerror(ctypes.get_errno())}")
    finally:
        os.close(rfd)
    return abi


# ----------------------------------------------------------------------------- seccomp

_LD_W_ABS = 0x20
_JEQ_K = 0x15
_JGE_K = 0x35
_JSET_K = 0x45
_RET_K = 0x06
_RET_ALLOW = 0x7FFF0000
_RET_KILL_PROCESS = 0x80000000
_RET_ERRNO = 0x00050000
_OFF_NR, _OFF_ARCH = 0, 4
_CLONE_THREAD = 0x00010000


def _ins(code, jt, jf, k):
    return struct.pack("<HBBI", code, jt, jf, k & 0xFFFFFFFF)


def _arg_lo(i):
    return 16 + 8 * i


def _arg_hi(i):
    return 20 + 8 * i


def build_seccomp_program(pid: int) -> bytes:
    idx, audit_arch = _arch()
    eperm = _RET_ERRNO | errno.EPERM
    p: list[bytes] = [
        _ins(_LD_W_ABS, 0, 0, _OFF_ARCH),
        _ins(_JEQ_K, 1, 0, audit_arch),
        _ins(_RET_K, 0, 0, _RET_KILL_PROCESS),
        _ins(_LD_W_ABS, 0, 0, _OFF_NR),
    ]
    if idx == 0:  # x86_64: refuse the x32 ABI entirely
        p += [_ins(_JGE_K, 0, 1, 0x40000000), _ins(_RET_K, 0, 0, _RET_KILL_PROCESS)]
    for nums in _DENY.values():
        n = nums[idx]
        if n is not None:
            p += [_ins(_JEQ_K, 0, 1, n), _ins(_RET_K, 0, 0, eperm)]
    # clone3's flags live in memory BPF cannot read: ENOSYS makes libc fall back to clone.
    p += [_ins(_JEQ_K, 0, 1, _nr("clone3", idx)), _ins(_RET_K, 0, 0, _RET_ERRNO | errno.ENOSYS)]
    # clone: threads only.
    p += [
        _ins(_JEQ_K, 0, 4, _nr("clone", idx)),
        _ins(_LD_W_ABS, 0, 0, _arg_lo(0)),
        _ins(_JSET_K, 0, 1, _CLONE_THREAD),
        _ins(_RET_K, 0, 0, _RET_ALLOW),
        _ins(_RET_K, 0, 0, eperm),
    ]
    # Signals: only to itself (arg0 == own pid, as a non-negative 64-bit value).
    for name in ("kill", "tgkill", "rt_sigqueueinfo", "rt_tgsigqueueinfo"):
        p += [
            _ins(_JEQ_K, 0, 6, _nr(name, idx)),
            _ins(_LD_W_ABS, 0, 0, _arg_hi(0)),
            _ins(_JEQ_K, 0, 3, 0),
            _ins(_LD_W_ABS, 0, 0, _arg_lo(0)),
            _ins(_JEQ_K, 0, 1, pid),
            _ins(_RET_K, 0, 0, _RET_ALLOW),
            _ins(_RET_K, 0, 0, eperm),
        ]
    p.append(_ins(_RET_K, 0, 0, _RET_ALLOW))
    if len(p) > 4096:
        raise IsolationUnavailable("seccomp program too long")
    return b"".join(p)


def _seccomp(pid: int) -> None:
    prog_bytes = build_seccomp_program(pid)
    n = len(prog_bytes) // 8
    filt = ctypes.create_string_buffer(prog_bytes, len(prog_bytes))

    class SockFprog(ctypes.Structure):
        _fields_ = [("len", ctypes.c_ushort), ("filter", ctypes.c_void_p)]

    fprog = SockFprog(n, ctypes.cast(filt, ctypes.c_void_p))
    SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC = 1, 1
    r = _sys(_nr("seccomp", _arch()[0]), ctypes.c_uint(SECCOMP_SET_MODE_FILTER), ctypes.c_uint(SECCOMP_FILTER_FLAG_TSYNC), ctypes.byref(fprog))
    if r != 0:
        raise IsolationUnavailable(f"seccomp filter install failed: {os.strerror(ctypes.get_errno()) if r < 0 else 'thread sync failed'}")


def _thread_count() -> int:
    try:
        return len(os.listdir("/proc/self/task"))
    except OSError as exc:
        raise IsolationUnavailable(f"cannot verify thread count: {exc}") from None


def apply(workdir: str, read_paths: list[str] | None = None) -> dict:
    """Lock this process down. Raises ``IsolationUnavailable``; callers must then refuse work."""
    if not sys.platform.startswith("linux"):
        raise IsolationUnavailable("decoder isolation requires Linux")
    if _thread_count() != 1:
        raise IsolationUnavailable("isolation must be applied while single-threaded")
    workdir = os.path.realpath(workdir)
    paths = runtime_read_paths() if read_paths is None else read_paths
    _prctl(PR_SET_DUMPABLE, 0)
    _prctl(PR_SET_NO_NEW_PRIVS, 1)
    abi = _landlock(workdir, paths)
    _seccomp(os.getpid())
    if _libc.prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1:
        raise IsolationUnavailable("no_new_privs not set")
    return {"landlock_abi": abi, "read_paths": paths, "workdir": workdir}
