"""Hard limits for the decoder and the API. Every bound in one place, so tests and docs can cite it."""

from __future__ import annotations

from dataclasses import dataclass

MIB = 1024 * 1024


@dataclass(frozen=True)
class Limits:
    # Request / upload
    max_decoded_bytes: int = 8 * MIB  # decoded asset bytes (after base64)
    max_body_bytes: int = 12 * MIB  # raw HTTP body (base64 of 8 MiB is ~10.7 MiB, plus JSON)
    max_query_chars: int = 8_000
    max_name_chars: int = 255
    max_openai_inputs: int = 64

    # Segmentation
    max_segments: int = 48  # per asset; more => partial=true
    text_chunk_chars: int = 1_200  # code points per text chunk
    excerpt_chars: int = 240

    # Text-like decoders
    max_text_chars: int = 2_000_000  # decoded TXT/MD/DOCX text before chunking
    docx_max_entries: int = 2_000
    docx_max_document_xml: int = 16 * MIB  # uncompressed word/document.xml
    docx_max_ratio: int = 200  # uncompressed / compressed for document.xml

    # PDF
    pdf_max_pages: int = 48  # pages examined (each yields >=1 segment)
    pdf_min_text_chars: int = 32  # fewer non-space chars => render the page visually
    pdf_max_page_points: float = 14_400.0  # PDF user-space side limit (200 in)
    pdf_render_max_side: int = 1_536
    pdf_render_max_pixels: int = 2_400_000

    # Images
    image_max_side: int = 8_192
    image_max_pixels: int = 40_000_000
    image_downscale_side: int = 1_536  # the processor resizes to ~280 soft tokens anyway

    # Audio (model: mono 16 kHz; pinned processor caps one input at 280 tokens * 40 ms = 11.2 s)
    audio_rate: int = 16_000
    audio_segment_ms: int = 8_000
    audio_min_tail_ms: int = 500  # a shorter trailing remainder is dropped and reported as partial
    audio_max_channels: int = 8
    audio_max_source_rate: int = 192_000

    # Video (model: ~1 fps visual frames, <=32 frames per clip, audio track NOT encoded)
    video_fps: float = 1.0
    video_segment_ms: int = 16_000  # 16 frames per segment at 1 fps (<= 32)
    video_max_side: int = 4_096
    video_max_pixels: int = 4_096 * 2_304
    video_frame_max_side: int = 768  # sampled frames are downscaled before the processor
    video_max_decoded_frames: int = 60 * 60 * 15  # raw decoded frames before giving up (partial)

    # Decoder process
    decode_mem_bytes: int = 3 * 1024 * MIB  # RLIMIT_AS for the decoder child
    decode_cpu_s: int = 90


DEFAULT_LIMITS = Limits()
