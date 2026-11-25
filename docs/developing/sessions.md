---
title: "Sessions and the edit layer"
sidebar:
  order: 26
---

# Sessions and the edit layer

A session is what `ambient record` leaves on disk: the audio, what the
recogniser heard, every later repair, and a markdown export derived from the
two. This chapter covers recording one, stopping it, editing it, and
exporting it.

`ambient record [--name <s>] [--app <bundle-id>]… [--seconds <n>]` captures
until stopped, then resamples, transcribes and writes a session under
`~/Documents/Ambient` (`AMBIENT_HOME` overrides):

```
2026-08-29T1101-smoke/
  session.json     metadata, written when the capture ends
  started_at       when capture began, written as it starts
  raw.jsonl        what the recogniser heard — written once per run
  edits.jsonl      speaker labels, then repairs, names, reverts — appended
  transcript.md    the export, regenerated on every change
  status           live level meter while recording, then the stage
  audio/room.wav   mono 16 kHz, mic
  audio/call.wav   mono 16 kHz, tap
  stages/          the helper's output per track and stage (checkpoints)
  previous/        earlier transcripts, archived by a re-run
```

While a job is pending or has failed, `attempts` counts its failures and
`transcribing.lock` names the pid of a running transcriber.

`ambient record --mic-only`, the menu's **Record microphone only** and the
`capture` setting set to `mic` record the microphone alone: no tap is created,
`audio/call.wav` is never written, and `session.json` carries `"capture": "mic"`
(`"all"` otherwise, and for sessions that predate the field). While the
recording is live, before `session.json` exists, a `capture` file in the session
directory holds the same word.

`capture_into` owns the tap and writes native-rate scratch audio. Nothing is
transcribed while recording. After Stop, the capture half resamples each track
to a 16 kHz wav and writes `session.json`, and `transcribe_session` then
transcribes, separates voices and writes `transcript.md`. The CLI does this
straight after capture. The menu bar app hands it to its serial queue, so
another recording can start while the previous session is transcribed. Either
way the models run in `ambient-engine`, one helper process per stage and
track, so the memory they use is returned when each stage ends. See
[transcription](asr.md).

