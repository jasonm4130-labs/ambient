# Speech recognition

The recogniser is Parakeet TDT 0.6b, run through ONNX Runtime on the CPU. This
chapter covers how audio reaches it, what it costs, and the Phase 0
measurements that settled the CPU-not-CoreML question.

## Feeding the recogniser

The capture layer runs at the device rate (48 kHz here) and everything
downstream demands 16 kHz, with nothing in between until now — `ambient tap`
followed by `ambient transcribe` could never have worked. `src/resample.rs`
closes it with rubato's band-limited FFT resampler. Verified by transcribing
the same speech at 16 kHz natively and via 48 kHz through the resampler: two
tokens differ out of ~35 (`standup`/`stand up`, one capital), everything
load-bearing identical. Naive 3:1 decimation would alias instead, and Parakeet
would render the result fluently and wrongly rather than fail.

The front-end in `src/features.rs` is Slaney, 128 bins, centre-padded and
per-feature normalised, because that is what NeMo wants. Audio is cut into
chunks on speech boundaries by the [VAD](vad-and-chunking.md) before it reaches
the encoder.

## End-to-end result

M5 Max / 128 GB / macOS 26.6.2, v3-int8, synthesised speech via `say`:

| Audio | Decode | Realtime | Peak RSS |
| ---: | ---: | ---: | ---: |
| 5 s | 0.12 s | 42× | — |
| 16.5 s | 0.32 s | 52× | — |
| 127.5 s (chunked) | 2.38 s | 54× | **2317 MB** |

Peak memory on the 127 s file is *flat* at the 30 s-chunk level rather than the
~3.7 GB an unchunked 120 s run needed, which is the chunking working.

Accuracy is good on ordinary speech and technical vocabulary — GDPR, DPO, ONNX,
"Q3", "the 14th of October" all correct. It fails on **proper nouns**: "Priya"
became "Crea", "Cloudflare" became "Cloudflow". That is the expected failure
class. The mitigation inside ambient is the roster — `ambient roster add Priya`,
then `ambient name` rewrites every line of a label at once. Repair and
summarisation are done outside ambient on the exported `transcript.md`; nothing
in this repo performs them.

Known artefact: chunk seams can duplicate a word ("flat. flat regardless…").
Fixable with overlap-and-dedupe, and a repair pass done outside ambient would
absorb it too.

## What Phase 0 has established

Measured on the home machine (M5 Max, 128 GB, macOS 26.6.2) on 2026-08-29:

| Assumption | Result |
| --- | --- |
| Core Audio process taps reachable from Rust without a C shim | **Yes.** `objc2-core-audio` 0.3.2 exposes `AudioHardwareCreateProcessTap`, `CATapDescription` with the mono/stereo global-tap initialisers, `AudioHardwareCreateAggregateDevice`, and the `kAudioSubTap*` property set including drift compensation. Compiles and runs; enumerated 21 audio processes with bundle IDs resolved. |
| ONNX Runtime CoreML EP available from a prebuilt binary | **Yes.** `ort` 2.0.0-rc.13 with the `coreml` feature reports `is_available() == Ok(true)` against the binary it downloads. No from-source ONNX Runtime build needed, contrary to some write-ups. |
| `ComputeUnits::CPUAndNeuralEngine` requestable | **Yes**, it is a documented variant of the CoreML EP config. |

## What Phase 0 measured

`cargo run --release --bin bench -- <encoder.onnx> <coreml|cpu> [seconds]`

Home machine (M5 Max, 128 GB, macOS 26.6.2), Parakeet TDT 0.6b encoder,
60 s of audio. Input is a zeroed tensor of the correct shape — wall clock for a
fixed-shape graph is content-independent, so the comparison holds, but these are
not accuracy numbers.

| Model | Provider | Steady run | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| v2 fp16 | CPU | 1013 ms | 59× | 3745 MB |
| v2 fp16 | CoreML (ANE requested) | 1012 ms | 59× | 3767 MB |
| v3 int8 | CPU | 1029 ms | 58× | 2594 MB |
| v3 int8 | CoreML (ANE requested) | 1670 ms | 36× | 16482 MB |

### Finding 1 — ONNX Runtime's CoreML provider does nothing for this model

On fp16 it is identical to CPU to within a millisecond: the provider registers,
reports available, and contributes no acceleration. On int8 it is actively
harmful — 1.6× slower and 16.5 GB peak, which would OOM a 16 GB machine outright.

`is_available() == true` means the provider loaded, not that any node was placed
on it. The `coreml` Cargo feature stays on — `probe` and `bench` need it to report
provider state at all — but **the recognizer's session must never register the
CoreML provider**: `src/asr.rs` builds its `Session` with no execution provider,
and that is the thing to preserve. The Neural Engine is still
reachable on this hardware, just not through ORT's graph-partitioning shim —
natively compiled CoreML models (what FluidAudio ships) are a different path and
would likely behave differently.

### Finding 2 — memory scales with audio length, so chunking is mandatory

v3 int8 on CPU:

| Audio | Steady run | Realtime | Peak RSS |
| ---: | ---: | ---: | ---: |
| 30 s | 488 ms | 62× | 2158 MB |
| 60 s | 1029 ms | 58× | 2594 MB |
| 120 s | 2252 ms | 53× | 3721 MB |
| 300 s | 7539 ms | 40× | 6291 MB |

Roughly linear in memory and worse than linear in time — attention cost grows
with sequence length. Extrapolated, a 30-minute session fed whole would want
~35 GB and would fail on the base M5.

The fix is ordinary and known: transcribe in **30 s windows with overlap and
stitch**, which makes cost constant regardless of session length. At that size a
30-minute session is ~60 chunks ≈ 30 s of compute at a flat ~2.2 GB.

### Revised design axiom

The original "< 2 GB, on the ANE" was written assuming continuous processing. The
real workload is bursty batch — promote a session, transcribe once. At ~60×
realtime on CPU the ANE is not load-bearing, and the budget becomes:

> **~2.2 GB during a transcription burst, chunked at 30 s; negligible while
> capturing.** CPU is fine. Revisit only if live streaming transcription is
> wanted, where the ANE would start to matter.

See [ADR-0005](../adr/0005-cpu-not-coreml.md) and [ADR-0006](../adr/0006-separate-frontends.md).
