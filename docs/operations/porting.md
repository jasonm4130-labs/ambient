# Porting to the work M5 (16 GB)

The point of the exercise. Every number in [Measurements](../reference/benchmarks.md)
came from the home machine — M5 Max, 128 GB — and the target is a base M5 with
16 GB. Chunked at 30 s this should fit, but *should* is not measured, and that
is one of the two things still open.

## The checklist, in order

1. `rustup` toolchain, then `git clone`, `./fetch-models.sh`, `cargo build --release`.
2. `cargo run --release -- probe` — confirms the Core Audio bindings link and
   reports the execution provider state. This needs no permissions.
3. `cargo run --release -- transcribe <model> <recording>` on a real recording,
   run under `/usr/bin/time -l`, and check `maximum resident set size` stays near
   2.3 GB. If it does, the 16 GB machine is fine for batch transcription.
4. Then `./make-app.sh`, and run the tap **via `open -a`**.

## What to expect at step 4

If the tap returns silence, that is TCC and not a bug. See
[capture](../architecture/capture.md) for the two independent failures and the
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

> **~2.2 GB during a transcription burst, chunked at 30 s; negligible while
> capturing.**

Unchunked, memory scales linearly with audio length — an extrapolated ~35 GB for
a 30-minute session, which fails on this machine outright. So step 3 is not a
formality: it is the whole port. At ~60× realtime on CPU there is no throughput
question to answer.

Diarization is a separate concern and a much smaller one — 249 MB peak on a
10.2-minute track, because the sliding window does not accumulate. It is a
separate verb partly so its two graphs are never resident alongside Parakeet,
which is what keeps the 16 GB target reachable. See
[ADR-0007](../adr/0007-diarize-separate-verb.md).

## Still open

1. **The base M5 (16 GB).** Every number is from the 128 GB machine.
2. **Tap creation under the work machine's TCC policy.** Enumeration needs no
   permission; creating a tap needs System Audio Recording.
