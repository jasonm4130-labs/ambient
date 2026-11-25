#!/usr/bin/env bash
# Shipped path vs Ultra + Nemotron 3 (batch and streaming) over PriMock57 two-speaker mixes.
set -u
B=~/Code/ambient-spike/FluidAudio/.build/release/fluidaudiocli
AMB=/private/tmp/claude-501/ambient-bench-target/release
export AMBIENT_MODELS=~/Code/ambient/models
P=~/.cache/ambient/bench/primock; O=~/Code/ambient-spike/bench/out-primock; mkdir -p $O/diar-nemo $O/diar-nemos
$AMB/der --manifest $P/manifest.json --collar 0 --rttm-out $O/diar-onnx > $O/diar-onnx.log 2>&1
for W in $P/wav/*.wav; do
  m=$(basename $W .wav)
  /usr/bin/time -l $B nemotron3-diarize $W --variant fast32 --output $O/diar-nemo/$m.rttm > $O/log-nemo-$m 2>&1
  /usr/bin/time -l $B nemotron3-diarize $W --variant fast32 --streaming --output $O/diar-nemos/$m.rttm > $O/log-nemos-$m 2>&1
  /usr/bin/time -l $B transcribe $W --model-version ultra --word-timestamps --output-json $O/asr-ultra-$m.json > $O/log-ultra-$m 2>&1
  /usr/bin/time -l $AMB/ambient transcribe ~/Code/ambient/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 $W > $O/asr-onnx-$m.txt 2> $O/log-onnx-$m
  echo "$m done"
done
echo ALL DONE
