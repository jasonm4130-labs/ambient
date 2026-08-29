# 1. Capture call audio with a Core Audio process tap

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

The far side of a Teams call has to be recorded on a Mac that may be centrally
managed, without announcing itself to the other participants and without
asking for permissions that a corporate policy is likely to refuse. What gets
recorded also has to be attributable: a track that is provably "what this Mac
rendered" is worth more than a track that is "whatever was audible", because
the second one carries the room as well.

## Considered Options

- **A Core Audio process tap** on the named processes, draining into a
  preallocated lock-free ring.
- **ScreenCaptureKit** audio capture, which is the documented modern route to
  system audio.
- **A virtual audio device** (loopback driver) that the user routes output
  through.
- **A meeting bot** joining the call and recording server-side.

## Decision Outcome

Tap the application's rendered output with `AudioHardwareCreateProcessTap` and
a `CATapDescription`, aggregated into a device this process owns.

The permission is what decides it. ScreenCaptureKit requires the Screen
Recording grant — a broad, visible, frequently policy-denied permission — to
obtain audio; a process tap needs only System Audio Recording, which is the
narrower grant and exactly the one a Jamf or Intune PPPC profile can be asked
for. A virtual device requires installing a kext-class system extension and
reroutes the user's own audio path; a bot appears in the participant list,
which defeats the point. `objc2-core-audio` 0.3.2 exposes the whole surface —
tap creation, the aggregate device, and the `kAudioSubTap*` properties
including drift compensation — so no C shim is needed.

## Consequences

The IO block runs on a realtime thread and therefore neither allocates, locks
nor logs; samples land in a preallocated ring drained by the caller. The call
track is exactly what this Mac renders and nothing else, which is what makes
the room/call split in [ADR-0003](0003-two-ioprocs-wall-clock.md) meaningful.

It also inherits the tap's constraints: a tap gates its aggregate's clock, and
TCC attributes the request to the responsible process rather than the binary —
the second of which is [ADR-0002](0002-signed-bundle-launch.md).

## Confirmation

`cargo run --release -- probe` links the Core Audio bindings and enumerates
audio processes with bundle IDs resolved; it is the cheapest signal that the
tap API is still reachable. Nothing automated proves a tap actually captures,
because no hosted runner has an audio device — see
[ADR-0012](0012-ci-verifies-assembly.md). To confirm behaviour a person must
run `./make-app.sh` and then
`open -a "$PWD/build/Ambient.app" --args tap /tmp/out.wav 14` with audio
playing, and check the reported call peak is non-zero.
