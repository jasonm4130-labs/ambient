---
title: "15. Capture and transcription are separate work, joined by a serial queue"
sidebar:
  order: 55
---

# 15. Capture and transcription are separate work, joined by a serial queue

**Status:** accepted · **Date:** 2026-09-02 · **Supersedes:** —

## Context and Problem Statement

Back-to-back meetings lost the start of the second one. `session::record_into`
was one blocking call — capture, resample, ASR, diarisation, transcript — on
one thread, and the menu bar's state machine owned exactly one of them. For
the minutes after Stop, Start was a no-op and the watcher did not fire, so a
call starting in Zoom was not even noticed, let alone offered.

The obvious fix, transcribing beside a live capture, has a cost the wait does
not: ASR runs on CPU ([ADR-0005](0005-cpu-not-coreml.md)), and a model
saturating cores next to a tap that must be drained every 200 ms risks dropped
audio. A late transcript is an inconvenience; a hole in the recording is the
only copy of what was said, gone.

## Considered Options

- **Keep one pipeline, allow a second worker.** Two `Live`s in the phase, two
  taps possible at once, and transcription of the first running under the
  capture of the second. Rejected: it is exactly the contention above, and it
  doubles every invariant the state machine exists to hold.
- **Split capture from transcription; transcribe concurrently.** Same
  contention, less state. Rejected for the same reason.
- **Split capture from transcription; transcribe on a serial queue.** Capture
  finishes when the 16 kHz audio and `session.json` are on disk. Transcription
  runs afterwards, one session at a time, on one long-lived thread. Chosen.

## Decision Outcome

`capture_into` and `transcribe_session` are separate functions, and
`record_into` is their composition for the CLI. In the menu bar app the phase
returns to `Idle` the moment capture finishes, and the session goes onto
`queue::Queue`, which transcribes one job at a time and reports each back
through the delegate's tick. One transcription at a time, never blocking a new
recording.

The one fact that decided it: the tap is the only thing that cannot be redone.
Everything downstream of it can wait.

## Consequences

- `session.json` is written at the end of capture, not after ASR. A session
  with `session.json` and no `raw.jsonl` is *captured and waiting*, not
  interrupted, and the window says so.
- `status` gains a value, `captured`, between `finishing` and `transcribing`.
- The phase's `Transcribing` variant is now `Stopping`: stop signalled, audio
  still being written, seconds not minutes. Transcription is not a phase.
- A crash between capture and transcript leaves audio on disk with no
  transcript. The app re-queues every such session at launch
  (`captured_awaiting_transcript`), so the state can exist but cannot persist.
- Quit waits for the queue as it already waited for the capture. The watcher
  does not start a recording while a quit is pending.
- `transcribe_session` creates `raw.jsonl` before it loads the models, so the
  window in which a second process could claim the same session is
  milliseconds rather than the seconds a model load takes.

## Confirmation

`queue::tests` drive the queue with a fake worker: order, the menu summary,
failure reporting, and a dead worker. `state::tests` cover the quit table with
a busy queue and where a failed transcript may enter `Failed`.
`menubar::tests::a_session_being_transcribed_does_not_deafen_the_watcher` is
the issue itself. `windowcheck` still walks a recording through Stop. What no
check covers is two real recordings back to back in the bundle; that is a
person with Zoom open.
