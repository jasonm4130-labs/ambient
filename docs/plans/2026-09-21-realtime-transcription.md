---
title: "Plan: transcripts during recording"
sidebar:
  order: 96
---

# Transcripts during recording

Ambient should publish completed speech turns while recording, so the session
window and an MCP client can read a conversation before Stop. Recognition stays
local. This plan guides implementation; it is not evidence of live capture.

## Scope

Process both tracks in bounded blocks on a background worker. Start with a
10-second cadence, supported by the earlier [block-cost measurements](../developing/measurements.md#verdict).
Carry unfinished speech into the next block, with a bounded maximum turn length.
The cadence is not a latency guarantee: model loading, long speech turns and
queued work can delay text.

Keep native audio as the recoverable source. The capture drain must never wait
for recognition, resampling or transcript writes. Serialize recognition work
with transcription of previous sessions, preserving the measured single-decoder
constraint.

Append completed lines in arrival order. Keep each track's sample clock separate,
and preserve every published line through Stop, finalization and retry. The
existing MCP `transcript` cursor and window polling should consume these lines
without a new transport or replacement-text protocol. Speaker separation remains
a finalization step.

## Implementation sequence

1. Add bounded incremental audio reading, turn segmentation and append recovery.
2. Connect the worker to capture and post-capture finalization, with serialized
   model access and failure isolation.
3. Verify growing transcripts through the existing API and window polling.
4. Update user documentation and run repository checks.

## Storage and handoff

`src/live.rs` uses `audio/room.live.pcm` and `audio/call.live.pcm` as temporary
native-rate mono 16-bit queues. This duplicates the native audio while recording,
but keeps the worker independent of a growing WAV header. Unread audio stays on
disk; a worker buffer covers at most 30 seconds of one track. Completed sessions
remove these temporary queues after writing `transcript.md`. Finalized native
WAVs are retained as `.source.wav` until that point for recovery.

`live-asr.json` retains each track's sample offset and the committed raw byte
count. Before appending a batch, the worker journals its exact lines and next
offsets. Recovery completes a partial append from that journal before advancing
the checkpoint. A completed checkpoint remains with the session so a later retry
does not replace its published transcript.

Model loading, VAD, recognition and speaker separation share one inference
lock within the process. Live work can stop while waiting for that lock; an
in-flight inference call finishes before it pauses. Separate processes do not
share this scheduling lock.

Stop closes the audio writers and pauses live work. Finalization catches up from
disk and flushes unfinished speech before speaker separation. Live recognition
failure leaves the recording available for that finalization path. Automatic
restart recovery still requires finalized audio and `session.json`; recovering
a process killed during capture is outside this change.

## Acceptance

- A synthetic capture source produces transcript lines before it closes.
- Speech crossing a block boundary is carried forward; Stop flushes the tail.
- Room and call timestamps use their own sample rates.
- A slow or failed decoder cannot block the capture drain or discard audio.
- Polling with the previous `next` returns only new committed lines, including
  across Stop and a recoverable transcription failure.
- Concurrent readers do not see partially written JSON records.
- Existing sessions remain readable and transcribable.
- Rust checks pass. Relevant window tests and all documentation checks pass.

Replay an existing public speech fixture through the real models when available,
without starting a microphone or system-audio tap. Record its observed result
separately from synthetic checks. A signed-app recording, actual end-to-end
latency and audio continuity still require a manual validation session.

## Replay check

Run `cargo run --bin livecheck -- /path/to/public-speech-fixture.wav` with a
public fixture at least 35 seconds long and the local models installed. The
harness creates a temporary session, verifies MCP text before source EOF,
finishes through `transcribe_session`, and checks that finalization and retry
preserve the published text and cursor. It never starts a capture device.

The first implementation replay used the public 88.89-second LibriSpeech speaker
121 fixture on both tracks. MCP returned two lines before source EOF, and the
finished transcript contained 62 lines with an unchanged prefix after retry.
The input was fed faster than wall clock; these counts verify the pipeline,
not microphone latency or transcription accuracy.
