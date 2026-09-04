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

Two releases are published, v0.0.1 and v0.0.2, both built by `release.sh` and
both signed, notarised and stapled. Running ambient from source is still the
documented path, which is why [Getting started](using/getting-started.md) opens
with a compiler.

What you get once it runs is a menu bar item and a window. The menu bar item is
the consent surface — it arms when a watched app starts audio and is answerable
without raising anything else — and the window, *Open Ambient* or ⌘0, is where
the sessions are: a list, a transcript, the recording in flight pinned at the
top of it, and the warnings a capture stored about itself. The CLI verbs remain
the whole surface for scripting and debugging.

## What the release script proves, and what it does not

The tooling exists and two releases have come out of it. `release.sh` builds,
signs with a Developer ID identity, notarises, staples and publishes;
`make-app.sh` applies the hardened runtime and the one entitlement the
microphone needs under it, and bundles the ~671 MB of models the app loads
so a download works with no `fetch-models.sh` and no repo on the machine.

What is verified: the hardened runtime does not break ONNX Runtime (it is
statically linked, so no library-validation exemption is needed), the
microphone records with only `com.apple.security.device.audio-input`, and a
bundle carrying its own models resolves them from a working directory that has
none.

What is not: **what a downloader sees**. Notarisation has run twice —
`release.sh` submitted, stapled and published v0.0.1 and v0.0.2, and it refuses
to publish a bundle that fails `xcrun stapler validate` — but that check and
`spctl` both run on the machine that built the bundle. Confirming the release
opens with no Gatekeeper dialog means fetching the published asset in a browser
and opening it, by hand. `setup-signing.sh` still produces a self-signed
identity for local builds, valid on the machine that made it and nowhere else.

Even with the releases out, neither `record` nor `tap` can be run straight from
a shell. They already are ordinary CLI verbs; the constraint is the launch, not
the argument parsing. The truth table in [when it does not
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
