# When it does not work

Almost every first-run failure here is the same one wearing different clothes:
the recording completes, reports success, and contains silence. macOS does not
warn you, because from its point of view nothing went wrong.

If your call track is flat, work through this page before suspecting the
recogniser or your microphone.

## It recorded silence

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

See [ADR-0001](../adr/0001-core-audio-process-tap.md) and [ADR-0002](../adr/0002-signed-bundle-launch.md).

## macOS asked and I clicked Don't Allow

A denial adds no fourth symptom. The tables above sort the ways a grant goes
missing; refusing the prompt is another way, and it produces the same recording
as the others — tap created, buffers at the right rate, every sample zero.

The binary does diagnose it, in `silent_tap_advice` in `src/capture.rs`, which
both `tap` and `record` call once the capture is over. When the call track is
flat while something was demonstrably rendering, it checks the parent pid — 1
means LaunchServices, anything else means a shell — and on the LaunchServices
branch it prints the paragraph written for exactly this case: what you are
looking at is a denied system-audio tap, Core Audio returns success, delivers
correctly-shaped buffers, and fills them with zeros, reporting no error and
prompting for nothing. It then blames the cdhash first, because a grant
orphaned by a rebuild is the commoner cause, and offers three things to do:
`codesign -dvvv build/Ambient.app 2>&1 | grep CDHash` to see the hash the app
now has, a `sqlite3` query against `~/Library/Application
Support/com.apple.TCC/TCC.db` for the `csreq` TCC expects, and, if the two
differ, either signing with a stable identity or

```sh
tccutil reset AudioCapture uk.ambient.cli
```

That last command is the one that clears a denial: it removes the row, so the
next bundle launch asks again. `./setup-signing.sh` runs the same command as its
final step, so re-running it clears a denial as a side effect of re-signing.
Neither has been exercised here against a row that was actually denied — the
cdhash mismatch they were written for is the case that was.

Two things about that advice are worth knowing before you go looking for it.
The `sqlite3` query selects `service` and `csreq` and not the field recording
whether the row allows or refuses, so it cannot tell a denial from a stale
identity; both end at the same `tccutil` line anyway. And the paragraph is
reached only when the tap was unfiltered. With `apps` set, an earlier branch
answers first and reports that those apps played no audio during the recording,
which is what a denied grant also looks like from inside the process.

The reason this page carries the advice at all is that nobody can read it where
it is printed. It goes to stderr, and the branch that produces it runs only when
LaunchServices started the process — the launch with nowhere for stderr to go.
The menu bar keeps its own log for that reason, but only what it routes through
`log` lands there, and a recording that completes is logged as `session
written:` whether the call track held anything or not.

## I started a second recording by accident

Nothing guards against it. `record` builds its directory and starts the tap
without ever asking whether a recording is already live, so a second
`ambient record` in a terminal — usually one started alongside the menu bar app
— gives you two taps, two directories and two sets of scratch wavs. The menu bar
cannot get you here on its own: its Start item does nothing unless the app is
Idle or Armed, and Stop is enabled only while it is Recording.

A bare `ambient stop` will not undo it. `live_session` collects every session
whose `audio/room.native.wav` was written to in the last ten seconds, sorts and
pops, so what comes back is the last directory name in sort order. That is the
newest when the two started in different minutes, which is the ordinary case —
but the name is a minute-resolution timestamp plus a `--name` slug, so inside
one minute the slug decides and a named session sorts after an unnamed one
whatever the clock said. Either way the stop reaches one of them, the other
carries on, and nothing on screen says so.

The way out is to name the directory:

```sh
ambient stop ~/Documents/Ambient/2026-08-29T1223-standup
```

`stop_recording` checks only that the path is a directory before writing the
`STOP` file the capture loop notices on its next 200 ms tick, so an explicit
path reaches the session the search will never return. List the sessions folder
— `ambient config` prints where it is — and the two in flight are the two
newest names. Why the mechanism can only answer "the newest" is in
[sessions and the edit layer](../developing/sessions.md).

