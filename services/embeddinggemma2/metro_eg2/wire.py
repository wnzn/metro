"""Framing between the service and its decoder worker.

The worker parses untrusted bytes, so the parent treats everything it sends back as untrusted
too: no pickle, only length-prefixed JSON headers plus raw little-endian array bytes, and every
field (type, dtype, shape, size, locator keys, string lengths, status codes) is validated
against the limits before anything reaches the encoder.

Frame: ``u32 header_len | header (UTF-8 JSON) | u64 blob_len | blob``.
"""

from __future__ import annotations

import json
import math
import os
import select
import struct
import threading
import time
from typing import Any

import numpy as np

from .decode import AssetError, Segment
from .limits import Limits

MAX_HEADER = 64 * 1024
_LOC_KEYS = {"kind", "start", "end", "page", "start_ms", "end_ms"}
_LOC_KINDS = {"text": {"text", "page"}, "image": {"image", "page"}, "audio": {"time"}, "video": {"time"}}
_CHILD_STATUSES = {413, 415, 422}


class WireError(Exception):
    """The worker sent something outside the protocol. Treated as a failed decode."""


class Cancelled(Exception):
    pass


class DecodeTimeout(Exception):
    pass


class IsolationFailed(Exception):
    def __init__(self, message: str):
        super().__init__(message)
        self.message = message


# ----------------------------------------------------------------------------- writing (worker side)


def frame(header: dict, blob: bytes = b"") -> bytes:
    h = json.dumps(header, separators=(",", ":"), ensure_ascii=True).encode()
    if len(h) > MAX_HEADER:
        raise ValueError("header too large")
    return struct.pack("<I", len(h)) + h + struct.pack("<Q", len(blob)) + blob


def segment_frame(seg: Segment) -> bytes:
    head: dict[str, Any] = {"t": "seg", "modality": seg.modality, "locator": seg.locator, "excerpt": seg.excerpt}
    if seg.modality == "text":
        head["text"] = seg.payload
        return frame(head)
    arr = np.ascontiguousarray(seg.payload)
    head["dtype"] = {np.dtype(np.uint8): "u1", np.dtype(np.float32): "<f4"}[arr.dtype]
    head["shape"] = list(arr.shape)
    return frame(head, arr.astype(arr.dtype.newbyteorder("<"), copy=False).tobytes())


# ----------------------------------------------------------------------------- reading (parent side)