`status` records the stage: `recording …` with the level meter, then `finishing`,
`captured`, `transcribing`, `separating voices`, `done` — or `failed: …`.
A session with `session.json` and no `transcript.md` when the app is not
running is waiting on a queue that no longer exists; the app re-queues it at
its next launch, unless it has already failed twice (see
[retries](asr.md#retries)). While a transcriber is working it holds `transcribing.lock`,
a file naming its pid, so the CLI and the app cannot both transcribe one
session; a lock whose pid is dead is a crash's leftovers and is taken over.

Two logs and one derived file, folded on read. Between runs both logs only
grow; a re-run archives them to `previous/` and starts new ones:

```mermaid
flowchart LR
    raw[("raw.jsonl<br/>written once per run<br/>what the recogniser heard")]
    edits[("edits.jsonl<br/>appended<br/>labels, repairs, names, reverts")]
    fold{{fold}}
    md["transcript.md<br/>derived and disposable"]

    raw --> fold
    edits --> fold
    fold --> md

    revert["a revert appends a record<br/>naming an earlier line"] -.-> edits
```

## Stopping one

`ambient stop` — with no argument it finds the recording in flight and prints
what it stopped:

```
$ ambient stop
  recording 00:22  room 0.310  call 0.828
/Users/…/Documents/Ambient/2026-08-29T1223-standup
```

It writes a `STOP` file that the capture loop notices on its next 200 ms tick.
A file rather than a signal because the only launch that gets system audio is
LaunchServices', which has no terminal to Ctrl-C and no pid a user can see —
and for the same reason the level meter is mirrored to `status`, since stderr
under `open -a` goes nowhere. Ctrl-C still works when there is a terminal, and
`--seconds <n>` bounds an unattended run.

Nothing guards against a second recording. `record` creates its directory
(`src/session.rs:246`) and starts the tap (`src/session.rs:248`) without ever
asking whether one is already live, and `live_session` — the only way a bare
`ambient stop` finds its target (`src/session.rs:948`) — collects every session
directory whose `audio/room.native.wav` was written to in the last ten seconds,
sorts them and pops (`src/session.rs:925`).

The sort is lexicographic over the whole directory name, and that name is a
minute-resolution timestamp plus, when `--name` was given, a slug
(`src/session.rs:240-243`). So what pops is the lexicographically last name,
which is the newest only when the two recordings started in different minutes;
inside one minute the slug decides, and a named session sorts after an unnamed
one whatever the clock said. A bare stop reaches whichever that is and leaves
the other running, so `ambient stop <dir>` (`src/main.rs:66`) is the reliable
form when two are in flight.

Two same-minute recordings with the same name — or with no name, which is what
both a bare `ambient record` and the menu bar use — compute the identical id,
and used to share the directory it named, because `create_dir_all` is
idempotent. They no longer can. `SessionDir::claim` is the only constructor and
claiming is what creates the directory, with `create_dir` rather than
`create_dir_all`: an occupied name comes back `AlreadyExists` instead of
succeeding into it, so the second recording retries as `-02`, `-03` and so on
up to `-99`. A `SessionDir` is therefore evidence that no other recording is
writing there, and the suffix is zero-padded because every listing here is a
byte sort and an unpadded `-10` would sort before `-2`. The id a session
reports is the directory's own name and never the string that was formatted to
ask for it.

That is the shape the sentinel forces rather than an oversight. A `STOP` file
carries no pid, and nothing keeps a registry of live sessions, so liveness is
inferred from one file's modification time and "the newest" is the only
question that mechanism can answer. The same check is why a `record` killed
mid-recording does not intercept a later stop: its scratch wavs stay on disk,
and without the ten-second window that corpse answers `ambient stop`, pointing
it at a directory nothing is writing to. The menu's level line used to be
exposed to the same thing from the other side: during transcription the scratch
wavs are gone, so it fell back to whichever entry in the sessions folder sorted
last — a corpse, or `app.log`, which sorts after every `2…` id. It reads the
capture worker's shared `Meter` now and consults the folder for nothing, which
is what let that fallback be deleted rather than repaired; `app.log` has also
moved out to `~/Library/Logs/Ambient/`. Clearing a corpse out is under [when it
does not work](../using/troubleshooting.md).

`ambient show <dir>` folds `edits.jsonl` over `raw.jsonl`; `--verbatim` skips
the fold. Reverts append a record naming an earlier line rather than deleting
it, so the raw transcript is always recoverable byte-for-byte — verified by
hashing `raw.jsonl` either side of applying an edit layer.

Track is attribution on its own, and for a one-to-one call it is already
complete: room is you, call is them. Several people round a table or on the
same call need the `diarize` setting on, which labels speakers within each
track; see [speaker separation](diarization.md).

Each line of `raw.jsonl` is one speaker's run of words, broken at a speaker
change, a pause of a second or more, or length; a line spanning two people
could not carry a speaker. The capture layer runs at the device rate and the
helper takes 16 kHz — see [resampling](asr.md#resampling) for how that gap is
closed.

Measured on one 19.8 s two-track recording — the same sentence reaching the tap
directly and the microphone acoustically:

```
call (tap, peak 0.869)   "... Priya merged the chunking fix, so the retry-storm ..."
room (mic, peak 0.261)   "... Pre emerge the chunking fix, so the retry storm ..."
```

Which is the argument for tapping rather than pointing a microphone at a
speaker, in one line.

## Naming speakers

`ambient name <dir> call-1 Priya` renames every line currently carrying that
label. It appends `speaker` edits like anything else, so it is undoable. A
later `ambient rerun` archives the session first and carries the name to the
new speaker whose lines overlap Priya's most in time, so a re-run does not turn
Priya back into `call-1`. See [re-running](asr.md#re-running).

## Export

`ambient export <dir> [--out <path>]` writes `transcript.md`, and `record`,
`rerun` and `name` each regenerate it, so the file on disk is never stale.
It is derived — `raw.jsonl` plus `edits.jsonl` stay the only source of truth,
and deleting the markdown loses nothing. YAML front matter carries the session
metadata, which is what a downstream Confluence pipeline would read — that
pipeline lives outside this repo:

```markdown
---
id: "2026-08-29T1223-standup"
name: "standup"
started_at: "2026-08-29T12:23:47.913732+10:00"
duration_s: 20.0
model: "parakeet-ultra/fluidaudio-e2ea498"
speakers: ["Priya", "call-2"]
---

# standup

**[00:00] Priya** (call)
Right, let's start with the Transcoda backlog…

---
_2 room line(s) suppressed as call-track duplicates._
```

### The microphone overhears the call

The room mic picks up the laptop speakers, so on speakerphone every remote
utterance is transcribed twice — once badly by the mic, once well by the tap.
Both tracks in the recording above carried all the same lines. Merging them
without dedup produces a document that reads as broken, and nothing errors:
the file is just wrong.

`dedup_bleed` drops a room line when a call line overlaps it by at least half
the shorter span **and** shares at least 60% of the shorter line's words, after
lowercasing and stripping punctuation. Only room lines are ever dropped — the
call track is both the better copy and the side that cannot be reconstructed if
it goes missing.

The threshold has to survive two independent ASR passes disagreeing (`retry
storm` against `retry-storm`, `Transcoda` against `Transcoder`) without eating
genuine cross-talk, which is the failure that would matter. Both cases are unit
tests, and the fixture strings are real ones the two tracks produced.

### Confidence

Each line in `raw.jsonl` carries `confidence`, the mean of the recogniser's
confidence over its words. Parakeet can produce a fluent, confident sentence
from room noise alone, and no level or probability threshold measured so far
separates quiet speech from an empty room (see
[measurements](measurements.md#three-signals-none-of-which-separates-quiet-speech-from-an-empty-room)).
That needs something that can read the words, so the number is recorded to
give a downstream cleanup pass something to weigh.

See [ADR-0008](../adr/0008-append-only-raw.md).
