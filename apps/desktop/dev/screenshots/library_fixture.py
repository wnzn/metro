#!/usr/bin/env python3
"""Loopback media-embedding vendor and inert media files for capture-library.sh.

A SCRIPTED FIXTURE, NOT A MODEL: it reuses the scripted vendor of
tests/e2e/library.py (MediaVendor), which "embeds" by reading the actual
bytes it is sent (PNG pixels, PDF page text, WAV loudness, TXT paragraphs) and
maps them to fixed topic axes. It speaks Metro's media contract
(POST /v1/media/embeddings) on 127.0.0.1 only. Nothing reaches a real vendor
and no google/embeddinggemma-2 weights are involved.

Usage: library_fixture.py <info.json> <media-dir>
Writes the generated files into <media-dir>, then {base_url, key, model}
into <info.json> (mode 600) once it is serving. Runs until killed.
"""

import json
import os
import signal
import sys
import threading
from pathlib import Path

repo = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(repo / "tests" / "e2e"))

from library import MediaVendor, pdf_doc, png, wav_clip  # noqa: E402


def main():
    info, media = Path(sys.argv[1]), Path(sys.argv[2])
    media.mkdir(mode=0o700, exist_ok=True)
    files = {
        # A red whiteboard-ish photo: the vendor reads its pixels (crimson axis).
        "whiteboard-crimson.png": png((200, 30, 40), tag="capture-red"),
        "sky-azure.png": png((30, 60, 210), tag="capture-blue"),
        # Page 2 carries the match; the vendor extracts each page's text.
        "shipping-contract.pdf": pdf_doc(["Cover page with a greeting.", "The cargo manifest lists every crate and its seal number."]),
        # Silence for 5 s, then a loud tone: the vendor scores 5 s windows by loudness (thunder axis).
        "standup-recording.wav": wav_clip(True),
        "harbor-notes.txt": b"Harbor notes for the week.\n\nThe harbor ledger lists every lantern purchase and who signed for it.\n\nNothing else to report.\n",
    }
    for name, data in files.items():
        (media / name).write_bytes(data)

    vendor = MediaVendor("capture")
    threading.Thread(target=vendor.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True).start()
    tmp = info.with_suffix(".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as out:
        json.dump({"base_url": vendor.base_url, "key": vendor.key, "model": vendor.model}, out)
    tmp.rename(info)
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())
    while not stop.wait(0.5):
        if vendor.error:
            print(f"fixture handler errors: {vendor.error}", file=sys.stderr, flush=True)
            vendor.error = None
    vendor.shutdown()


if __name__ == "__main__":
    main()