class FrameReader:
    def __init__(self, fd: int, deadline: float, cancel: threading.Event | None):
        self.fd, self.deadline, self.cancel = fd, deadline, cancel
        self.buf = bytearray()

    def _fill(self, need: int) -> None:
        while len(self.buf) < need:
            if self.cancel is not None and self.cancel.is_set():
                raise Cancelled()
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                raise DecodeTimeout()
            r, _, _ = select.select([self.fd], [], [], min(remaining, 0.25))
            if not r:
                continue
            chunk = os.read(self.fd, min(4 * 1024 * 1024, max(need - len(self.buf), 65536)))
            if not chunk:
                raise EOFError()
            self.buf += chunk

    def read_exact(self, n: int) -> bytes:
        self._fill(n)
        out = bytes(self.buf[:n])
        del self.buf[:n]
        return out

    def next(self) -> tuple[dict, bytes]:
        (hlen,) = struct.unpack("<I", self.read_exact(4))
        if hlen == 0 or hlen > MAX_HEADER:
            raise WireError("bad header length")
        try:
            head = json.loads(self.read_exact(hlen).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise WireError("bad header") from None
        if not isinstance(head, dict):
            raise WireError("bad header")
        (blen,) = struct.unpack("<Q", self.read_exact(8))
        return head, self._blob(blen)

    def _blob(self, n: int) -> bytes:
        if n > 1 << 31:
            raise WireError("blob too large")
        return self.read_exact(n) if n else b""


def _int(v: Any, hi: int = 1 << 53) -> int:
    if type(v) is not int or not (0 <= v <= hi):
        raise WireError("bad integer")
    return v


def _short(v: Any, n: int) -> str:
    if not isinstance(v, str) or len(v) > n:
        raise WireError("bad string")
    return v


def _locator(modality: str, loc: Any) -> dict:
    if not isinstance(loc, dict) or not set(loc) <= _LOC_KEYS or loc.get("kind") not in _LOC_KINDS[modality]:
        raise WireError("bad locator")
    out = {"kind": loc["kind"]}
    for k in ("start", "end", "page", "start_ms", "end_ms"):
        if k in loc:
            out[k] = _int(loc[k])
    if ("start" in out) != ("end" in out) or ("start" in out and out["end"] < out["start"]):
        raise WireError("bad locator range")
    if ("start_ms" in out) != ("end_ms" in out) or ("start_ms" in out and out["end_ms"] < out["start_ms"]):
        raise WireError("bad locator time")
    if out["kind"] == "time" and "start_ms" not in out:
        raise WireError("time locator without times")
    if out["kind"] == "page" and "page" not in out:
        raise WireError("page locator without page")
    return out


def _array(head: dict, blob: bytes, dtype: str, shape_ok) -> np.ndarray:
    if head.get("dtype") != dtype:
        raise WireError("bad dtype")
    shape = head.get("shape")
    if not isinstance(shape, list) or not all(type(d) is int and d > 0 for d in shape) or not shape_ok(shape):
        raise WireError("bad shape")
    itemsize = 1 if dtype == "u1" else 4
    if len(blob) != math.prod(shape) * itemsize:
        raise WireError("blob size mismatch")
    return np.frombuffer(blob, dtype=np.dtype(dtype)).reshape(shape).copy()


def parse_message(head: dict, blob: bytes, limits: Limits) -> Segment | tuple:
    """Validate one worker message. Returns a Segment, ("done", partial, notes), or raises."""
    t = head.get("t")
    if t == "seg":
        mod = head.get("modality")
        if mod not in _LOC_KINDS:
            raise WireError("bad modality")
        loc = _locator(mod, head.get("locator"))
        excerpt = head.get("excerpt")
        if excerpt is not None:
            if mod != "text":
                raise WireError("excerpt on non-text segment")
            excerpt = _short(excerpt, limits.excerpt_chars)
        if mod == "text":
            if blob:
                raise WireError("text segment with blob")
            text = _short(head.get("text"), limits.text_chunk_chars)
            if not text.strip():
                raise WireError("empty text")
            return Segment("text", text, loc, excerpt)
        if mod == "image":
            side = max(limits.image_downscale_side, limits.pdf_render_max_side)
            arr = _array(head, blob, "u1", lambda s: len(s) == 3 and s[2] == 3 and s[0] <= side and s[1] <= side)
            return Segment("image", arr, loc)
        if mod == "audio":
            n = limits.audio_rate * limits.audio_segment_ms // 1000
            arr = _array(head, blob, "<f4", lambda s: len(s) == 1 and s[0] <= n)
            return Segment("audio", np.nan_to_num(arr, nan=0.0, posinf=0.0, neginf=0.0).clip(-1.0, 1.0), loc)
        frames = max(1, int(round(limits.video_segment_ms / 1000 * limits.video_fps)))
        side = limits.video_frame_max_side
        arr = _array(head, blob, "u1", lambda s: len(s) == 4 and s[0] <= min(frames + 1, 32) and s[1] <= side and s[2] <= side and s[3] == 3)
        return Segment("video", arr, loc)
    if t == "done":
        partial, notes = head.get("partial"), head.get("notes")
        if type(partial) is not bool or not isinstance(notes, list) or len(notes) > 16:
            raise WireError("bad done")
        return ("done", partial, [_short(n, 200) for n in notes])
    if t == "error":
        msg = _short(head.get("message"), 300)
        msg = "".join(ch for ch in msg if ch.isprintable())
        if head.get("kind") == "isolation":
            raise IsolationFailed(msg)
        status = head.get("status")
        if status not in _CHILD_STATUSES:
            raise WireError("bad status")
        raise AssetError(status, msg)
    raise WireError("unknown message")
