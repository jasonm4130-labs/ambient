---
title: "Using ambient"
sidebar:
  order: 10
---

# Using ambient

Start with installation, then follow the guide for the task at hand.

**[Getting started](getting-started.md)** — install a release or build from
source, make a short test recording, and read your transcript.

**[Commands](commands.md)** — every verb the binary takes, what each one writes,
and what it prints.

**[Settings](settings.md)** — what you can change, where the values live, and
which ones take effect on the next recording rather than immediately.

**[What is kept](what-is-kept.md)** — consent, audio retention and speaker
names. Read this before recording anyone who is not you.

**[When it does not work](troubleshooting.md)** — a recording that succeeds and
contains silence is the failure you will hit, and it can be a permissions or launch problem. Diagnose both tracks before changing settings.

**[Reading sessions from an assistant](mcp.md)** — `ambient mcp` hands the
transcript to Claude Code or any other MCP client as data. Registration, the
three read-only tools, and how to follow a session while it records.

## What a recording produces

One directory per session under `~/Documents/Ambient`: `transcript.md` to read,
`raw.jsonl` and `edits.jsonl` holding what the recogniser heard and everything
applied on top of it, `session.json`, and — until retention sweeps it — `audio/`.
[What is kept](what-is-kept.md) describes each file and how long it lives.

Reading one is the window's job rather than a page's: *Open Ambient* in the
status menu, ⌘0, lists every session and shows the transcript beside the
naming and diarization controls for whichever is selected. The verbs on
[commands](commands.md) do the same work from a script.
