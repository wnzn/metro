"""Synthetic test media, generated in memory. No real user files and nothing downloaded."""

from __future__ import annotations

import io
import math
import wave
import zipfile

import numpy as np


def png(w=64, h=48, color=(255, 0, 0)) -> bytes:
    from PIL import Image

    buf = io.BytesIO()
    Image.new("RGB", (w, h), color).save(buf, "PNG")
    return buf.getvalue()


def jpeg(w=64, h=48, color=(0, 0, 255)) -> bytes:
    from PIL import Image

    buf = io.BytesIO()
    Image.new("RGB", (w, h), color).save(buf, "JPEG")
    return buf.getvalue()


def shape_png(shape: str, color, size=256) -> bytes:
    """A filled circle or square on white, for the real-model smoke."""
    from PIL import Image, ImageDraw

    im = Image.new("RGB", (size, size), (255, 255, 255))
    d = ImageDraw.Draw(im)
    m = size // 5
    if shape == "circle":
        d.ellipse((m, m, size - m, size - m), fill=color)
    else:
        d.rectangle((m, m, size - m, size - m), fill=color)
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


def wav(seconds=20.0, rate=44_100, channels=2, freq=440.0) -> bytes:
    n = int(seconds * rate)
    t = np.arange(n) / rate
    tone = (0.3 * np.sin(2 * math.pi * freq * t) * 32767).astype("<i2")
    frames = np.repeat(tone[:, None], channels, axis=1).tobytes()
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(frames)
    return buf.getvalue()


def _encode_av(fmt: str, video_codec: str | None, seconds: float, fps: int = 10, w=96, h=64, audio_codec: str | None = None) -> bytes:
    import av

    buf = io.BytesIO()
    with av.open(buf, mode="w", format=fmt) as out:
        vs = None
        if video_codec:
            vs = out.add_stream(video_codec, rate=fps)
            vs.width, vs.height, vs.pix_fmt = w, h, "yuv420p"
        a_s = out.add_stream(audio_codec, rate=44_100) if audio_codec else None
        if vs is not None:
            for i in range(int(seconds * fps)):
                img = np.zeros((h, w, 3), dtype=np.uint8)
                img[..., 0] = (i * 7) % 256
                img[: h // 2, : w // 2, 1] = 200
                frame = av.VideoFrame.from_ndarray(img, format="rgb24")
                for p in vs.encode(frame):
                    out.mux(p)
            for p in vs.encode(None):
                out.mux(p)
        if a_s is not None:
            rate = 44_100
            spf = a_s.codec_context.frame_size or 1152
            total = int(seconds * rate)
            pts = 0
            while pts < total:
                n = min(spf, total - pts)
                t = (np.arange(pts, pts + n) / rate).astype(np.float32)
                data = (0.3 * np.sin(2 * math.pi * 440.0 * t)).astype(np.float32)[None, :]
                fr = av.AudioFrame.from_ndarray(np.ascontiguousarray(data), format="fltp", layout="mono")
                fr.sample_rate = rate
                fr.pts = pts
                for p in a_s.encode(fr):
                    out.mux(p)
                pts += n
            for p in a_s.encode(None):
                out.mux(p)
    return buf.getvalue()


def mp3(seconds=3.0) -> bytes:
    return _encode_av("mp3", None, seconds, audio_codec="libmp3lame")


def mp4(seconds=20.0, fps=10, with_audio=False) -> bytes:
    return _encode_av("mp4", "libx264", seconds, fps=fps, audio_codec="aac" if with_audio else None)


def webm(seconds=5.0, fps=10) -> bytes:
    return _encode_av("webm", "libvpx", seconds, fps=fps)


def docx(paragraphs: list[str], extra_xml_prolog: str = "", extra_entries: dict[str, bytes] | None = None) -> bytes:
    body = "".join(f"<w:p><w:r><w:t xml:space=\"preserve\">{p}</w:t></w:r></w:p>" for p in paragraphs)
    xml = (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>{extra_xml_prolog}'
        '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
        f"<w:body>{body}</w:body></w:document>"
    )
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>')
        z.writestr("word/document.xml", xml)
        for k, v in (extra_entries or {}).items():
            z.writestr(k, v)
    return buf.getvalue()


def pdf(pages: list[str | None]) -> bytes:
    """Minimal hand-built PDF. A ``str`` page draws that text (Helvetica); ``None`` draws only a
    filled rectangle, i.e. a page with no extractable text (stands in for a scanned page)."""
    objs: list[bytes] = []
    n = len(pages)
    # 1: catalog, 2: pages, 3: font, then per page: page obj, content obj
    kids = " ".join(f"{4 + 2 * i} 0 R" for i in range(n))
    objs.append(b"<< /Type /Catalog /Pages 2 0 R >>")
    objs.append(f"<< /Type /Pages /Kids [{kids}] /Count {n} >>".encode())
    objs.append(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    for i, text in enumerate(pages):
        content_id = 5 + 2 * i
        objs.append(
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>".encode()
        )
        if text is None:
            stream = b"0.2 0.4 0.8 rg 100 300 400 300 re f"
        else:
            safe = text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
            lines = [safe[j : j + 80] for j in range(0, len(safe), 80)] or [""]
            ops = ["BT /F1 11 Tf 14 TL 50 740 Td"] + [f"({ln}) Tj T*" for ln in lines] + ["ET"]
            stream = "\n".join(ops).encode("latin-1")
        objs.append(b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream")
    out = io.BytesIO()
    out.write(b"%PDF-1.4\n")
    offsets = []
    for i, body in enumerate(objs):
        offsets.append(out.tell())
        out.write(f"{i + 1} 0 obj\n".encode() + body + b"\nendobj\n")
    xref = out.tell()
    out.write(f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode())
    for off in offsets:
        out.write(f"{off:010d} 00000 n \n".encode())
    out.write(f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode())
    return out.getvalue()
