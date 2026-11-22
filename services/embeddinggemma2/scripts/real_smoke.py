"""OPT-IN real-weight smoke over HTTP, on CPU, with synthetic media only.

Requires the pinned snapshot already on disk (see scripts/fetch_weights.sh). It downloads
nothing itself and runs with HF_HUB_OFFLINE=1. It starts the real service on a loopback port,
then:

1. embeds two synthetic PNGs (red circle and blue square on white) as image *assets*,
   with neutral filenames and no captions,
2. embeds the queries "a red circle on white" and "a blue square on white" through the
   query route (SearchQuery prefix applied once by the service),
3. checks HTTP 200, finite unit 768-d vectors, distinct image vectors, and that each query ranks
   its matching image first,
4. with --all, also sends a synthetic WAV, MP4, WebM and mixed text/scanned PDF and checks
   finite vectors and locators (dispatch only; no quality claim).

It prints a JSON report (cosines, ranks, revision, versions, peak RSS, timings). A rank miss is
a failure to investigate, never a reason to substitute captions or filenames.

    python -I scripts/real_smoke.py --model-dir <snapshot> [--all] [--threads 4]
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tests import fixtures as fx  # noqa: E402

MODEL = "google/embeddinggemma-2"


def post(port, path, body, timeout=900):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=json.dumps(body).encode(), headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, json.loads(r.read())


def vec(item):
    v = np.array(item["embedding"], dtype=np.float64)
    assert v.shape == (768,) and np.isfinite(v).all(), "bad vector"
    assert abs(np.linalg.norm(v) - 1.0) < 1e-3, f"not unit: {np.linalg.norm(v)}"
    return v


def peak_rss_kib(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
    except OSError:
        return None
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model-dir", required=True)
    ap.add_argument("--all", action="store_true", help="also dispatch audio/video/PDF (loads all encoders)")
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--dtype", default="float32", choices=["float32", "bfloat16"])
    ap.add_argument("--no-verify", action="store_true", help="skip the pinned sha256 check (script self-test only; result is never ok)")
    args = ap.parse_args()

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/tmp"),
        "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
        "HF_HUB_OFFLINE": "1",
        "TRANSFORMERS_OFFLINE": "1",
        "HF_HUB_DISABLE_TELEMETRY": "1",
        "CUDA_VISIBLE_DEVICES": "",
        "HIP_VISIBLE_DEVICES": "",
        "ROCR_VISIBLE_DEVICES": "",
        "OMP_NUM_THREADS": str(args.threads),
        "EG2_THREADS": str(args.threads),
        "EG2_BIND": f"127.0.0.1:{port}",
        "EG2_MODEL_DIR": args.model_dir,
        "EG2_DTYPE": args.dtype,
        "EG2_DEVICE": "cpu",
        "EG2_MODALITIES": "text,image,audio,video" if args.all else "text,image",
        "EG2_VERIFY_SHA256": "0" if args.no_verify else "1",
    }
    t0 = time.monotonic()
    proc = subprocess.Popen(
        [sys.executable, "-I", "-c", f"import sys; sys.path.insert(0, {str(ROOT)!r}); from metro_eg2.main import main; main()"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    report: dict = {"model": MODEL, "dtype": args.dtype, "threads": args.threads, "modalities": env["EG2_MODALITIES"]}
    try:
        while True:
            if proc.poll() is not None:
                raise SystemExit("service exited during startup:\n" + proc.stderr.read()[-4000:])
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=2) as r:
                    if r.status == 200:
                        break
            except OSError:
                time.sleep(1)
            if time.monotonic() - t0 > 900:
                raise SystemExit("service did not become healthy in 900 s")
        report["startup_s"] = round(time.monotonic() - t0, 1)
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/v1/models") as r:
            report["served"] = json.loads(r.read())["data"][0]

        images = {
            "red_circle": fx.shape_png("circle", (220, 20, 20)),
            "blue_square": fx.shape_png("square", (20, 40, 220)),
        }
        queries = {"red_circle": "a red circle on white", "blue_square": "a blue square on white"}
        t1 = time.monotonic()
        iv, qv = {}, {}
        for i, (k, data) in enumerate(images.items()):
            st, body = post(port, "/v1/media/embeddings", {"model": MODEL, "asset": {"name": f"img{i}.png", "mime_type": "image/png", "data_base64": base64.b64encode(data).decode()}})
            assert st == 200 and body["data"][0]["locator"] == {"kind": "image"} and "excerpt" not in body["data"][0]
            iv[k] = vec(body["data"][0])
        for k, q in queries.items():
            st, body = post(port, "/v1/media/embeddings", {"model": MODEL, "query": q})
            assert st == 200
            qv[k] = vec(body["data"][0])
        report["image_text_s"] = round(time.monotonic() - t1, 2)
        cos = {f"{q}~{i}": round(float(qv[q] @ iv[i]), 4) for q in qv for i in iv}
        report["cosines"] = cos
        report["image_image_cos"] = round(float(iv["red_circle"] @ iv["blue_square"]), 4)
        assert report["image_image_cos"] < 0.9999, "image vectors are identical"
        ranks = {q: max(iv, key=lambda i: float(qv[q] @ iv[i])) == q for q in qv}
        report["rank_ok"] = ranks

        if args.all:
            t2 = time.monotonic()
            extra = {}
            for name, data, mime in [
                ("wav", fx.wav(10.0, 22_050, 2), "audio/wav"),
                ("mp4", fx.mp4(18.0, fps=8), "video/mp4"),
                ("webm", fx.webm(3.0), "video/webm"),
                ("pdf", fx.pdf(["A page of extracted text about tidal energy and estuaries.", None]), "application/pdf"),
            ]:
                st, body = post(port, "/v1/media/embeddings", {"model": MODEL, "asset": {"name": f"x.{name}", "mime_type": mime, "data_base64": base64.b64encode(data).decode()}})
                assert st == 200
                for d in body["data"]:
                    vec(d)
                extra[name] = {"locators": [d["locator"] for d in body["data"]], "modalities": [d["modality"] for d in body["data"]], "partial": body["partial"]}
            report["dispatch"] = extra
            report["dispatch_s"] = round(time.monotonic() - t2, 2)
        report["peak_rss_mib"] = round((peak_rss_kib(proc.pid) or 0) / 1024)
    finally:
        proc.terminate()
        try:
            proc.wait(20)
        except subprocess.TimeoutExpired:
            proc.kill()

    import importlib.metadata as md

    report["versions"] = {p: md.version(p) for p in ("torch", "transformers", "sentence-transformers", "av", "pypdfium2", "pillow")}
    report["weights_verified"] = not args.no_verify
    report["ok"] = all(report["rank_ok"].values()) and not args.no_verify
    print(json.dumps(report, indent=2))
    sys.exit(0 if report["ok"] else 1)


if __name__ == "__main__":
    main()
