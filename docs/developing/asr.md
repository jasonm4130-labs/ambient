---
title: "Transcription"
sidebar:
  order: 24
---

# Transcription

Nothing is transcribed while recording. After Stop, the capture half writes
each track as a 16 kHz mono wav, and `session::transcribe_session` turns those
into a transcript. The models do not run in the `ambient` process: they run in
`ambient-engine`, a Swift helper built from `engine/` on FluidAudio and shipped
beside `ambient` in the app bundle. [ADR 0020](../adr/0020-models-run-in-a-core-ml-helper.md)
records why.

## The helper

`ambient-engine` has three commands:

| Command | What it does |
| --- | --- |
| `fetch --models <dir>` | Downloads both models. The only command that touches the network; `fetch-models.sh` runs it. |
| `transcribe --models <dir> --audio <wav> --out <json> --key <k>` | Parakeet TDT 0.6b Ultra over one track. Writes words with start, end and confidence. |
| `diarize --models <dir> --audio <wav> --out <json> --key <k>` | Nemotron 3 Diarization, `fast32` preset, over one 16 kHz mono track. Writes speaker turns. |

One call does one stage for one track, writes one JSON file atomically, and
exits, so the memory the models used goes back to the system when the stage
ends rather than staying in a menu bar app that runs all day. `src/engine.rs`
is the Rust side. It finds the helper at `$AMBIENT_ENGINE`, or else beside the
running executable. It reads the helper's stdout, where a line is either
`progress <0..1>` or `error <message>` before a non-zero exit. The progress
fractions are parsed and currently dropped: the window shows the phase
(`transcribing`, `separating voices`), not a percentage.

The app holds the helper's stdin open and never writes to it. When the app
exits, the pipe closes and the helper exits too, rather than running on with
nobody to read its output. The helper also sets itself to background priority,
because a spawned process does not inherit its parent thread's QoS class.

After each stage, `engine::run` logs `ENGINE_PEAK <stage> <bytes>`, the
helper's peak resident set from `wait4`. `scripts/memory` gates on the largest
of these; see [Memory gate](measurements.md#memory-gate).

The helper loads models from the models root only: `AMBIENT_MODELS`, else a
`models/` directory in the working directory or above the executable (see
[commands](../using/commands.md#environment)). Before any stage, `pipeline::Engine::check` confirms that
`parakeet-ultra/` and `nemotron-3-diarization/monolithic/v2/` exist under it.

## One job, stage by stage

`pipeline::run` does the work for one session:

1. Transcribe every track that has a wav (`room`, then `call`).
2. If the `diarize` setting is on and any track produced words, diarize each
   track that has words. A track with no words is skipped.
3. Assemble lines and speaker labels from the words and turns (see
   [speaker separation](diarization.md)).
4. Write `raw.jsonl`, then `edits.jsonl`, then `transcript.md`, each
   atomically. `transcript.md` goes last because its existence is what
   "finished" means to the launch-time re-queue and to the audio sweep.

`status` reads `transcribing`, then `separating voices`, then `done`, or
`failed: …` if a stage errors.

### Checkpoints

Each stage's output is kept under `stages/` as `<track>.<stage>.json`, for
example `stages/call.transcribe.json`. Every file carries a key made of the
model name and the track's size and modification time. A stage reuses its file
only while the key still matches, so a job that dies resumes at the first stage
it had not finished. If the audio or the model changes, that stage runs again.

### Retries

A failed job writes `failed: …` to `status` and adds one to the session's
`attempts` file. At launch the app re-queues every session that has
`session.json`, audio and no `transcript.md`, provided it has failed fewer than
two times (`pipeline::RETRY_CAP`). After that, the session waits for
`ambient retry` or the window's Retry. A job abandoned because the app quit is
never counted. Neither is a missing helper or missing model: that is the
machine's fault rather than the session's, and counting it would park every
session after two launches without models.

If the job is on its last allowed attempt (`attempts + 1 >= RETRY_CAP`) and
the diarize stage fails, the words are still written, without speaker labels.
`session.json` then gets a warning beginning `voices were not separated`. The
audio sweep skips a session with that warning, so the session can be
transcribed again later. `retry` leaves the count as it is, `rerun` resets it,
and a successful job removes the file.

### Re-running

`ambient rerun` and the window's *Transcribe again* run a finished session
again from its audio. `pipeline::prepare_rerun` first copies `raw.jsonl`,
`edits.jsonl` and `transcript.md` into `previous/<timestamp>/`, then removes
`transcript.md`. Nothing a person wrote is lost. If the archive already holds
typed edits, `carry-from` names it, and the new run carries each typed name to
the new speaker whose lines overlap the old named lines most in time, one name
per speaker. Text repairs are not carried over, because the words they
corrected may no longer exist; they stay in the archive. A session whose audio
has been swept cannot be re-run.

## Resampling

The capture layer runs at the device rate (48 kHz here, 44.1 kHz on some
interfaces), and the helper is handed 16 kHz mono. `src/resample.rs` closes
that gap with rubato's band-limited FFT resampler. Naive 3:1 decimation would
alias everything above 8 kHz into the speech band, and the recogniser would
transcribe the result fluently and wrongly rather than fail. `wer --via <rate>`
measures the transcript through this path; see
[measurements](measurements.md#word-error-rate-through-the-resampler).

## What it gets wrong

On the ONNX build of Parakeet that came before, proper nouns were the usual
failure: names and product names came back as similar-sounding words. That has
not been re-checked on Ultra. The mitigation inside Ambient is the roster and
naming (`ambient roster add Priya`, then `ambient name`). Any further repair or
summary happens outside Ambient, on the exported `transcript.md`.

Accuracy and memory figures for the helper, and for the ONNX pipeline it
replaced, are in [measurements](measurements.md#the-core-ml-helper-adr-0020).
