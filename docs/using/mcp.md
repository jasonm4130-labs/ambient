---
title: "Reading sessions from an assistant"
sidebar:
  order: 16
---

# Reading sessions from an assistant

`ambient mcp` turns the binary into a [Model Context
Protocol](https://modelcontextprotocol.io) server so another program — Claude
Code, or anything else that speaks MCP — can read your sessions as data instead
of scraping `transcript.md`. It speaks newline-delimited JSON-RPC 2.0 over
stdio: no port, no token, no daemon. The client starts the process, and the
process exits when the client closes its stdin.

It exposes three tools and **nothing writes**. There is no verb here to start a
recording, name a speaker or delete a session; a reader can read, and that is
the whole surface. Ambient supplies the ears and nothing else: no lookups, no
summaries, no prompting built in. The assistant on the other end brings its own.

## Access and privacy

The MCP server can read sessions available to the Ambient process. It does not
prompt before each read, and read-only access still exposes conversation text.
Only register it with a client you trust. An assistant may send tool results to
its cloud provider under that client's settings; local transcription does not
make that later sharing local.

To limit access, start the server with `AMBIENT_HOME` pointing at a separate
folder containing only sessions you intend to share. This is a directory scope,
not per-session authorization. The server does not authenticate a client over
stdio; the process that launches it controls its access.

## Registering it

With the binary on your `PATH`:

```sh
claude mcp add ambient -- "$(which ambient)" mcp
```

The signed bundle is the same binary, so if you launch Ambient as an app and
never installed a copy on `PATH`, point at the executable inside it:

```sh
claude mcp add ambient -- "$PWD/build/Ambient.app/Contents/MacOS/ambient" mcp
```

Either path works: the MCP verb reads files and needs no microphone, so it
does not care which of the two started it. Only `tap` and `record` do — see
[launching the verbs that need system audio](commands.md#launching-the-verbs-that-need-system-audio).

Other clients have their own configuration location and schema. For clients
that accept an `mcpServers` object, the example below shows the command and
arguments. Give the command an absolute path: a stdio server
is spawned directly rather than through a shell, so `~` and `$(which …)` arrive
as literal characters.

```json
{"mcpServers":{"ambient":{"command":"/path/to/ambient","args":["mcp"]}}}
```

To check the registration without a client, pipe a request in yourself. The
server answers one line per request:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | ambient mcp
{"id":1,"jsonrpc":"2.0","result":{"tools":[{"description":"Every recorded session on this machine, newest first.","inputSchema":{"properties":{},"type":"object"},"name":"sessions"},{"description":"The lines of one session, including one being recorded now.","inputSchema":{"properties":{"session":{"description":"The session id, as `sessions` reports it.","type":"string"},"since":{"description":"Skip this many lines; pass back the previous reply's `next`.","type":"integer"},"verbatim":{"description":"Return the words as recognised, before any naming edits.","type":"boolean"}},"required":["session"],"type":"object"},"name":"transcript"},{"description":"What Ambient is doing right now: the live session, if any, and what is waiting.","inputSchema":{"properties":{},"type":"object"},"name":"status"}]}}
```

## The three tools

Every tool answers in the MCP shape — `{"content":[{"type":"text","text":"…"}],"isError":false}`
— where the text is the tool's JSON, serialised compactly on one line. The
examples below show that text, indented so it can be read. A tool that ran and
failed answers `isError: true` with the reason as plain text, so the assistant
sees it rather than the transport swallowing it:

```json
{"content":[{"text":"no session nope","type":"text"}],"isError":true}
```

### `status`

No arguments. What Ambient is doing this second — cheap enough to call on every
turn.

```json
{"awaiting_transcript":0,"live":{"id":"2026-09-05-1608"},"sessions":2}
```

`live` is `null` when nothing is recording. `awaiting_transcript` counts
sessions whose audio is captured but not yet transcribed, and `sessions` is how
many `sessions` will list.

### `sessions`

No arguments. Every session on this machine, newest first — the same rows
`ambient sessions --json` prints.

```json
[
  {
    "dir": "/Users/…/Documents/Ambient/2026-09-05-1608",
    "duration_s": null,
    "error": null,
    "id": "2026-09-05-1608",
    "live": true,
    "name": null,
    "started_at": null,
    "transcribed": false,
    "transcribing": false
  },
  {
    "dir": "/Users/…/Documents/Ambient/2026-09-05-1412",
    "duration_s": 1184.0,
    "error": null,
    "id": "2026-09-05-1412",
    "live": false,
    "name": "Roadmap sync",
    "started_at": "2026-09-05T14:12:03Z",
    "transcribed": true,
    "transcribing": false
  }
]
```

That first row is the session being recorded, and its `name`, `started_at` and
`duration_s` are `null` because `session.json` is written when capture ends —
a live row carries its `id` and `live: true` and nothing more. A session whose
`session.json` will not parse comes back with `error` set rather than being
hidden. Symlinks are skipped: only real directories under the sessions folder
are listed.

### `transcript`

`session` (required, an `id` from `sessions`), `since` (default 0) and
`verbatim` (default false, which folds naming edits over what the recogniser
heard).

```json
{
  "lines": [
    {"end_ms": 3120, "speaker": null, "start_ms": 0, "text": "Right, shall we start with the migration?", "track": "room"},
    {"end_ms": 11040, "speaker": null, "start_ms": 8200, "text": "Did the backfill finish?", "track": "room"},
    {"end_ms": 7980, "speaker": "Priya", "start_ms": 3400, "text": "Yes. I pushed the schema change on Friday.", "track": "call"}
  ],
  "next": 3,
  "session": "2026-09-05-1412",
  "state": "done"
}
```

`track` is `room` for the microphone and `call` for what the tap heard, and
`speaker` is `null` until something has named it — the same fields
[`ambient show --json`](commands.md#show) prints. The lines arrive in the order
they were written, every room line then every call line, which is why the call
line above sits after a room line with a later `start_ms`; sort on `start_ms`
if you want the clock.

## Following a session as it is recorded

`next` is a cursor: the number of lines the session holds. Pass it back as
`since` and you get only what has appeared since, so a poll loop is one call
with the previous reply's `next`.

```json
{"lines":[],"next":3,"session":"2026-09-05-1412","state":"done"}
```

`state` says whether asking again is worth it:

| `state` | What it means | Ask again? |
| --- | --- | --- |
| `live` | Audio is being captured right now. | Yes |
| `transcribing` | Capture finished; the recogniser is running. | Yes |
| `pending` | Captured, waiting its turn in the queue. | Yes |
| `done` | `transcript.md` is written. `next` will not move. | No |

The cursor counts lines rather than milliseconds because `raw.jsonl` is written
one track at a time — every room line, then every call line — and the two
tracks' clocks run independently. A cursor on `start_ms` would skip every call
line that landed after a later room line.

One caveat, and it is the reason `live` reads as it does: **`raw.jsonl` is
written after capture ends**, so a session that is still recording answers with
`state: "live"` and no lines at all.

```json
{"lines":[],"next":0,"session":"2026-09-05-1608","state":"live"}
```

Live in the sense of "you can see it happening", not yet "you can read what was
just said". Making the transcript grow during capture is a separate decision,
and it waits on a measurement — [ADR 0016](../adr/0016-mcp-verb-for-live-reading.md)
records why.

## When it will not answer

Nothing but JSON-RPC ever goes to stdout; diagnostics go to stderr, so a client
that shows you the server's log is showing you stderr. A malformed line gets a
JSON-RPC error back and the loop keeps going — the server does not exit because
a client sent it rubbish.

A `session` argument is one path segment, and it must name a real directory
inside the sessions folder. `../something`, an empty string, `.` and a symlink
are each refused, and no file outside the folder is ever read. If a tool answers
`refusing symlink …`, something inside the session directory is a link rather
than a file; Ambient will not follow it.
