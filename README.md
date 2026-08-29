# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text.

Design doc: <https://claude.ai/code/artifact/6ff48ff8-7978-46fc-ae1b-387fd69974e3>

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly.

## Quick start

```sh
./fetch-models.sh                 # ~3.3 GB: recogniser, diarization, VAD
cargo build --release
cargo run --release -- probe      # is this machine viable?
cargo run --release -- transcribe models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 audio.wav
```

`transcribe` accepts any sample rate and resamples. To convert anything else
first: `ffmpeg -i in.m4a -ac 1 -ar 16000 out.wav`

## End-to-end result

M5 Max / 128 GB / macOS 26.6.2, v3-int8, synthesised speech via `say`:

| Audio | Decode | Realtime | Peak RSS |
| ---: | ---: | ---: | ---: |
| 5 s | 0.12 s | 42× | — |
| 16.5 s | 0.32 s | 52× | — |
| 127.5 s (chunked) | 2.38 s | 54× | **2317 MB** |

Peak memory on the 127 s file is *flat* at the 30 s-chunk level rather than the
~3.7 GB an unchunked 120 s run needed, which is the chunking working.

Accuracy is good on ordinary speech and technical vocabulary — GDPR, DPO, ONNX,
"Q3", "the 14th of October" all correct. It fails on **proper nouns**: "Priya"
became "Crea", "Cloudflare" became "Cloudflow". That is the expected failure
class and the reason the pipeline has a Claude repair pass with a roster and
glossary before anything is summarised.

Known artefact: chunk seams can duplicate a word ("flat. flat regardless…").
Fixable with overlap-and-dedupe; the repair pass also absorbs it.

## Capture: the launch method is load-bearing

`ambient tap <out.wav> <secs> [bundle-id...]` records system audio through a
Core Audio process tap — no virtual device, no bot in the meeting, and only the
System Audio Recording permission rather than ScreenCaptureKit's Screen
Recording.

**It must be launched as a bundled app through LaunchServices.** Run the bare
binary from a terminal and the tap is created successfully, delivers buffers at
the correct rate, and every sample is zero. TCC attributes the request to the
*responsible process* — the terminal — which has no audio permission and does
not prompt, so the failure is silent in the most literal sense.

```sh
./setup-signing.sh                                   # once — stable identity
./make-app.sh                                        # bundle + sign
open -a "$PWD/build/Ambient.app" --args tap /tmp/out.wav 14
```

### The signing identity is load-bearing too

`codesign --sign -` (ad-hoc) makes the app's designated requirement its **own
cdhash**, which changes with every rebuild. TCC stores that hash in the grant's
`csreq`, so a rebuild orphans the permission while leaving the row in place
still reading *allowed*:

```
stored   FADE0C00 00000028 00000001 00000008 00000014 103644AA…8E014B
                                             ↑ opcode 8 = cdhash
current  CDHash = 81c297a4f118a4f1efdb84b2511fddd7c927eb2a
```

The two services then diverge, which is what makes this so confusing:

| Service | csreq matches | Behaviour |
|---|---|---|
| `kTCCServiceMicrophone` | no | keeps working |
| `kTCCServiceAudioCapture` | no | **runs, returns zeros, no error, no prompt** |

These are two independent failures and it is easy to conflate them:

| Launch | Signing | Result |
|---|---|---|
| shell (`build/…/MacOS/ambient`) | anything | **silent** — TCC blames the terminal |
| `open -a` | stale cdhash | **hangs forever** on a prompt nobody sees |
| `open -a` | stable identity | works |

The first row holds regardless of how the app is signed, so fixing the
certificate does not make a terminal launch work — measured after the fix:
direct launch still gave `call peak 0.000`, `open -a` gave `0.869`.

`./setup-signing.sh` fixes this permanently by signing with a certificate, so
the requirement becomes `identifier "uk.ambient.cli" and certificate leaf = …`
and survives rebuilds. `ambient tap` and `ambient record` now detect the silent
case at runtime — they sample `kAudioProcessPropertyIsRunningOutput` during
capture and, if the call track is flat while something was demonstrably playing,
name the likely cause — checking the launch method first (parent pid 1 means
LaunchServices, anything else means a shell), and only then the cdhash.

