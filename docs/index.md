---
title: "ambient"
sidebar:
  order: 0
---

# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text. Nothing joins the call, and no audio
leaves the machine.

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly. The base M5 with 16 GB is the target and is not yet
measured — see [porting](developing/porting.md).

There is no published release yet. Running ambient still means building it,
which is why [Getting started](using/getting-started.md) opens with a compiler.

What you get once it runs is a menu bar item and a window. The menu bar item is
the consent surface — it arms when a watched app starts audio and is answerable
without raising anything else — and the window, *Open Ambient* or ⌘0, is where
the sessions are: a list, a transcript, the recording in flight pinned at the
top of it, and the warnings a capture stored about itself. The CLI verbs remain
the whole surface for scripting and debugging.

## What a packaged release still needs

The tooling exists and v0.0.2 went out through it. `release.sh` builds, signs
with a Developer ID identity, notarises, staples and publishes; `make-app.sh`
applies the hardened runtime and the one entitlement the microphone needs under
it, and bundles the ~671 MB of models the app loads so a download works with no
`fetch-models.sh` and no repo on the machine.

What is verified: the hardened runtime does not break ONNX Runtime (it is
statically linked, so no library-validation exemption is needed), the
microphone records with only `com.apple.security.device.audio-input`, and a
bundle carrying its own models resolves them from a working directory that has
none.

What is not: that a downloader gets a clean open. **Notarisation first ran for
v0.0.2** — an ~11-minute round trip through Apple's queue, stapled and
published by `release.sh`, which will not publish a bundle that fails `stapler
validate` — so Apple has issued a ticket and the bundle carries it. Whether a
zip pulled from a browser, quarantine attribute and all, opens with no
Gatekeeper dialog is still checked by hand.

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

The [decision records](adr/README.md) sit behind both: thirteen of them, each
naming a decision, the alternatives that were live at the time, and the check
that fails if it drifts.
