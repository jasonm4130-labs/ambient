---
title: "14. Developer ID and notarisation, not a self-signed identity"
sidebar:
  order: 54
---

# 14. Developer ID and notarisation, not a self-signed identity

**Status:** accepted · **Date:** 2026-08-30 · **Supersedes:** —
· **Extends:** [ADR-0002](0002-signed-bundle-launch.md)

## Context and Problem Statement

[ADR-0002](0002-signed-bundle-launch.md) established that the tap only works
from a signed bundle launched through LaunchServices, and `setup-signing.sh`
answered that with a **self-signed** certificate added to the login keychain as
a trusted code-signing root. That solved the problem it was built for — an
ad-hoc signature's designated requirement is its own cdhash, so every rebuild
orphaned the TCC grant while the permission row still read *allowed*.

It solves nothing for a second machine. The trust it relies on is a root this
script installed locally, so a build signed with it is rejected everywhere else.
The requirement that surfaced this: a release someone can download and run.

Gatekeeper makes the constraint concrete. A file fetched over the network
carries `com.apple.quarantine`, and macOS refuses to launch a quarantined bundle
unless it is signed with a **Developer ID Application** certificate *and*
notarised by Apple. Signing alone is not enough, and notarisation additionally
requires the **hardened runtime**, which the build did not enable.

## Considered Options

**Keep the self-signed identity and document a workaround.** Right-click-Open,
or `xattr -d com.apple.quarantine`. Cheapest, and it is what the project did
implicitly by having no release at all. Rejected because it teaches the one
habit that should never be taught — bypassing Gatekeeper on a downloaded binary
— for a tool whose entire purpose is recording private conversations.

**Developer ID with notarisation and stapling.** Chosen.

## Decision Outcome

`make-app.sh` prefers a Developer ID Application identity when one is in the
keychain, falls back to the self-signed `Ambient Dev`, and signs with
`--options runtime` plus `Ambient.entitlements`. `release.sh` builds, verifies,
notarises, staples and publishes.

Three findings shaped the entitlements, and each was measured rather than
assumed:

- **ONNX Runtime is statically linked** (`otool -L` shows no onnx dylib), so
  the hardened runtime needs no `disable-library-validation`,
  `allow-unsigned-executable-memory` or `allow-jit`. The full Parakeet encoder
  was run under the hardened runtime to confirm it: 3.0 s of audio in 0.67 s.
- **The microphone needs `com.apple.security.device.audio-input`** and nothing
  else. Verified by recording through the hardened bundle and reading a peak of
  0.0337 off the written track.
- **System-audio capture needs no entitlement at all** — it is gated on TCC and
  `NSAudioCaptureUsageDescription`, both already in place.

The entitlements file is therefore one key. Every additional entitlement is a
hole in the runtime protections, so the rule is to add one only after a real run
has failed without it.

**Models ship inside the bundle**, in `Contents/Resources/models`. A download
that then demands `./fetch-models.sh` is not a release. This cost a change to
`models_root()`: it walked only *parents* of the executable, and
`Contents/Resources/models` is a sibling of `Contents/MacOS`, so it was
unreachable. Only the ~671 MB the app actually loads is copied — `models/`
accumulates a second recogniser and hand-fetched archives, and measured 3.3 GB
on the development machine.

## Consequences

The release is ~699 MB and every version re-uploads all of it. That is the
direct cost of the grab-and-use property and is accepted.

Notarisation adds an Apple round trip of minutes to every release, and a
dependency on an App Store Connect API key. The key is held in 1Password and
reached through the committed `.env.op`, which carries only `op://` references.

The private key in `.signing/developerID.key` is still plaintext on disk. It can
re-sign anything as its owner and belongs in 1Password.

Changing signing identity changes the designated requirement, so the existing
TCC grant is orphaned exactly once on the machine that had one. That is the
same mechanism ADR-0002 describes, arriving one final time.

**First exercised on 2026-08-30**: v0.0.1 was notarised, stapled and published
by `release.sh`, and v0.0.2 followed the same day by the same path. The round
trip through Apple's queue took ~11 minutes for this bundle. What `release.sh`
still cannot exercise is the hand check below — the assessment of a downloaded,
quarantined copy on a machine that did not build it.

## Confirmation

`release.sh` fails closed rather than publishing something unusable. It refuses
to start without a Developer ID identity, and after building it asserts
`codesign --verify --deep --strict` and that the bundle carries
`flags=…runtime`, both *before* spending minutes on notarisation. After
stapling it runs `stapler validate` and `spctl -a -vvv -t install`, which is the
check that actually predicts what a downloader sees.

`./release.sh --dry-run` runs everything except notarisation and publication.

The claim that matters cannot be checked by any of those, and is checked by
hand: download the published asset in a fresh browser, confirm the zip carries
`com.apple.quarantine`, unzip, and open it with no Gatekeeper dialog.