On a managed Mac a PPPC profile denying `kTCCServiceAudioCapture` produces
**identical** symptoms, so rule the cdhash out before blaming IT.

### Two tracks, two clocks

`ambient tap` records **you and the far side as separate files**: a room track
from the microphone and a call track from the process tap.

They used to share one aggregate device, on the reasoning that a single IOProc
delivering both on one clock beats two streams sliding apart over a long
meeting. That reasoning was sound and the arrangement did not work, because
**a process tap gates the clock of the aggregate it belongs to**. With nothing
rendering audio the aggregate delivers zero frames in six seconds — not a quiet
track, no callbacks at all — and the microphone sharing that aggregate is
starved along with the tap. Every capture that ever worked had `say` or `afplay`
running, which is why a system built to record office conversations had never
recorded one. Neither `kAudioAggregateDeviceMainSubDeviceKey` nor
`kAudioAggregateDeviceClockDeviceKey` changes this.

So the microphone now runs its own IOProc on the input device directly, and the
tap keeps the aggregate to itself. Two consequences worth knowing:

- **Order matters.** The microphone IOProc is started *before* the tap exists.
  Starting one on the input device while the tap's aggregate is already running
  blocks forever — the mic alone starts instantly, the same call after the
  aggregate never returns.
- **Alignment is reconstructed from wall-clock time**, which is independent of
  both device clocks. Each drain works out how many frames a track should have
  by now (`elapsed × rate`) and pads a short track with silence in front of the
  new samples, since the gap happened before them. A track that overruns is
  never trimmed. This is good to within one drain tick (200 ms) against
  sample-exact before — the honest cost of being able to record a silent room.

Padding is counted separately from real device frames, so a stalled tap reports
`20.1s, 17.4s real, 2.7s padded` rather than passing for a healthy one.

The split does more than separate speakers. The call track is *exactly what
this Mac renders* and nothing else; the room track is the whole room. A test recording made that
concrete: `say` played through the speakers while a YouTube video ran on a TV
across the room.

| Track | Transcript |
| --- | --- |
| call | "This voice is arriving on the call track through the process tab." |
| room | "…through the process tag. **Once his build is complete, he quickly finds himself.**" |

The second sentence is the television, picked up acoustically. It is on the mic
track and absent from the tap track, which is the boundary working exactly as
designed.

**Implication for the room, and for consent.** The microphone captures
everything audible: a TV, a radio, and in an office the conversation at the
next desk — people who are not in your meeting and have not agreed to anything.
The tap has no such problem. This is a further argument for building calls
first, and for the mic track to be VAD-gated and treated as the sensitive one.

Verified end to end: tapped system audio, resampled to 16 kHz, transcribed as
"Right, the process tab is capturing system audio with nothing joining the
meeting." (`tap` -> "tab" and `Priya` -> "CRO" are the usual proper-noun and
homophone errors.)

## Voice activity detection

Silero VAD (`models/silero_vad.onnx`, 629 KB) runs before transcription and
does two jobs. It skips silence, and — more valuably — it decides where to cut.

Chunk boundaries were previously chosen by local energy, which sliced through
words: a 127 s file came back containing "Chunking keeps the memory flat. flat
regardless of how long the meeting actually runs." Cutting on VAD boundaries
instead, the same file yields 375 words with **zero adjacent duplicates**.

| File | Speech / total | Chunks | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| 127 s continuous | 127.5 / 127.5 s | 6 | 39x | 2255 MB |
| mic track | 3.6 / 9.7 s | 1 | 14x | — |

The VAD pass costs throughput (39x against 54x without) because it runs one
inference per 32 ms frame. Worth it: memory stays flat, silence is never sent
to the encoder, and the seams stop mangling words.

**One behaviour to know.** VAD also discarded the television audible in the
background of the mic recording — it fell below the speech threshold. Good for
noise, but quiet speech that genuinely matters would go the same way, so the
thresholds in `vad.rs` are a knob to revisit against real room recordings
rather than settled.

## Porting to the work M5 (16 GB)

The point of the exercise. In order:

