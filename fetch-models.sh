#!/usr/bin/env bash
# Fetch the models the app runs (ADR 0020): Parakeet Ultra for words and
# Nemotron 3 Diarization fast32 for voices, ~0.8 GB, through `ambient-engine
# fetch`, which builds the helper first if needed. Every file is then checked
# against licenses/model-sha256.txt, so a change upstream stops here rather
# than reaching a bundle.
#
#   ./fetch-models.sh          the app's models
#   ./fetch-models.sh --onnx   also the ONNX set the older benchmarks use
#                              (wer, der, asrbench); not bundled. An extra
#                              argument names a different ONNX recogniser.
set -euo pipefail
cd "$(dirname "$0")"

onnx=0
if [ "${1:-}" = "--onnx" ]; then
  onnx=1
  shift
fi

engine="${AMBIENT_ENGINE:-}"
if [ -z "$engine" ]; then
  echo "building ambient-engine ..."
  engine="$(scripts/build-engine)"
fi
mkdir -p models
echo "fetching the app's models ..."
# A closed stdin is fine: `fetch` does not watch it.
"$engine" fetch --models models </dev/null
# An `if`, not `&&`: set -e does not stop a script at a failed `a && b`.
if ! grep -v '^#' licenses/model-sha256.txt | shasum -a 256 -c --quiet -; then
  echo "a model file does not match licenses/model-sha256.txt" >&2
  exit 1
fi
echo "ready: models/parakeet-ultra, models/nemotron-3-diarization (checksums verified)"

[ "$onnx" = 1 ] || exit 0
cd models

ASR_BASE=https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models
SEG_BASE=https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models
# Upstream's spelling of "recognition", not a typo here.
SPK_BASE=https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models

MODEL="${1:-sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8}"

if [ -d "$MODEL" ]; then
  echo "already present: models/$MODEL"
else
  echo "fetching $MODEL ..."
  curl -fSL --progress-bar -o "$MODEL.tar.bz2" "$ASR_BASE/$MODEL.tar.bz2"
  tar xjf "$MODEL.tar.bz2"
  rm "$MODEL.tar.bz2"
  echo "ready: models/$MODEL"
fi

# Diarization: pyannote decides who is speaking when, WeSpeaker decides whether
# two stretches are the same person. Flattened to the layout session.rs expects.
if [ -f pyannote-segmentation-3.0/model.onnx ]; then
  echo "already present: models/pyannote-segmentation-3.0"
else
  echo "fetching pyannote segmentation ..."
  curl -fSL --progress-bar -o seg.tar.bz2 "$SEG_BASE/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2"
  tar xjf seg.tar.bz2
  mkdir -p pyannote-segmentation-3.0
  mv sherpa-onnx-pyannote-segmentation-3-0/model.onnx pyannote-segmentation-3.0/
  rm -rf seg.tar.bz2 sherpa-onnx-pyannote-segmentation-3-0
  echo "ready: models/pyannote-segmentation-3.0"
fi

if [ -f wespeaker_en_voxceleb_resnet34_LM.onnx ]; then
  echo "already present: models/wespeaker_en_voxceleb_resnet34_LM.onnx"
else
  echo "fetching WeSpeaker embeddings ..."
  curl -fSL --progress-bar -o wespeaker_en_voxceleb_resnet34_LM.onnx \
    "$SPK_BASE/wespeaker_en_voxceleb_resnet34_LM.onnx"
  echo "ready: models/wespeaker_en_voxceleb_resnet34_LM.onnx"
fi

if [ -f silero_vad.onnx ]; then
  echo "already present: models/silero_vad.onnx"
else
  echo "fetching Silero VAD ..."
  curl -fSL --progress-bar -o silero_vad.onnx \
    https://github.com/snakers4/silero-vad/raw/v4.0/files/silero_vad.onnx
  echo "ready: models/silero_vad.onnx"
fi
