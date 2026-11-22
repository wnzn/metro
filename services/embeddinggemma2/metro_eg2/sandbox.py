"""Run the native decoders in an isolated, short-lived worker process.

The worker is a fresh ``python -I`` interpreter (``subprocess``, not ``multiprocessing``) with:

* an **allowlisted environment** built from scratch (no parent variables: no key file path,
  no HF settings, no HOME, no proxies),
* no inherited file descriptors besides stdin/stdout (stderr goes to /dev/null, never logged),
* its own session/process group, and a private ``0700`` working directory that is removed
  afterwards,
* rlimits plus Landlock and seccomp applied by the worker itself before it touches the asset
  (:mod:`metro_eg2.isolation`). If isolation is unavailable the worker refuses and the request
  fails closed with 503.

Results come back over the validated framing in :mod:`metro_eg2.wire` (never pickle). On
deadline, cancellation (client disconnect) or any error the whole process group is SIGKILLed and
reaped **before** this generator returns, so the caller's concurrency slot never outlives native
decoder work.
"""

from __future__ import annotations

import dataclasses
import json
import os
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
from collections.abc import Iterator
from dataclasses import dataclass

from .decode import AssetError, Segment, decode_asset
from .limits import Limits
from .wire import Cancelled, DecodeTimeout, FrameReader, IsolationFailed, WireError, parse_message

__all__ = ["Done", "DecodeTimeout", "Cancelled", "IsolationFailed", "decode_in_sandbox", "decode_inline", "child_env", "live_workers"]

_PKG_PARENT = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
_live: set[int] = set()
_live_lock = threading.Lock()


@dataclass
class Done:
    partial: bool
    notes: list[str]


def live_workers() -> set[int]:
    """PIDs of decoder workers that have been started and not yet reaped."""
    with _live_lock:
        return set(_live)


def child_env(workdir: str) -> dict[str, str]:
    """The worker's complete environment. Nothing from the parent environment is copied."""
    return {
        "PATH": "/usr/bin:/bin",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "HOME": workdir,
        "TMPDIR": workdir,
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONNOUSERSITE": "1",
        "OPENBLAS_NUM_THREADS": "1",
        "OMP_NUM_THREADS": "1",
        "MKL_NUM_THREADS": "1",
        "MALLOC_ARENA_MAX": "2",
    }


def _bootstrap() -> str:
    return f"import sys; sys.path.insert(0, {_PKG_PARENT!r}); from metro_eg2.decoder_worker import main; main()"


def decode_in_sandbox(
    data: bytes, mime_type: str, limits: Limits, deadline: float, cancel: threading.Event | None = None
) -> Iterator[Segment | Done]:
    """Yield validated ``Segment`` objects, then one ``Done``.

    Raises ``AssetError`` (client error), ``DecodeTimeout``, ``Cancelled`` or ``IsolationFailed``.
    """
    base = os.environ.get("EG2_DECODE_TMPDIR") or None
    workdir = tempfile.mkdtemp(prefix="eg2-decode-", dir=base)
    os.chmod(workdir, 0o700)
    request = json.dumps({"mime_type": mime_type, "limits": dataclasses.asdict(limits)}).encode()
    proc = None
    writer = None
    try:
        proc = subprocess.Popen(
            [sys.executable, "-I", "-c", _bootstrap()],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            cwd=workdir,
            env=child_env(workdir),
            close_fds=True,
            start_new_session=True,
        )
        with _live_lock:
            _live.add(proc.pid)

        def feed():
            try:
                proc.stdin.write(struct.pack("<I", len(request)) + request + struct.pack("<Q", len(data)))
                proc.stdin.write(data)
            except (BrokenPipeError, ValueError, OSError):
                pass
            finally:
                try:
                    proc.stdin.close()
                except OSError:
                    pass

        writer = threading.Thread(target=feed, name="eg2-feed", daemon=True)
        writer.start()
        reader = FrameReader(proc.stdout.fileno(), deadline, cancel)
        count = 0
        while True:
            try:
                head, blob = reader.next()
            except EOFError:
                raise AssetError(422, "asset could not be decoded (decoder exited)") from None
            except WireError:
                raise AssetError(422, "asset could not be decoded (protocol error)") from None
            try:
                msg = parse_message(head, blob, limits)
            except WireError:
                raise AssetError(422, "asset could not be decoded (protocol error)") from None
            if isinstance(msg, Segment):
                count += 1
                if count > limits.max_segments:
                    raise AssetError(422, "asset could not be decoded (too many segments)")
                yield msg
            else:
                _, partial, notes = msg
                yield Done(partial, notes)
                return
    finally:
        if proc is not None:
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
            proc.wait()  # reap before the caller may release its slot
            for stream in (proc.stdout, proc.stdin):
                try:
                    if stream:
                        stream.close()
                except OSError:
                    pass
            with _live_lock:
                _live.discard(proc.pid)
        if writer is not None:
            writer.join(5)
        shutil.rmtree(workdir, ignore_errors=True)


def decode_inline(
    data: bytes, mime_type: str, limits: Limits, deadline: float, cancel: threading.Event | None = None
) -> Iterator[Segment | Done]:
    """In-process decoding with NO isolation. Unit tests of parser behaviour only; never served."""
    result = decode_asset(data, mime_type, limits)
    yield from result.segments
    yield Done(result.partial, result.notes)
