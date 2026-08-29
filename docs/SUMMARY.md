# Summary

[ambient](index.md)
[Getting started](getting-started.md)

# Architecture

- [How it fits together](architecture/index.md)
  - [Capture](architecture/capture.md)
  - [Two tracks, two clocks](architecture/two-tracks.md)
  - [Voice activity detection](architecture/vad-and-chunking.md)
  - [Speech recognition](architecture/asr.md)
  - [Diarization](architecture/diarization.md)
  - [Sessions and the edit layer](architecture/sessions.md)
  - [Settings and the UI](architecture/settings-and-ui.md)
  - [Consent, retention and names](architecture/privacy.md)

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

# Reference

- [Commands](reference/cli.md)
- [Settings](reference/config.md)
- [Measurements](reference/benchmarks.md)

# Operations

- [Porting to the work M5](operations/porting.md)
- [What CI checks](operations/ci.md)
- [Building these docs](operations/docs.md)

