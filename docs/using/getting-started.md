---
title: "Getting started"
sidebar:
  order: 11
---

# Getting started

Make a short test recording before using Ambient for a conversation you need to
keep. This checks your permissions, input device and system-audio capture together.

## Install a release

Prebuilt downloads are being refreshed for the public launch. Use
[Build from source](#build-from-source) until a new signed release is available.
The steps below apply to that release.

Release builds target Apple Silicon Macs running macOS 14.4 or later. Allow
roughly 1 GB for the extracted app and models, plus storage for recordings.

1. Download a ZIP from [Releases](https://github.com/jasonm4130-labs/ambient/releases).
2. Unzip it and move `Ambient.app` into `/Applications`.
3. Open Ambient from Applications.
4. Open its menu bar item, then choose **Open Ambient** to show the window.

The models are bundled; you do not need Rust, Node or `fetch-models.sh` for a
release build. macOS permissions are separate from the app's recording controls.
Allow **Microphone** and **System Audio Recording** when requested. If a prompt
is missing or a track is silent, use [troubleshooting](troubleshooting.md).

## Make a test recording

1. Choose **Start Recording** from the menu bar or welcome page.
2. Speak a short sentence and play a short piece of audio on the Mac.
3. Check that the microphone and system-audio levels respond.
4. Choose **Stop** and wait for transcription to finish.
5. Select the session in the window and check the transcript.

You can open the session during recording to read completed speech turns. Text
arrives in blocks, with additional delay for model loading or ongoing speech.
After Stop, Ambient finishes the remaining text and speaker labels. The menu
shows queued work, so a new recording can start while the previous one finishes.

Use **Reveal in Finder** to inspect the session files. The `audio/` directory
contains separate room and call tracks. Verify both if the transcript is missing
speech. A successful build or a clean `probe` does not prove capture works.

## Work with the transcript

The sidebar lists sessions. Search names and tags there, or use ⌘K to search
transcript text. Rename a session, add notes, pin it or apply tags above the
selected transcript.

**Tidied** shows the recognition output with naming edits applied; **Verbatim**
shows the original recognition output. Use **Separate voices** to group speakers
when audio is still available, then assign names. **Copy Markdown** and **Export**
let you take the transcript elsewhere.

Speaker labels and words can be wrong. Review them before sharing the transcript.
[Settings](settings.md) covers devices, watched apps and retention;
[what is kept](what-is-kept.md) explains deletion and the limits of local storage.

## Build from source

You need macOS 14.4 or later, the Xcode Command Line Tools and Rust installed
through `rustup`. `rust-toolchain.toml` selects the compiler. The models occupy
about 670 MB; allow additional space for the download archive and build artifacts.

```sh
git clone https://github.com/jasonm4130-labs/ambient.git
cd ambient
./fetch-models.sh
cargo run --release --bin ambient -- probe
./setup-signing.sh
./make-app.sh
open -a "$PWD/build/Ambient.app"
```

`setup-signing.sh` creates a stable local signing identity. It can ask for
keychain access. `make-app.sh` assembles and signs the bundle; a local development
signature is not a notarized distribution build.

**Launch the bundle to record system audio.** A binary started directly in a
terminal can receive silent buffers because macOS attributes the capture request
to the wrong process. Repeated ad-hoc signing can also invalidate an earlier
permission grant. Follow [troubleshooting](troubleshooting.md) if capture is silent.

The committed UI bundle means Node is not required for a Rust-only build. For UI
and docs changes, see the
[contributor guide](https://github.com/jasonm4130-labs/ambient/blob/main/CONTRIBUTING.md).

## Use the CLI

Commands that read files or inspect the machine can run in a terminal:

```sh
cargo run --release --bin ambient -- sessions
cargo run --release --bin ambient -- transcribe \
  models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 audio.wav
```

For a release installed in Applications, use its executable directly for those
commands:

```sh
/Applications/Ambient.app/Contents/MacOS/ambient sessions
```

Recording commands must go through the app bundle. Quit an already-running
Ambient instance before launching it with CLI arguments: `open` otherwise
activates the existing process without applying those arguments.

```sh
open -a "$PWD/build/Ambient.app" --args record --name test
```

Stop that recording from another terminal:

```sh
cargo run --release --bin ambient -- stop
```

The [command reference](commands.md) describes export formats, environment
variables, naming, speaker separation and diagnostics.
