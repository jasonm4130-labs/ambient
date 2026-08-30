# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text. Nothing joins the call, and no audio
leaves the machine.

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly. The base M5 with 16 GB is the target and is not yet
measured — see [porting](developing/porting.md).

There is no packaged release. Running ambient means building it, which is why
[Getting started](using/getting-started.md) opens with a compiler.

## What a packaged release would take

A Developer ID certificate, notarisation and a stapled artefact built on CI:
`setup-signing.sh` creates a self-signed certificate and adds it to the login
keychain as a trusted code-signing root, so the identity it produces is valid on
this machine and nowhere else, and the repo holds no notarisation tooling and no
release job at all. Models would also have to be fetched on first run, or the
default recogniser made smaller, so that 670 MB of ONNX stops being something
you download by hand before anything works.

Even then neither `record` nor `tap` could be run straight from a shell. They
already are ordinary CLI verbs; the constraint is the launch, not the argument
parsing. The truth table in [when it does not
work](using/troubleshooting.md) holds that a shell launch leaves the call track
silent regardless of how the app is signed, because TCC attributes the request
to the terminal — so both have to be started through LaunchServices, `open -a …
--args`. That is the ceiling on how simple this can get.

## Two doors

**[Using ambient](using/index.md)** — get it running, record something, read the
transcript, and know what is kept on disk. Start here.

**[Developing ambient](developing/index.md)** — what the pieces are, why each one
is shaped the way it is, and what CI enforces. Read this before changing code.

The [decision records](adr/README.md) sit behind both: twelve of them, each
naming a decision, the alternatives that were live at the time, and the check
that fails if it drifts.
