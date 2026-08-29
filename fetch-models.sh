#!/usr/bin/env bash
# Download every ONNX model ambient needs. ~500 MB in total, dominated by the
# recogniser; diarization adds 32 MB and VAD under a megabyte.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p models && cd models

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