1. `rustup` toolchain, then `git clone`, `./fetch-models.sh`, `cargo build --release`.
2. `ambient probe` — confirms Core Audio bindings link and reports the EP state.
3. `ambient transcribe` on a real recording, run under `/usr/bin/time -l`, and
   check `maximum resident set size` stays near 2.3 GB. If it does, the 16 GB
   machine is fine for batch transcription.
4. Then `./make-app.sh` and run the tap **via `open -a`**. If it returns
   silence, that is TCC, not a bug — and on a managed Mac it is exactly what a
   PPPC profile from Jamf/Intune exists to grant. Ask for System Audio
   Recording; you do not need Screen Recording.

## What Phase 0 has established

Measured on the home machine (M5 Max, 128 GB, macOS 26.6.2) on 2026-08-29:

| Assumption | Result |
| --- | --- |
| Core Audio process taps reachable from Rust without a C shim | **Yes.** `objc2-core-audio` 0.3.2 exposes `AudioHardwareCreateProcessTap`, `CATapDescription` with the mono/stereo global-tap initialisers, `AudioHardwareCreateAggregateDevice`, and the `kAudioSubTap*` property set including drift compensation. Compiles and runs; enumerated 21 audio processes with bundle IDs resolved. |
| ONNX Runtime CoreML EP available from a prebuilt binary | **Yes.** `ort` 2.0.0-rc.13 with the `coreml` feature reports `is_available() == Ok(true)` against the binary it downloads. No from-source ONNX Runtime build needed, contrary to some write-ups. |
| `ComputeUnits::CPUAndNeuralEngine` requestable | **Yes**, it is a documented variant of the CoreML EP config. |

## What Phase 0 measured

`cargo run --release --bin bench -- <encoder.onnx> <coreml|cpu> [seconds]`

Home machine (M5 Max, 128 GB, macOS 26.6.2), Parakeet TDT 0.6b encoder,
60 s of audio. Input is a zeroed tensor of the correct shape — wall clock for a
fixed-shape graph is content-independent, so the comparison holds, but these are
not accuracy numbers.

| Model | Provider | Steady run | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| v2 fp16 | CPU | 1013 ms | 59× | 3745 MB |
| v2 fp16 | CoreML (ANE requested) | 1012 ms | 59× | 3767 MB |
| v3 int8 | CPU | 1029 ms | 58× | 2594 MB |
| v3 int8 | CoreML (ANE requested) | 1670 ms | 36× | 16482 MB |

### Finding 1 — ONNX Runtime's CoreML provider does nothing for this model

On fp16 it is identical to CPU to within a millisecond: the provider registers,
reports available, and contributes no acceleration. On int8 it is actively
harmful — 1.6× slower and 16.5 GB peak, which would OOM a 16 GB machine outright.

`is_available() == true` means the provider loaded, not that any node was placed
on it. **Do not enable the `coreml` feature.** The Neural Engine is still
reachable on this hardware, just not through ORT's graph-partitioning shim —
natively compiled CoreML models (what FluidAudio ships) are a different path and
would likely behave differently.

### Finding 2 — memory scales with audio length, so chunking is mandatory

v3 int8 on CPU:

| Audio | Steady run | Realtime | Peak RSS |
| ---: | ---: | ---: | ---: |
| 30 s | 488 ms | 62× | 2158 MB |
| 60 s | 1029 ms | 58× | 2594 MB |
| 120 s | 2252 ms | 53× | 3721 MB |
| 300 s | 7539 ms | 40× | 6291 MB |

Roughly linear in memory and worse than linear in time — attention cost grows
with sequence length. Extrapolated, a 30-minute session fed whole would want
~35 GB and would fail on the base M5.

The fix is ordinary and known: transcribe in **30 s windows with overlap and
stitch**, which makes cost constant regardless of session length. At that size a
30-minute session is ~60 chunks ≈ 30 s of compute at a flat ~2.2 GB.

### Revised design axiom

The original "< 2 GB, on the ANE" was written assuming continuous processing. The
real workload is bursty batch — promote a session, transcribe once. At ~60×
realtime on CPU the ANE is not load-bearing, and the budget becomes:

