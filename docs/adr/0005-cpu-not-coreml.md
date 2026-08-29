# 5. Run ASR on CPU only, with no CoreML execution provider

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

The original design axiom was "under 2 GB, on the ANE". ONNX Runtime ships a
CoreML execution provider, `ort` 2.0.0-rc.13 with the `coreml` feature reports
`is_available() == Ok(true)` against the binary it downloads, and
`ComputeUnits::CPUAndNeuralEngine` is a documented variant — so on paper the
Neural Engine was one feature flag away.

## Considered Options

- **Enable the `coreml` feature** and request `CPUAndNeuralEngine`.
- **CPU only**, and drop the ANE from the design.

## Decision Outcome

CPU only. The `coreml` feature stays off.

Measured on the home machine (M5 Max, 128 GB, macOS 26.6.2), Parakeet TDT
0.6b encoder over 60 s of audio:

| Model | Provider | Steady run | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| v2 fp16 | CPU | 1013 ms | 59× | 3745 MB |
| v2 fp16 | CoreML (ANE requested) | 1012 ms | 59× | 3767 MB |
| v3 int8 | CPU | 1029 ms | 58× | 2594 MB |
| v3 int8 | CoreML (ANE requested) | 1670 ms | 36× | 16482 MB |

On fp16 it is identical to CPU to within a millisecond. On int8 — the model
actually shipped — it is 1.6× slower at 16.5 GB peak, which would OOM the
16 GB target outright. `is_available() == true` means the provider loaded, not
that any node was placed on it.

## Consequences

At ~58× realtime on CPU for a bursty batch workload the ANE is simply not
load-bearing, and the memory budget becomes ~2.2 GB during a transcription
burst. This is only worth revisiting if live streaming transcription is
wanted.

The Neural Engine remains reachable on this hardware — natively compiled
CoreML models are a different path and would likely behave differently. The
rejection is of ORT's graph-partitioning shim, not of the ANE.

The `coreml` feature is still enabled in `Cargo.toml` for the two diagnostic
binaries — `ambient probe` and `bench`, both of which use `ort::ep::CoreML` and
do not compile without it — so they can report the EP state. It should be removed
once both are retired.

## Confirmation

Nothing automated catches a re-enabled provider — it would compile, pass every
test, and simply be slower and fatter. A person confirms it by running
`cargo run --release --bin bench -- <encoder.onnx> coreml 60` against
`… cpu 60` and comparing steady run and peak RSS; a CoreML run that is not
clearly faster *and* leaner than CPU means the shim is still doing nothing.
`ambient probe` reports the current EP state on any machine being ported to.
