---
title: "16. An `ambient mcp` verb is how other programs read sessions"
sidebar:
  order: 56
---

# 16. An `ambient mcp` verb is how other programs read sessions

**Status:** accepted · **Date:** 2026-09-05 · **Supersedes:** —

## Context and Problem Statement

Issue #6 asks for a way to read a session from another program while it is
being written: an assistant that answers "what did they just say" needs the
transcript as data, not a Markdown file to scrape. Today the only readers are
the window and `ambient show`, both for a person. The transcript format is
already stable and append-only ([0008](0008-append-only-raw.md)); what is
missing is a wire.

The reader is an assistant such as Claude Code running in a session of its
own, which already carries its own tools for Jira, Confluence and the rest.
Ambient supplies the ears and nothing else: no lookups, no summaries, no
prompting built in.

## Considered Options

- **A Model Context Protocol server as a verb of the one binary.** Every
  assistant that matters speaks it; stdio transport needs no port, no auth, no
  daemon; the user registers `ambient mcp` once.
- **A local HTTP API.** A port to pick, a token to manage, and a client library
  every reader writes itself.
- **A file-watching convention (readers tail `raw.jsonl`).** Nothing to build,
  but every reader reimplements the edit fold and the session-directory rules.

## Decision Outcome

`ambient mcp` serves the Model Context Protocol over stdio, hand-rolled:
newline-delimited JSON-RPC 2.0, synchronous, one request at a time, no async
runtime. Ambient has no tokio and the protocol's server side needs none for
stdio; a dependency the size of an MCP SDK buys nothing here. The server
exposes three tools, `sessions`, `transcript` and `status`, and reads exactly
what `ambient show --json` and `ambient sessions --json` print, so the CLI and
the server cannot disagree. Consent is the armed state
([0009](0009-armed-consent-state.md)): a session that exists was consented to
when it was recorded, and reading it back needs nothing more.

## Consequences

Live reading is only as live as the transcript on disk: today `raw.jsonl` is
written after capture ends, so a live session answers with what is there and a
`live: true` flag. Making the file grow during capture is a separate decision
that waits on a measurement (the live-transcription plan). The server is a
single-threaded loop; a slow tool call blocks the next request, which is
acceptable for a reader of local files. Out of scope: push notifications,
resources, prompts, HTTP transport.

## Confirmation

`cargo test mcp::` pipes a scripted JSON-RPC conversation through the loop and
checks every response line; `docs/using/mcp.md` carries the registration
command, and the docs link check on pull requests keeps it pointing at real
pages.
