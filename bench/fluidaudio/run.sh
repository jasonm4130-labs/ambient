#!/usr/bin/env bash
# All candidates over the 16 AMI test meetings, headset mix and single far-field mic.
set -u
B=~/Code/ambient-spike/FluidAudio/.build/release/fluidaudiocli
AMB=/private/tmp/claude-501/ambient-bench-target/release
export AMBIENT_MODELS=~/Code/ambient/models
A=~/.cache/ambient/bench/ami; O=~/Code/ambient-spike/bench/out; mkdir -p $O
M="IS1009a IS1009b IS1009c IS1009d ES2004a ES2004b ES2004c ES2004d TS3003a TS3003b TS3003c TS3003d EN2002a EN2002b EN2002c EN2002d"
mem(){ awk '/peak memory/{printf "%d", $1/1048576}' "$1"; }
for mic in Mix-Headset Array1-01; do
  # baseline diariser via the der harness (writes RTTM per meeting)
  python3 - "$mic" <<PY
import json,os,sys
mic=sys.argv[1]; A=os.path.expanduser("~/.cache/ambient/bench")
M="$M".split()
json.dump([{"wav":f"{A}/ami/amicorpus/{m}/audio/{m}.{mic}.wav","rttm":f"{A}/ami-setup/only_words/rttms/test/{m}.rttm","meeting":m} for m in M],open(f"{os.path.expanduser('~/Code/ambient-spike/bench/out')}/manifest-{mic}.json","w"))
PY
  /usr/bin/time -l $AMB/der --manifest $O/manifest-$mic.json --collar 0 --rttm-out $O/diar-onnx-$mic > $O/diar-onnx-$mic.log 2>&1
  for m in $M; do
    W=$A/amicorpus/$m/audio/$m.$mic.wav
    mkdir -p $O/diar-nemo-$mic
    /usr/bin/time -l $B nemotron3-diarize $W --variant fast32 --output $O/diar-nemo-$mic/$m.rttm > $O/log-nemo-$mic-$m 2>&1
    for v in v3 ultra phonon2; do
      /usr/bin/time -l $B transcribe $W --model-version $v --word-timestamps --output-json $O/asr-$v-$mic-$m.json > $O/log-$v-$mic-$m 2>&1
    done
    /usr/bin/time -l $AMB/ambient transcribe ~/Code/ambient/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 $W > $O/asr-onnx-$mic-$m.txt 2> $O/log-onnx-$mic-$m
    echo "$mic $m done"
  done
done
echo ALL DONE
