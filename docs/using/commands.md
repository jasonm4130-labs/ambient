---
title: "Commands"
sidebar:
  order: 12
---

# Commands

One binary. Run with no arguments it is the app — a menu bar item and the
session browser behind it; run with arguments it stays a CLI, so a single
signed executable serves both. The window is the daily loop now: `show`,
`name`, `diarize` and `export` all have a control in it, and this page stays a
complete reference because scripting and debugging still go through the verbs.
A verb it does not recognise is not an error: `ambient wibble` prints the usage banner on stdout
and exits 0, exactly as asking for help would, so a mistyped verb reads as
success to whatever is scripting it.

## Launching the verbs that need system audio

`tap` and `record` create a Core Audio process tap, and a tap only returns audio
when the process was launched through LaunchServices. Run the bare binary from a
terminal and the tap is created, delivers buffers at the right rate, and every
sample is zero — TCC attributes the request to the terminal, which has no audio
permission and does not prompt.

```sh
./setup-signing.sh                                   # once — stable identity
./make-app.sh                                        # bundle + sign
open -a "$PWD/build/Ambient.app" --args tap /tmp/out.wav 14
```

Why this is the only launch that works, and why a stale signing identity fails
differently from a shell launch, is in [when it does not
work](troubleshooting.md).

## probe

```sh
ambient probe
```

Checks the machine is viable: enumerates audio processes through the Core Audio
bindings, then reports the ONNX Runtime execution-provider state. Needs no
permission — enumeration is not capture — so a clean `probe` says nothing about
whether a tap will return audio.

## tap

```sh
ambient tap <out.wav> <secs> [bundle-id...] [--no-mic] [--via-child]
```

Records both tracks for `<secs>` seconds and writes them as two files,
`<out>.room.wav` and `<out>.call.wav`, at their own device rates — the `.wav`
suffix on the argument is stripped to form the base name. With no bundle IDs it
falls back to the `apps` setting, and if that is empty taps all system audio;
an argument beats the setting, and the settings case is marked `(from settings)`
in the log. `<secs>` defaults to 10 if the argument is missing or unparsable.

Each track is reported with its peak, its total length and its *real* device
seconds, so a stalled tap prints `20.1s, 17.4s real, 2.7s padded` rather than
passing for a healthy one — see [two tracks, two
clocks](../developing/two-tracks.md). If the call track is flat while
something was demonstrably rendering output, it names the likely cause instead
of exiting quietly.

`--no-mic` records the call track only. `--via-child` is a diagnostic: it
re-execs this binary as a child process running `tap <out> <secs>` — dropping
any bundle IDs and `--no-mic` — to answer whether a spawned process inherits the
bundle's audio-capture grant.

## record

```sh
ambient record [--name <s>] [--app <bundle-id>]... [--model <dir>] [--seconds <n>]
```

Captures until stopped, then resamples, transcribes and writes a session
directory, printing its path on stdout. `--app` may be repeated; with no `--app`
the `apps` setting decides. `--model` overrides the ASR directory, which
otherwise defaults to `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` under the
models root; a missing ASR or VAD model is a hard error before any audio is
captured. `--seconds` bounds an unattended run.

Turn segmentation here is `Vad::turns`, not the chunking `transcribe` uses —
one record per turn, because a record spanning two people cannot carry a
speaker. See [sessions](../developing/sessions.md) and [voice activity
detection](../developing/vad-and-chunking.md).

## stop

```sh
ambient stop [<session-dir>]
```

With no argument it finds the recording in flight under the sessions folder.
It writes a `STOP` file that the capture loop notices on its next 200 ms tick,
echoes the session's `status` line to stderr, and prints the directory. A file
rather than a signal because the launch that gets system audio has no terminal
to Ctrl-C and no pid a user can see; Ctrl-C still works when there is a
terminal. With two recordings in flight at once, a bare `stop` writes `STOP`
into whichever live session directory sorts last by name — the newest, when the
two started in different minutes — and leaves the other running; name the
directory to reach a specific one. [When it does not
work](troubleshooting.md) has the cases where sort order and start order come
apart. See [capture](../developing/capture.md).

## show

```sh
ambient show <session-dir> [--verbatim]
```

Prints the session with `edits.jsonl` folded over `raw.jsonl`. `--verbatim`
skips the fold and shows what the recogniser actually produced. Nothing is
mutated. The window's *Tidied*/*Verbatim* toggle is the same pair of views. See
[sessions](../developing/sessions.md).

## name

```sh
ambient name <session-dir> <label> <name>
```

