---
title: "The session API"
sidebar:
  order: 29
---

# The session API

`src/api.rs` holds one dispatcher, `api::call(method, params, paths)`. `ambient
mcp` ([../using/mcp.md](../using/mcp.md)) is a JSON-RPC parser in front of it
today, and the window ([../adr/0017-one-web-page-for-the-window.md](../adr/0017-one-web-page-for-the-window.md))
calls the same function directly once it exists — one dispatcher that both
cannot drift apart on, because there is only one implementation of each verb.

A method either answers with a JSON value or fails with an `ApiError`:
`InvalidParams` is the caller's bug (`ambient mcp` maps it to JSON-RPC
`-32602`), `Failed` is a call that ran and could not answer (`ambient mcp`
maps it to a tool result with `isError: true`).

## `sessions`

Every recorded session on this machine, newest first.

Request:

```json
{}
```

Reply:

```json
[
  {
    "id": "2026-09-05-1200",
    "name": null,
    "started_at": "2026-09-05T12:00:00+01:00",
    "duration_s": 612.5,
    "transcribed": true,
    "live": false,
    "transcribing": false,
    "error": null,
    "tags": [],
    "pinned": false
  }
]
```

## `transcript`

The lines of one session, including one being recorded now.

Request:

```json
{"session": "2026-09-05-1200", "since": 4, "verbatim": false}
```

`since` and `verbatim` are optional; `since` defaults to `0` and `verbatim` to
`false`.

Reply:

```json
{
  "session": "2026-09-05-1200",
  "state": "done",
  "next": 6,
  "lines": [{"track": "room", "start_ms": 5000, "end_ms": 6000, "text": "…"}]
}
```

## `search`

Case-insensitive substring search over every session's folded transcript,
newest session first.

Request:

```json
{"query": "budget", "limit": 50}
```

`limit` is optional and defaults to `50`.

Reply:

```json
[
  {
    "session": "2026-09-05-1200",
    "index": 3,
    "track": "room",
    "start_ms": 5000,
    "speaker": null,
    "text": "the budget review is at noon"
  }
]
```

`index` is the line's position in `transcript_appended` order — the order
lines were written, not the order their clocks say — so it can be used to
re-fetch the same line from `transcript`.

## `session.update`

Rename a session, add or remove one tag, replace its notes, or pin it —
whichever fields are present in the request. Rewrites `session.json` only;
`raw.jsonl` and `edits.jsonl` are untouched.

Request:

```json
{"session": "2026-09-05-1200", "name": "Standup", "add_tag": "1:1", "pinned": true}
```

`name`, `notes`, `pinned`, `add_tag` and `remove_tag` are all optional; only
the fields present are applied. `add_tag` of a tag already on the session and
`remove_tag` of one that is not are no-ops, so two callers each adding a
different tag both keep theirs.

Reply, the session's full metadata after the change:

```json
{
  "id": "2026-09-05-1200",
  "name": "Standup",
  "started_at": "2026-09-05T12:00:00+01:00",
  "ended_at": "2026-09-05T12:10:12+01:00",
  "duration_s": 612.5,
  "device_hz": 48000,
  "mic_hz": 48000,
  "channels": 1,
  "mic_channels": 1,
  "apps": [],
  "model": "parakeet",
  "warnings": [],
  "tags": ["1:1"],
  "notes": "",
  "pinned": true
}
```

A session still recording has no `session.json` yet and answers `Failed`
saying so; a session a transcriber currently holds the lock on answers
`Failed` saying it is being transcribed.

## `session.delete`

Remove a session directory and its audio entirely. The window shows its own
confirm dialog before calling this; the API does not ask again.

Request:

```json
{"session": "2026-09-05-1200"}
```

Reply:

```json
{"session": "2026-09-05-1200", "deleted": true}
```

Refuses a live capture — the session recording now, or either native scratch
track still growing — saying the session is still recording, and a session a
transcriber currently holds the lock on answers `Failed` saying it is being
transcribed. A directory with no `session.json` and no fresh audio is a
failed capture and is removed like any other.

## `export`

A session rendered as `markdown`, `text`, `json`, `srt`, `vtt` or `assistant`.
Dispatcher-only: not in `methods()` and not offered over MCP, the same
posture as `search`.

Request:

```json
{"session": "2026-09-05-1200", "format": "srt"}
```

`format` is optional and defaults to `"markdown"`; a name it does not
recognise is `InvalidParams` naming all six.

Reply:

```json
{
  "session": "2026-09-05-1200",
  "format": "srt",
  "text": "1\n00:00:05,000 --> 00:00:06,000\nthe budget review is at noon\n"
}
```

Every format but `markdown` reads the raw transcript folded with
`edits.jsonl`, not `markdown`'s bleed-deduped view, so exporting a call with
speakerphone overlap keeps both copies where the markdown document would drop
one.

## `status`

What Ambient is doing right now: the live session, if any, and what is
waiting.

Request:

```json
{}
```

Reply:

```json
{"live": null, "awaiting_transcript": 0, "sessions": 3}
```
