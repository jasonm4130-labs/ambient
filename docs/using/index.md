---
title: "Using ambient"
sidebar:
  order: 10
---

# Using ambient

Five pages, in the order you will want them.

**[Getting started](getting-started.md)** — build the binary, fetch the models,
launch it, and read what the app is once it is running. The launch method is
not optional: run the bare binary and every sample comes back zero, because the
microphone grant lands on the terminal rather than on ambient. The page explains
the signed bundle before anything else.

**[Commands](commands.md)** — every verb the binary takes, what each one writes,
and what it prints.

**[Settings](settings.md)** — what you can change, where the values live, and
which ones take effect on the next recording rather than immediately.

**[What is kept](what-is-kept.md)** — consent, audio retention and speaker
names. Read this before recording anyone who is not you.

**[When it does not work](troubleshooting.md)** — a recording that succeeds and
contains silence is the failure you will hit, and it is a permissions problem
every time. Two truth tables and the fix.

## What a recording produces

One directory per session under `~/Documents/Ambient`: `transcript.md` to read,
`raw.jsonl` and `edits.jsonl` holding what the recogniser heard and everything
applied on top of it, `session.json`, and — until retention sweeps it — `audio/`.
[What is kept](what-is-kept.md) describes each file and how long it lives.

Reading one is the window's job rather than a page's: *Open Ambient* in the
status menu, ⌘0, lists every session and shows the transcript beside the
naming and diarization controls for whichever is selected. The verbs on
[commands](commands.md) do the same work from a script.
