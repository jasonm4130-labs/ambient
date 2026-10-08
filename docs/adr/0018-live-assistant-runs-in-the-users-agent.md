---
title: "18. The live assistant's judgement runs in the user's own agent; Ambient serves the tools"
sidebar:
  order: 58
---

# 18. The live assistant's judgement runs in the user's own agent; Ambient serves the tools

**Status:** accepted · **Date:** 2026-10-09 · **Supersedes:** —

## Context and Problem Statement

The live assistant follows a meeting as it is recorded and speaks through a
local voice when it has something worth adding. Something has to decide, after
each stretch of transcript, whether to speak and what to say. The first build
put that decision in Ambient: an `ambient assist` loop that asked a cheap
jump-in model on OpenRouter whether to speak, then a reply model what to say,
with the key supplied through 1Password.

Before it shipped, the captain asked for the opposite: run Haiku on his
existing Claude subscription and tell it to watch the meeting over MCP.
[0016](0016-mcp-verb-for-live-reading.md) already made `ambient mcp` the way
another program reads a live session.

## Considered Options

- **Ambient calls the models (the first build).** Ambient owns the prompts and
  the latency. It also needs an API key, a separate bill, an HTTP client in the
  binary, and network calls from Ambient.
- **The user's agent calls the models; Ambient serves tools.** The agent runs
  in the user's Claude Code session, on their plan. Ambient adds tools to wait
  for the transcript and to speak, and serves the instructions as an MCP
  prompt.
- **Both, with the built-in loop as an option.** It keeps every cost of the
  first option to duplicate the second.

## Decision Outcome

The agent decides; Ambient enforces. `ambient mcp` gains `watch_meeting`,
`wait_for_transcript`, `speak` and `stop_watching`, and a `watch` prompt. What
must hold whatever the agent does lives in those tools, where an agent cannot
skip it:

- the `assistant` switch;
- the spoken consent notice before anything else;
- the heartbeat behind the menu bar's listening notice;
- the cooldown and per-meeting cap on speaking;
- dropping the assistant's own voice from what the agent reads.

The built-in loop and its settings were removed. The deciding fact was that a
model the user already pays for, in a client they already run, makes the
assistant free per meeting and takes Ambient off the network entirely.

## Consequences

- `ambient mcp` is no longer read-only. The session tools still are. The
  assistant tools write only the heartbeat beside the config file and the
  session's `assistant.jsonl`, and they speak.
- `wait_for_transcript` blocks the synchronous MCP loop for up to two minutes.
  A client that calls one tool at a time does not notice; one that pipelines
  calls would wait behind it.
- Ambient no longer controls the model, its latency or its prompt adherence.
  The tools' refusals are worded for an agent, and the prompt is served by
  Ambient so every client gets the same instructions.
- The cooldown and cap are per meeting: each watch resumes them from the
  session's `assistant.jsonl`, so they are shared by every `ambient mcp`
  process watching that meeting. There is no locking, so two processes that
  speak at the same instant can each pass the check.
- The transcript leaves the Mac through the user's agent, under that client's
  terms rather than a zero-data-retention route Ambient chose.

## Confirmation

The `assist::tests` suite covers the tools: off by default, the notice first,
cooldown and cap, echo filtering, the toggle and the meeting ending. The
`mcp::tests` suite covers the tool list and the prompt. Cargo.toml lists no
HTTP client. Prompt adherence has no automated check: the design note records
a sample-meeting run with Haiku, and a change to the prompt needs a fresh run.
