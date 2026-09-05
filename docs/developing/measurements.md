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

`cargo run --release --bin wer -- [--manifest <path>] [--json <path>]`, on
2026-09-05. The harness runs what `ambient record` runs for a finished track —
`session::model_paths`, `Vad::turns(&samples, 30)`, `transcribe_segments` — so
these are the numbers a user gets, on the shipped v3-int8 model.

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

## Live transcription: cost per block

`cargo run --release --bin asrbench -- [--wav <path>] [--block-seconds <n>]
[--json <path>]`, on 2026-09-05, on speaker 1089 of the fixture upsampled to
48 kHz — 302 s of audio in eleven 30 s blocks. Each block is resampled, run
through `Vad::turns`, and its turns decoded, which is the work a live
transcriber does and `wer` does not: `wer` gets 16 kHz audio and segments the
whole track once.

| Blocks | Audio | Prep | Decode | RTF | Worst block | Model load | Peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 11 | 302.11 s | 1.99 s | 26.59 s | 0.095 | 3.49 s | 2.47 s | 2050 MB |

RTF is work per second of audio, so 0.095 is a tenth of realtime and the
headroom is roughly 10×. Prep — resample plus VAD — is 1.99 s against 26.59 s
of decode: the per-block resample and VAD pass a live transcriber pays and a
whole-track pass does not costs 7% of the total, not a doubling. The worst
block is block 3 at 0.36 s of prep and 3.13 s of decode over six turns; that
3.49 s is the number the queue simulation turns into lag. Peak RSS is
`maximum resident set size` from `/usr/bin/time -l` on the whole run.

| Workload | Tracks | Max lag | Final lag |
| --- | ---: | ---: | ---: |
| 302 s | 1 | 3.49 s | 0.47 s |
| 302 s | 2 | 6.97 s | 2.86 s |
| 604 s (`repeat`, ×2) | 1 | 3.49 s | 0.47 s |
| 604 s (`repeat`, ×2) | 2 | 6.97 s | 2.86 s |

The backlog is bounded: doubling the workload leaves the maximum lag
unchanged, so the worker drains each block before the next one lands and a
two-hour meeting lags no more than a five-minute one. Two tracks — a room and
a call, which arrive at the same instant — cost the second track a wait behind
the first, and 6.97 s is still one block's audio behind, not a growing queue.
The rule: bounded when the doubled workload's maximum lag is within 1 s of the
single workload's, accumulating when it roughly doubles.

Two things the benchmark charges itself that a reader should know about.
`Vad::turns` resets its recurrent state on every call, so a per-block pass pays
that reset eleven times where a whole-track pass pays it once; the cost is left
in, because a live transcriber built on today's `Vad` would pay it too. And
speech still running at a block's edge is not decoded there — its samples are
carried into the next block — so a long utterance splits at an interior quiet
frame as the whole-track path splits it, rather than at the block boundary; the
carried audio is counted once, where it is decoded, while the cost of preparing
it again lands in the next block's prep.

These are one run's numbers on a machine that had other builds on it. A repeat
run measured RTF 0.149 and a 6.56 s worst block — same verdict, half the
headroom — so read the margin as an order of magnitude, not a constant.

---

Every number on this page is from the 128 GB machine; the 16 GB target is still
unmeasured — see [porting to the work M5](porting.md).
