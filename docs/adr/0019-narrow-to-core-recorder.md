---
title: "19. Narrow Ambient to the core recorder"
sidebar:
  order: 59
---

# 19. Narrow Ambient to the core recorder

**Status:** accepted · **Date:** 2026-11-25 · **Supersedes:** [ADR-0018](0018-live-assistant-runs-in-the-users-agent.md)

## Context and Problem Statement

Ambient grew three things beside its job. The job is to record a meeting,
transcribe it, separate the speakers, and let a person browse, name, search and
export the result. The additions were the live assistant ([ADR 0018](0018-live-assistant-runs-in-the-users-agent.md)),
the `ambient talk` path that sent spoken notes to firstmate and read the reply
aloud, and the `voice/` Python package that made the speech. Each carried its
own settings, menu items, CI job, documentation and tests, and none of them
made the recorder better at recording. The owner decided to spend the effort on
a polished recorder instead.

## Considered Options

- **Keep everything, hide it.** Leaves the settings keys, the CI job and the
  code to maintain for features nobody is working on.
- **Park the assistant behind a flag.** The same upkeep, plus a flag whose
  default is a promise no one is keeping.
- **Remove the assistant, talk path and voice package; keep MCP read-only.**
  Chosen.

## Decision Outcome

The live assistant (`src/assist/`), the talk path (`src/talk.rs`) and the
voice package (`voice/`) are removed, with their `assistant.*`, `voice.*` and
`talk.*` settings, the **Live Assistant** menu items, the `ambient talk` verb,
the assistant's MCP tools and `watch` prompt, and the `voice` CI job.

`ambient mcp` stays, read-only, for the session library: the `sessions`,
`transcript` and `status` tools. [ADR 0016](0016-mcp-verb-for-live-reading.md)
still stands. Capture, transcription, diarisation, session storage and export
are unchanged, including the "Copy for an assistant" export, which pastes text
and needs no assistant code.

## Consequences

An older `config.json` that still holds `assistant`, `voice` or `talk` keys
loads: unknown keys are ignored, and the next save drops them. `ambient config
assistant on` and its siblings are now refused as unknown settings. An
agent registered against `ambient mcp` that called `watch_meeting`, `speak`,
`wait_for_transcript` or `stop_watching`, or fetched the `watch` prompt, gets
an error, and `ambient mcp --no-play` or `--save-audio` now exits with a usage
error rather than serving, so a client registered with either flag must drop
it. Sessions recorded with the assistant keep their `assistant.jsonl`; nothing
reads it any more, and deleting the session removes it. The design behind the removed features is kept in
[ADR 0018](0018-live-assistant-runs-in-the-users-agent.md) and the git history,
so reviving it means writing a new record.

## Confirmation

`config::tests::a_config_file_with_removed_keys_still_loads` loads a file with
the removed keys. `mcp::tests::tools_list_describes_the_session_tools` and
`the_assistant_tools_and_prompt_are_gone` pin the MCP surface.
