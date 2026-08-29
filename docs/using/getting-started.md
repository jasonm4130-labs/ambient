# Getting started

## Build it

```sh
./fetch-models.sh                 # ~3.3 GB: recogniser, diarization, VAD
cargo build --release
cargo run --release -- probe      # is this machine viable?
```

`probe` enumerates the audio processes it can see and reports the execution
provider state. It needs no permissions, so it works from a terminal — which is
the last thing on this page that does.

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

## Check the settings

```sh
cargo run --release -- config     # resolved settings, and the input devices it can see
```

Every setting is readable and settable from the CLI, so nothing depends on the
GUI being open. The full list is in [Settings](settings.md).

## What to read next

If it recorded and transcribed, [architecture](../developing/index.md) explains
what just happened. If it recorded silence, [when it does not
work](troubleshooting.md) has the three-row truth table that says which of the
two independent failures you hit.
