---
title: "Ambient"
description: "Install Ambient, record locally, and work with your transcripts."
sidebar:
  order: 0
---

# Ambient

Ambient records room audio and calls on your Mac, transcribes speech locally,
and separates speakers. Its menu bar controls recording; its window lets you
browse, search, name speakers and export transcripts. No bot joins the call.

Start with **[Getting started](using/getting-started.md)** for installation and
a short test recording. Release builds target Apple Silicon Macs with macOS
14.4 or later and include the speech models.

## Choose a guide

| Your task | Guide |
| --- | --- |
| Install and record for the first time | [Getting started](using/getting-started.md) |
| Select audio devices or watched apps | [Settings](using/settings.md) |
| Understand consent, storage and deletion | [What is kept](using/what-is-kept.md) |
| Fix an empty recording or a permission problem | [Troubleshooting](using/troubleshooting.md) |
| Use the CLI | [Commands](using/commands.md) |
| Give an assistant read access to sessions | [MCP](using/mcp.md) |
| Change the app | [Developing Ambient](developing/index.md) |
| Build the documentation | [Building these docs](developing/docs.md) |

## Know the limits

Ambient is early software. Speaker separation groups voices; it does not know
people's names until you assign them. Recognition can get names and specialist
terms wrong. Check the transcript before treating it as a record of a decision.

Capture, transcription and diarization have been exercised on an Apple Silicon
Mac. The [measurements](developing/measurements.md) name the hardware and fixtures;
they are not a performance promise for every Mac. Hosted CI checks the code and
bundle assembly, not microphone permissions or live capture.

The repository has releases through v0.0.3. See
[Releases](https://github.com/jasonm4130-labs/ambient/releases) for available
builds. The release process signs, notarizes and staples the app; a fresh-machine
installation and capture check remains part of release acceptance.

## Keep control of the data

Capture and inference stay on the Mac. Exporting a transcript or connecting an
MCP client creates another way to share its contents. A cloud-synced sessions
folder can upload files independently of Ambient. Record only with appropriate
permission, and read [what is kept](using/what-is-kept.md) before changing retention.

Ambient's source is MIT licensed. Models have separate terms documented in the
repository's [third-party notices](https://github.com/jasonm4130-labs/ambient/blob/main/THIRD_PARTY_NOTICES.md).
