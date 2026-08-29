# 3. Two independent IOProcs, realigned on wall clock

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

The microphone and the process tap originally shared one aggregate device with
drift compensation, so a single IOProc delivered both on one clock and ch0 was
always you and ch1 always the far side, sample for sample. Two independent
streams would slowly slide apart over a long meeting and re-aligning them
afterwards is guesswork, so the reasoning was sound.

The arrangement did not work. **A process tap gates the clock of the aggregate
it belongs to.** With nothing rendering audio the aggregate delivered zero
frames in six seconds — not a quiet track, no callbacks at all — and the
microphone sharing that aggregate was starved along with it. Every capture
that had ever worked had `say` or `afplay` running, which is why a system
built to record office conversations had never recorded one.

## Considered Options

- **Keep the single aggregate** and try to make it run: neither
  `kAudioAggregateDeviceMainSubDeviceKey` nor
  `kAudioAggregateDeviceClockDeviceKey` changes the gating.
- **Two IOProcs**, the mic on the input device directly and the tap keeping its
  aggregate, realigned afterwards from wall-clock time.

## Decision Outcome

The microphone runs its own IOProc on the input device and the tap keeps the
aggregate to itself. Alignment is reconstructed from wall clock, which is
independent of both device clocks: each drain works out how many frames a
track should have by now (`elapsed × rate`) and pads a short track with
silence *in front of* the new samples, because the gap happened before them. A
track that overruns is never trimmed.

Order matters and is not optional: the microphone IOProc must be started
**before** the tap exists. Starting one on the input device while the tap's
aggregate is already running blocks forever — the mic alone starts instantly,
the same call after the aggregate never returns.

## Consequences

Alignment is good to within one drain tick (200 ms) instead of sample-exact.
That is the honest price of being able to record a silent room at all, which
the previous design could not do.

Inserted silence is counted separately from real device frames, so a stalled
tap reports `20.1s, 17.4s real, 2.7s padded` rather than passing for a healthy
one. The two tracks now differ in kind as well as in speaker: the call track
is exactly what this Mac renders, the room track is the whole room including
a television across it and, in an office, the next desk — which is what makes
the mic track the sensitive one and feeds
[ADR-0009](0009-armed-consent-state.md) and
[ADR-0011](0011-audio-retention-sweep.md).

## Confirmation

Nothing automated checks this: the failure only appears with a real input
device and a real tap, and CI has neither. A person confirms it by running the
bundled app with **no audio playing at all** and checking the room track
accumulates real frames — `ambient stop` prints `room` and `call` levels, and
the drain report must show real seconds rather than only padded ones. A
regression to the shared aggregate would show as zero room frames in a silent
room, which is exactly the symptom that is easy to mistake for a quiet mic.
