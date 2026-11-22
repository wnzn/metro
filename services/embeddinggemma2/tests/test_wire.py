"""The parent validates every worker message; malformed or oversized output is rejected."""

from __future__ import annotations

import numpy as np
import pytest

from metro_eg2.decode import AssetError, Segment
from metro_eg2.limits import DEFAULT_LIMITS as L
from metro_eg2.wire import IsolationFailed, WireError, parse_message


def img_head(shape, dtype="u1"):
    return {"t": "seg", "modality": "image", "locator": {"kind": "image"}, "excerpt": None, "dtype": dtype, "shape": shape}


def test_valid_image_roundtrip():
    arr = np.zeros((4, 5, 3), np.uint8)
    seg = parse_message(img_head([4, 5, 3]), arr.tobytes(), L)
    assert isinstance(seg, Segment) and seg.payload.shape == (4, 5, 3)


@pytest.mark.parametrize(
    "head,blob",
    [
        (img_head([4, 5, 3]), b"\0" * 59),  # size mismatch
        (img_head([4, 5, 4]), b"\0" * 80),  # bad channels
        (img_head([100000, 5, 3]), b""),  # too large
        (img_head([4, 5, 3], dtype="<f8"), b"\0" * 480),  # dtype not allowed
        ({"t": "seg", "modality": "text", "locator": {"kind": "text", "start": 0, "end": 1}, "text": "x" * 5000}, b""),
        ({"t": "seg", "modality": "text", "locator": {"kind": "text", "start": 5, "end": 1}, "text": "x"}, b""),
        ({"t": "seg", "modality": "image", "locator": {"kind": "image", "path": "/etc"}, "dtype": "u1", "shape": [1, 1, 3]}, b"\0\0\0"),
        ({"t": "seg", "modality": "image", "locator": {"kind": "image"}, "excerpt": "caption", "dtype": "u1", "shape": [1, 1, 3]}, b"\0\0\0"),
        ({"t": "seg", "modality": "audio", "locator": {"kind": "time", "start_ms": 0, "end_ms": 1}, "dtype": "<f4", "shape": [10**7]}, b""),
        ({"t": "seg", "modality": "shell", "locator": {"kind": "text"}}, b""),
        ({"t": "error", "status": 200, "message": "x"}, b""),
        ({"t": "done", "partial": "yes", "notes": []}, b""),
        ({"t": "__reduce__"}, b""),
    ],
)
def test_rejects_bad_messages(head, blob):
    with pytest.raises(WireError):
        parse_message(head, blob, L)


def test_error_and_isolation_messages():
    with pytest.raises(AssetError) as e:
        parse_message({"t": "error", "status": 415, "message": "nope"}, b"", L)
    assert e.value.status == 415
    with pytest.raises(IsolationFailed):
        parse_message({"t": "error", "kind": "isolation", "message": "no landlock"}, b"", L)


def test_audio_is_sanitised():
    arr = np.array([np.nan, 5.0, -5.0, 0.5], np.float32)
    seg = parse_message({"t": "seg", "modality": "audio", "locator": {"kind": "time", "start_ms": 0, "end_ms": 1}, "dtype": "<f4", "shape": [4]}, arr.tobytes(), L)
    assert seg.payload.tolist() == [0.0, 1.0, -1.0, 0.5]