> **~2.2 GB during a transcription burst, chunked at 30 s; negligible while
> capturing.** CPU is fine. Revisit only if live streaming transcription is
> wanted, where the ANE would start to matter.

## Sessions

`ambient record [--name <s>] [--app <bundle-id>]… [--seconds <n>]` captures
until stopped, then resamples, transcribes and writes a session under
`~/Documents/Ambient` (`AMBIENT_HOME` overrides):

```
2026-08-29T1101-smoke/
  session.json     metadata
  raw.jsonl        what the recogniser heard — never rewritten
  edits.jsonl      repairs, speaker names, reverts — append-only
  transcript.md    the export, regenerated on every change
  status           live level meter while recording
  audio/room.wav   mono 16 kHz, mic
  audio/call.wav   mono 16 kHz, tap
```

### Stopping one

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

`ambient show <dir>` folds `edits.jsonl` over `raw.jsonl`; `--verbatim` skips
the fold. Reverts append a record naming an earlier line rather than deleting
it, so the raw transcript is always recoverable byte-for-byte — verified by
hashing `raw.jsonl` either side of applying an edit layer.

Track is the attribution `record` writes on its own, and for a one-to-one call
it is already complete: room is you, call is them. Several people round a table
or on the same call need `ambient diarize`, below.

One record per turn, not per recogniser call. `Vad::chunks` merges neighbouring
speech into few large calls, which is right when the output is one block of
text and wrong here — a record spanning two people cannot carry a speaker, and
`diarize` can only label whole records. `record` uses `Vad::turns` instead,
which splits over-long speech but never merges across a silence.

Measured on one 19.8 s two-track recording — the same sentence reaching the tap
directly and the microphone acoustically:

```
call (tap, peak 0.869)   "... Priya merged the chunking fix, so the retry-storm ..."
room (mic, peak 0.261)   "... Pre emerge the chunking fix, so the retry storm ..."
```

Which is the argument for tapping rather than pointing a microphone at a
speaker, in one line.

### Naming speakers

`ambient name <dir> call-1 Priya` renames every line currently carrying that
label. It appends `speaker` edits like anything else, so it is undoable, and
re-running `diarize` afterwards leaves the name alone — diarization reverts its
own labels but skips any record a person has since named. Without that guard
the later edit would win the fold and quietly turn Priya back into `call-1`.

### Resampling

The capture layer runs at the device rate (48 kHz here) and everything
downstream demands 16 kHz, with nothing in between until now — `ambient tap`
followed by `ambient transcribe` could never have worked. `src/resample.rs`
closes it with rubato's band-limited FFT resampler. Verified by transcribing
the same speech at 16 kHz natively and via 48 kHz through the resampler: two
tokens differ out of ~35 (`standup`/`stand up`, one capital), everything
load-bearing identical. Naive 3:1 decimation would alias instead, and Parakeet
would render the result fluently and wrongly rather than fail.

## Diarization: who spoke when

`ambient diarize <session-dir> [--threshold <f>]` assigns speakers to a
recording that already exists. Two models, in sequence:

| stage | model | what it decides |
|---|---|---|
| segment | pyannote-segmentation-3.0 | how many people are talking in each 10 s window, and which parts belong to each |
| embed | WeSpeaker resnet34 (VoxCeleb) | a 256-d vector per (window, speaker) |
| cluster | agglomerative, cosine, average linkage | which of those are the same person |

Segmentation's speaker indices are local to a window and mean nothing across
windows, so nothing tries to stitch windows together by permutation — the
global clustering is what recovers a consistent identity. Each raw record then
takes the speaker holding the most of it, labelled `room-1`, `call-2`: speaker
1 in the room and speaker 1 on the call are different people, and nothing
downstream should be able to assume otherwise.

It is a separate verb rather than part of `record` for three reasons. It
roughly doubles processing time for something not always wanted; `--threshold`
needs tuning against real room audio; and the two graphs are never resident
alongside Parakeet, which matters on the 16 GB target. Re-running appends
`revert` records for the previous run's labels before writing new ones, so a
bad threshold costs an appended revert rather than a lost recording.

### The front-end is the part that fails silently

