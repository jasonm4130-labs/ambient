---
title: "20. Transcribe after Stop in a Core ML helper process"
sidebar:
  order: 60
---

# 20. Transcribe after Stop in a Core ML helper process

**Status:** accepted · **Date:** 2026-11-25 · **Supersedes:** [ADR-0004](0004-vad-chunking.md), [ADR-0005](0005-cpu-not-coreml.md), [ADR-0006](0006-separate-frontends.md)

## Context and Problem Statement

Ambient's job, after [ADR 0019](0019-narrow-to-core-recorder.md), is a
diarised blow-by-blow of a meeting or a 1:1. The shipped pipeline runs
Parakeet TDT v3 (int8, sherpa-onnx export) through ONNX Runtime on the CPU,
pyannote segmentation with WeSpeaker clustering for speakers, and a live
transcriber during capture. Measured against references people wrote
(issue #168), it is the weakest part of the product:

- **Words.** On the 16 AMI test meetings it scores 29.7% WER on the headset mix
  and 34.1% on a single far-field mic. About a quarter of reference words are
  deleted in the decode (issue #169).
- **Speakers.** It finds the right number of speakers in 3 of 16 meetings on
  the headset mix and 0 of 16 on the far-field mic.
- **Memory.** Speech-to-text alone peaks near 1.5 GB of process memory, which
  is why each job already runs in a child process (`src/finalize.rs`).

Since ADR 0005, FluidAudio (Apache-2.0) has shipped natively compiled Core ML
ports of Parakeet and of NVIDIA's Nemotron 3 diariser, which run on the
Neural Engine. ADR 0005 rejected ONNX Runtime's Core ML shim, not the Neural
Engine; it said a native Core ML path "would likely behave differently". It
does.

## Considered Options

- **Keep ONNX on the CPU and fix the decode.** Fixing #169 would recover some
  WER. It would not fix speaker counts or memory, and every model upgrade
  would still need a hand-written front end (ADR 0006).
- **Rewrite the app in Swift.** This gains nothing the helper does not, and
  throws away working capture, signing, the window and the session format.
- **A Swift helper process on FluidAudio, called by the Rust app after Stop.**
  Chosen.

## Decision Outcome

Transcription runs after Stop, in `ambient-engine`, a Swift executable built
from `engine/` and shipped beside `ambient` in the app bundle. Its models are:

- Parakeet TDT 0.6b **Ultra** for speech-to-text (CC-BY-4.0);
- **Nemotron 3 Diarization**, `fast32` preset, fed through its streaming path,
  for speakers (OpenMDW 1.1).

Each call does one stage for one track and writes one JSON file atomically.
`transcribe` writes words with times and confidences. `diarize` writes
speaker turns. The Rust side keeps everything else: it turns words and turns
into `raw.jsonl` lines and `edits.jsonl` speaker edits, drops speakers who hold
under 5 s of speech in total, and writes the transcript. The helper loads
models from the app's models directory only; it touches the network in
`ambient-engine fetch` and nowhere else.

The decision rests on issue #168's benchmark. Every set it used was left out
of Nemotron's training:

| | ONNX, shipped | Ultra + Nemotron 3 |
| --- | ---: | ---: |
| WER, AMI headset mix | 29.7% | 18.4% |
| WER, AMI far-field mic | 34.1% | 21.9% |
| WER, PriMock57 (two-person calls) | 15.2% | 12.7% |
| Speaker count exact, AMI headset / far-field | 3/16, 0/16 | 16/16, 14/16 |
| Speaker count exact, PriMock57 | 45/57 | 57/57 |
| Words given the wrong speaker, AMI headset / far-field | 10.9%, 16.1% | 5.6%, 7.0% |
| Words given the wrong speaker, PriMock57 | 6.8% | 1.9% |
| Peak process memory, speech-to-text | ~1.3–1.6 GB | ~120–260 MB |
| Peak process memory, speakers | — | ~400–600 MB |

The speaker counts for Ultra + Nemotron 3 include the 5 s filter. Without it
they were 15/16, 14/16 and 50/57. Nemotron's training covers the AMI train and
dev splits, VoxConverse dev and test, Fisher and CALLHOME Part 1. A
VoxConverse result was dropped for that reason.

## Consequences

- **The bundle gains a second executable.** `ambient-engine` is signed with
  the app's identity and hardened runtime. The finalize child no longer runs
  `ambient` itself, which `src/finalize.rs` had promised.
- **Nothing is transcribed while recording.** The live transcriber and its
  decoder lane go, and a transcript appears only after Stop. The scratch-file
  repair that recovers a capture killed mid-way stays.
- **Stages are checkpointed.** Each output is keyed by the track's size and
  modification time and the model names, so a job that dies resumes at the
  first missing stage. Changed audio or a changed model re-runs from scratch.
- **Retries are capped.** A session whose job fails twice is not re-queued at
  launch. It waits for a person to press Retry. A job abandoned because the
  app quit does not count.
- **Re-running a session is supported** while its audio exists. The previous
  `raw.jsonl`, `edits.jsonl` and `transcript.md` move to
  `previous/<timestamp>/`, never deleted (ADR 0008). Names a person typed carry
  over to the new speaker that overlaps them most in time; text repairs do not
  carry over and stay in the archive.
- **Models are fetched by `fetch-models.sh`**, through `ambient-engine fetch`,
  and pinned by a checksum list. About 0.8 GB replaces the 0.67 GB ONNX set.
- **Licences.** Ultra's CC-BY-4.0 needs attribution in
  `THIRD_PARTY_NOTICES.md`, and so does Nemotron's OpenMDW 1.1. FluidAudio's
  Apache-2.0 licence ships in `licenses/`.
- **macOS 14** is FluidAudio's floor. The app already requires 14.4.
- **Still to check by hand:** whether a job running beside a live capture in
  back-to-back meetings costs the capture any audio. The helper runs at
  background QoS, as the old child did, but this has not been re-measured on
  the Neural Engine.

## Confirmation

- `swift test` in `engine/` covers the token-to-word merge, the atomic write
  and argument parsing. It runs in CI's macOS job.
- The Rust tests drive a stub helper that fails at a named stage. They check
  resumption from checkpoints, the retry cap and re-running, and use no model
  or real audio.
- `bench/` in the repository is not part of CI. Accuracy is re-checked by
  running the `der` and `wer` binaries against the fixtures from
  `scripts/fetch-fixtures`, and their numbers should match the table above.
