# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text.

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly. The base M5 with 16 GB is the target and is not yet
measured — see [porting](operations/porting.md).

## How to read this

- **[Getting started](getting-started.md)** — build it, fetch the models, run it.
  Start here; the launch method is not optional and is explained there.
- **[Architecture](architecture/index.md)** — what the pieces are and why each one
  is shaped the way it is. Two diagrams at the top of that page carry most of it.
- **[Decisions](adr/README.md)** — twelve records, each naming a decision, the
  alternatives that were live at the time, and the check that fails if it drifts.
  Read these when you want to know why rather than what.
- **[Reference](reference/cli.md)** — commands, settings, and every measurement in
  one place.
- **[Operations](operations/porting.md)** — porting to the work machine, what CI
  checks, and how these docs are built.

The original design document lives outside version control at
<https://claude.ai/code/artifact/6ff48ff8-7978-46fc-ae1b-387fd69974e3>. It is
history rather than specification: nothing here depends on it, and where the two
disagree, these pages and the code are right.

## A note on the evidence

Almost every page here reports something that was measured rather than something
that was expected, and several of them exist because the expectation was wrong.
Numbers carry the machine and date they came from. Where nothing has been
measured, the page says so instead of estimating.
