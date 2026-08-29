# Summary

[ambient](index.md)

# Using

- [Using ambient](using/index.md)
  - [Getting started](using/getting-started.md)
  - [Commands](using/commands.md)
  - [Settings](using/settings.md)
  - [What is kept](using/what-is-kept.md)
  - [When it does not work](using/troubleshooting.md)

# Developing

- [How it fits together](developing/index.md)
  - [Capture](developing/capture.md)
  - [Two tracks, two clocks](developing/two-tracks.md)
  - [Voice activity detection](developing/vad-and-chunking.md)
  - [Speech recognition](developing/asr.md)
  - [Diarization](developing/diarization.md)
  - [Sessions and the edit layer](developing/sessions.md)
  - [Settings and the UI](developing/settings-and-ui.md)
- [Measurements](developing/measurements.md)
- [Porting to the work M5](developing/porting.md)
- [What CI checks](developing/ci.md)
- [Building these docs](developing/docs.md)

# Decisions

- [How we record decisions](adr/README.md)
  - [ADR-0001 Capture with a Core Audio process tap](adr/0001-core-audio-process-tap.md)
  - [ADR-0002 Launch only as a certificate-signed bundle](adr/0002-signed-bundle-launch.md)
  - [ADR-0003 Two IOProcs realigned on wall clock](adr/0003-two-ioprocs-wall-clock.md)
  - [ADR-0004 Chunk at VAD boundaries in 30 s windows](adr/0004-vad-chunking.md)
  - [ADR-0005 Run ASR on CPU, no CoreML provider](adr/0005-cpu-not-coreml.md)
  - [ADR-0006 A separate feature front-end per model](adr/0006-separate-frontends.md)
  - [ADR-0007 Diarization is its own verb](adr/0007-diarize-separate-verb.md)
  - [ADR-0008 raw.jsonl is append-only](adr/0008-append-only-raw.md)
  - [ADR-0009 Consent is an Armed menu bar state](adr/0009-armed-consent-state.md)
  - [ADR-0010 Store names, never voiceprints](adr/0010-names-not-voiceprints.md)
  - [ADR-0011 Sweep audio on a schedule](adr/0011-audio-retention-sweep.md)
  - [ADR-0012 CI verifies assembly, binaries verify behaviour](adr/0012-ci-verifies-assembly.md)
