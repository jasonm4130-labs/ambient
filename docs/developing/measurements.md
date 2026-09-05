---
title: "Measurements"
sidebar:
  order: 28
---

# Measurements

Everything below was measured on the home machine — M5 Max, 128 GB,
macOS 26.6.2 — on 2026-08-29. Any table measured differently says so.

## End to end: decode, realtime and peak RSS

v3-int8, synthesised speech via `say`.

| Audio | Decode | Realtime | Peak RSS |
| ---: | ---: | ---: | ---: |
| 5 s | 0.12 s | 42× | — |
| 16.5 s | 0.32 s | 52× | — |
| 127.5 s (chunked) | 2.38 s | 54× | **2317 MB** |

Peak memory on the 127 s file is flat at the 30 s-chunk level rather than the
~3.7 GB an unchunked 120 s run needed, which is the chunking working.

## Phase 0: provider comparison, CPU against CoreML

Parakeet TDT 0.6b encoder, 60 s of audio, via
`cargo run --release --bin bench -- <encoder.onnx> <coreml|cpu> [seconds]`.
Input is a zeroed tensor of the correct shape — wall clock for a fixed-shape
graph is content-independent, so the comparison holds, but these are not
accuracy numbers.

| Model | Provider | Steady run | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| v2 fp16 | CPU | 1013 ms | 59× | 3745 MB |
| v2 fp16 | CoreML (ANE requested) | 1012 ms | 59× | 3767 MB |
| v3 int8 | CPU | 1029 ms | 58× | 2594 MB |
| v3 int8 | CoreML (ANE requested) | 1670 ms | 36× | 16482 MB |

On fp16 CoreML is identical to CPU to within a millisecond. On int8 it is
actively harmful — 1.6× slower and 16.5 GB peak, which would OOM a 16 GB machine
outright.

## Memory scales with audio length

v3 int8 on CPU.

| Audio | Steady run | Realtime | Peak RSS |
| ---: | ---: | ---: | ---: |
| 30 s | 488 ms | 62× | 2158 MB |
| 60 s | 1029 ms | 58× | 2594 MB |
| 120 s | 2252 ms | 53× | 3721 MB |
| 300 s | 7539 ms | 40× | 6291 MB |

Roughly linear in memory and worse than linear in time. Extrapolated, a
30-minute session fed whole would want ~35 GB and would fail on the base M5;
chunked at 30 s it is ~60 chunks ≈ 30 s of compute at a flat ~2.2 GB.

## VAD chunking: speech, chunks, realtime and RSS

| File | Speech / total | Chunks | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| 127 s continuous | 127.5 / 127.5 s | 6 | 39x | 2255 MB |
| mic track | 3.6 / 9.7 s | 1 | 14x | — |

The VAD pass costs throughput — 39x against 54x without — because it runs one
inference per 32 ms frame. The same 127 s file yields 375 words with zero
adjacent duplicates once cut on VAD boundaries rather than on local energy.

## Embedding front-end: confusion matrix

`bin/embtest.rs`, on two clips each of two `say` voices.

```
           A1     A2     B1     B2
    A1  1.000  0.880  0.192  0.138
    A2  0.880  1.000  0.145  0.132
    B1  0.192  0.145  1.000  0.823
    B2  0.138  0.132  0.823  1.000

same voice 0.851   cross voice 0.152   separation 0.700
```

A wrong filterbank does not error — it collapses that matrix toward uniform and
the damage arrives as confidently mislabelled speakers.

## Diarization cost on a 10.2-minute track

Peak RSS **249 MB**, 5.9 s wall. The sliding window does not accumulate.

## Three signals, none of which separates quiet speech from an empty room

| Signal | Real quiet speech | Hallucination from silence |
| --- | --- | --- |
| level (p90) | −42 dB | −44 dB |
| Silero probability | 0.6–0.9 | 0.6–0.9 |
| decoder confidence | 0.959 | 0.917 |

Two decibels apart, so any absolute level threshold that rejects the empty room
also rejects real speech. Decoder confidence does catch the *derailment* case —
0.992 for the trimmed utterance against 0.613 for the same one with noise in
front.

## What noise in front of an utterance does to the decode

| Fed to the recogniser | Out |
| --- | --- |
| 2.6 s of room noise + the utterance | "The gap had not closed. If anything, it had wide." |
| the utterance alone | "The migration is scheduled for Thursday morning." |

A noise prefix does not merely add junk; it derails the decode into a different
sentence outright.

## Fixtures

