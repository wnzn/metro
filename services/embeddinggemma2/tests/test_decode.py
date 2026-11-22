"""Decoder behaviour on synthetic media: formats, locators, excerpts, bounds. No model."""

from __future__ import annotations

import dataclasses
import io
import zipfile

import numpy as np
import pytest

from metro_eg2.decode import AssetError, chunk_spans, decode_asset, validate_name
from metro_eg2.limits import DEFAULT_LIMITS

from . import fixtures as fx

L = DEFAULT_LIMITS


def test_chunk_spans_cover_text_exactly():
    text = ("word " * 700).strip() + "\n\n" + "x" * 3000
    spans = chunk_spans(text, 1200)
    assert spans[0][0] == 0 and spans[-1][1] == len(text)
    assert all(a[1] == b[0] for a, b in zip(spans, spans[1:]))
    assert all(e - s <= 1200 for s, e in spans)


def test_text_chunks_have_real_offsets_and_excerpts():
    text = "Alpha paragraph.\n\n" + "beta " * 400
    res = decode_asset(text.encode(), "text/markdown")
    assert not res.partial
    for seg in res.segments:
        assert seg.modality == "text"
        loc = seg.locator
        assert loc["kind"] == "text"
        assert text[loc["start"] : loc["end"]] == seg.payload
        assert seg.excerpt and seg.excerpt.split()[0] in seg.payload


def test_text_segment_cap_sets_partial():
    text = ("lorem ipsum " * 100 + "\n") * 80  # ~96k chars -> > 48 chunks
    res = decode_asset(text.encode(), "text/plain")
    assert len(res.segments) == L.max_segments and res.partial


def test_text_rejects_binary_and_non_utf8():
    with pytest.raises(AssetError) as e:
        decode_asset(fx.png(), "text/plain")
    assert e.value.status == 415
    with pytest.raises(AssetError) as e:
        decode_asset("café".encode("latin-1"), "text/plain")
    assert e.value.status == 422


def test_mime_allowlist_and_magic_must_agree():
    with pytest.raises(AssetError) as e:
        decode_asset(fx.png(), "image/gif")
    assert e.value.status == 415
    with pytest.raises(AssetError) as e:
        decode_asset(fx.jpeg(), "image/png")
    assert e.value.status == 415
    with pytest.raises(AssetError) as e:
        decode_asset(b"<svg xmlns='http://www.w3.org/2000/svg'/>", "image/svg+xml")
    assert e.value.status == 415


@pytest.mark.parametrize("name", ["../etc/passwd", "a/b.png", "c:\\x.png", "x\x00.png", "", "..", "C:evil.png", "a" * 256])
def test_names_must_be_bare_basenames(name):
    with pytest.raises(AssetError):
        validate_name(name)


def test_name_never_affects_decoding():
    # Same bytes, different names: decoding takes no name at all (the API only validates it).
    import inspect

    assert "name" not in inspect.signature(decode_asset).parameters


def test_png_and_jpeg_are_pixels():
    for data, mime in ((fx.png(64, 48, (255, 0, 0)), "image/png"), (fx.jpeg(64, 48), "image/jpeg")):
        res = decode_asset(data, mime)
        (seg,) = res.segments
        assert seg.modality == "image" and seg.locator == {"kind": "image"} and seg.excerpt is None
        assert seg.payload.shape == (48, 64, 3) and seg.payload.dtype == np.uint8
    red = decode_asset(fx.png(8, 8, (255, 0, 0)), "image/png").segments[0].payload
    assert tuple(red[0, 0]) == (255, 0, 0)


def test_image_pixel_bound():
    from PIL import Image

    buf = io.BytesIO()
    Image.new("1", (9000, 10)).save(buf, "PNG")  # tiny file, side over the limit
    with pytest.raises(AssetError) as e:
        decode_asset(buf.getvalue(), "image/png")
    assert e.value.status == 413


def test_large_image_is_downscaled_before_the_model():
    from PIL import Image

    buf = io.BytesIO()
    Image.new("RGB", (4000, 1000), (0, 255, 0)).save(buf, "PNG")
    seg = decode_asset(buf.getvalue(), "image/png").segments[0]
    assert max(seg.payload.shape[:2]) == L.image_downscale_side


