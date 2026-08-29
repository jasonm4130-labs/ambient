# Settings

Every setting has code behind it. A stored value that nothing reads is a promise
the app does not keep, so the struct in `src/config.rs` and the key list in
`Config::set` are the whole surface — anything not below is not a setting.

| Key | Effect | Type / accepted values | Default |
| --- | --- | --- | --- |
| `apps` | Bundle IDs to tap. Empty taps everything the Mac plays. | Comma-separated list; blanks trimmed and dropped | empty |
| `input_device` | Record the room with a named device rather than the system default. | Device name as `ambient config` lists it; `default` or the empty string clears it | system default |
| `diarize` | Separate voices once the transcript exists. | `true`/`yes`/`on`/`1`, or `false`/`no`/`off`/`0` | `true` |
| `threshold` | How readily two utterances are called different people. | Float | `0.5` |
| `sessions_dir` | Where sessions are written. | Path; `default` or the empty string clears it | `~/Documents/Ambient` |
| `ask_before_recording` | Wait to be told before recording a call the app noticed. | `true`/`yes`/`on`/`1`, or `false`/`no`/`off`/`0` | `true` |
| `audio_retention_days` | How long the track wavs are kept. `0` deletes them as soon as the transcript exists. | Unsigned integer, or `forever` / `never` to keep them indefinitely | `7` |

Booleans accept nothing outside those eight words: `ambient config diarize maybe`
is refused and leaves the setting as it was, rather than reading an unrecognised
word as a no. An unknown *key* is likewise refused by name, and the error lists the keys
that do exist.

## Precedence

**Environment → CLI flag → config file → default.** So `AMBIENT_HOME` beats
`sessions_dir`, a `--app` on the command line beats the stored `apps` list, and
an existing harness that sets neither is unaffected. `ambient config` with no
arguments prints the resolved values and says outright when `AMBIENT_HOME` is
overriding `sessions_dir`.

The file is `~/Library/Application Support/Ambient/config.json` — deliberately
not under the sessions folder, since that folder is itself a setting, and a
config that lives inside the thing it configures cannot be found before it is
read. `AMBIENT_CONFIG` moves the file for tests.

Unknown fields in the file are ignored rather than discarding the known ones, so
a config written by a newer build still loads in an older one; a partial file
keeps the defaults for everything it omits.

## The roster

`roster.json` sits beside the config — same reasoning, and `AMBIENT_ROSTER`
overrides its path. It is a plain JSON list of names and nothing else: adding a
name already present is a no-op rather than an error, and the list is kept
sorted case-insensitively.

It stores no voiceprints. An embedding kept to recognise someone later is
biometric data under Article 9, a different compliance regime from a text file
of names, so the roster removes the retyping and not the choosing — see
[privacy](../architecture/privacy.md).

## Behaviour on bad input

A **missing** config file is the ordinary first run: defaults, silently. A file
that cannot be read, or that is not valid JSON, prints a `WARNING` naming the
path and the parse error and falls back to defaults rather than failing a
recording. The roster behaves the same way and treats an unreadable list as
empty.

A named `input_device` that has gone away **warns and names the alternatives**
before falling back to the system default:

```
  WARNING: no input device named "Iriun Webcam Audio" — recording with the system default instead. Available: MacBook Pro Microphone, …
```

Silent fallback is this project's recurring failure and the one thing a settings
layer must not reintroduce.
