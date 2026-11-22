"""Defensive isolation tests with benign sentinels: the decoder worker must not reach sibling
files, the network, the parent process or secrets, and must fail closed when isolation cannot
be applied. Linux only (the service refuses to decode elsewhere)."""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time

import pytest

from metro_eg2 import isolation
from metro_eg2.decode import AssetError
from metro_eg2.limits import DEFAULT_LIMITS
from metro_eg2.sandbox import IsolationFailed, _PKG_PARENT, child_env, decode_in_sandbox, live_workers

pytestmark = pytest.mark.skipif(not sys.platform.startswith("linux"), reason="Linux isolation")

if isolation.landlock_abi() < isolation.MIN_LANDLOCK_ABI:  # pragma: no cover - depends on host
    pytest.skip("host kernel lacks Landlock ABI >= 3; the service fails closed here", allow_module_level=True)

ROOT = os.path.dirname(_PKG_PARENT) if os.path.basename(_PKG_PARENT) == "site-packages" else _PKG_PARENT


def run_probe(tmp_path, extra_env=None):
    sibling = tmp_path / "sibling-job"
    sibling.mkdir()
    sentinel = sibling / "sentinel.txt"
    sentinel.write_text("benign sentinel")
    sentinel.chmod(0o600)
    work = tmp_path / "work"
    work.mkdir(mode=0o700)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(0.5)
    port = listener.getsockname()[1]
    env = child_env(str(work))
    if extra_env:
        env.update(extra_env)
    boot = f"import sys; sys.path.insert(0, {_PKG_PARENT!r}); sys.path.insert(0, {ROOT!r}); from tests.sandbox_probe import main; main()"
    proc = subprocess.run(
        [sys.executable, "-I", "-c", boot, str(sibling), str(sentinel), str(os.getpid()), str(port)],
        cwd=work, env=env, capture_output=True, text=True, timeout=60, start_new_session=True,
    )
    accepted = True
    try:
        listener.accept()
    except (TimeoutError, socket.timeout):
        accepted = False
    finally:
        listener.close()
    assert proc.returncode == 0, proc.stderr[-2000:]
    res = json.loads(proc.stdout.strip().splitlines()[-1])
    res["listener_accepted"] = accepted
    res["sentinel_mode"] = oct(sentinel.stat().st_mode & 0o777)
    return res


def test_worker_isolation_denies_sibling_files_network_and_parent(tmp_path, monkeypatch):
    monkeypatch.setenv("EG2_SECRET_SENTINEL", "parent-only-value")  # must not reach the worker
    r = run_probe(tmp_path)
    for key in (
        "read_sentinel", "list_sibling_dir", "write_sibling_dir", "chmod_sentinel", "read_parent_environ",
        "read_etc_hostname", "tcp_connect_loopback", "unix_socket", "signal_parent", "ptrace_parent",
        "process_vm_readv_parent", "fork", "exec",
    ):
        assert r[key].startswith("denied"), (key, r[key])
    assert r["listener_accepted"] is False
    assert r["sentinel_mode"] == "0o600"
    for key in ("signal_self", "thread", "write_own_workdir", "import_numpy"):
        assert r[key] == "allowed", (key, r[key])
    assert r["landlock_abi"] >= isolation.MIN_LANDLOCK_ABI


def test_worker_environment_is_allowlisted(tmp_path, monkeypatch):
    monkeypatch.setenv("EG2_API_KEY_FILE", "/run/secrets/should-not-leak")
    monkeypatch.setenv("HF_TOKEN", "hf_should_not_leak")
    env = child_env(str(tmp_path))
    assert set(env) == {
        "PATH", "LANG", "LC_ALL", "HOME", "TMPDIR", "PYTHONDONTWRITEBYTECODE", "PYTHONNOUSERSITE",
        "OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS", "MALLOC_ARENA_MAX",
    }
    assert env["HOME"] == env["TMPDIR"] == str(tmp_path)
    r = run_probe(tmp_path)
    assert "EG2_API_KEY_FILE" not in r["env_keys"] and "HF_TOKEN" not in r["env_keys"]
    assert "EG2_SECRET_SENTINEL" not in r["env_keys"]


def test_worker_fails_closed_when_isolation_cannot_apply(tmp_path, monkeypatch):
    """A second thread before isolation (as if a library had started one) must stop the worker
    before it reads or decodes the asset; the service turns that into IsolationFailed -> 503."""
    import metro_eg2.sandbox as sb

    boot = (
        "import sys, threading, time; "
        f"sys.path.insert(0, {_PKG_PARENT!r}); "
        "threading.Thread(target=time.sleep, args=(5,), daemon=True).start(); "
        "from metro_eg2.decoder_worker import main; main()"
    )
    monkeypatch.setattr(sb, "_bootstrap", lambda: boot)
    with pytest.raises(IsolationFailed) as e:
        list(decode_in_sandbox(b"hello", "text/plain", DEFAULT_LIMITS, time.monotonic() + 30))
    assert "single-threaded" in e.value.message
    assert live_workers() == set()


def test_parent_can_be_made_nondumpable():
    code = (
        "import ctypes; from metro_eg2.isolation import set_nondumpable; set_nondumpable(); "
        "print(ctypes.CDLL(None).prctl(3, 0, 0, 0, 0))"
    )
    out = subprocess.run([sys.executable, "-I", "-c", f"import sys; sys.path.insert(0, {_PKG_PARENT!r}); {code}"], capture_output=True, text=True, timeout=30)
    assert out.stdout.strip() == "0", out.stderr


def test_seccomp_program_is_well_formed():
    prog = isolation.build_seccomp_program(1234)
    assert len(prog) % 8 == 0 and 0 < len(prog) // 8 <= 4096


def test_timeout_kills_and_reaps_worker():
    from . import fixtures as fx

    data = fx.mp4(20.0, fps=10)
    t0 = time.monotonic()
    with pytest.raises(Exception) as e:
        list(decode_in_sandbox(data, "video/mp4", DEFAULT_LIMITS, time.monotonic() + 0.05))
    assert type(e.value).__name__ == "DecodeTimeout"
    assert time.monotonic() - t0 < 5
    assert live_workers() == set()


def test_cancel_kills_and_reaps_worker():
    from . import fixtures as fx

    cancel = threading.Event()
    cancel.set()
    with pytest.raises(Exception) as e:
        list(decode_in_sandbox(fx.mp4(20.0, fps=10), "video/mp4", DEFAULT_LIMITS, time.monotonic() + 30, cancel))
    assert type(e.value).__name__ == "Cancelled"
    assert live_workers() == set()


def test_worker_temp_dir_removed(tmp_path, monkeypatch):
    monkeypatch.setenv("EG2_DECODE_TMPDIR", str(tmp_path))
    list(decode_in_sandbox(b"hello world", "text/plain", DEFAULT_LIMITS, time.monotonic() + 30))
    assert list(tmp_path.iterdir()) == []


def test_worker_errors_are_client_errors_not_crashes():
    with pytest.raises(AssetError) as e:
        list(decode_in_sandbox(b"%PDF-1.4 garbage", "application/pdf", DEFAULT_LIMITS, time.monotonic() + 30))
    assert e.value.status == 422
