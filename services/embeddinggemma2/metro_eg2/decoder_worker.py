"""Decoder worker process entry point. Started by :mod:`metro_eg2.sandbox`, never by hand.

Order matters: rlimits, then kernel isolation (while still single-threaded and before any
native decoder library is imported), then read the request from stdin, decode, and stream
validated-shape frames to stdout. If isolation cannot be applied, the worker reports that and
exits without reading or decoding anything.
"""

from __future__ import annotations

import json
import os
import resource
import struct
import sys


def _write(fd: int, data: bytes) -> None:
    view = memoryview(data)
    while view:
        n = os.write(fd, view)
        view = view[n:]


def _read_exact(fd: int, n: int) -> bytes:
    out = bytearray()
    while len(out) < n:
        chunk = os.read(fd, min(n - len(out), 1 << 20))
        if not chunk:
            raise EOFError()
        out += chunk
    return bytes(out)


def frame(header: dict, blob: bytes = b"") -> bytes:
    # Same format as wire.frame; duplicated so nothing heavy (numpy) loads before isolation.
    h = json.dumps(header, separators=(",", ":"), ensure_ascii=True).encode()
    return struct.pack("<I", len(h)) + h + struct.pack("<Q", len(blob)) + blob


def main() -> None:
    from . import isolation

    out = sys.stdout.fileno()
    inp = sys.stdin.fileno()
    workdir = os.getcwd()
    try:
        (hlen,) = struct.unpack("<I", _read_exact(inp, 4))
        req = json.loads(_read_exact(inp, hlen))
        lim = req["limits"]
        resource.setrlimit(resource.RLIMIT_AS, (lim["decode_mem_bytes"], lim["decode_mem_bytes"]))
        resource.setrlimit(resource.RLIMIT_CPU, (lim["decode_cpu_s"], lim["decode_cpu_s"]))
        resource.setrlimit(resource.RLIMIT_FSIZE, (0, 0))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_NOFILE, (64, 64))
        isolation.apply(workdir)
    except isolation.IsolationUnavailable as exc:
        _write(out, frame({"t": "error", "kind": "isolation", "message": str(exc)[:300]}))
        os._exit(3)
    except Exception as exc:  # malformed request from the parent: nothing to decode
        _write(out, frame({"t": "error", "kind": "isolation", "message": f"worker setup failed ({type(exc).__name__})"}))
        os._exit(3)

    # Only now load the decoders (FFmpeg, pdfium, Pillow, numpy) and touch the untrusted bytes.
    from .decode import AssetError, decode_asset
    from .limits import Limits
    from .wire import segment_frame

    try:
        limits = Limits(**lim)
        (dlen,) = struct.unpack("<Q", _read_exact(inp, 8))
        if dlen > limits.max_decoded_bytes:
            raise AssetError(413, "asset too large")
        data = _read_exact(inp, dlen)
        result = decode_asset(data, req["mime_type"], limits)
        del data
        for seg in result.segments:
            _write(out, segment_frame(seg))
        _write(out, frame({"t": "done", "partial": bool(result.partial), "notes": [n[:200] for n in result.notes[:16]]}))
    except AssetError as exc:
        _write(out, frame({"t": "error", "status": exc.status, "message": exc.message[:300]}))
    except MemoryError:
        _write(out, frame({"t": "error", "status": 413, "message": "decoding the asset exceeded the memory limit"}))
    except Exception as exc:  # never leak internals; type name only
        _write(out, frame({"t": "error", "status": 422, "message": f"asset could not be decoded ({type(exc).__name__})"}))
    os._exit(0)


if __name__ == "__main__":
    main()
