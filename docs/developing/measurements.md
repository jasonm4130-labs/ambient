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
[--model <dir>]`, on 2026-09-05. The harness runs what `ambient record` runs
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

`cargo run --release --bin wer -- --manifest ~/.cache/ambient/fixtures/calls/manifest.json`,
on 2026-09-06, against the `calls` fixture: three Earnings-21 conference-bridge
calls, 300 s of each. Each call's reference is its whole human transcript, but
the fixture wav is only the call's first 300 s, so the reference is truncated
to the hypothesis (`wer::score_prefix`) before scoring — otherwise everything
said after the cut would count as deletions.

| Speaker | Seconds | Ref words | S | I | D | WER | Decode | Realtime |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4320211 | 300.00 | 755 | 37 | 32 | 4 | 0.0967 | 7.40 s | 40.6x |
| 4330115 | 300.00 | 773 | 54 | 25 | 5 | 0.1087 | 7.09 s | 42.3x |
| 4341191 | 300.00 | 762 | 43 | 26 | 3 | 0.0945 | 7.22 s | 41.5x |
| **calls total** | 900.00 | 2290 | 134 | 83 | 12 | **0.1000** | 21.71 s | 41.5x |

Substitutions and insertions are of the same order here, unlike the `clean`
total's 39-against-3. Deletions stay small, which is the check that the
truncation is scoring the audio the fixture contains rather than penalising
the call for having ended.

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

## Diarization error rate on the fixture

`cargo run --release --bin der -- [--manifest <path>] [--threshold <f32>]
[--collar <f64>] [--json <path>]`, on 2026-09-05. The harness runs what
`ambient record` runs for a finished session — the model resolution of
`session::diarize_session`, the same pyannote segmentation and WeSpeaker
embedding, `Diarizer::diarize` at the shipped 0.5 threshold — so these are the
numbers a user gets. Scored at the conventional 0.25 s collar; `--collar 0`
scores every frame instead.

| Meeting | Seconds | Ref spk | Hyp spk | Missed | False alarm | Confusion | DER | Run |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ES2004a | 300.00 | 3 | 6 | 9.65 s | 4.49 s | 2.61 s | 0.1279 | 1.90 s |
| IS1009a | 300.00 | 4 | 7 | 4.80 s | 8.21 s | 17.23 s | 0.2058 | 2.21 s |
| TS3003a | 300.00 | 4 | 6 | 15.69 s | 2.27 s | 7.08 s | 0.1116 | 2.67 s |
| **total** | 900.00 | 11 | 19 | 30.14 s | 14.97 s | 26.92 s | **0.1434** | 6.77 s |

The total is computed over the summed error seconds rather than averaged over
the rows, so a meeting with less speech in it cannot outvote a talkative one.
Speaker counts sum rather than dedupe: speaker 0 of one meeting is not speaker
0 of the next.

Every meeting over-clusters: 19 hypothesis speakers against 11 in the
reference, and IS1009a pays for it with 17.23 s of confusion and a DER nearly
double the other two. Missed speech is still the largest component at 30.14 s
across the three, ahead of confusion at 26.92 s, so the shipped threshold is
not the only thing between this fixture and a lower number. What the threshold
alone can reach is the next task's sweep, scored on this same harness.

`--json` writes the same rows, the total among them with
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

## Diarization error rate on the fixture

`cargo run --release --bin der -- [--manifest <path>] [--threshold <f32>]
[--collar <f64>] [--json <path>]`, on 2026-09-05. The harness runs what
`ambient record` runs for a finished session — the model resolution of
`session::diarize_session`, the same pyannote segmentation and WeSpeaker
embedding, `Diarizer::diarize` at the shipped 0.5 threshold — so these are the
numbers a user gets. Scored at the conventional 0.25 s collar; `--collar 0`
scores every frame instead.

| Meeting | Seconds | Ref spk | Hyp spk | Missed | False alarm | Confusion | DER | Run |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ES2004a | 300.00 | 3 | 6 | 9.65 s | 4.49 s | 2.61 s | 0.1279 | 1.90 s |
| IS1009a | 300.00 | 4 | 7 | 4.80 s | 8.21 s | 17.23 s | 0.2058 | 2.21 s |
| TS3003a | 300.00 | 4 | 6 | 15.69 s | 2.27 s | 7.08 s | 0.1116 | 2.67 s |
| **total** | 900.00 | 11 | 19 | 30.14 s | 14.97 s | 26.92 s | **0.1434** | 6.77 s |

The total is computed over the summed error seconds rather than averaged over
the rows, so a meeting with less speech in it cannot outvote a talkative one.
Speaker counts sum rather than dedupe: speaker 0 of one meeting is not speaker
0 of the next.

Every meeting over-clusters — 19 hypothesis speakers against 11 in the
reference — and IS1009a pays for it with 17.23 s of confusion and a DER nearly
double the other two. It is still not where most of the error is. Missed
speech is the largest single component at 30.14 s, and `--threshold` cannot
touch it: swept from 0.4 to 0.9 it leaves missed and false alarm at exactly
30.14 s and 14.97 s while confusion ranges from 16.00 s to 58.71 s.
`Diarizer::diarize` takes one winning speaker per frame, so relabelling
clusters cannot change how many speakers are heard at once — and the
references carry 21.5 s of overlapping speech that a single-label output has
no way to attribute. Those 45.11 s put a floor of 0.090 under the DER on this
fixture that no clustering change reaches; the rest is segmentation.

The threshold does move confusion, and not monotonically: 0.6 scores 0.1217
against the shipped 0.5's 0.1434, and 0.7 gives 0.1375 back. Three meetings is
not a holdout to move the default on. Speed is not the constraint: 900 s of
audio separated in 6.77 s.

`--json` writes the same rows as
`{"meeting", "seconds", "reference_speakers", "hypothesis_speakers",
"missed_s", "false_alarm_s", "confusion_s", "der", "run_s"}`, the total among
them with `meeting` reading `total`.

## DER by threshold

`cargo run --release --bin der -- --threshold <t> --json <tmp>`, on 2026-09-05,
on the same fixture and the same 0.25 s collar as the section above. Each
meeting cell is that meeting's DER with the hypothesis speaker count beside it;
the reference count is in the header.

| Threshold | ES2004a (ref 3) | IS1009a (ref 4) | TS3003a (ref 4) | Total DER |
| ---: | ---: | ---: | ---: | ---: |
| 0.30 | 0.2242 (12 spk) | 0.3204 (22 spk) | 0.2739 (17 spk) | 0.2745 |
| 0.40 | 0.1538 (9 spk) | 0.2332 (13 spk) | 0.1332 (8 spk) | 0.1678 |
| **0.50 (shipped)** | **0.1279 (6 spk)** | **0.2058 (7 spk)** | **0.1116 (6 spk)** | **0.1434** |
| 0.60 | 0.1218 (5 spk) | 0.1741 (5 spk) | 0.0873 (4 spk) | 0.1217 |
| 0.70 | 0.1163 (4 spk) | 0.2443 (3 spk) | 0.0800 (2 spk) | 0.1375 |
| 0.80 | 0.1157 (3 spk) | 0.3588 (2 spk) | 0.0800 (2 spk) | 0.1709 |

The default holds at 0.5. The lowest total is 0.6 at 0.1217, and its speaker
counts stay inside twice the reference on every meeting, but the keep rule for
this sweep asks for more than 0.01 of DER on *every* meeting and ES2004a gives
0.0061 — 0.1279 to 0.1218. One meeting of three carrying a change to a shipped
constant is exactly what that margin is there to refuse.

Missed speech and false alarm do not move: 30.14 s and 14.97 s at all six
thresholds. Clustering only relabels spans, so the whole sweep is confusion,
from 16.00 s at 0.6 to 92.78 s at 0.3. Nor is the curve monotonic — past 0.6 the
total climbs back to 0.1375 and 0.1709 while ES2004a keeps improving, because
IS1009a merges its four speakers down to three and then two and pays 22.89 s
and 39.72 s of confusion for it. A threshold that suits one meeting is already
wrong for another at three meetings; that is the argument for a holdout, not
for a new constant.

## DER by minimum embedding length

`MIN_EMBED` in `src/diarize.rs` swapped between builds, `cargo run --release
--bin der -- --json <tmp>` each time, on 2026-09-05, on the same fixture and
the same 0.25 s collar as the sections above. A (window, local speaker)
candidate holding less than `MIN_EMBED` of audio is never embedded, so it
never becomes a cluster: raising the constant drops the short fragments that
would have been their own speaker, and lowering it admits them.

| `MIN_EMBED` | ES2004a (ref 3) | IS1009a (ref 4) | TS3003a (ref 4) | Missed | Total DER |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0.5 s | 0.1315 (9 spk) | 0.2058 (9 spk) | 0.1184 (10 spk) | 30.11 s | 0.1474 |
| **1.0 s (shipped)** | **0.1279 (6 spk)** | **0.2058 (7 spk)** | **0.1116 (6 spk)** | **30.14 s** | **0.1434** |
| 1.5 s | 0.1279 (6 spk) | 0.2032 (7 spk) | 0.1044 (4 spk) | 30.14 s | 0.1394 |
| 2.0 s | 0.1386 (5 spk) | 0.1578 (5 spk) | 0.0877 (1 spk) | 34.87 s | 0.1215 |

The default holds at one second. No value beats it by more than 0.01 of DER on
every meeting, which is what the keep rule asks: 1.5 s ties ES2004a exactly at
0.1279, and 2.0 s — the lowest total on the page at 0.1215 — is 0.0107 *worse*
on ES2004a, so the two candidates fail on the same meeting from opposite
directions.

Unlike the threshold sweep, this constant does move missed speech, because it
decides what gets embedded rather than how embeddings cluster: 30.11 s at
0.5 s against 34.87 s at 2.0 s, and ES2004a alone pays 9.65 s → 12.67 s of that.
It buys confusion back — 28.47 s down to 13.06 s over the same range — and the
totals are the trade netting out, not one term improving.

The 2.0 s row is worth reading before believing its total. TS3003a scores
0.0877 there with **one** hypothesis speaker against four in the reference,
and zero confusion, because that meeting is very nearly a monologue: its
reference gives 242.03 s of 243.80 s of speech to `MTD009PM` and the other
three speakers 10.25 s between them. Calling the whole meeting one person is
almost right there and would be badly wrong anywhere else. IS1009a is the
honest part of that row — 0.2058 to 0.1578, seven speakers to five, on a
meeting with four real ones — and it is not enough on its own.

## Live transcription: cost per block

M5 Max, 128 GB, macOS 26.6.2, on 2026-09-06. `cargo build --release --bin
asrbench`, then the built binary run directly (not under `cargo run`, so
`/usr/bin/time -l`'s rusage is the binary's own, not cargo's) with no `--wav`,
so it replays the manifest's first entry — the 1089 fixture upsampled once to
`~/.cache/ambient/bench/1089.48k.wav` (302.11 s):

```
/usr/bin/time -l <target>/release/asrbench --block-seconds 30 --json <path>
```

The harness cuts native-rate 48 kHz audio into `--block-seconds` blocks, each
resampled to 16 kHz, run through VAD, its turns decoded — a turn ending within
`PAD_MS` of a block edge is carried into the next block rather than cut.
`model_load_s` is a one-time cost measured outside the per-block figures below.

Total row, 30 s blocks:

| blocks | audio_s | prep_s | decode_s | rtf | max_block_work_s | model_load_s |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 11 | 302.11 | 0.881 | 5.966 | 0.023 | 0.866 | 0.607 |

Lag rows, 30 s blocks — `tracks=N` is the workload once through; `tracks=N
(x2)` is `bench::repeat(rows, 2)`, the same blocks played twice in a row to
tell a bounded backlog from a growing one:

| | max_lag_s | final_lag_s |
| --- | ---: | ---: |
| tracks=1 | 0.866 | 0.125 |
| tracks=1 (x2) | 0.866 | 0.125 |
| tracks=2 | 1.731 | 0.249 |
| tracks=2 (x2) | 1.731 | 0.249 |

The `(x2)` rows land on exactly the same numbers as their single-pass
counterparts, at both track counts. That is the finding, not a coincidence:
at an `rtf` of 0.023 each block finishes long before the next one arrives, so
a worker that repeats the workload never falls further behind than it did the
first time through — a bounded backlog, not a growing one.

`maximum resident set size` from `/usr/bin/time -l` (bytes on macOS, divided
by 1048576 for MB): **2038 MB** peak for the 30 s run.

The single most expensive block is index 5 (`end_s=180.00`): `prep_s=0.107`,
`decode_s=0.758`, `turns=3`, `prep_s + decode_s = 0.866 s` — the block behind
`max_block_work_s` above.

The benchmark pays `Vad`'s per-call recurrent-state reset on every block, and
accepts it, because a live pass built on the current `Vad` would pay it too.

### Verdict

The rule was fixed before these numbers were seen: a **go** requires, at 30 s
blocks, `tracks=2` `max_lag_s` under 30, the `tracks=2` doubled-workload
`max_lag_s` within 1 s of it, the `tracks=2` implied `rtf` — `2 ×
(prep_s + decode_s) / audio_s` — under 0.5, and the worst drain overshoot
(see the section below) under 1 s; otherwise a **no-go** naming the
constraint that failed.

| block_seconds | prep_s + decode_s | rtf | tracks=2 max_lag_s | tracks=2 (x2) max_lag_s |
| ---: | ---: | ---: | ---: | ---: |
| 10 | 7.064 | 0.023 | 0.859 | 0.859 |
| 20 | 6.864 | 0.023 | 1.390 | 1.390 |
| 30 | 6.846 | 0.023 | 1.731 | 1.731 |

At 30 s blocks `tracks=2` `max_lag_s` is 1.731 s, under 30. The doubled
workload (`tracks=2 (x2)`) is also 1.731 s, 0.000 s from the single pass, well
within 1 s. The implied `rtf` is `2 × (0.881 + 5.966) / 302.11 = 13.692 /
302.11 = 0.045`, under 0.5. The worst drain overshoot across the three runs
below is 10.87 ms = 0.011 s, under 1 s. All four clauses hold: **go**.

A go means a follow-up plan is in scope, and these are the facts it must
respect. `session::transcribe_session` opens `raw.jsonl` with
`std::fs::File::create` before the models load, deliberately: the file's
existence is what the window's "Interrupted" test reads, and `File::create`
truncates, so a second transcriber on the same session must be refused rather
than allowed to race the first for that file — `claim_transcription` is what
refuses it today. `capture_into` writes both tracks through `hound` writers
and only calls `finalize()` on them when the drain loop ends, so the RIFF and
data length fields in a still-growing capture's WAV header are stale until
then; anything that reads a capture before it stops cannot trust that header's
length. The overshoot number above assumes the ASR worker runs at
`QOS_CLASS_BACKGROUND`, the same class `queue::Queue` uses for a real
transcription job; the per-block costs in the table above, by contrast, were
measured on `asrbench`'s main thread at default QoS with nothing else
running, so they are optimistic relative to a background-QoS live worker.
And `asr::Recognizer::transcribe_segments` takes `&mut self` — ADR-0015's
serial queue transcribes one job at a time on one long-lived thread, so the
`tracks=2` rows above are one worker carrying two tracks' work, not two
workers running at once.

## Live transcription: drain overshoot beside ASR

M5 Max, 128 GB, macOS 26.6.2, on 2026-09-06, from the three `drainbench` runs
already logged for this outcome: `cargo run --release --bin drainbench -- 60
~/.cache/ambient/bench/1089.48k.wav`, each exit 0. The worst run's max
overshoot is **10.87 ms** (run 2, under ASR).

| run | periods | decodes completed | mean overshoot | max overshoot | ring margin |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 (idle) | 25 | — | 6.38 ms | 10.03 ms | 0.033% of 30 s |
| 1 (under ASR) | 289 | 4 | 8.00 ms | 10.10 ms | 0.034% of 30 s |
| 2 (idle) | 25 | — | 6.91 ms | 10.03 ms | 0.033% of 30 s |
| 2 (under ASR) | 289 | 4 | 8.23 ms | 10.87 ms | 0.036% of 30 s |
| 3 (idle) | 25 | — | 7.06 ms | 10.03 ms | 0.033% of 30 s |
| 3 (under ASR) | 289 | 4 | 8.19 ms | 10.08 ms | 0.034% of 30 s |

The idle rows are the same 200 ms drain loop with no ASR beside it; no
capture was started (no tap, per the non-goal). The ASR worker ran at
`QOS_CLASS_BACKGROUND`, which is the QoS the number above assumes. Every run's
verdict line reads the same: `ok — worst overshoot is under 1% of the 30 s
ring`.

---

## Word error rate through the resampler

`cargo run --release --bin wer -- --via <rate>`, on 2026-09-06. Each fixture
wav is transcoded with `ffmpeg -ar <rate> -ac 1 -sample_fmt s16` first, cached
under `~/.cache/ambient/fixtures/via/<rate>/`, then read back with
`resample::read_wav_any` and downsampled with `resample::to_16k` — the same
path Core Audio's 48 kHz delivery (or a 44.1 kHz interface) takes before
`features::read_wav`'s hard 16 kHz check could ever see it. The native row is
`features::read_wav` on the fixture unchanged, restated here rather than
copied from the section above because this is a fresh run. The fixture itself
is 16 kHz LibriSpeech, so the 48 000/44 100 rows measure a round trip through
the resampler on already band-limited audio, not native capture with real
energy above 8 kHz — they exercise the code path, not Core Audio's content.

| Via | Seconds | Ref words | S | I | D | WER |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| native | 913.74 | 2152 | 39 | 3 | 5 | 0.0218 |
| 48 000 | 913.74 | 2152 | 45 | 8 | 6 | 0.0274 |
| 44 100 | 913.74 | 2152 | 48 | 6 | 5 | 0.0274 |

Both resampled rows land 0.0056 above native, more than the 0.005 keep-rule
margin, so the resampler is the suspect: going through a 48 kHz or 44.1 kHz
round trip before `to_16k` costs more than half a point of WER on this
fixture. That is a finding, not a fix — `resample::to_16k` is unchanged by
this section, and no shipped constant moved, so `quality/wer.json` does not
change with it.

## Insertions by gap

`scripts/fetch-fixtures --gap <ms>` and `cargo run --release --bin wer`, on
2026-09-06. The default fixture puts 700 ms of silence between utterances;
`--gap 3000` builds the same three speakers, the same chapters, the same
`.trans.txt` lines with 3 s of silence between them instead, under its own
`~/.cache/ambient/fixtures/wer-3000ms/`. Same words, more silence: both total
rows read 2152 reference words, so the raw insertion count is comparable
between them without normalising for anything. Total seconds rise too — 2.3 s
extra per gap per speaker, 218.49 s over the fixture — so `turns(&samples,
30)` sees different turn boundaries at 3 s than at 700 ms; gap length is not
the only thing that changed between the rows.

| Gap | Seconds | Ref words | S | I | D | WER |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 700 ms | 913.74 | 2152 | 39 | 3 | 5 | 0.0218 |
| 3 s | 1132.23 | 2152 | 40 | 3 | 5 | 0.0223 |

Insertions net out at 3 in both totals (one speaker gains one, another loses
one), and total WER moves 0.0218 → 0.0223, inside the 0.005 keep-rule margin
the resampler section above cites. On this fixture a longer silence between
utterances does not make Parakeet invent words. That is a finding, not a fix:
nothing in the pipeline changed, so `quality/wer.json` does not change with
it. If insertions had risen with gap length, the lever to name would be
`last_confidence` — not tuned here, since it also drops real quiet speech and
needs its own number.

## Turn padding

`cargo run --release --bin wer -- --pad <ms>`, on 2026-09-06. `--pad` overrides
`Vad::pad_ms`, which reaches both places `vad::PAD_MS` is read: the margin
`trim_quiet` re-applies after cutting a turn's quiet edges, and the hysteresis
pad `segments_from` applies before merging overlapping turns. 200 ms is the
shipped default; `cargo run --release --bin wer -- --pad 200` reproduces the
native row from the section above exactly, confirming the parameter reaches
both sites rather than one of them twice. Total reference words hold at 2152
across all four rows — padding does not change what is being scored, only how
much room noise sits at each turn's edges before the recogniser sees it.

| Pad | Seconds | Ref words | S | I | D | WER |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 ms | 913.74 | 2152 | 36 | 7 | 6 | 0.0228 |
| 200 ms (default) | 913.74 | 2152 | 39 | 3 | 5 | 0.0218 |
| 300 ms | 913.74 | 2152 | 39 | 3 | 3 | 0.0209 |
| 400 ms | 913.74 | 2152 | 39 | 3 | 3 | 0.0209 |

The keep rule is per speaker: a value replaces 200 ms only if it beats the
shipped per-speaker WER (`1089` 0.0042, `1188` 0.0270, `121` 0.0667) by more
than 0.005 on every speaker, not just in the total. None do. At 300 ms and
400 ms `1089` barely moves (0.0042 → 0.0028, then flat at 0.0042 — 0.0014 and
0.0000 of the 0.005 margin) while `121` — the shortest speaker at 88.89 s —
gets worse (0.0667 → 0.0889); `1188` improves at both but only by 0.0031 and
0.0039, still under the bar. At 100 ms `1089` gets worse (0.0042 → 0.0097),
`121` is unchanged (0.0667), and `1188`'s 0.0015 gain is again under the
margin; 100 ms is also the only pad value where insertions move at all, 3 at
200 ms and 300 ms up to 7 here. **The default holds.** `src/vad.rs`'s
`PAD_MS` stays at 200 and `quality/wer.json` does not change with it, because
no shipped constant moved.

## Chunk length

`cargo build --release` then `/usr/bin/time -l ./target/release/wer --chunk
<seconds> --json <path>`, on 2026-09-06. `--chunk` sets the `max_seconds`
argument to `Vad::turns` — the length above which a turn is split at its
least-voiced frame, the best available approximation of a pause — not
`Vad::chunks`, a separate method that *merges* turns back together up to
`max_seconds` and which `record` deliberately does not call. Peak RSS is
`maximum resident set size` from `/usr/bin/time -l`, converted from the bytes
Darwin reports (not the kilobytes Linux would) by dividing by 1,048,576;
models load once per manifest entry inside the loop, so each row's figure is
one number for the whole three-speaker run, not a per-speaker one, the same
way the WER section's decode column is qualified. `--chunk 30` reproduces the
native row exactly (913.74 s, 2152 reference words, S=39 I=3 D=5, WER
0.0218), confirming the parameter reaches the shipped path rather than a
second copy of it.

| Chunk | Seconds | Ref words | S | I | D | WER | Peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 15 s | 913.74 | 2152 | 40 | 3 | 5 | 0.0223 | 2063 MB |
| 20 s | 913.74 | 2152 | 39 | 3 | 5 | 0.0218 | 2055 MB |
| 30 s (default) | 913.74 | 2152 | 39 | 3 | 5 | 0.0218 | 2145 MB |
| 40 s | 913.74 | 2152 | 39 | 3 | 5 | 0.0218 | 2147 MB |

This 30 s row's 2145 MB is the figure the keep rule below compares against.
It is not the 2317 MB at the 30 s-chunk level in `## End to end`, the 2158 MB
in the 30 s row of `## Memory scales with audio length`, or the 2255 MB in
`## VAD chunking` — those three predate this sweep and come from `bench` and
`say`-synthesised audio, not from the `wer` binary run over LibriSpeech, so
they are context here, not the bar.

The keep rule is per speaker, and RSS besides: a value replaces the shipped
30 s only if it beats the shipped per-speaker WER (`1089` 0.0042, `1188`
0.0270, `121` 0.0667) by more than 0.005 on every speaker **and** does not
raise peak RSS above the 30 s row above. None do. 20 s and 40 s produce the
identical per-speaker rows as 30 s — no turn in this fixture is long enough
for the cap to matter between 20 s and 40 s, so widening or narrowing it in
that range changes nothing about where turns are split — and 40 s costs 2 MB
more RSS on top of tying, not beating, the WER. 15 s is the one value that
changes anything: `1188` gets worse (0.0270 → 0.0278, one more substitution)
and the total moves from 0.0218 to 0.0223, both the wrong direction, while
`1089` and `121` are unchanged; 15 s also has the lowest RSS of the four,
which is the RSS half of the keep rule doing nothing useful when the WER half
already fails. **The default holds.** `src/session.rs`'s shipped chunk
length stays at 30 and `quality/wer.json` does not change with it, because no
shipped constant moved.

---

Every number on this page is from the 128 GB machine; the 16 GB target is still
unmeasured — see [porting to the work M5](porting.md).
