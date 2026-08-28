#!/usr/bin/env bash
# Download the Parakeet TDT ONNX models. ~465 MB for v3-int8 (default).
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p models && cd models

BASE=https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models
MODEL="${1:-sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8}"

if [ -d "$MODEL" ]; then
  echo "already present: models/$MODEL"; exit 0
fi

echo "fetching $MODEL ..."
curl -fSL --progress-bar -o "$MODEL.tar.bz2" "$BASE/$MODEL.tar.bz2"
tar xjf "$MODEL.tar.bz2"
rm "$MODEL.tar.bz2"
echo "ready: models/$MODEL"
