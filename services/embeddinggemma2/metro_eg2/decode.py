"""Safe, bounded decoders that turn one uploaded asset into model-ready segments.

Rules this module keeps:

* Input is only ever ``bytes`` held in memory. Nothing here opens a path, follows a URL, or
  touches a network or file protocol. The asset name is validated as a bare basename and is
  never used to read anything or to infer content.
* The declared MIME type must be in the allowlist *and* agree with the file's magic bytes.
* Every decoder is bounded: pages, pixels, samples, frames, zip entries and segments.
* No semantic content is invented. Text segments carry text that was really extracted;
  image/audio/video segments carry decoded pixels or samples. If a PDF page has no
  extractable text, the page is rendered and embedded as an image, and the response says so.

The functions here are pure (no model). ``decode_asset`` runs inside the sandboxed child
process in :mod:`metro_eg2.sandbox`.
"""

from __future__ import annotations

import io
import re
import unicodedata
import zipfile
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from .limits import DEFAULT_LIMITS, Limits


class AssetError(Exception):
    """A client error about the asset. ``status`` is the HTTP status to return."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


@dataclass
class Segment:
    modality: str  # "text" | "image" | "audio" | "video"
    payload: Any  # str | uint8 HxWx3 | float32 [n] @16k | uint8 TxHxWx3
    locator: dict[str, Any]
    excerpt: str | None = None


@dataclass
class DecodeResult:
    segments: list[Segment]
    partial: bool
    notes: list[str] = field(default_factory=list)


# --------------------------------------------------------------------------- formats

# Declared MIME type -> internal kind. Anything else is 415.
MIME_KINDS: dict[str, str] = {
    "text/plain": "text",
    "text/markdown": "text",
    "text/x-markdown": "text",
    "application/pdf": "pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
    "image/png": "png",
    "image/jpeg": "jpeg",
    "audio/wav": "wav",
    "audio/x-wav": "wav",
    "audio/wave": "wav",
    "audio/vnd.wave": "wav",
    "audio/mpeg": "mp3",
    "audio/mp3": "mp3",
    "video/mp4": "mp4",
    "video/webm": "webm",
}


def _sniff(data: bytes) -> str | None:
    if data.startswith(b"%PDF-"):
        return "pdf"
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if data.startswith(b"\xff\xd8\xff"):
        return "jpeg"
    if len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WAVE":
        return "wav"
    if data.startswith(b"ID3") or (len(data) >= 2 and data[0] == 0xFF and (data[1] & 0xE0) == 0xE0):
        return "mp3"
    if len(data) >= 12 and data[4:8] == b"ftyp":
        return "mp4"
    if data.startswith(b"\x1a\x45\xdf\xa3"):
        return "webm"
    if data.startswith(b"PK\x03\x04"):
        return "zip"
    return None


_NAME_BAD = re.compile(r"[\x00-\x1f\x7f/\\]")


def validate_name(name: str, limits: Limits = DEFAULT_LIMITS) -> str:
    """Accept only a bare basename. It is never opened, fetched or used as content."""
    if not isinstance(name, str) or not name or len(name) > limits.max_name_chars:
        raise AssetError(422, "asset.name must be a non-empty basename of at most 255 characters")
    if _NAME_BAD.search(name) or name in (".", "..") or ":" in name:
        raise AssetError(422, "asset.name must be a bare basename (no path separators or control characters)")
    return name


def detect_kind(mime_type: str, data: bytes) -> str:
    mime = (mime_type or "").split(";", 1)[0].strip().lower()
    kind = MIME_KINDS.get(mime)
    if kind is None:
        raise AssetError(415, f"unsupported mime_type {mime!r}; allowed: {sorted(MIME_KINDS)}")
    sniffed = _sniff(data)
    if kind == "text":
        # Valid UTF-8 never starts with 0xFF, so the loose MP3 frame-sync check can't hit real text.
        if sniffed is not None or b"\x00" in data[:8192]:
            raise AssetError(415, "declared text but content is binary")
        return kind
    if kind == "docx":
        if sniffed != "zip":
            raise AssetError(415, "declared DOCX but content is not a zip container")
        return kind
    if sniffed != kind:
        raise AssetError(415, f"declared {mime} but content looks like {sniffed or 'unknown'}")
    return kind


# --------------------------------------------------------------------------- text helpers


def _clean_text(text: str) -> str:
    text = unicodedata.normalize("NFC", text)
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    # Drop control characters except newline and tab.
    return "".join(ch for ch in text if ch in "\n\t" or unicodedata.category(ch)[0] != "C")


def chunk_spans(text: str, size: int) -> list[tuple[int, int]]:
    """Split ``text`` into [start, end) code-point spans of at most ``size``.

    Prefers to break at a paragraph, line or space boundary in the last third of the window.
    Spans cover the text exactly once, so locators are real offsets into the extracted text.
    """
    spans: list[tuple[int, int]] = []
    n = len(text)
    start = 0
    while start < n:
        end = min(start + size, n)
        if end < n:
            window = text[start:end]
            floor = len(window) * 2 // 3
            for sep in ("\n\n", "\n", " "):
                cut = window.rfind(sep, floor)
                if cut > 0:
                    end = start + cut + len(sep)
                    break
        spans.append((start, end))
        start = end
    return spans


def _excerpt(chunk: str, limits: Limits) -> str | None:
    s = " ".join(chunk.split())
    if not s:
        return None
    return s if len(s) <= limits.excerpt_chars else s[: limits.excerpt_chars - 1].rstrip() + "…"


def _text_segments(
    text: str, limits: Limits, *, kind: str = "text", page: int | None = None, budget: int
) -> tuple[list[Segment], bool]:
    """Chunk extracted text into ``kind`` segments, at most ``budget``. Returns (segments, truncated)."""
    out: list[Segment] = []
    for start, end in chunk_spans(text, limits.text_chunk_chars):
        chunk = text[start:end]
        if not chunk.strip():
            continue
        if len(out) >= budget:
            return out, True
        loc: dict[str, Any] = {"kind": kind, "start": start, "end": end}
        if page is not None:
            loc["page"] = page
        out.append(Segment("text", chunk, loc, _excerpt(chunk, limits)))
    return out, False


# --------------------------------------------------------------------------- TXT / MD


def decode_text(data: bytes, limits: Limits) -> DecodeResult:
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError:
        raise AssetError(422, "text assets must be UTF-8") from None
    text = _clean_text(text)
    notes = []
    partial = False
    if len(text) > limits.max_text_chars:
        text = text[: limits.max_text_chars]
        partial = True
        notes.append("text truncated before chunking")
    segs, truncated = _text_segments(text, limits, budget=limits.max_segments)
    if truncated:
        notes.append(f"stopped at {limits.max_segments} chunks")
    if not segs:
        raise AssetError(422, "the text asset is empty")
    return DecodeResult(segs, partial or truncated, notes)


# --------------------------------------------------------------------------- DOCX

_W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"


def decode_docx(data: bytes, limits: Limits) -> DecodeResult:
    """Extract body text from ``word/document.xml`` only.

    Relationships, external templates, embedded objects, headers and fields are never read or
    followed. The XML parser refuses DTDs and entity declarations.
    """
    from defusedxml import ElementTree as DET

    try:
        zf = zipfile.ZipFile(io.BytesIO(data))
    except zipfile.BadZipFile:
        raise AssetError(422, "DOCX is not a valid zip container") from None
    with zf:
        infos = zf.infolist()
        if len(infos) > limits.docx_max_entries:
            raise AssetError(422, "DOCX has too many zip entries")
        try:
            info = zf.getinfo("word/document.xml")
        except KeyError:
            raise AssetError(422, "DOCX has no word/document.xml") from None
        if info.flag_bits & 0x1:
            raise AssetError(422, "encrypted DOCX is not supported")
        if info.file_size > limits.docx_max_document_xml:
            raise AssetError(413, "DOCX document.xml is too large")
        if info.compress_size and info.file_size / info.compress_size > limits.docx_max_ratio:
            raise AssetError(422, "DOCX document.xml compression ratio is suspicious")
        # Read with an independent bound; header sizes can lie.
        with zf.open(info) as fh:
            xml = fh.read(limits.docx_max_document_xml + 1)
        if len(xml) > limits.docx_max_document_xml:
            raise AssetError(413, "DOCX document.xml is too large")

    try:
        root = DET.fromstring(xml, forbid_dtd=True, forbid_entities=True, forbid_external=True)
    except Exception:
        raise AssetError(422, "DOCX document.xml is not well-formed or uses forbidden XML features") from None

    paras: list[str] = []
    body = root.find(f"{_W}body")
    if body is None:
        raise AssetError(422, "DOCX has no document body")
    for p in body.iter(f"{_W}p"):
        parts: list[str] = []
        for el in p.iter():
            tag = el.tag
            if tag == f"{_W}t" and el.text:
                parts.append(el.text)
            elif tag == f"{_W}tab":
                parts.append("\t")
            elif tag in (f"{_W}br", f"{_W}cr"):
                parts.append("\n")
        paras.append("".join(parts))
        if sum(len(x) for x in paras) > limits.max_text_chars:
            break
    text = _clean_text("\n\n".join(x for x in paras if x.strip()))
    partial = False
    notes = []
    if len(text) > limits.max_text_chars:
        text = text[: limits.max_text_chars]
        partial = True
        notes.append("text truncated before chunking")
    segs, truncated = _text_segments(text, limits, budget=limits.max_segments)
    if truncated:
        notes.append(f"stopped at {limits.max_segments} chunks")
    if not segs:
        raise AssetError(422, "DOCX has no extractable body text")
    return DecodeResult(segs, partial or truncated, notes)


# --------------------------------------------------------------------------- PDF


def decode_pdf(data: bytes, limits: Limits) -> DecodeResult:
    """One or more segments per page, in page order.

    * Page with extractable text (>= ``pdf_min_text_chars`` non-space chars): text chunks with
      ``locator.kind == "page"``, 1-based ``page``, and char ``start``/``end`` within that page's
      extracted text. ``excerpt`` is real extracted text.
    * Page without extractable text (scanned or image-only): the page is rendered with forms and
      annotations off and embedded as an **image**, ``locator.kind == "page"``, no excerpt.
      No OCR is performed.

    pdfium is used without a form environment, so PDF JavaScript and XFA never run; embedded
    files, links and remote resources are never opened.
    """
    import pypdfium2 as pdfium

    if not isinstance(data, (bytes, bytearray)):  # pdfium treats str input as a file path
        raise TypeError("PDF input must be bytes")
    try:
        pdf = pdfium.PdfDocument(bytes(data))
    except pdfium.PdfiumError as exc:
        msg = str(exc).lower()
        if "password" in msg:
            raise AssetError(422, "encrypted PDF is not supported") from None
        raise AssetError(422, "PDF could not be parsed") from None

    segments: list[Segment] = []
    notes: list[str] = []
    partial = False
    try:
        n_pages = len(pdf)
        if n_pages == 0:
            raise AssetError(422, "PDF has no pages")
        rendered = 0
        for index in range(n_pages):
            if index >= limits.pdf_max_pages or len(segments) >= limits.max_segments:
                partial = True
                notes.append(f"stopped at page {index} of {n_pages}")
                break
            page = pdf[index]
            try:
                w, h = page.get_size()
                if not (0 < w <= limits.pdf_max_page_points and 0 < h <= limits.pdf_max_page_points):
                    partial = True
                    notes.append(f"page {index + 1} skipped: page size out of bounds")
                    continue
                tp = page.get_textpage()
                try:
                    count = min(tp.count_chars(), limits.max_text_chars)
                    raw = tp.get_text_range(0, count) if count > 0 else ""
                finally:
                    tp.close()
                text = _clean_text(raw)
                if sum(1 for ch in text if not ch.isspace()) >= limits.pdf_min_text_chars:
                    segs, truncated = _text_segments(
                        text, limits, kind="page", page=index + 1, budget=limits.max_segments - len(segments)
                    )
                    segments.extend(segs)
                    if truncated:
                        partial = True
                        notes.append(f"stopped inside page {index + 1}")
                        break
                else:
                    scale = min(
                        limits.pdf_render_max_side / max(w, h),
                        (limits.pdf_render_max_pixels / (w * h)) ** 0.5,
                        4.0,
                    )
                    bitmap = page.render(scale=scale, may_draw_forms=False, draw_annots=False)
                    try:
                        arr = np.ascontiguousarray(bitmap.to_numpy()[..., :3])
                        # pdfium renders BGR(x) by default
                        if bitmap.rev_byteorder is False:
                            arr = arr[..., ::-1]
                        arr = np.ascontiguousarray(arr, dtype=np.uint8)
                    finally:
                        bitmap.close()
                    segments.append(Segment("image", arr, {"kind": "page", "page": index + 1}, None))
                    rendered += 1
            finally:
                page.close()
        if rendered:
            notes.append(f"{rendered} page(s) had no extractable text and were embedded as rendered images (no OCR)")
    finally:
        pdf.close()
    if not segments:
        raise AssetError(422, "PDF produced no embeddable pages")
    return DecodeResult(segments, partial, notes)


# --------------------------------------------------------------------------- images


def decode_image(data: bytes, kind: str, limits: Limits) -> DecodeResult:
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = limits.image_max_pixels
    fmt = {"png": "PNG", "jpeg": "JPEG"}[kind]
    try:
        with Image.open(io.BytesIO(data), formats=[fmt]) as im:
            w, h = im.size
            if w <= 0 or h <= 0 or w > limits.image_max_side or h > limits.image_max_side or w * h > limits.image_max_pixels:
                raise AssetError(413, "image dimensions out of bounds")
            if kind == "jpeg":
                im.draft("RGB", (limits.image_downscale_side, limits.image_downscale_side))
            im.seek(0)  # first frame only (APNG)
            rgb = im.convert("RGB")
    except AssetError:
        raise
    except Image.DecompressionBombError:
        raise AssetError(413, "image dimensions out of bounds") from None
    except Exception:
        raise AssetError(422, "image could not be decoded") from None
    if max(rgb.size) > limits.image_downscale_side:
        rgb.thumbnail((limits.image_downscale_side, limits.image_downscale_side), Image.Resampling.BICUBIC)
    arr = np.asarray(rgb, dtype=np.uint8).copy()
    return DecodeResult([Segment("image", arr, {"kind": "image"}, None)], False, [])


# --------------------------------------------------------------------------- audio / video (PyAV)

_AV_FORMATS = {"wav": "wav", "mp3": "mp3", "mp4": "mp4", "webm": "webm"}
_VIDEO_CODECS = {"h264", "hevc", "vp8", "vp9", "av1", "mpeg4"}
_AUDIO_CODECS = {
    "wav": {"pcm_s16le", "pcm_s24le", "pcm_s32le", "pcm_f32le", "pcm_f64le", "pcm_u8", "pcm_s16be", "pcm_mulaw", "pcm_alaw"},
    "mp3": {"mp3", "mp3float"},
}


def _av_open(data: bytes, kind: str):
    import av

    options = {
        # MOV/MP4: never resolve data references to other files.
        "enable_drefs": "0",
        "use_absolute_path": "0",
    }
    try:
        return av.open(io.BytesIO(data), mode="r", format=_AV_FORMATS[kind], options=options, timeout=None)
    except Exception:
        raise AssetError(422, f"{kind} container could not be opened") from None


def _to_mono(frame) -> np.ndarray:
    a = frame.to_ndarray().astype(np.float32, copy=False)  # planar: (channels, samples)
    return a.mean(axis=0) if a.ndim == 2 else a.reshape(-1)


def decode_audio(data: bytes, kind: str, limits: Limits) -> DecodeResult:
    """Decode to mono float32 at 16 kHz and cut fixed ``audio_segment_ms`` windows.

    Each window is a separate native audio embedding (no text prefix). Locators are
    ``{"kind":"time","start_ms","end_ms"}`` relative to the start of the decoded stream.
    """
    import av

    seg_samples = limits.audio_rate * limits.audio_segment_ms // 1000
    max_samples = seg_samples * limits.max_segments
    chunks: list[np.ndarray] = []
    total = 0
    partial = False
    notes: list[str] = []
    container = _av_open(data, kind)
    try:
        streams = [s for s in container.streams if s.type == "audio"]
        if not streams:
            raise AssetError(422, "no audio stream")
        stream = streams[0]
        ctx = stream.codec_context
        if ctx.name not in _AUDIO_CODECS[kind]:
            raise AssetError(415, f"audio codec {ctx.name!r} not allowed for {kind}")
        if (ctx.channels or 0) > limits.audio_max_channels or (ctx.sample_rate or 0) > limits.audio_max_source_rate:
            raise AssetError(422, "audio channel count or sample rate out of bounds")
        # Resample each channel to 16 kHz, then average channels ourselves: FFmpeg's mono
        # downmix applies -3 dB per channel, which over-scales correlated stereo by sqrt(2).
        resampler = av.AudioResampler(format="fltp", layout=ctx.layout, rate=limits.audio_rate)
        try:
            for frame in container.decode(stream):
                if (frame.sample_rate or 0) > limits.audio_max_source_rate or len(frame.layout.channels) > limits.audio_max_channels:
                    raise AssetError(422, "audio channel count or sample rate out of bounds")
                for out in resampler.resample(frame):
                    a = _to_mono(out)
                    chunks.append(a)
                    total += a.size
                if total >= max_samples:
                    partial = True
                    notes.append(f"stopped after {limits.max_segments} segments")
                    break
            else:
                for out in resampler.resample(None):
                    a = _to_mono(out)
                    chunks.append(a)
                    total += a.size
        except AssetError:
            raise
        except av.FFmpegError:
            if total == 0:
                raise AssetError(422, "audio could not be decoded") from None
            partial = True
            notes.append("decoding stopped at a corrupt frame")
    finally:
        container.close()

    if total == 0:
        raise AssetError(422, "audio stream is empty")
    pcm = np.concatenate(chunks)[:max_samples]
    pcm = np.nan_to_num(pcm, nan=0.0, posinf=0.0, neginf=0.0).clip(-1.0, 1.0)
    segments: list[Segment] = []
    min_tail = limits.audio_rate * limits.audio_min_tail_ms // 1000
    for start in range(0, pcm.size, seg_samples):
        piece = pcm[start : start + seg_samples]
        if piece.size < min_tail and segments:
            notes.append(f"dropped a trailing {piece.size * 1000 // limits.audio_rate} ms remainder")
            partial = True
            break
        start_ms = start * 1000 // limits.audio_rate
        end_ms = (start + piece.size) * 1000 // limits.audio_rate
        segments.append(Segment("audio", np.ascontiguousarray(piece), {"kind": "time", "start_ms": start_ms, "end_ms": end_ms}))
        if len(segments) >= limits.max_segments:
            if start + seg_samples < pcm.size or partial:
                partial = True
            break
    return DecodeResult(segments, partial, notes)


def decode_video(data: bytes, kind: str, limits: Limits) -> DecodeResult:
    """Sample visual frames at ``video_fps`` and group them into fixed ``video_segment_ms`` clips.

    Only the **visual** track is embedded. The audio track of a video is ignored (it is not
    decoded and does not contribute to the vector). Locators are
    ``{"kind":"time","start_ms","end_ms"}`` from the stream's presentation timestamps.
    """
    import av

    container = _av_open(data, kind)
    period = 1.0 / limits.video_fps
    frames_per_seg = max(1, int(round(limits.video_segment_ms / 1000 * limits.video_fps)))
    max_samples = frames_per_seg * limits.max_segments
    sampled: list[tuple[float, np.ndarray]] = []
    partial = False
    notes: list[str] = ["video: visual frames only; the audio track is not embedded"]
    last_t = None
    try:
        streams = [s for s in container.streams if s.type == "video"]
        if not streams:
            raise AssetError(422, "no video stream")
        stream = streams[0]
        ctx = stream.codec_context
        if ctx.name not in _VIDEO_CODECS:
            raise AssetError(415, f"video codec {ctx.name!r} not allowed")
        w, h = ctx.width or 0, ctx.height or 0
        if not (0 < w <= limits.video_max_side and 0 < h <= limits.video_max_side and w * h <= limits.video_max_pixels):
            raise AssetError(413, "video dimensions out of bounds")
        stream.thread_type = "AUTO"
        tb = stream.time_base
        start_pts = stream.start_time
        next_t = 0.0
        decoded = 0
        try:
            for frame in container.decode(stream):
                decoded += 1
                if decoded > limits.video_max_decoded_frames:
                    partial = True
                    notes.append("stopped: decoded-frame budget exhausted")
                    break
                if frame.width != w or frame.height != h:
                    if not (0 < frame.width <= limits.video_max_side and 0 < frame.height <= limits.video_max_side and frame.width * frame.height <= limits.video_max_pixels):
                        raise AssetError(413, "video dimensions out of bounds")
                if frame.pts is None or tb is None:
                    continue
                t = float((frame.pts - (start_pts or 0)) * tb)
                if t < 0:
                    continue
                if t * 1000 >= limits.max_segments * limits.video_segment_ms:
                    partial = True
                    notes.append(f"stopped after {limits.max_segments} segments")
                    break
                last_t = t if last_t is None else max(last_t, t)
                if t + 1e-6 < next_t:
                    continue
                scale = min(1.0, limits.video_frame_max_side / max(frame.width, frame.height))
                fw = max(2, int(frame.width * scale) // 2 * 2)
                fh = max(2, int(frame.height * scale) // 2 * 2)
                rgb = frame.reformat(width=fw, height=fh, format="rgb24").to_ndarray()
                sampled.append((t, np.ascontiguousarray(rgb, dtype=np.uint8)))
                while next_t <= t + 1e-6:
                    next_t += period
                if len(sampled) > max_samples:  # belt and braces; the time check above binds first
                    partial = True
                    break
        except AssetError:
            raise
        except av.FFmpegError:
            if not sampled:
                raise AssetError(422, "video could not be decoded") from None
            partial = True
            notes.append("decoding stopped at a corrupt frame")
        duration_s = None
        if stream.duration is not None and tb is not None:
            duration_s = float(stream.duration * tb)
        rate = stream.average_rate or stream.guessed_rate
        frame_dur = float(1 / rate) if rate else 0.0
    finally:
        container.close()

    if not sampled:
        raise AssetError(422, "video has no decodable frames")
    # Fixed-duration clips [k*seg, (k+1)*seg); frames are assigned by their own timestamp.
    seg_ms = limits.video_segment_ms
    # End of the last clip: the last decoded frame's end (or the stream duration when complete).
    last_end = (last_t or 0.0) + frame_dur
    end_of_media_ms = int(round((last_end if partial else max(duration_s or 0.0, last_end)) * 1000))
    groups: dict[int, list[np.ndarray]] = {}
    for t, arr in sampled:
        groups.setdefault(int(t * 1000) // seg_ms, []).append(arr)
    segments: list[Segment] = []
    for k in sorted(groups):
        if len(segments) >= limits.max_segments:
            partial = True
            break
        group = groups[k]
        shape = group[0].shape
        if any(a.shape != shape for a in group):  # mid-stream resolution change
            from PIL import Image

            group = [a if a.shape == shape else np.asarray(Image.fromarray(a).resize((shape[1], shape[0]))) for a in group]
        start_ms = k * seg_ms
        end_ms = max(start_ms + 1, min((k + 1) * seg_ms, end_of_media_ms))
        segments.append(Segment("video", np.stack(group), {"kind": "time", "start_ms": start_ms, "end_ms": end_ms}))
    return DecodeResult(segments, partial, notes)


# --------------------------------------------------------------------------- entry point


def decode_asset(data: bytes, mime_type: str, limits: Limits = DEFAULT_LIMITS) -> DecodeResult:
    if len(data) > limits.max_decoded_bytes:
        raise AssetError(413, f"asset exceeds {limits.max_decoded_bytes} decoded bytes")
    if not data:
        raise AssetError(422, "asset is empty")
    kind = detect_kind(mime_type, data)
    if kind == "text":
        return decode_text(data, limits)
    if kind == "docx":
        return decode_docx(data, limits)
    if kind == "pdf":
        return decode_pdf(data, limits)
    if kind in ("png", "jpeg"):
        return decode_image(data, kind, limits)
    if kind in ("wav", "mp3"):
        return decode_audio(data, kind, limits)
    if kind in ("mp4", "webm"):
        return decode_video(data, kind, limits)
    raise AssetError(415, "unsupported asset")  # unreachable