WeSpeaker wants Kaldi fbank: HTK mel, 80 bins, Povey window, snip-edges,
pre-emphasis, int16-scale samples. `src/features.rs` is Slaney, 128 bins,
centre-padded and per-feature normalised, because that is what NeMo wants.
Almost nothing is shareable, so `src/fbank.rs` is a second front-end rather
than a configurable one.

A near-miss filterbank does not error here. It yields embeddings that still
cluster, only worse — so the damage arrives as confidently mislabelled
speakers, which is exactly the output a reader cannot audit. `bin/embtest.rs`
tests it directly, on two clips each of two `say` voices:

```
           A1     A2     B1     B2
    A1  1.000  0.880  0.192  0.138
    A2  0.880  1.000  0.145  0.132
    B1  0.192  0.145  1.000  0.823
    B2  0.138  0.132  0.823  1.000

same voice 0.851   cross voice 0.152   separation 0.700
```

A wrong front-end collapses that matrix toward uniform. `bin/diartest.rs` is
the companion tool for tuning `--threshold` against a bare wav;
`AMBIENT_DEBUG_DIAR=1` adds the per-window powerset class histogram, which is
how the class layout was confirmed rather than assumed — every known silent gap
decodes to class 0.

### Measured

A 38 s four-turn conversation between two `say` voices, played through the
speakers and captured by both tracks at once:

```
[00:00] call call-1  Right, let's start with the Transcoda backlog...
[00:11] call call-2  That's good news. The chunking fix went in on Tuesday...
[00:20] call call-1  Did anyone confirm whether customers were actually affected...
[00:29] call call-2  Still guessing, honestly, I will pull the request logs...
```

Both tracks alternate correctly across all four turns, and `raw.jsonl` hashes
identically before and after. The room track — a microphone listening to the
laptop's own speakers — over-clusters at the default threshold (three groups
rather than two, five at `--threshold 0.3`), though the surplus groups are
short fragments that never win a whole line. The tap does not have that
problem, which is the same argument as the tap-versus-microphone one above.

Peak RSS on a 10.2-minute track: **249 MB**, 5.9 s wall — the sliding window
does not accumulate.

## Export

`ambient export <dir> [--out <path>]` writes `transcript.md`, and `record`,
`diarize` and `name` each regenerate it, so the file on disk is never stale.
It is derived — `raw.jsonl` plus `edits.jsonl` stay the only source of truth,
and deleting the markdown loses nothing. YAML front matter carries the session
metadata, which is what the downstream Confluence stage reads:

