"""TEST-ONLY probe run inside a worker-like process (same launcher, env and isolation as the
decoder worker). It applies the real ``metro_eg2.isolation.apply`` and then attempts a fixed
list of *benign* actions against sentinels the test created (read a sentinel file, open a TCP
connection to a test listener on loopback, signal the parent with signal 0, ...). It prints
which were allowed. It contains no exploit and never targets anything the test didn't create.

argv: <sibling_dir> <sentinel_file> <parent_pid> <tcp_port>
"""

from __future__ import annotations

import ctypes
import json
import os
import sys


def attempt(fn):
    try:
        fn()
        return "allowed"
    except PermissionError:
        return "denied"
    except OSError as exc:
        return f"denied:{exc.errno}"


def main() -> None:
    sibling, sentinel, ppid, port = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
    from metro_eg2 import isolation

    info = isolation.apply(os.getcwd())
    libc = ctypes.CDLL(None, use_errno=True)

    def errno_call(res):
        if res == -1:
            raise OSError(ctypes.get_errno(), os.strerror(ctypes.get_errno()))

    def tcp():
        import socket

        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(("127.0.0.1", port))

    def unix():
        import socket

        socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)

    def ptrace():
        libc.ptrace.restype = ctypes.c_long
        errno_call(libc.ptrace(ctypes.c_long(16), ctypes.c_long(ppid), None, None))  # PTRACE_ATTACH

    def vm_read():
        class Iov(ctypes.Structure):
            _fields_ = [("base", ctypes.c_void_p), ("len", ctypes.c_size_t)]

        buf = ctypes.create_string_buffer(8)
        local = Iov(ctypes.cast(buf, ctypes.c_void_p), 8)
        remote = Iov(ctypes.c_void_p(0x400000), 8)
        libc.process_vm_readv.restype = ctypes.c_ssize_t
        errno_call(libc.process_vm_readv(ppid, ctypes.byref(local), 1, ctypes.byref(remote), 1, 0))

    def thread():
        import threading

        t = threading.Thread(target=lambda: None)
        t.start()
        t.join()

    def write_own():
        with open(os.path.join(os.getcwd(), "own.txt"), "w") as fh:
            fh.write("ok")

    def numpy_import():
        import numpy  # noqa: F401  (runtime read-only access must still work)

    results = {
        "read_sentinel": attempt(lambda: open(sentinel).read()),
        "list_sibling_dir": attempt(lambda: os.listdir(sibling)),
        "write_sibling_dir": attempt(lambda: open(os.path.join(sibling, "x"), "w").close()),
        "chmod_sentinel": attempt(lambda: os.chmod(sentinel, 0o644)),
        "read_parent_environ": attempt(lambda: open(f"/proc/{ppid}/environ", "rb").read()),
        "read_etc_hostname": attempt(lambda: open("/etc/hostname").read()),
        "tcp_connect_loopback": attempt(tcp),
        "unix_socket": attempt(unix),
        "signal_parent": attempt(lambda: os.kill(ppid, 0)),
        "ptrace_parent": attempt(ptrace),
        "process_vm_readv_parent": attempt(vm_read),
        "fork": attempt(lambda: os._exit(0) if os.fork() == 0 else None),
        "exec": attempt(lambda: os.execv("/bin/true", ["/bin/true"])),
        # positive controls: the worker must still be able to do its job
        "signal_self": attempt(lambda: os.kill(os.getpid(), 0)),
        "thread": attempt(thread),
        "write_own_workdir": attempt(write_own),
        "import_numpy": attempt(numpy_import),
        "env_keys": sorted(os.environ),
        "landlock_abi": info["landlock_abi"],
    }
    print(json.dumps(results))


if __name__ == "__main__":
    main()
