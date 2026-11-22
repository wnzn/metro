"""API contract tests with the TEST-ONLY FakeEncoder (no weights, no semantics).

Most tests go through the real sandboxed child-process decoder.
"""

from __future__ import annotations

import base64
import dataclasses

import numpy as np
import pytest
from fastapi.testclient import TestClient

from metro_eg2.app import create_app
from metro_eg2.limits import DEFAULT_LIMITS
from metro_eg2.sandbox import decode_inline

from . import fixtures as fx
from .fake_encoder import FakeEncoder

MODEL = "google/embeddinggemma-2"


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode()


@pytest.fixture
def enc():
    return FakeEncoder()


@pytest.fixture
def client(enc):
    return TestClient(create_app(enc))


def media(client, **body):
    return client.post("/v1/media/embeddings", json={"model": MODEL, **body})


def asset(data, mime, name="file.bin"):
    return {"name": name, "mime_type": mime, "data_base64": b64(data)}


def test_healthz_and_models(client):
    assert client.get("/healthz").json() == {"status": "ok", "model": MODEL}
    m = client.get("/v1/models").json()
    assert m["object"] == "list" and m["data"][0]["id"] == MODEL
    assert m["data"][0]["metro"]["dimensions"] == 768


def test_query_uses_query_kind_and_is_unit_768(client, enc):
    r = media(client, query="red circle")
    assert r.status_code == 200
    body = r.json()
    assert body["partial"] is False and body["model"] == MODEL
    (item,) = body["data"]
    assert item["index"] == 0 and item["locator"] == {"kind": "query"} and "excerpt" not in item
    v = np.array(item["embedding"])
    assert v.shape == (768,) and abs(np.linalg.norm(v) - 1) < 1e-5
    assert enc.calls == [("query", "red circle")]  # prefix is the encoder's job, applied once there


def test_query_that_looks_like_a_url_or_path_stays_text(client, enc):
    for q in ("https://example.invalid/cat.png", "/etc/hostname.png", "file:///etc/passwd"):
        assert media(client, query=q).status_code == 200
    assert [k for k, _ in enc.calls] == ["query"] * 3
    assert [p for _, p in enc.calls] == ["https://example.invalid/cat.png", "/etc/hostname.png", "file:///etc/passwd"]


def test_dimensions_mrl(client):
    v = np.array(media(client, query="x", dimensions=256).json()["data"][0]["embedding"])
    assert v.shape == (256,) and abs(np.linalg.norm(v) - 1) < 1e-5
    assert media(client, query="x", dimensions=300).status_code == 422


def test_exactly_one_of_query_or_asset(client):
    assert media(client).status_code == 422
    assert media(client, query="a", asset=asset(b"hi", "text/plain")).status_code == 422


def test_wrong_model_404(client):
    r = client.post("/v1/media/embeddings", json={"model": "google/embeddinggemma-300m", "query": "a"})
    assert r.status_code == 404


def test_unknown_fields_rejected_and_not_echoed(client):
    r = client.post("/v1/media/embeddings", json={"model": MODEL, "query": "a", "url": "https://example.invalid/x.mp4"})
    assert r.status_code == 422
    assert "example.invalid" not in r.text


def test_asset_url_field_is_not_accepted(client):
    a = asset(fx.png(), "image/png")
    a["url"] = "https://example.invalid/x.png"
    assert media(client, asset=a).status_code == 422


def test_text_asset_chunks_are_documents_with_excerpts(client, enc):
    text = "Solar panels on the roof.\n\n" + "More details. " * 200
    body = media(client, asset=asset(text.encode(), "text/plain", "notes.txt")).json()
    assert body["partial"] is False and len(body["data"]) >= 2
    for i, item in enumerate(body["data"]):
        assert item["index"] == i and item["modality"] == "text"
        loc = item["locator"]
        assert loc["kind"] == "text" and text[loc["start"] : loc["end"]] == enc.calls[i][1]
        assert item["excerpt"]
    assert all(k == "document" for k, _ in enc.calls)


