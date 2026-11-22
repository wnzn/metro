"""TEST-ONLY deterministic encoder. It is NOT a model and its vectors carry NO semantics.

It exists so parser, locator, limit and API tests run without weights. It hashes the exact
payload it receives, so tests can also assert *what* reached the encoder (for example, that a
query was not turned into an image fetch, or that a document chunk got exactly one prefix).
The service never imports this module.
"""

from __future__ import annotations

import hashlib

import numpy as np

from metro_eg2.encoder import MODEL_ID, finish


class FakeEncoder:
    model_id = MODEL_ID

    def __init__(self, modalities=frozenset({"text", "image", "audio", "video"})):
        self.modalities = frozenset(modalities)
        self.calls: list[tuple[str, object]] = []

    def encode(self, kind, payload, dim):
        from metro_eg2.encoder import ModalityDisabled

        mod = {"query": "text", "document": "text"}.get(kind, kind)
        if mod not in self.modalities:
            raise ModalityDisabled(mod)
        self.calls.append((kind, payload))
        if isinstance(payload, str):
            raw = payload.encode()
        else:
            arr = np.asarray(payload)
            raw = arr.tobytes() + str(arr.shape).encode()
        seed = int.from_bytes(hashlib.sha256(kind.encode() + b"\0" + raw).digest()[:8], "little")
        return finish(np.random.default_rng(seed).standard_normal(768), dim)
