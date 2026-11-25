---
title: "Porting to the work M5 (16 GB)"
sidebar:
  order: 29
---

# Porting to the work M5 (16 GB)

The point of the exercise. Every number in [Measurements](measurements.md)
came from the home machine — M5 Max, 128 GB — and the target is a base M5 with
16 GB. The helper's measured peak should fit easily, but *should* is not
measured, and that is one of the two things still open.

## The checklist, in order

1. `rustup` toolchain, then `git clone`, `./fetch-models.sh`, `cargo build --release`.
2. `cargo run --release -- probe` — confirms the Core Audio bindings link and
   lists the processes that could be tapped. This needs no permissions.
3. `scripts/fetch-fixtures`, then `scripts/memory`. It builds `ambient-engine`,
   transcribes a 25-minute public meeting through it the way the app does after
   Stop, and fails if any process involved peaks above its budget (see
   [Memory gate](measurements.md#memory-gate)). If it passes, the 16 GB machine
   is fine for transcription.
4. Then `./make-app.sh`, and run the tap **via `open -a`**.

## What to expect at step 4

If the tap returns silence, that is TCC and not a bug. See
[capture](capture.md) for the two independent failures and the
truth table that separates them — the first thing to check is the launch method,
because a terminal launch fails identically no matter how the app is signed.

On a managed Mac this is exactly what a PPPC profile from Jamf or Intune exists
to grant. **Ask for System Audio Recording. You do not need Screen Recording** —
that is the whole reason this uses a process tap rather than ScreenCaptureKit,
and asking for the larger permission would give away the argument. A PPPC
profile *denying* `kTCCServiceAudioCapture` produces symptoms identical to a
stale cdhash, so rule the signing identity out before raising a ticket.

## The number that decides it

The memory budget is the binding constraint, not throughput:

> **About 410 MiB at peak while transcribing, in the helper; nothing while
> capturing.**

That figure is from the [Memory gate](measurements.md#memory-gate): about
300 MiB for the transcribe stage and 409 MiB for the diarize stage on a
1505.6 s meeting. The stages run one at a time, each in its own `ambient-engine`
process, so only one model is resident at once and its memory is returned when
the stage ends. Both models read the audio from disk in pieces, so memory does
not grow with the length of the meeting. The ONNX pipeline this replaced
needed about 1.3 GB (see [Weight prepacking](measurements.md#weight-prepacking)).
See [ADR 0020](../adr/0020-models-run-in-a-core-ml-helper.md).

## Still open

1. **The base M5 (16 GB).** Every number is from the 128 GB machine.
2. **Tap creation under the work machine's TCC policy.** Enumeration needs no
   permission; creating a tap needs System Audio Recording.
