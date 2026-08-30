---
title: "2. Launch only as a certificate-signed bundle through LaunchServices"
sidebar:
  order: 42
---

# 2. Launch only as a certificate-signed bundle through LaunchServices

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

A process tap started from a terminal is created successfully, delivers
buffers at exactly the right rate, and every sample is zero. No error, no
prompt, no log line. TCC attributes the request to the *responsible* process —
the terminal — which holds no audio permission and does not prompt for one.

A second, independent failure sits on top of it. `codesign --sign -` makes the
app's designated requirement its own cdhash, which changes on every rebuild.
TCC stores that hash in the grant's `csreq`, so a rebuild orphans the
permission while the row stays in place still reading *allowed*. The two
services then diverge — `kTCCServiceMicrophone` keeps working,
`kTCCServiceAudioCapture` runs and returns zeros — which is what makes the
pair so easy to conflate.

## Considered Options

- **Run the binary directly**, and accept configuring TCC for the terminal.
- **Ad-hoc signing** (`codesign --sign -`) of the bundle, the zero-setup
  default.
- **A self-signed certificate identity** created once by `setup-signing.sh`,
  with the bundle launched through LaunchServices.

## Decision Outcome

Capture runs only from `build/Ambient.app` launched via `open -a`, signed with
the certificate identity `setup-signing.sh` creates once.

Granting the terminal the permission does not help: the responsible-process
attribution holds regardless of how the app is signed, measured after the
signing fix as `call peak 0.000` for a direct launch against `0.869` for
`open -a`. The certificate makes the designated requirement
`identifier "uk.ambient.cli" and certificate leaf = …`, which survives
rebuilds, where the ad-hoc cdhash does not.

## Consequences

Every capture path — `tap`, `record`, the menu bar app — inherits the bundle
launch. Under LaunchServices the working directory is `/` and stderr goes
nowhere, which is why the settings page is embedded with `include_str!` and
the level meter is mirrored to the `status` file.

Because the failure is silent, `tap` and `record` diagnose it at runtime: they
sample `kAudioProcessPropertyIsRunningOutput` during capture and, if the call
track is flat while something was demonstrably playing, name the likely cause
— launch method first (parent pid 1 means LaunchServices, anything else means
a shell), then the cdhash. On a managed Mac a PPPC profile denying
`kTCCServiceAudioCapture` produces identical symptoms, so the cdhash is ruled
out before IT is blamed.

## Confirmation

CI runs `./make-app.sh` and then `codesign -v --verbose=2 build/Ambient.app`,
`plutil -lint` on the Info.plist, and a test that the bundle's executable
exists — so an unsigned or malformed bundle fails the `rust` job. It cannot
check that the grant survived: no runner can answer a TCC prompt. A person
confirms that half by rebuilding and re-running the tap through `open -a`,
checking the call peak is non-zero rather than `0.000`; the truth tables in
[when it does not work](../using/troubleshooting.md) are the evidence for which
failure a given symptom is.
