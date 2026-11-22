#!/usr/bin/env bash
# Intentional one-time setup: download the pinned google/embeddinggemma-2 snapshot (~1.53 GB).
# Usage: scripts/fetch_weights.sh <target-dir>
# The service itself never downloads; it reads this directory with HF_HUB_OFFLINE=1.
set -euo pipefail
REV=f9972ac22e696a105c482202646979e33e4b5a1e
SHA=197a32965d4b1105faf060417baa899e193fb73cd401f42ec9295234d5553d79
DIR=${1:?usage: fetch_weights.sh <target-dir>}
mkdir -p "$DIR"
hf download google/embeddinggemma-2 --revision "$REV" --local-dir "$DIR" \
  --include "*.json" --include "*.jinja" --include "tokenizer.*" --include "model.safetensors"
echo "$SHA  $DIR/model.safetensors" | sha256sum -c -
echo "pinned snapshot ready in $DIR (revision $REV)"
