---
title: "Two tracks, two clocks"
sidebar:
  order: 22
---

# Two tracks, two clocks

Why capture is split across two independent IOProcs, and what that costs in
alignment. The launch and signing requirements behind capture itself are in
[Capture](capture.md).

`ambient tap` records **you and the far side as separate files**: a room track
from the microphone and a call track from the process tap.

They used to share one aggregate device, on the reasoning that a single IOProc
delivering both on one clock beats two streams sliding apart over a long
meeting. That reasoning was sound and the arrangement did not work, because
**a process tap gates the clock of the aggregate it belongs to**. With nothing
rendering audio the aggregate delivers zero frames in six seconds — not a quiet
track, no callbacks at all — and the microphone sharing that aggregate is
starved along with the tap. Every capture that ever worked had `say` or `afplay`
running, which is why a system built to record office conversations had never
recorded one. Neither `kAudioAggregateDeviceMainSubDeviceKey` nor
`kAudioAggregateDeviceClockDeviceKey` changes this.

So the microphone now runs its own IOProc on the input device directly, and the
tap keeps the aggregate to itself. Two consequences worth knowing:

- **Order matters.** The microphone IOProc is started *before* the tap exists.
  Starting one on the input device while the tap's aggregate is already running
  blocks forever — the mic alone starts instantly, the same call after the
  aggregate never returns.
- **Alignment is reconstructed from wall-clock time**, which is independent of
  both device clocks. Each drain works out how many frames a track should have
  by now (`elapsed × rate`) and pads a short track with silence in front of the
  new samples, since the gap happened before them. A track that overruns is
  never trimmed. This is good to within one drain tick (200 ms) against
  sample-exact before — the honest cost of being able to record a silent room.

Padding is counted separately from real device frames, so a stalled tap reports
`20.1s, 17.4s real, 2.7s padded` rather than passing for a healthy one.

The split does more than separate speakers. The call track is *exactly what
this Mac renders* and nothing else; the room track is the whole room. A test recording made that
concrete: `say` played through the speakers while a YouTube video ran on a TV
across the room.

| Track | Transcript |
| --- | --- |
| call | "This voice is arriving on the call track through the process tab." |
| room | "…through the process tag. **Once his build is complete, he quickly finds himself.**" |

The second sentence is the television, picked up acoustically. It is on the mic
track and absent from the tap track, which is the boundary working exactly as
designed.

**Implication for the room, and for consent.** The microphone captures
everything audible: a TV, a radio, and in an office the conversation at the
next desk — people who are not in your meeting and have not agreed to anything.
The tap has no such problem. This is a further argument for building calls
first, and for the mic track to be VAD-gated and treated as the sensitive one.

Verified end to end: tapped system audio, resampled to 16 kHz, transcribed as
"Right, the process tab is capturing system audio with nothing joining the
meeting." (`tap` -> "tab" and `Priya` -> "CRO" are the usual proper-noun and
homophone errors.)

See [ADR-0003](../adr/0003-two-ioprocs-wall-clock.md).

Two tracks mean two streams of decode work arriving at one serial
recogniser, and that is exactly what the benchmark's `tracks=2` rows measure:
one worker carrying both tracks' turns rather than two workers running
alongside each other. The verdict on this machine, at 30 s blocks, is a go —
see [the verdict](measurements.md#verdict).