```markdown
---
id: "2026-08-29T1223-standup"
name: "standup"
started_at: "2026-08-29T12:23:47.913732+10:00"
duration_s: 20.0
model: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
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

### Room noise does not merely add junk — it derails the decode

Silero reports 0.6–0.9 on quiet room noise, confidently enough that no
probability threshold separates it from real speech at 0.92–1.00. So a VAD turn
would open in noise and run into the speech, and Parakeet given that prefix did
not transcribe the speech plus some junk — it invented a different sentence
outright. The same audio, measured:

| Fed to the recogniser | Out |
| --- | --- |
| 2.6 s of room noise + the utterance | "The gap had not closed. If anything, it had wide." |
| the utterance alone | "The migration is scheduled for Thursday morning." |

Level separates a turn's quiet EDGES from its speech, and trimming them matters
because a noise prefix does not merely add junk — it derails the decode. So each
turn is trimmed on level before the recogniser sees it.

The floor is **relative to the track's own loudest content, and only that**. An
absolute floor was tried and removed after it threw away a real recording whole:
quiet speech captured across a room measured −42 dB p90 while a genuinely silent
room measured −44 dB. **Two decibels apart.** Any absolute threshold that rejects
the empty room also rejects real speech.

Three signals were measured and none of them separates quiet speech from an
empty room:

| Signal | Real quiet speech | Hallucination from silence |
| --- | --- | --- |
| level (p90) | −42 dB | −44 dB |
| Silero probability | 0.6–0.9 | 0.6–0.9 |
| decoder confidence | 0.959 | 0.917 |

Decoder confidence does catch the *derailment* case — 0.992 for the trimmed
utterance against 0.613 for the same one with noise in front — which is why the
trim is worth having. But Parakeet is confidently wrong on pure noise, so a
room track that is nothing but noise still reaches the recogniser and can still
produce an invented line.

That is a semantic problem, not a threshold problem: it needs something that can
read the words. `confidence` is recorded per line in `raw.jsonl` to give a
downstream cleanup pass something to weigh.

## Settings

Four settings have code behind them, and nothing is stored that nothing reads:

| Setting | Effect |
| --- | --- |
| `apps` | tap only these bundle IDs; empty taps everything the Mac plays |
| `input_device` | record the room with a named device rather than the system default |
| `diarize` / `threshold` | separate voices once the transcript exists, and how readily |
| `sessions_dir` | where sessions are written |

They live in `~/Library/Application Support/Ambient/config.json` — deliberately
not under the sessions folder, since that folder is itself a setting and a
config inside the thing it configures cannot be found before it is read.
Precedence runs **environment → CLI flag → config file → default**, so
`AMBIENT_HOME` and `--app` still win and every existing harness is unaffected.
A missing file is an ordinary first run; a corrupt one warns and falls back
rather than failing a recording.

`ambient config` prints the resolved settings and the input devices it can see;
`ambient config <key> <value>` sets one. That exists so every setting is
verifiable without a GUI.

A named input device that has gone away **warns and names the alternatives**
before falling back. Silent fallback is this project's recurring failure and
the one thing a settings layer must not reintroduce.

### The settings window

An `NSWindow` holding a `WKWebView`, rendering the same page the design canvas
draws. The page is embedded with `include_str!`: launched through
LaunchServices the working directory is `/`, and that is the only launch with
the audio-capture grant, so a relative path would break the one path that
matters.

The bridge carries a **JSON string** each way. `WKScriptMessage::body` otherwise
arrives as an `NSDictionary` that Rust would unpick a value at a time; a string
goes straight to serde. The page never edits its own state — it sends a patch,
Rust merges it onto the file and pushes the whole config back, so the file is
the single source of truth rather than the DOM.

### Building the page — `ui/`

TypeScript and [Effect](https://effect.website), bundled by Vite through
`vite-plugin-singlefile` into one self-contained `assets/settings.html`; a
`WKWebView` loaded via `loadHTMLString` has no origin to fetch siblings from,
so nothing may stay external. Linting and formatting are oxlint and oxfmt.

```
cd ui && pnpm install
pnpm run build     # → ../assets/settings.html
pnpm run check     # tsc --noEmit
pnpm run lint      # oxlint
```

The built file is committed, so `cargo build` never needs node. `make-app.sh`
refreshes it only when `ui/node_modules` is present.

Effect earns its place on two counts and is otherwise heavy for a settings
page: `Schema` decodes the payload Rust pushes in rather than trusting it, so a
field of the wrong shape leaves the page as it was instead of rendering a
control that lies about the setting behind it; and the bridge is a real
effectful boundary worth having a testable seam at.

`cargo run --release --bin uicheck -- out.png` drives the built page in a real
`WKWebView`, pushes a config in, synthesises every click and prints what
reaches the bridge. Compiling and type-checking prove the page *says* the right
thing; only this proves it does anything.

## Still open

1. **Very quiet recordings.** The speech floor has an absolute term at
   −38 dB, so a recording made at a much lower input gain than the machines
   here would be rejected wholesale rather than transcribed. This is the first
   thing to check if a real recording ever comes back empty.
2. **The base M5 (16 GB).** Every number above is from the 128 GB machine.
   Chunked at 30 s this should fit, but should is not measured.
3. **Tap creation under the work machine's TCC policy.** Enumeration needs no
   permission; creating a tap needs System Audio Recording.

## Dependency notes

- The `coreml` feature is enabled in `Cargo.toml` for the probe only. Per
  Finding 1 it should be removed once the probe is retired.
- `ort` has **no stable release** — pinned to `=2.0.0-rc.13`. It has been in
  release-candidate for a long time; treat API churn as a live risk and do not
  let it leak past the ASR module.
- `objc2-core-audio` 0.3.2 was last published 2025-10-04. It is generated
  bindings over a stable C API, so staleness matters less than it would
  elsewhere, but it is worth knowing.

## Licence

MIT.
