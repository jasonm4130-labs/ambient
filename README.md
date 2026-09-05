# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text.

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly.

## Quick start

You need macOS 14.4 or later, `rustup` (`rust-toolchain.toml` pins the compiler
to 1.95.0), the macOS SDK from Xcode or the Command Line Tools, and about 670 MB
of disk for the models. [Getting started](docs/using/getting-started.md) says
why each.

```sh
./fetch-models.sh                 # ~670 MB: recogniser, diarization, VAD
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
open -a "$PWD/build/Ambient.app"  # menu bar item, and the window behind it
```

That failure is the single most confusing thing in this project and
[docs/using/troubleshooting.md](docs/using/troubleshooting.md) is the page that
explains it.

The app is a menu bar item and a window. The menu bar item is the consent
surface — it arms when a watched app starts audio and is answerable without
raising anything. *Open Ambient*, ⌘0, opens the window: a sidebar of sessions
over the transcript, the recording in flight pinned at the top of the list, and
the warnings a capture stored about itself. The CLI verbs below all still work
and are what scripting and debugging use.

`ambient mcp` serves the same sessions to Claude Code and other MCP clients as
data rather than Markdown — three read-only tools over stdio, registered once:
[docs/using/mcp.md](docs/using/mcp.md).

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
class. Inside ambient the mitigation is the roster: `ambient roster add Priya`
then either the window's naming strip or `ambient name`, both of which rewrite
every line of a label. Repair and
summarisation happen outside ambient, on the exported `transcript.md` — no part
of that is in this repo.

## Where the docs are

Everything that used to be in this file now lives in `docs/`. The markdown is
the source of truth and reads correctly on GitHub, diagrams included;
`docs-site/` builds it into a Nimbus site themed with Cirrus —
`cd docs-site && npm ci && npm run dev`.

The split is by what you are doing, not by what the pages are about.

| | |
| --- | --- |
| [docs/using/](docs/using/index.md) | Build it, run it, read the transcript, know what is kept on disk. Start here. |
| [docs/developing/](docs/developing/index.md) | What the pieces are and why each is shaped that way, plus porting, CI and the docs build. |
| [docs/adr/](docs/adr/README.md) | Fifteen decision records — the alternatives that were live, and the check that fails if the decision drifts. |

## Still open

1. **The base M5 (16 GB).** Every number above is from the 128 GB machine.
   Chunked at 30 s this should fit, but should is not measured.
2. **Tap creation under the work machine's TCC policy.** Enumeration needs no
   permission; creating a tap needs System Audio Recording.

## Dependency notes

- The `coreml` feature is enabled in `Cargo.toml` for the diagnostic binaries
  `probe` and `bench`, which do not compile without it. The recognizer itself
  registers no execution provider. Per
  [ADR-0005](docs/adr/0005-cpu-not-coreml.md) the feature should be removed once
  both are retired.
- `ort` has **no stable release** — pinned to `=2.0.0-rc.13`. It has been in
  release-candidate for a long time; treat API churn as a live risk and do not
  let it leak past the ASR module.
- `objc2-core-audio` 0.3.2 was last published 2025-10-04. It is generated
  bindings over a stable C API, so staleness matters less than it would
  elsewhere, but it is worth knowing.

## Licence

MIT.