def test_image_asset_is_pixels_not_name(client, enc):
    r = media(client, asset=asset(fx.png(32, 32, (0, 255, 0)), "image/png", "a-red-circle.png"))
    item = r.json()["data"][0]
    assert item["locator"] == {"kind": "image"} and "excerpt" not in item and item["modality"] == "image"
    kind, payload = enc.calls[0]
    assert kind == "image" and isinstance(payload, np.ndarray) and tuple(payload[0, 0]) == (0, 255, 0)
    # The filename never reaches the encoder.
    r2 = media(client, asset=asset(fx.png(32, 32, (0, 255, 0)), "image/png", "something-else.png"))
    assert r2.json()["data"][0]["embedding"] == item["embedding"]


def test_pdf_mixed_pages(client):
    data = fx.pdf(["A page with plenty of extractable words about rivers and bridges.", None])
    body = media(client, asset=asset(data, "application/pdf", "doc.pdf")).json()
    assert [(d["modality"], d["locator"]["page"]) for d in body["data"]] == [("text", 1), ("image", 2)]
    assert "excerpt" in body["data"][0] and "excerpt" not in body["data"][1]
    assert any("no OCR" in n for n in body["notes"])


def test_audio_and_video_locators(client):
    a = media(client, asset=asset(fx.wav(10.0, 16_000, 1), "audio/wav", "a.wav")).json()
    assert [d["locator"] for d in a["data"]] == [
        {"kind": "time", "start_ms": 0, "end_ms": 8000},
        {"kind": "time", "start_ms": 8000, "end_ms": 10000},
    ]
    v = media(client, asset=asset(fx.mp4(4.0, fps=5), "video/mp4", "v.mp4")).json()
    assert [d["locator"] for d in v["data"]] == [{"kind": "time", "start_ms": 0, "end_ms": 4000}]
    assert any("audio track is not embedded" in n for n in v["notes"])


def test_partial_flag_on_segment_cap(enc):
    lim = dataclasses.replace(DEFAULT_LIMITS, max_segments=2)
    c = TestClient(create_app(enc, limits=lim))
    body = media(c, asset=asset(("word " * 2000).encode(), "text/plain")).json()
    assert body["partial"] is True and len(body["data"]) == 2


def test_upload_limits(enc):
    lim = dataclasses.replace(DEFAULT_LIMITS, max_decoded_bytes=1000, max_body_bytes=4000)
    c = TestClient(create_app(enc, limits=lim, decoder=decode_inline))
    assert media(c, asset=asset(b"a" * 1001, "text/plain")).status_code == 413
    assert media(c, asset=asset(b"a" * 1000, "text/plain")).status_code == 200
    r = c.post("/v1/media/embeddings", content=b"{" + b" " * 5000 + b"}", headers={"content-type": "application/json"})
    assert r.status_code == 413


def test_bad_base64_and_mime(client):
    bad = {"name": "x.png", "mime_type": "image/png", "data_base64": "!!!notbase64"}
    assert media(client, asset=bad).status_code == 422
    assert media(client, asset=asset(fx.png(), "image/webp")).status_code == 415
    assert media(client, asset=asset(fx.png(), "video/mp4")).status_code == 415


def test_path_like_names_rejected(client):
    assert media(client, asset=asset(b"hello", "text/plain", "../../etc/passwd")).status_code == 422


def test_api_key(enc):
    c = TestClient(create_app(enc, api_key="k" * 32))
    assert c.get("/healthz").status_code == 200
    assert c.get("/v1/models").status_code == 401
    assert media(c, query="a").status_code == 401
    h = {"Authorization": "Bearer " + "k" * 32}
    assert c.post("/v1/media/embeddings", json={"model": MODEL, "query": "a"}, headers=h).status_code == 200


def test_openai_embeddings_text(client, enc):
    r = client.post("/v1/embeddings", json={"model": MODEL, "input": ["alpha", "beta"]})
    body = r.json()
    assert body["object"] == "list" and [d["index"] for d in body["data"]] == [0, 1]
    assert all(len(d["embedding"]) == 768 for d in body["data"])
    assert [k for k, _ in enc.calls] == ["document", "document"]
    client.post("/v1/embeddings", json={"model": MODEL, "input": "q", "input_type": "query"})
    assert enc.calls[-1] == ("query", "q")


