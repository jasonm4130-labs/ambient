# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text.

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly.

## Quick start

```sh
./fetch-models.sh                 # ~3.3 GB: recogniser, diarization, VAD
cargo build --release
cargo run --release -- probe      # is this machine viable?
cargo run --release -- transcribe models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 audio.wav
```

`transcribe` accepts any sample rate and resamples. To convert anything else
first: `ffmpeg -i in.m4a -ac 1 -ar 16000 out.wav`

To record anything you must build and launch the signed bundle — a bare binary
run from a terminal creates a working tap that delivers nothing but zeros, with
no error and no permission prompt:

```sh
./setup-signing.sh                # once — stable signing identity
./make-app.sh                     # bundle + sign
open -a "$PWD/build/Ambient.app"  # menu bar app
```

That failure is the single most confusing thing in this project and
[docs/architecture/capture.md](docs/architecture/capture.md) is the page that
explains it.

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

## Where the docs are

Everything that used to be in this file now lives in `docs/`, built as an
mdBook site. `mdbook serve --open` renders it; every page also reads correctly
on GitHub, diagrams included.

| | |
| --- | --- |
| [docs/architecture/](docs/architecture/index.md) | What the pieces are and why each is shaped that way. Two diagrams at the top carry most of it. |
| [docs/adr/](docs/adr/README.md) | Twelve decision records — the alternatives that were live, and the check that fails if the decision drifts. |
| [docs/reference/](docs/reference/cli.md) | Commands, settings, and every measurement in one place. |
| [docs/operations/](docs/operations/porting.md) | Porting to the work M5, what CI checks, how these docs are built. |

## Still open

1. **The base M5 (16 GB).** Every number above is from the 128 GB machine.
   Chunked at 30 s this should fit, but should is not measured.
2. **Tap creation under the work machine's TCC policy.** Enumeration needs no
   permission; creating a tap needs System Audio Recording.

## Dependency notes

- The `coreml` feature is enabled in `Cargo.toml` for the probe only. Per
  [ADR-0005](docs/adr/0005-cpu-not-coreml.md) it should be removed once the
  probe is retired.
- `ort` has **no stable release** — pinned to `=2.0.0-rc.13`. It has been in
  release-candidate for a long time; treat API churn as a live risk and do not
  let it leak past the ASR module.
- `objc2-core-audio` 0.3.2 was last published 2025-10-04. It is generated
  bindings over a stable C API, so staleness matters less than it would
  elsewhere, but it is worth knowing.

## Licence

MIT.