Renames every line currently carrying `<label>`, e.g. `call-1 Priya`, reports
how many lines moved, then prints the session. It appends a `speaker` edit like
anything else, so it is undoable, and a later `diarize` leaves a human-assigned
name alone. The window's naming strip appends the identical edit, on whichever
session is selected.

Asking for a label nothing carries is how you find out which labels exist. It
refuses and lists them: `ambient name <dir> call-2 Priya` on a session holding
two speakers answers `no lines labelled "call-2" (available: call-1, room-1)`,
and on a session that has not been through `diarize` the list reads ``none —
run `ambient diarize` first``. What it lists is the labels as they stand now, so
once `call-1` has been named Priya it is `Priya` that comes back, not the label
she replaced. See [diarization](../developing/diarization.md).

## diarize

```sh
ambient diarize <session-dir> [--threshold <f>]
```

Assigns speakers to a session that already has a transcript, appends the edits
and prints the result. `--threshold` defaults to `0.5` — this path never reads
the config file, so the `threshold` setting does not apply here; it governs only
the automatic diarization at the end of `record`. Re-running appends
`revert` records for the previous run's labels first, so a bad threshold costs a
revert rather than a recording. It **refuses** on a session whose audio has been
swept by retention, and on one with no records in `raw.jsonl` — the window's
*Separate voices* button disables itself and says why in both cases rather than
letting you ask. See
[diarization](../developing/diarization.md).

## export

```sh
ambient export <session-dir> [--out <path>]
```

Writes `transcript.md` — into the session directory unless `--out` says
otherwise — and prints the path. The file is derived; `raw.jsonl` plus
`edits.jsonl` remain the only source of truth, and `record`, `diarize` and
`name` each regenerate it. See [sessions](../developing/sessions.md).

## transcribe

```sh
ambient transcribe <model-dir> <a.wav>
```

Transcribes one file to stdout, with timing on stderr. It accepts any sample
rate and resamples to 16 kHz rather than insisting on 16 kHz input. If
`models/silero_vad.onnx` loads it chunks on VAD boundaries in 30 s windows and
reports the speech-to-total split; if that model is missing it silently falls
back to unchunked long-form decoding, which costs memory linear in length. See
[speech recognition](../developing/asr.md).

## vad

```sh
ambient vad <a.wav>
```

Prints each detected speech segment with its start, end and duration, then the
segment count, total speech and the percentage skipped. Unlike `transcribe` it
requires a 16 kHz wav — it does not resample — and it loads the VAD model from
the relative path `models/silero_vad.onnx`. See [voice activity
detection](../developing/vad-and-chunking.md).

## config

```sh
ambient config [<key> <value>]
```

With no arguments it prints the resolved settings, where sessions will actually
go, the config file path, and the input devices it can currently see — and says
so explicitly when `AMBIENT_HOME` is set and overriding `sessions_dir`. With a
key and a value it sets one setting and writes the file. A key with no value is
an error rather than a read. Anything after the value is dropped without
complaint — `ambient config threshold 0.4 wibble` sets the threshold and says
nothing about `wibble` — where `record`, `export` and `diarize` bail on an
argument they do not recognise. Keys and defaults are in
[settings](settings.md).

## roster

```sh
ambient roster [add <name> | rm <name>]
```

With no arguments it lists the people you record with, one per line. `add` is a
no-op on a name already present (case-insensitively) and keeps the list sorted;
`rm` errors if the name is not there. As with `config`, anything past the name
is dropped in silence: `ambient roster add Priya wibble` adds Priya and never
mentions the rest. Removing someone does not unname them in
past recordings — those names live in each session's `edits.jsonl`. The roster
holds names and nothing else; see [what is kept](what-is-kept.md).

## Environment

| Variable | Read by | Effect |
| --- | --- | --- |
| `AMBIENT_HOME` | `session::home` | Sessions directory. Wins over the `sessions_dir` setting. |
| `AMBIENT_CONFIG` | `config::path` | Path to the config file, so a test can point somewhere harmless. |
| `AMBIENT_ROSTER` | `roster::path` | Path to `roster.json`, which otherwise sits beside the config. |
| `AMBIENT_MODELS` | `session::models_root` | Models directory. Otherwise `models/` relative to the working directory, then searched upward from the executable. |
| `AMBIENT_DEBUG_DIAR` | `diarize` | Set to anything: adds the per-window powerset class histogram. |
| `AMBIENT_DEBUG_TAP` | `capture` | Set to anything: extra tap-level diagnostics during capture. |
| `HOME` | `config::path`, `session::home`, `settings` | Falls back to `.` if unset, so both paths become relative. |
