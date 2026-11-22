"""PLUMBING CHECK ONLY: random-initialised, shrunken EmbeddingGemma 2 architecture. NOT the model.

Builds a 2-layer random-init copy of the pinned architecture from the pinned *config* and
tokenizer files (no weights downloaded), saves it as a Sentence Transformers folder, and runs the
service's real ``SentenceTransformerEncoder`` over every modality the decoders produce. It proves
the typed-input plumbing, the processor kwargs, shapes, normalisation and MRL truncation work end
to end. The vectors are meaningless, so it says nothing about retrieval quality.

    python -I scripts/plumbing_check.py <dir-with-pinned-config-and-tokenizer> <scratch-dir>
"""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))


def main(meta_dir: str, scratch: str) -> None:
    import torch
    from transformers import AutoConfig, AutoModel

    from metro_eg2.decode import decode_asset
    from metro_eg2.encoder import SentenceTransformerEncoder
    from tests import fixtures as fx

    meta, out = Path(meta_dir), Path(scratch) / "eg2-random-tiny"
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(meta, out, ignore=shutil.ignore_patterns(".cache", "model.safetensors", "README.md"))
    raw = json.loads((meta / "config.json").read_text(encoding="utf-8"))
    tc = raw["text_config"]
    tc["num_hidden_layers"] = 2
    tc["layer_types"] = ["sliding_attention", "full_attention"]
    tc["per_layer_config"] = {"01": tc["per_layer_config"]["05"]}
    raw["vision_config"]["num_hidden_layers"] = 2
    raw["audio_config"]["num_hidden_layers"] = 2
    (out / "config.json").write_text(json.dumps(raw), encoding="utf-8")
    cfg = AutoConfig.from_pretrained(str(out), local_files_only=True)
    torch.manual_seed(0)
    model = AutoModel.from_config(cfg, dtype=torch.float32)
    model.save_pretrained(str(out))

    enc = SentenceTransformerEncoder(str(out), verify_sha256=False)
    assert enc.query_prefix == "task: search result | query: " and enc.document_prefix == "title: none | text: "
    cases = {
        "query": ("query", "https://example.invalid/cat.png"),
        "document": ("document", "Plain chunk of extracted text."),
    }
    results = {}
    for name, (kind, payload) in cases.items():
        results[name] = enc.encode(kind, payload, 768)
    for name, data, mime in [
        ("image", fx.png(200, 120), "image/png"),
        ("pdf-render", fx.pdf([None]), "application/pdf"),
        ("audio", fx.wav(9.0, 22_050, 2), "audio/wav"),
        ("video", fx.mp4(18.0, fps=8), "video/mp4"),
        ("webm", fx.webm(3.0), "video/webm"),
    ]:
        for i, seg in enumerate(decode_asset(data, mime).segments):
            kind = "document" if seg.modality == "text" else seg.modality
            results[f"{name}[{i}]"] = enc.encode(kind, seg.payload, 768)
    for k, v in results.items():
        assert v.shape == (768,) and np.isfinite(v).all() and abs(float(np.linalg.norm(v)) - 1) < 1e-4, k
        print(f"{k:14s} dim={v.size} norm={np.linalg.norm(v):.5f}")
    v256 = enc.encode("query", "x", 256)
    assert v256.shape == (256,) and abs(float(np.linalg.norm(v256)) - 1) < 1e-4
    # Parity: our typed + manual-prefix path == ST's own prompt_name path on safe text.
    ours = enc.encode("query", "northern lights", 768)
    theirs = enc.model.encode("northern lights", prompt_name="SearchQuery", normalize_embeddings=True)
    print("prompt parity max|diff| =", float(np.abs(ours - theirs).max()))
    assert np.allclose(ours, theirs, atol=1e-5)
    doc_theirs = enc.model.encode("Plain chunk of extracted text.", prompt_name="Document", normalize_embeddings=True)
    assert np.allclose(results["document"], doc_theirs, atol=1e-5)
    print("PLUMBING OK (random-init tiny model; no quality claim)")
    if "--keep" not in sys.argv:
        shutil.rmtree(out)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