def test_pdf_text_pages_and_rendered_scanned_pages():
    data = fx.pdf(["First page talks about tidal energy in coastal estuaries and turbines.", None, "Third page covers grid storage batteries and their chemistry."])
    res = decode_asset(data, "application/pdf")
    kinds = [(s.modality, s.locator.get("page")) for s in res.segments]
    assert kinds == [("text", 1), ("image", 2), ("text", 3)]
    t1 = res.segments[0]
    assert t1.locator["kind"] == "page" and t1.locator["start"] == 0 and "tidal energy" in t1.payload
    assert "tidal energy" in t1.excerpt
    img = res.segments[1]
    assert img.locator == {"kind": "page", "page": 2} and img.excerpt is None
    assert img.payload.ndim == 3 and img.payload.shape[2] == 3 and max(img.payload.shape[:2]) <= L.pdf_render_max_side
    # The rendered rectangle is blue-ish (0.2,0.4,0.8): pixels really came from rendering.
    mid = img.payload[img.payload.shape[0] // 2, img.payload.shape[1] // 2]
    assert mid[2] > mid[0]
    assert any("no OCR" in n for n in res.notes)


def test_pdf_page_cap_is_partial():
    data = fx.pdf([f"Page number {i} has enough words to count as extractable text here." for i in range(60)])
    res = decode_asset(data, "application/pdf")
    assert res.partial and len(res.segments) == L.max_segments
    assert res.segments[-1].locator["page"] == L.max_segments


def test_pdf_garbage_is_422():
    with pytest.raises(AssetError) as e:
        decode_asset(b"%PDF-1.4\nnot really", "application/pdf")
    assert e.value.status == 422


def test_docx_body_text_with_offsets():
    data = fx.docx(["Quarterly report", "Revenue grew in the northern region."])
    res = decode_asset(data, "application/vnd.openxmlformats-officedocument.wordprocessingml.document")
    (seg,) = res.segments
    assert seg.payload == "Quarterly report\n\nRevenue grew in the northern region."
    assert seg.locator == {"kind": "text", "start": 0, "end": len(seg.payload)}


def test_docx_rejects_dtd_entities():
    evil = fx.docx(["&x;"], extra_xml_prolog='<!DOCTYPE d [<!ENTITY x "boom">]>')
    with pytest.raises(AssetError) as e:
        decode_asset(evil, "application/vnd.openxmlformats-officedocument.wordprocessingml.document")
    assert e.value.status == 422


def test_docx_external_entity_never_resolved():
    evil = fx.docx(["&x;"], extra_xml_prolog='<!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/passwd">]>')
    with pytest.raises(AssetError):
        decode_asset(evil, "application/vnd.openxmlformats-officedocument.wordprocessingml.document")


def test_docx_zip_bomb_ratio():
    big = b"<w:document xmlns:w='http://schemas.openxmlformats.org/wordprocessingml/2006/main'><w:body>" + b" " * (8 * 1024 * 1024) + b"</w:body></w:document>"
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("word/document.xml", big)
    with pytest.raises(AssetError) as e:
        decode_asset(buf.getvalue(), "application/vnd.openxmlformats-officedocument.wordprocessingml.document")
    assert "ratio" in e.value.message


def test_docx_too_many_entries():
    entries = {f"x/{i}": b"" for i in range(L.docx_max_entries + 1)}
    with pytest.raises(AssetError):
        decode_asset(fx.docx(["hi"], extra_entries=entries), "application/vnd.openxmlformats-officedocument.wordprocessingml.document")


def test_wav_is_resampled_to_mono_16k_in_8s_windows():
    res = decode_asset(fx.wav(seconds=20.0, rate=44_100, channels=2), "audio/wav")
    locs = [s.locator for s in res.segments]
    assert locs == [
        {"kind": "time", "start_ms": 0, "end_ms": 8000},
        {"kind": "time", "start_ms": 8000, "end_ms": 16000},
        {"kind": "time", "start_ms": 16000, "end_ms": 20000},
    ]
    for s in res.segments:
        assert s.modality == "audio" and s.payload.dtype == np.float32 and s.payload.ndim == 1 and s.excerpt is None
    assert res.segments[0].payload.size == 8 * 16_000
    assert 0.2 < float(np.abs(res.segments[0].payload).max()) <= 0.31  # the real tone survived
    assert not res.partial


def test_mp3_decodes():
    res = decode_asset(fx.mp3(3.0), "audio/mpeg")
    (seg,) = res.segments
    assert seg.locator["start_ms"] == 0 and 2900 <= seg.locator["end_ms"] <= 3200


def test_audio_segment_cap_is_partial():
    lim = dataclasses.replace(L, max_segments=2)
    res = decode_asset(fx.wav(seconds=20.0, rate=16_000, channels=1), "audio/wav", lim)
    assert len(res.segments) == 2 and res.partial


def test_mp4_visual_segments_at_1fps():
    res = decode_asset(fx.mp4(seconds=20.0, fps=10), "video/mp4")
    assert [s.locator for s in res.segments] == [
        {"kind": "time", "start_ms": 0, "end_ms": 16000},
        {"kind": "time", "start_ms": 16000, "end_ms": 20000},
    ]
    assert res.segments[0].payload.shape[0] == 16 and res.segments[1].payload.shape[0] == 4
    assert res.segments[0].payload.dtype == np.uint8 and res.segments[0].payload.shape[-1] == 3
    assert any("audio track is not embedded" in n for n in res.notes)


def test_mp4_audio_track_is_ignored():
    res = decode_asset(fx.mp4(seconds=3.0, fps=5, with_audio=True), "video/mp4")
    assert all(s.modality == "video" for s in res.segments)


def test_webm_decodes():
    res = decode_asset(fx.webm(seconds=5.0), "video/webm")
    (seg,) = res.segments
    assert seg.payload.shape[0] == 5 and seg.locator == {"kind": "time", "start_ms": 0, "end_ms": 5000}


def test_video_segment_cap_is_partial():
    lim = dataclasses.replace(L, max_segments=1)
    res = decode_asset(fx.mp4(seconds=20.0, fps=5), "video/mp4", lim)
    assert len(res.segments) == 1 and res.partial and res.segments[0].locator["end_ms"] == 16000


def test_oversize_asset():
    with pytest.raises(AssetError) as e:
        decode_asset(b"a" * (L.max_decoded_bytes + 1), "text/plain")
    assert e.value.status == 413
