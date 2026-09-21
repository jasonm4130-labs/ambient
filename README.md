# Ambient

**Record on your Mac. Transcribe on your Mac. Keep the conversation yours.**

Ambient is a macOS menu bar app for recording room audio and calls, transcribing
speech locally, and separating speakers. Browse, search and correct transcripts
in one window, then export them as Markdown, text, JSON or subtitles. No meeting
bot, transcription account or cloud inference service is required.

[Get started](docs/using/getting-started.md) · [Releases](https://github.com/jasonm4130-labs/ambient/releases) · [Documentation](https://jasonm4130-labs.github.io/ambient/) · [Contribute](CONTRIBUTING.md)

![Ambient session browser showing a sample transcript in dark mode](docs/developing/img/ui-dark.png)

*Example data from the UI test harness.*

## What it does

- Captures microphone and system audio as separate tracks, without joining the call.
- Transcribes speech and groups speakers using models running on the CPU.
- Lets you name speakers, edit session details, pin sessions and search transcripts.
- Exports Markdown, text, JSON, SRT and WebVTT; a read-only MCP server lets an
  assistant read the session library made available to it.
- Asks before recording a watched app by default. Audio retention defaults to
  seven days; transcripts remain until you delete the session.

**Early software:** capture and transcription have been exercised on an Apple
Silicon Mac. Accuracy, speaker labels and permissions need checking on your own
setup. Intel Macs and lower-memory machines are not validated release targets.
See [measurements](docs/developing/measurements.md) for the conditions behind the
published results.

## Install and make your first recording

Prebuilt downloads are being refreshed for the public launch. For now, use the
[build-from-source steps](#build-from-source). The release instructions below
apply once a new signed download is available.

You need an **Apple Silicon Mac running macOS 14.4 or later** for the release
builds. Allow roughly 1 GB for the app and bundled models, plus space for your
recordings.

1. Download the ZIP from [Releases](https://github.com/jasonm4130-labs/ambient/releases).
2. Unzip it, move `Ambient.app` into Applications, and open it.
3. Allow Microphone and System Audio Recording when macOS requests them.
4. Choose **Start Recording** in Ambient's menu, record a short test, then **Stop**.
5. Open Ambient's window and check both the transcript and the recorded tracks.

Release bundles include the models. Source builds have a separate model-download
step. If you get silence or a permission error, follow
[troubleshooting](docs/using/troubleshooting.md); do not disable Gatekeeper or
remove quarantine as a first step.

## Privacy and control

Ambient performs capture, transcription and speaker separation locally. Model
downloads require a network connection; recording and inference do not require a
cloud service. Sessions live in `~/Documents/Ambient` by default.

Local files are not an encrypted vault. A synced sessions folder, a backup, an
export or an assistant connected through MCP can move data beyond this Mac.
Only connect clients you trust, and obtain permission from the people you record.
Ambient's recording prompt is your control; it does not notify other participants.

Retention removes eligible audio during a later sweep, not at an exact deadline.
Failed transcription can retain audio indefinitely. Read
[what is kept](docs/using/what-is-kept.md) before relying on automatic deletion.

## Build from source

Install the Xcode Command Line Tools and Rust through `rustup`. The repository
pins its Rust toolchain. Node is only needed when changing the UI or docs.

```sh
git clone https://github.com/jasonm4130-labs/ambient.git
cd ambient
./fetch-models.sh
cargo run --release --bin ambient -- probe
./setup-signing.sh
./make-app.sh
open -a "$PWD/build/Ambient.app"
```

`setup-signing.sh` creates a local signing identity. System-audio capture needs a
signed bundle opened through macOS LaunchServices; running the capture binary
straight from a terminal can produce silence. See the full
[source-build guide](docs/using/getting-started.md#build-from-source) for signing,
model storage and CLI use.

## Find your next step

| I want to… | Read |
| --- | --- |
| Install, record and read a transcript | [Getting started](docs/using/getting-started.md) |
| Change devices, watched apps or retention | [Settings](docs/using/settings.md) |
| Script Ambient or export a session | [Command reference](docs/using/commands.md) |
| Connect an assistant to existing sessions | [MCP setup and access](docs/using/mcp.md) |
| Understand storage and deletion | [What is kept](docs/using/what-is-kept.md) |
| Fix silence or signing problems | [Troubleshooting](docs/using/troubleshooting.md) |
| Build, test or contribute | [Contributing](CONTRIBUTING.md) |
| Understand the architecture | [Developer guide](docs/developing/index.md) and [decisions](docs/adr/README.md) |

## License

Ambient's code is [MIT licensed](LICENSE). Bundled models and third-party
components retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).