There is a sharper version of this, and it is the likelier one, because the
menu bar always records unnamed and so does a bare `ambient record`: two
unnamed recordings begun inside the same minute derive the *same* id and the
same directory. They do not get one each. The second capture's
`WavWriter::create` re-creates `room.native.wav` under the first, both poll the
same `STOP` file, and one stop ends both. Pass `--name` to keep them apart.
The collision has not been reproduced here.

## A session directory with only audio/ in it

A recording killed part-way through leaves a directory that looks like a session
and is not one: `audio/room.native.wav` and `audio/call.native.wav` at the
devices' own rates, a `status` file whose last line still begins `recording`,
and nothing else.

The order of writes is what makes it recognisable. Both native wavs are created
before the capture loop starts; `session.json` is written at the very end, after
the resample, after the recogniser and after `raw.jsonl`. Nothing that dies
during capture gets near the second, so natives with no `session.json` is a
recording that was killed rather than one that finished.

Those scratch files are also how a live recording is found, which is why
`live_session` insists the file was written to within the last ten seconds —
without that check the corpse answers to `ambient stop`, pointing it at a
directory nothing is writing to. The menu's level meter is only partly covered:
while the app is transcribing the scratch wavs are already gone, so it falls
back to whichever session directory sorts last, and a corpse sorting last will
show a stale level there.

Nothing will ever reclaim it. Retention skips any session with no
`transcript.md`, on the grounds that the audio is the only copy of what was
said, and this directory will never have one, so its wavs stay for as long as it
does — see [what is kept](what-is-kept.md). Delete it by hand. There is no
`raw.jsonl` behind it and no edit layer to lose, so `rm -r` on the directory
costs nothing but the audio nobody transcribed.

## The disk filled

The failure surfaces late, not at the write that fills the disk.
`WavWriter::create` wraps the file in a `BufWriter`, so `write_sample` keeps
returning `Ok` into an 8 KB buffer and the `ENOSPC` comes back from whichever
sample happens to trigger a flush — up to a buffer's worth of audio is gone
with no error attached to it. That error then returns out of `record` into
`main`, which returns a `Result`, so the failure prints to stderr and the
process exits non-zero. Nothing is finalised: both `finalize()` calls sit after
the capture loop and are never reached.

The two writers are dropped instead, and `hound`'s `Drop` calls `update_header`
and throws the result away — a drop must not panic, so its own comment says the
failure is ignored silently. The header patch itself only overwrites four bytes
that already exist, which needs no new space; what fails is the seek in front
of it, because `BufWriter`'s `Seek` flushes the still-buffered samples first.
So `update_header` returns early, the data-chunk length written as zero at
creation stays zero, and the wav declares no samples however many bytes are in
it. That has not been reproduced against a genuinely full disk here; it is read
off `hound`'s `write.rs` and `src/session.rs`.

Launched as a bundle you will see none of that, because stderr has nowhere to
go. The menu bar writes its own log to `app.log` beside the sessions —
`~/Documents/Ambient/app.log` unless `sessions_dir` or `AMBIENT_HOME` moves it —
and a recording that fails appends one line: `recording FAILED:` and the
outermost message only, since it formats the error with `Display` rather than
the `{e:#}` that would carry the causes. The full chain goes to stderr, which
is exactly where a bundle launch cannot show it. Thin as that line is, it is
the only place a bundle-launched user sees a failure at all, so it is the first
file to open when a recording ends with no transcript.

None of this weakens the append-only guarantee. `raw.jsonl` is created after the
capture loop, after both `finalize()` calls and after the resample, and
`edits.jsonl` a few lines after that, so a write failure during capture returns
before either file exists. There is no half-written record layer to fold, and
what is left behind is the corpse described above. See
[ADR-0008](../adr/0008-append-only-raw.md).
