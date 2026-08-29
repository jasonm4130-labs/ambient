# ambient

Local-first ambient capture for macOS: record conversations in the room and on
Teams calls, transcribe and attribute them on-device, and hand the result to
Claude as clean speaker-labelled text. Nothing joins the call, and no audio
leaves the machine.

**Status: capture and transcription both work on the home machine.** A Core
Audio process tap records system audio with nothing joining the call, and that
audio transcribes correctly. The base M5 with 16 GB is the target and is not yet
measured — see [porting](developing/porting.md).

There is no packaged release. Running ambient means building it, which is why
[Getting started](using/getting-started.md) opens with a compiler.

## Two doors

**[Using ambient](using/index.md)** — get it running, record something, read the
transcript, and know what is kept on disk. Start here.

**[Developing ambient](developing/index.md)** — what the pieces are, why each one
is shaped the way it is, and what CI enforces. Read this before changing code.

The [decision records](adr/README.md) sit behind both: twelve of them, each
naming a decision, the alternatives that were live at the time, and the check
that fails if it drifts.