Word error rate is measured against LibriSpeech test-clean (CC BY 4.0,
[openslr.org/12](https://www.openslr.org/12)). Build it with:

```sh
scripts/fetch-fixtures            # cached after the first run
scripts/fetch-fixtures --force    # rebuild the stitched fixture
```

Everything lands in `~/.cache/ambient/`, never in the repo: the archive is
330 MB and the licence is the corpus's, not ours. The archive caches at
`~/.cache/ambient/librispeech/`; the fixture the harness reads is
`~/.cache/ambient/fixtures/wer/`, holding a wav and a reference transcript per
speaker and a `manifest.json` naming both.

The three speakers are the first three directories of `test-clean` in sorted
order, each one's first chapter — which audio this scores is a property of the
corpus, not a list someone typed. A speaker's utterances are concatenated in id
order with 700 ms of silence between them, so the fixture is one long track of
the kind `ambient record` sees and the gaps give the VAD turn boundaries to
find.

| Speaker | Utterances | Seconds |
| --- | ---: | ---: |
| 1089 | 38 | 302.11 |
| 1188 | 45 | 522.73 |
| 121 | 15 | 88.89 |

A second run does no network and rewrites nothing; `--force` restitches from
the extracted corpus without re-downloading it.

Diarization error rate is measured against AMI Mix-Headset audio (CC BY 4.0,
[groups.inf.ed.ac.uk/ami](https://groups.inf.ed.ac.uk/ami/corpus/)) with the
pyannote
[only_words](https://github.com/pyannote/AMI-diarization-setup/tree/67c2d539286e89f68952d5dcf83912bd9f01dfae/only_words)
references, pinned to that revision. The same command builds it:

```sh
scripts/fetch-fixtures            # cached after the first run
scripts/fetch-fixtures --force    # recut the 300 s clips
```

The meetings are `ES2004a`, `IS1009a` and `TS3003a` — the first meeting of
three AMI test-set series, one per recording site, so which meetings this
scores is a property of the corpus and not a list someone typed. The full
recordings cache at `~/.cache/ambient/ami/`; the fixture the harness reads is
`~/.cache/ambient/fixtures/der/`, holding each meeting's first 300 s as 16 kHz
mono 16-bit wav, the reference RTTM cut to match (turns starting past 300 s
dropped, a turn straddling the cut clipped), and a `manifest.json` naming both.

All six downloads are pinned by `shasum -a 256` in the script and checked on
every run, so an upstream re-encode or a reference edit fails the fetch rather
than moving the DER baseline underneath the quality gate.

This set needs `ffmpeg` and `ffprobe` on `PATH`; the WER set does not, and
still builds without them. A machine missing either is told so on the first
line and gets the WER set only — the DER harness then reports the missing
manifest and names this script.

| Meeting | Seconds | Reference turns |
| --- | ---: | ---: |
| ES2004a | 300 | 41 |
| IS1009a | 300 | 47 |
| TS3003a | 300 | 36 |

## Word error rate on the fixture

`cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
[--model <dir>] [--via <rate>]`, on 2026-09-05. The harness runs what `ambient record` runs
for a finished track — `session::model_paths`, `Vad::turns(&samples, 30)`,
`transcribe_segments` — so these are the numbers a user gets, on the shipped
v3-int8 model. The decode column is one run; see the section below for how far
it moves between runs.

| Speaker | Seconds | Ref words | S | I | D | WER | Decode | Realtime |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1089 | 302.11 | 721 | 3 | 0 | 0 | 0.0042 | 6.61 s | 46x |
| 1188 | 522.73 | 1296 | 29 | 2 | 4 | 0.0270 | 11.12 s | 47x |
| 121 | 88.89 | 135 | 7 | 1 | 1 | 0.0667 | 2.03 s | 44x |
| **total** | 913.74 | 2152 | 39 | 3 | 5 | **0.0218** | 19.77 s | 46x |

The total is computed over the summed counts rather than averaged over the
rows, so the 89-second speaker cannot outvote the 522-second one. Substitutions
dominate at 39 against 3 insertions and 5 deletions: on clean read speech the
segmenter is not losing words and the recogniser is not inventing them, it is
getting them wrong. `--json` writes the same rows, the total among them with
`speaker` reading `total`.

## The two shipped models on the same fixture

Both Parakeet builds in `models/`, same fixture, same VAD turns, `--model` the
only difference — on 2026-09-05, this machine. Three runs of each, interleaved
so both models see the same machine load; the table is each model's median run
by total decode.

| Model | Speaker | Seconds | Ref words | S | I | D | WER | Decode | Realtime |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| v3 int8 | 1089 | 302.11 | 721 | 3 | 0 | 0 | 0.0042 | 7.73 s | 39x |
| v3 int8 | 1188 | 522.73 | 1296 | 29 | 2 | 4 | 0.0270 | 12.69 s | 41x |
| v3 int8 | 121 | 88.89 | 135 | 7 | 1 | 1 | 0.0667 | 2.16 s | 41x |
| **v3 int8** | **total** | 913.74 | 2152 | 39 | 3 | 5 | **0.0218** | **22.58 s** | **40x** |
| v2 fp16 | 1089 | 302.11 | 721 | 1 | 0 | 0 | 0.0014 | 6.50 s | 46x |
| v2 fp16 | 1188 | 522.73 | 1296 | 26 | 1 | 1 | 0.0216 | 14.84 s | 35x |
| v2 fp16 | 121 | 88.89 | 135 | 4 | 1 | 1 | 0.0444 | 3.50 s | 25x |
| **v2 fp16** | **total** | 913.74 | 2152 | 31 | 2 | 2 | **0.0163** | **24.84 s** | **37x** |

The two columns are not equally trustworthy, and the runs say which is which.
Every run of a model returned the same counts to the last substitution — v3 at
39/3/5 and v2 at 31/2/2, v3's reproducing `quality/wer.json` exactly — so the
accuracy gap is a property of the models: 0.0163 against 0.0218, half a point
of word error rate, and v2 fp16 wins on each speaker individually rather than
on one fixture carrying it.

Decode seconds are the opposite. Across the three runs the total moved between
21.59 s and 27.44 s for v3 and between 20.80 s and 26.15 s for v2 — ranges that
overlap for most of their length, on a machine with other work on it. The
19.77 s recorded for v3 in the section above is a further run of the same
binary on the same fixture, and it sits below all three here. So this fixture
on this machine does not separate the two models on speed at all, and any
decode figure quoted from a single run, including the ones in the table above,
is worth roughly ±3 s. That is why `scripts/quality` gates the counts and not
the timings.

Accuracy and speed are not the whole decision, and here only accuracy is
measured well. The Phase 0 table above puts v2 fp16 at 3745 MB peak against v3
int8's 2594 MB for the same 60 s of audio, and the 16 GB target is still
[unmeasured](porting.md). The default stays v3 int8 — `session::model_paths`
with no `--model` — because switching spends 1.1 GB of headroom on a machine
nobody has run this on to buy half a point of word error rate. Measure the port
first; the table says which way it should then go.

## What the sample-rate conversion costs

The fixture is native 16 kHz, so `resample::to_16k` returns its input
untouched and the word error rate above never measured it. A real capture
always crosses it: Core Audio hands back 48 kHz on this Mac and 44.1 kHz on
some interfaces. `--via <rate>` upsamples each fixture wav to that rate with
`ffmpeg` once, caches it under `~/.cache/ambient/fixtures/via/<rate>/`, and
reads it back through `resample::to_16k`. v3 int8, same VAD turns, same
fixture — on 2026-09-05, this machine:

```sh
cargo run --release --bin wer                     # native 16 kHz
cargo run --release --bin wer -- --via 48000
cargo run --release --bin wer -- --via 44100
```

| Path | Seconds | Ref words | S | I | D | WER | Decode | Realtime |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| native 16 kHz | 913.74 | 2152 | 39 | 3 | 5 | **0.0218** | 21.16 s | 43.2x |
| via 48 kHz | 913.74 | 2152 | 45 | 8 | 6 | **0.0274** | 24.26 s | 37.7x |
| via 44.1 kHz | 913.74 | 2152 | 48 | 6 | 5 | **0.0274** | 28.03 s | 32.6x |

The round trip preserves the duration, so all three rows score the same speech
and the word counts compare directly. `to_16k` sizes its output for the worst
case and truncates to what the resampler produced, which need not be the sample
count it started from — but the 48 kHz path returns all 14,619,760 samples
exactly, and 44.1 kHz returns two more across the three files (one each on 1089
and 1188, none on 121), 0.000125 s in total. Decode seconds, as everywhere on
this page, are worth about ±3 s and separate nothing.

Per speaker at 48 kHz: 1089 0.0083, 1188 0.0347, 121 0.0593 — worse on the two
long speakers, slightly better on the 89-second one. 44.1 kHz reaches the same
0.0274 by a different route, three more substitutions against two fewer
insertions and one fewer deletion, so the cost does not track how awkward the
ratio is: 48 kHz is an exact 3:1
decimation and pays the same as 44.1 kHz, which is not.

**That gap is 0.56 points, over the half-point line the plan drew, so this
stops here and names `resample::to_16k` as the suspect.** Fixing it is a task
of its own. What points at the down-leg rather than the round trip as a whole
is the fixture itself: it is band-limited to 8 kHz, so a 16→48 kHz upsample
adds no information and, done correctly, is near-lossless — the original
samples survive as every third output sample. There is nothing for the up-leg
to lose. Whatever the round trip costs is therefore spent on the way back down,
which is the function under suspicion.

Two things this does not establish, and a fix should not assume. It does not
say the loss is *large*: everything above 8 kHz in the intermediate signal is
conversion artifact, so this is a lower bound on the arithmetic's own noise,
not what genuine 48 kHz microphone audio costs on the way down. Real capture
carries content above 8 kHz that a downsampler has to discard properly, and
nothing here tests that. And it does not say *which* part of `to_16k` is at
fault — the rubato configuration, the chunking, or the truncation — because
this harness only sees the function's output. Measuring the first needs a
fixture recorded at 48 kHz rather than manufactured from a 16 kHz one; that is
a fixture task. The second is a resampler task.

A run's counts are reproducible: `--via 48000` returned 45/8/6 twice. Both runs
read the byte-identical cached wav, so that is the decoder repeating itself —
the same determinism [recorded above](#the-two-shipped-models-on-the-same-fixture)
— and it says nothing about variance in the `ffmpeg` leg, which runs once and
is then cached. The cache entry is named after a hash of the source wav's
bytes, so `fetch-fixtures --force` recutting a fixture produces a new key
rather than scoring stale audio against the new reference.

---

Every number on this page is from the 128 GB machine; the 16 GB target is still
unmeasured — see [porting to the work M5](porting.md).
