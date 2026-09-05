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
    "error": null
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