def test_disabled_modality(enc):
    c = TestClient(create_app(FakeEncoder(modalities={"text", "image"})))
    r = media(c, asset=asset(fx.wav(2.0, 16_000, 1), "audio/wav"))
    assert r.status_code == 422 and "audio" in r.json()["error"]["message"]


def test_sandbox_timeout_is_504(enc):
    c = TestClient(create_app(enc, request_timeout_s=0.001))
    r = media(c, asset=asset(fx.mp4(4.0, fps=5), "video/mp4"))
    assert r.status_code in (503, 504)


def test_sandbox_memory_limit(enc):
    lim = dataclasses.replace(DEFAULT_LIMITS, decode_mem_bytes=64 * 1024 * 1024)
    c = TestClient(create_app(enc, limits=lim))
    r = media(c, asset=asset(fx.mp4(4.0, fps=5), "video/mp4"))
    assert r.status_code in (413, 422)
    assert "Traceback" not in r.text


# ----------------------------------------------------------------------------- admission and deadlines


class BlockingDecoder:
    """TEST-ONLY decoder that blocks until released and ignores cancellation, standing in for
    native work that cannot be interrupted. It yields one text segment."""

    def __init__(self):
        self.entered = __import__("threading").Event()
        self.release = __import__("threading").Event()
        self.finished = __import__("threading").Event()

    def __call__(self, raw, mime, limits, deadline, cancel=None):
        from metro_eg2.decode import Segment
        from metro_eg2.sandbox import Done

        self.entered.set()
        self.release.wait(10)
        self.finished.set()
        yield Segment("text", "chunk", {"kind": "text", "start": 0, "end": 5}, "chunk")
        yield Done(False, [])


def _bg(fn):
    import threading

    box = {}
    t = threading.Thread(target=lambda: box.setdefault("r", fn()), daemon=True)
    t.start()
    return t, box


def test_saturation_fails_fast_with_503(enc):
    dec = BlockingDecoder()
    c = TestClient(create_app(enc, decoder=dec, max_concurrency=1, request_timeout_s=20))
    t, box = _bg(lambda: media(c, asset=asset(b"hello", "text/plain")))
    assert dec.entered.wait(5)
    import time

    t0 = time.monotonic()
    r = media(c, query="second request")
    assert r.status_code == 503 and r.headers.get("retry-after") == "5"
    assert time.monotonic() - t0 < 1.0
    # queries share the same admission as decoding
    r2 = c.post("/v1/embeddings", json={"model": MODEL, "input": "x"})
    assert r2.status_code == 503
    dec.release.set()
    t.join(10)
    assert box["r"].status_code == 200


def test_deadline_504_and_slot_held_until_native_work_ends(enc):
    import time

    dec = BlockingDecoder()
    c = TestClient(create_app(enc, decoder=dec, max_concurrency=1, request_timeout_s=0.5))
    t0 = time.monotonic()
    r = media(c, asset=asset(b"hello", "text/plain"))
    assert r.status_code == 504 and time.monotonic() - t0 < 3
    # The orphaned work is still running: the slot must not be free yet.
    assert not dec.finished.is_set()
    assert media(c, query="x").status_code == 503
    dec.release.set()
    assert dec.finished.wait(5)
    for _ in range(50):
        if media(c, query="x").status_code == 200:
            break
        time.sleep(0.05)
    else:
        raise AssertionError("slot never released")
    # The cancelled job must not have encoded its segment after the deadline.
    assert all(k != "document" for k, _ in enc.calls)


def test_timeout_bounds():
    with pytest.raises(ValueError):
        create_app(FakeEncoder(), request_timeout_s=300)
    with pytest.raises(ValueError):
        create_app(FakeEncoder(), max_concurrency=0)


def test_isolation_failure_is_503_fail_closed(enc):
    from metro_eg2.wire import IsolationFailed

    def broken(*a, **k):
        raise IsolationFailed("Landlock ABI 0 < required 3")
        yield  # pragma: no cover

    c = TestClient(create_app(enc, decoder=broken))
    r = media(c, asset=asset(fx.png(), "image/png"))
    assert r.status_code == 503 and r.json()["error"]["code"] == "sandbox_unavailable"
    assert enc.calls == []
