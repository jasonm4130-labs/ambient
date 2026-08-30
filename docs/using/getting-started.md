# Getting started

## What you need first

macOS 14.4 or later: that is the `LSMinimumSystemVersion` `make-app.sh` writes
into the bundle's Info.plist.

`rustup`, before anything else. `rust-toolchain.toml` pins the compiler to
1.95.0, and rustup is what reads that file and fetches the pinned compiler. A
cargo installed any other way — Homebrew, a distro package — ignores the pin
silently and builds with whatever rustc it has, which is the worse failure of
the two, so rustup comes first. `Cargo.toml` separately names 1.85 as the
oldest release the crate claims to build on; the pin is what actually compiles
it.

The macOS SDK, from Xcode or the Command Line Tools. `objc2`,
`objc2-core-audio` and `objc2-app-kit` are bindings over system frameworks, and
the built binary links CoreAudio, AppKit, WebKit and CoreML out of
`/System/Library/Frameworks`. What a machine with no SDK actually reports has
not been tested here. The Command Line Tools alone are enough:
the machine these numbers come from carries no full Xcode, and `xcode-select -p`
answers `/Library/Developer/CommandLineTools`.

About 670 MB of disk for the models. `./fetch-models.sh` fetches four things —
the Parakeet recogniser at 640 MB, WeSpeaker embeddings at 25 MB, pyannote
segmentation at 5.7 MB, and Silero VAD under a megabyte. The recogniser arrives
as a 465 MB archive that is unpacked and then deleted, so the download wants
roughly 1.1 GB free while it runs.

## Build it

```sh
./fetch-models.sh                 # ~670 MB: recogniser, diarization, VAD
cargo build --release
cargo run --release -- probe      # is this machine viable?
```

`probe` enumerates the audio processes it can see and reports the execution
provider state. It needs no permissions, so it works from a terminal, unlike
`record` and `tap` below.

## Transcribe a file

```sh
cargo run --release -- transcribe models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 audio.wav
```

`transcribe` accepts any sample rate and resamples. To convert anything else
first: `ffmpeg -i in.m4a -ac 1 -ar 16000 out.wav`

## Record something

This is the part that will not work the way you expect.

```sh
./setup-signing.sh                # once — creates a stable signing identity
./make-app.sh                     # bundle and sign
open -a "$PWD/build/Ambient.app"  # menu bar app
```

**Run the bare binary from a terminal and every sample is zero.** The tap is
created successfully, delivers buffers at the correct rate, and contains
silence, with no error and no permission prompt — because macOS attributes the
request to the terminal, which has no audio grant. `./setup-signing.sh` and
`open -a` are both required, and [when it does not
work](troubleshooting.md) explains why in detail. If you skip this section you will spend an afternoon debugging a
working program.

For the CLI verbs through the bundle, append `--args`:

```sh
open -a "$PWD/build/Ambient.app" --args record --name standup
cargo run --release -- stop       # or: the menu bar item
```

## It asks twice

The bundle declares two usage descriptions, not one:
`NSAudioCaptureUsageDescription` for the audio a call plays, and
`NSMicrophoneUsageDescription` for the room. They are two TCC services and two
grants, and the code holds them apart as well — `ProcessTap::start` opens the
input device before the tap exists, on its own device and its own clock, and the
microphone is deliberately kept out of the tap's aggregate.

So the first recording asks for both, and allowing one is not allowing the
other. Deny the microphone and the call track still records while the room track
comes back flat, which `record` says out loud: `WARNING: the room track is
silent — check the microphone grant.` Deny system audio and it is the call track
that holds zeros: macOS returns no error and never prompts again, but `record`
does notice — a flat call track while something was demonstrably playing prints
a warning naming a denied tap and pointing at the cdhash check
(`silent_tap_advice`, `src/capture.rs`). That warning goes to stderr, so a
bundle launch has nowhere to show it; [when it does not
work](troubleshooting.md) carries it. That both prompts arrive
during the first recording rather than at launch is read off the code path here
and has not been watched happening.

Both prompts are macOS asking. What ambient itself asks — whether to record a
call it noticed, and what it keeps once the transcript exists — is [what is
kept](what-is-kept.md).

## Check the settings

```sh
cargo run --release -- config     # resolved settings, and the input devices it can see
```

Every setting is readable and settable from the CLI, so nothing depends on the
GUI being open. The same values are editable in the running app through the menu
bar's *Settings…* item, key equivalent `,`, which opens a window writing that
same `config.json` plus the roster and the names you put on a finished
recording's speakers. The full list is in [Settings](settings.md).

## Five things you will want to do

Recording a meeting is `ambient record`, launched the way the section above
insists on; the flags are in [commands](commands.md). Renaming a speaker who
came out wrong is `ambient name <dir> call-1 Priya`, which rewrites every line
carrying that label. It appends one edit per line rather than overwriting
anything, so the raw transcript survives and re-running `diarize` will not turn
the name back — but there is no undo verb, and getting a name back means
editing `edits.jsonl` by hand. The settings window's roster dropdown appends
the identical edit. Getting the markdown out needs
nothing at all: `record` writes `transcript.md` into the session directory
itself, and [export](commands.md) exists to regenerate it or put a copy
somewhere else. Declining a call the app noticed is the *Not this one* item
that appears in the menu bar when a watched app starts audio — but only once
you have named apps to watch, since the default list is empty and an empty list
watches nothing. [What is kept](what-is-kept.md) describes both that and how
long a refusal is remembered. A denied
or missing permission is [when it does not work](troubleshooting.md), which
sorts the ways a grant goes missing into two truth tables.

## What to read next

If it recorded and transcribed, [architecture](../developing/index.md) explains
what just happened. If it recorded silence, [when it does not
work](troubleshooting.md) has the three-row truth table that says which of the
two independent failures you hit.
