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

## Word error rate on the fixture

`cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
[--model <dir>] [--via <hz>] [--via-ffmpeg]`, on 2026-09-05. The harness runs what `ambient record` runs
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

## What the resampler costs: native, 48 k and 44.1 k

`cargo run --release --bin wer -- --via <hz>` upsamples each fixture wav to
`<hz>` with `ffmpeg`, caches it under `~/.cache/ambient/fixtures/via/<hz>/`,
and reads it back through `resample::to_16k` before the same VAD turns and the
same decode. The fixture is native 16 kHz, so without `--via` the resampler
every real recording goes through — Core Audio delivers 48 kHz — never runs at
all.

`--via-ffmpeg` is the control that makes those numbers readable. It takes the
identical round trip with `ffmpeg` on the return leg too, from the same cached
intermediate file, so `resample::to_16k` never runs and what is left is
everything a `--via` run shares with it: the upsample, the `pcm_s16le`
intermediate, and a second band-limiting pass. Measured on 2026-09-05, this
machine, v3-int8, all five runs in one sweep.

| Path | Speaker | Seconds | Ref words | S | I | D | WER |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| native 16 k | 1089 | 302.11 | 721 | 3 | 0 | 0 | 0.0042 |
| native 16 k | 1188 | 522.73 | 1296 | 29 | 2 | 4 | 0.0270 |
| native 16 k | 121 | 88.89 | 135 | 7 | 1 | 1 | 0.0667 |
| **native 16 k** | **total** | 913.74 | 2152 | 39 | 3 | 5 | **0.0218** |
| 48 k, ours | 1089 | 302.11 | 721 | 3 | 3 | 0 | 0.0083 |
| 48 k, ours | 1188 | 522.73 | 1296 | 36 | 4 | 5 | 0.0347 |
| 48 k, ours | 121 | 88.89 | 135 | 6 | 1 | 1 | 0.0593 |
| **48 k, ours** | **total** | 913.74 | 2152 | 45 | 8 | 6 | **0.0274** |
| 48 k, ffmpeg | 1089 | 302.11 | 721 | 4 | 2 | 1 | 0.0097 |
| 48 k, ffmpeg | 1188 | 522.73 | 1296 | 37 | 4 | 5 | 0.0355 |
| 48 k, ffmpeg | 121 | 88.89 | 135 | 9 | 1 | 1 | 0.0815 |
| **48 k, ffmpeg** | **total** | 913.74 | 2152 | 50 | 7 | 7 | **0.0297** |
| 44.1 k, ours | 1089 | 302.11 | 721 | 4 | 3 | 0 | 0.0097 |
| 44.1 k, ours | 1188 | 522.74 | 1296 | 35 | 2 | 4 | 0.0316 |
| 44.1 k, ours | 121 | 88.89 | 135 | 9 | 1 | 1 | 0.0815 |
| **44.1 k, ours** | **total** | 913.74 | 2152 | 48 | 6 | 5 | **0.0274** |
| 44.1 k, ffmpeg | 1089 | 302.11 | 721 | 4 | 2 | 0 | 0.0083 |
| 44.1 k, ffmpeg | 1188 | 522.73 | 1296 | 35 | 4 | 4 | 0.0332 |
| 44.1 k, ffmpeg | 121 | 88.89 | 135 | 9 | 1 | 1 | 0.0815 |
| **44.1 k, ffmpeg** | **total** | 913.74 | 2152 | 48 | 7 | 5 | **0.0279** |

`resample::to_16k` is not what the round trip costs. Going through 48 kHz and
back costs 0.0056 of word error rate with our resampler on the return leg —
0.0274 against 0.0218 native, over the half-point line this plan uses — and
0.0079 with ffmpeg's own `swr` on that leg instead, 0.0297. Removing our code
from the path made the score worse, by five words in 2152. At 44.1 k the two
legs are 0.0274 and 0.0279, the same story inside a word or two. On this
fixture rubato is at worst indistinguishable from the resampler ffmpeg ships,
and the half point belongs to the round trip itself: an upsample, an s16
intermediate, and two band-limiting passes where a real recording has one.

That is the reading the earlier version of this section got wrong, and the
control is what separates them: a `--via` run alone cannot tell our resampler
apart from the ffmpeg upsample it is built on.

The `Seconds` column answers the other thing a resampler can get wrong. Every
path returns the same durations: 913.735 s for four of the five runs and
913.7351 s for 44.1 k through `to_16k` — two samples in 14.6 million, on the
one ratio that is not an integer. Nothing is being lost or duplicated at
441:160 any more than at 3:1.

Speaker 121 is 135 reference words, so a single word moves it 0.0074 and its
0.0593-to-0.0815 spread across these paths is not signal. The 1188 rows carry
the weight and they move by two or three words across every resampled path.

The plan's stop condition is met numerically — 0.0274 against 0.0218 is over
half a point — but the suspect it hands to the next task is the round trip, not
`resample::to_16k`, and this run decided it that way rather than leaving the
question open. A task spent tuning rubato would be chasing ffmpeg's `swr`. What
no fixture of native 16 kHz audio can measure is the thing that actually ships:
one downsample of genuinely 48 kHz audio, with no upsample before it. That
needs a fixture recorded at 48 kHz, and it is the task worth writing.

The decode timings are off this table on purpose: the sweep ran under a load
average near 8 on 18 cores and returned 38-42x realtime against the 46x
recorded above for the identical native run. The counts are what this table
needs and they are solid under that load — this sweep reproduced the native
39/3/5 and both earlier `--via` totals, 45/8/6 at 48 k and 48/6/5 at 44.1 k,
to the last substitution. The clock is not.

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

---

Every number on this page is from the 128 GB machine; the 16 GB target is still
unmeasured — see [porting to the work M5](porting.md).
