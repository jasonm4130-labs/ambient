---
title: "4. Chunk at VAD-chosen boundaries in 30 s windows"
sidebar:
  order: 44
---

# 4. Chunk at VAD-chosen boundaries in 30 s windows

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

Parakeet's cost grows with sequence length: measured on v3 int8, peak RSS runs
2158 MB at 30 s, 2594 MB at 60 s, 3721 MB at 120 s and 6291 MB at 300 s, with
time worse than linear because attention cost grows with the sequence.
Extrapolated, a 30-minute session fed whole would want **~35 GB** and would
fail outright on the 16 GB target machine.

Chunking is therefore mandatory, and the only question is where to cut. The
first implementation cut at the quietest point within ±2 s of each 30 s
boundary, which sliced through words: a 127 s file came back containing
"Chunking keeps the memory flat. flat regardless of how long the meeting
actually runs."

## Considered Options

- **No chunking**, and require enough RAM.
- **Energy-chosen boundaries** — cut at the local minimum near each 30 s mark.
- **VAD-chosen boundaries** — run Silero VAD first and cut only between speech
  segments.

## Decision Outcome

Transcribe in 30 s windows, with boundaries chosen by Silero VAD
(`models/silero_vad.onnx`, 629 KB) rather than by local energy. Where a single
speech segment is longer than the window it is split at the least-voiced frame
near the boundary, which is the closest thing to a pause when there is no real
one.

That split is not incidental: building the VAD pass exposed a bug in the
energy chunker it replaced, where an uninterrupted speech segment was never
split at all, so continuous speech still reached the encoder whole and a long
monologue would have exhausted memory exactly as an unchunked file did.

Cutting on speech boundaries, the same 127 s file yields 375 words with **zero
adjacent duplicates**.

## Consequences

Peak memory is flat at ~2255 MB regardless of session length, and a 30-minute
session becomes ~60 chunks at roughly 30 s of compute. This is what makes the
16 GB machine viable and what let the design budget move from "< 2 GB on the
ANE" to "~2.2 GB during a burst, negligible while capturing".

Throughput drops from 54× to 39× realtime, because VAD runs one inference per
32 ms frame. Silence is also never sent to the encoder, which is the smaller
of the two benefits.

The thresholds are a knob, not a settled value: VAD discarded a television
audible behind the mic recording, which is welcome for noise and is the same
mechanism that would drop quiet speech that mattered.

## Confirmation

The trimming and floor behaviour is pinned by the unit tests in `src/vad.rs` —
`a_turn_loses_its_quiet_lead_in`, `speech_throughout_is_left_alone`,
`a_quiet_recording_is_judged_against_itself` and
`a_noise_only_track_is_no_longer_rejected_here` — which run in CI under
`cargo test --all-targets`.

The memory claim has no automated check, because CI never transcribes. To
confirm it a person runs `ambient transcribe` on a multi-minute recording
under `/usr/bin/time -l` and checks `maximum resident set size` stays near
2.3 GB rather than climbing with the file. `cargo run --release --bin bench`
reproduces the length-versus-memory curve that motivated the decision.
