---
title: "The session API"
sidebar:
  order: 29
---

# The session API

`src/api.rs` holds one dispatcher, `api::call(method, params, paths)`. `ambient
mcp` ([../using/mcp.md](../using/mcp.md)) is a JSON-RPC parser in front of it
today, and the window ([../adr/0017-one-web-page-for-the-window.md](../adr/0017-one-web-page-for-the-window.md))
calls the same function directly — one dispatcher that both
cannot drift apart on, because there is only one implementation of each verb.

A method either answers with a JSON value or fails with an `ApiError`:
`InvalidParams` is the caller's bug (`ambient mcp` maps it to JSON-RPC
`-32602`), `Failed` is a call that ran and could not answer (`ambient mcp`
maps it to a tool result with `isError: true`).

## `sessions`

Every recorded session on this machine, pinned sessions first and newest first
within each group. Saved notes are included so reopening the editor preserves them.
Capture warnings remain visible in the window, and `audio_available` tells the
page whether retained audio can be used to separate voices.

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
    "notes": "",
    "warnings": [],
    "audio_available": true,
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

## `config.get`

The settings payload behind the window's Settings page: the same shape
`src/settings.rs`'s `push` sends it today, minus `unnamed` (now
`speakers.unnamed`, which takes a `session`).

Request:

```json
{}
```

Reply:

```json
{
  "apps": [],
  "input_device": null,
  "diarize": true,
  "threshold": 0.55,
  "sessions_dir": null,
  "devices": ["MacBook Pro Microphone"],
  "default_dir": "/Users/ana/Documents/Ambient",
  "ask_before_recording": true,
  "audio_retention": "7",
  "roster": ["Ana"],
  "latest_session": "2026-09-05-1200"
}
```

`devices` is whatever CoreAudio reports as attached microphones right now, so
its contents vary between calls. `audio_retention` is a string (`"forever"`
or a number of days as a string); the *settable* key is
`audio_retention_days`, a number or `"forever"`.

## `config.set`

Change one setting, then answer the fresh `config.get` payload.

Request:

```json
{"key": "diarize", "value": "off"}
```

Reply: `config.get`'s reply, with the change applied.

Refuses `sessions_dir` while a session is recording, saying so; refuses a key
`Config::set` does not recognise as `InvalidParams`. Nothing is written on
either refusal.

## `roster.list`

Every name on the roster, alphabetically.

Request:

```json
{}
```

Reply:

```json
["Ana"]
```

## `roster.add`

Add a name to the roster (a no-op if it is already there), then answer the
roster.

Request:

```json
{"name": "Ana"}
```

Reply:

```json
["Ana"]
```

## `roster.remove`

Remove a name from the roster, then answer the roster. Does not unname
anyone in a past recording — those names live in each session's
`edits.jsonl`.

Request:

```json
{"name": "Ana"}
```

Reply:

```json
[]
```

## `speakers.name`

Give every line labelled `label` in one session the name `name`. Appends to
`edits.jsonl`; `raw.jsonl` is untouched.

Request:

```json
{"session": "2026-09-05-1200", "label": "SPEAKER_00", "name": "Ana"}
```

Reply:

```json
{"renamed": 2}
```

## `speakers.undo`

Take back the newest batch of names a person typed for one session.

Request:

```json
{"session": "2026-09-05-1200"}
```

Reply:

```json
{"reverted": 2}
```

## `speakers.unnamed`

Speaker labels nobody has named yet in one session, each with the first thing
that voice said.

Request:

```json
{"session": "2026-09-05-1200"}
```

Reply:

```json
[
  {"label": "call-1", "sample": "shall we start with the export spec"},
  {"label": "room-1", "sample": "yes, go ahead"}
]
```

## `devices`

Input device names, for the Settings page's device picker.

Request:

```json
{}
```

Reply:

```json
{"devices": ["MacBook Pro Microphone"]}
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

## `doctor`

The same ten checks `ambient doctor` prints, in `doctor::run`'s fixed order,
for the window's Welcome page. Each check is `ok` or not; when it is not, the
fix is the `detail` string.

Request:

```json
{}
```

Reply:

```json
[
  {"name": "models/root", "ok": true, "detail": "/Users/you/Library/Application Support/ambient/models"},
  {"name": "models/asr", "ok": true, "detail": "…"},
  {"name": "config", "ok": true, "detail": "defaults"},
  {"name": "sessions/writable", "ok": true, "detail": "/Users/you/Documents/Ambient"},
  {"name": "sessions/stale-lock", "ok": true, "detail": "none"}
]
```
