"""The real encoder: ``google/embeddinggemma-2`` through Sentence Transformers.

Inputs are always passed as typed single-key dicts (``{"text": ...}``, ``{"image": PIL}``,
``{"audio": {...}}``, ``{"video": {...}}``). Sentence Transformers' modality inference treats a
*bare string* that looks like a URL or an existing file path as an image/audio/video to fetch
or open; the typed dict form bypasses that heuristic, so user text is only ever tokenized.

Prefixes, exactly once, from the pinned ``config_sentence_transformers.json``:

* query text      -> ``prompts["SearchQuery"]``  (``task: search result | query: ``)
* document chunks -> ``prompts["Document"]``     (``title: none | text: ``)
* image / audio / video -> no prefix.

There is no test double in this module. Tests inject their own encoder object.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
from pathlib import Path
from typing import Any, Protocol

import numpy as np

MODEL_ID = "google/embeddinggemma-2"
PINNED_REVISION = "f9972ac22e696a105c482202646979e33e4b5a1e"
PINNED_WEIGHTS_SHA256 = "197a32965d4b1105faf060417baa899e193fb73cd401f42ec9295234d5553d79"
NATIVE_DIM = 768
MRL_DIMS = (768, 512, 256, 128)

log = logging.getLogger("metro_eg2")


class Encoder(Protocol):
    model_id: str
    modalities: frozenset[str]

    def encode(self, kind: str, payload: Any, dim: int) -> np.ndarray:
        """``kind`` in {"query", "document", "image", "audio", "video"} -> unit float32 vector [dim]."""
        ...


def finish(vec: np.ndarray, dim: int) -> np.ndarray:
    """Truncate to ``dim`` (Matryoshka) and L2-renormalize. Refuses non-finite or zero vectors."""
    v = np.asarray(vec, dtype=np.float32).reshape(-1)
    if v.size < dim:
        raise RuntimeError(f"encoder returned {v.size} dims, expected >= {dim}")
    v = v[:dim]
    if not np.all(np.isfinite(v)):
        raise RuntimeError("encoder returned non-finite values")
    n = float(np.linalg.norm(v))
    if n == 0.0:
        raise RuntimeError("encoder returned a zero vector")
    return v / n


def _snapshot_revision(model_dir: Path) -> str | None:
    """Revision recorded by ``hf download --local-dir`` (first line of the .metadata file)."""
    meta = model_dir / ".cache" / "huggingface" / "download" / "model.safetensors.metadata"
    if meta.is_file():
        return meta.read_text(encoding="utf-8").splitlines()[0].strip()
    return None


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(8 * 1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


class SentenceTransformerEncoder:
    def __init__(
        self,
        model_dir: str,
        *,
        dtype: str = "float32",
        device: str = "cpu",
        modalities: frozenset[str] = frozenset({"text", "image", "audio", "video"}),
        verify_sha256: bool = True,
        threads: int | None = None,
    ):
        import torch
        from sentence_transformers import SentenceTransformer

        if dtype not in ("float32", "bfloat16"):
            raise ValueError("EG2_DTYPE must be float32 or bfloat16 (float16 overflows in this model)")
        path = Path(model_dir)
        if not (path / "model.safetensors").is_file():
            raise FileNotFoundError(f"no model.safetensors in {model_dir}; run the setup download first")
        rev = _snapshot_revision(path)
        if rev is not None and rev != PINNED_REVISION:
            raise RuntimeError(f"model snapshot revision {rev} != pinned {PINNED_REVISION}")
        if verify_sha256:
            digest = _sha256(path / "model.safetensors")
            if digest != PINNED_WEIGHTS_SHA256:
                raise RuntimeError("model.safetensors sha256 does not match the pinned revision")
        st_cfg = json.loads((path / "config_sentence_transformers.json").read_text(encoding="utf-8"))
        self.prompts: dict[str, str] = st_cfg["prompts"]
        self.query_prefix = self.prompts["SearchQuery"]
        self.document_prefix = self.prompts["Document"]

        if threads:
            torch.set_num_threads(threads)
        config_kwargs: dict[str, Any] = {}
        if "image" not in modalities and "video" not in modalities:
            config_kwargs["vision_config"] = None
        if "audio" not in modalities:
            config_kwargs["audio_config"] = None
        torch_dtype = torch.float32 if dtype == "float32" else torch.bfloat16
        self.model = SentenceTransformer(
            str(path),
            device=device,
            local_files_only=True,
            trust_remote_code=False,
            model_kwargs={"dtype": torch_dtype},
            config_kwargs=config_kwargs or None,
        )
        self.model.eval()
        self.model_id = MODEL_ID
        self.revision = rev or "unverified-local-dir"
        self.modalities = frozenset(modalities)
        self.dtype = dtype
        self._torch = torch

    # -- encode -------------------------------------------------------------------------------

    def _run(self, item: dict[str, Any], dim: int) -> np.ndarray:
        with self._torch.inference_mode():
            out = self.model.encode(
                [item],
                prompt=None,
                batch_size=1,
                convert_to_numpy=True,
                normalize_embeddings=True,
                show_progress_bar=False,
            )
        return finish(out[0], dim)

    def encode(self, kind: str, payload: Any, dim: int) -> np.ndarray:
        if kind == "query":
            return self._run({"text": self.query_prefix + payload}, dim)
        if kind == "document":
            return self._run({"text": self.document_prefix + payload}, dim)
        if kind == "image":
            self._need("image")
            from PIL import Image

            return self._run({"image": Image.fromarray(payload, mode="RGB")}, dim)
        if kind == "audio":
            self._need("audio")
            return self._run({"audio": {"array": np.asarray(payload, dtype=np.float32), "sampling_rate": 16_000}}, dim)
        if kind == "video":
            self._need("video")
            from transformers.video_utils import VideoMetadata

            frames = np.asarray(payload, dtype=np.uint8)
            t, h, w = frames.shape[:3]
            # Frames are already sampled at 1 fps by the decoder; metadata says so, so the
            # processor's own 1 fps sampler keeps every frame (<= 32).
            meta = VideoMetadata(total_num_frames=t, fps=1.0, width=w, height=h, duration=float(t))
            return self._run({"video": {"array": frames, "video_metadata": meta}}, dim)
        raise ValueError(f"unknown kind {kind}")

    def _need(self, modality: str) -> None:
        if modality not in self.modalities:
            raise ModalityDisabled(modality)


class ModalityDisabled(Exception):
    def __init__(self, modality: str):
        super().__init__(modality)
        self.modality = modality


def load_from_env() -> SentenceTransformerEncoder:
    mods = frozenset(
        m.strip() for m in os.environ.get("EG2_MODALITIES", "text,image,audio,video").split(",") if m.strip()
    )
    bad = mods - {"text", "image", "audio", "video"}
    if bad or "text" not in mods:
        raise ValueError("EG2_MODALITIES must include text and only text,image,audio,video")
    threads = os.environ.get("EG2_THREADS")
    return SentenceTransformerEncoder(
        os.environ.get("EG2_MODEL_DIR", "/models/embeddinggemma-2"),
        dtype=os.environ.get("EG2_DTYPE", "float32"),
        device=os.environ.get("EG2_DEVICE", "cpu"),
        modalities=mods,
        verify_sha256=os.environ.get("EG2_VERIFY_SHA256", "1") != "0",
        threads=int(threads) if threads else None,
    )
