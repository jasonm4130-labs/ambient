# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text.

Design doc: <https://claude.ai/code/artifact/6ff48ff8-7978-46fc-ae1b-387fd69974e3>

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly.

## Quick start

```sh
./fetch-models.sh                 # ~465 MB, v3-int8
cargo build --release
cargo run --release -- probe      # is this machine viable?
cargo run --release -- transcribe models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 audio.wav
```

Input must be 16 kHz WAV. To convert anything else:
`ffmpeg -i in.m4a -ac 1 -ar 16000 out.wav`

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
class and the reason the pipeline has a Claude repair pass with a roster and
glossary before anything is summarised.

Known artefact: chunk seams can duplicate a word ("flat. flat regardless…").
Fixable with overlap-and-dedupe; the repair pass also absorbs it.

## Capture: the launch method is load-bearing

`ambient tap <out.wav> <secs> [bundle-id...]` records system audio through a
Core Audio process tap — no virtual device, no bot in the meeting, and only the
System Audio Recording permission rather than ScreenCaptureKit's Screen
Recording.

**It must be launched as a bundled app through LaunchServices.** Run the bare
binary from a terminal and the tap is created successfully, delivers buffers at
the correct rate, and every sample is zero. TCC attributes the request to the
*responsible process* — the terminal — which has no audio permission and does
not prompt, so the failure is silent in the most literal sense.

```sh
./make-app.sh                                        # bundle + ad-hoc sign
open -a "$PWD/build/Ambient.app" --args tap /tmp/out.wav 14   # works
./build/Ambient.app/Contents/MacOS/ambient tap ...            # silence
```

### Two tracks, sample-aligned

`ambient tap` records **you and the far side on separate channels**: ch0 is the
microphone, ch1 is the tapped call audio. Both live in one aggregate device
with drift compensation, so a single IOProc delivers them on one clock —
two independent streams would slide apart over a long meeting.

The split does more than separate speakers. ch1 is *exactly what this Mac
renders* and nothing else; ch0 is the whole room. A test recording made that
concrete: `say` played through the speakers while a YouTube video ran on a TV
across the room.

| Track | Transcript |
| --- | --- |
| ch1 call | "This voice is arriving on the call track through the process tab." |
| ch0 mic | "…through the process tag. **Once his build is complete, he quickly finds himself.**" |

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

## Porting to the work M5 (16 GB)

The point of the exercise. In order:

1. `rustup` toolchain, then `git clone`, `./fetch-models.sh`, `cargo build --release`.
2. `ambient probe` — confirms Core Audio bindings link and reports the EP state.
3. `ambient transcribe` on a real recording, run under `/usr/bin/time -l`, and
   check `maximum resident set size` stays near 2.3 GB. If it does, the 16 GB
   machine is fine for batch transcription.
4. Then `./make-app.sh` and run the tap **via `open -a`**. If it returns
   silence, that is TCC, not a bug — and on a managed Mac it is exactly what a
   PPPC profile from Jamf/Intune exists to grant. Ask for System Audio
   Recording; you do not need Screen Recording.

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
on it. **Do not enable the `coreml` feature.** The Neural Engine is still
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

## Still open

1. **Accuracy.** Nothing here transcribed a word. Needs the decoder, joiner,
   tokens and a mel front-end wired up against real speech.
2. **The base M5 (16 GB).** Every number above is from the 128 GB machine.
   Chunked at 30 s this should fit, but should is not measured.
3. **Tap creation under the work machine's TCC policy.** Enumeration needs no
   permission; creating a tap needs System Audio Recording.

## Dependency notes

- The `coreml` feature is enabled in `Cargo.toml` for the probe only. Per
  Finding 1 it should be removed once the probe is retired.
- `ort` has **no stable release** — pinned to `=2.0.0-rc.13`. It has been in
  release-candidate for a long time; treat API churn as a live risk and do not
  let it leak past the ASR module.
- `objc2-core-audio` 0.3.2 was last published 2025-10-04. It is generated
  bindings over a stable C API, so staleness matters less than it would
  elsewhere, but it is worth knowing.

## Licence

MIT.
