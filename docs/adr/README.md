---
title: "How we record decisions"
sidebar:
  order: 40
---

# How we record decisions

An architecture decision record exists to stop a settled argument being had
again. It earns its number when a decision had genuinely live alternatives,
when choosing wrongly would have cost something real, and when someone reading
the code a year from now would otherwise reopen it — usually by "fixing" the
thing the record explains.

Most of what looks like a decision is not one. The `STOP` sentinel file, the
`status` file, `dedup_bleed`'s overlap and containment thresholds, `Vad::turns`
against `Vad::chunks`, using rustfmt's defaults, the pnpm version pin in CI,
and CDLA-Permissive-2.0 sitting in the licence allowlist are all reasoned
choices with no serious rival. Each is a good paragraph in the chapter that
owns it — [capture](../developing/capture.md),
[sessions](../developing/sessions.md),
[what CI checks](../developing/ci.md) — and none is an ADR. A record whose
alternatives were never seriously live is a paragraph with a number on it, and
a directory of those is how the practice dies: nobody reads records that
mostly say nothing.

## Filenames and numbering

Files are `NNNN-kebab-slug.md`: four digits, monotonic, never reused and never
renamed. Commit messages cite records by number, so renaming
`0004-vad-chunking.md` silently breaks every message that said `ADR-0004`. A
withdrawn record keeps its number and gains a status; the number is not
returned to the pool.

## Immutability

A record is immutable once accepted. Fix a typo; do not revise an argument.
When the decision changes, write a new record and add reciprocal links: the
new one carries `**Supersedes:** [ADR-NNNN](NNNN-slug.md)` on its metadata line, the old one gains a matching `Superseded by`. Both files stay in the
tree, because the point of the record is the reasoning at the time, and that
reasoning is what a superseding record has to argue against.

`rejected` records are kept for the same reason, and are often the most useful
files here — the alternative someone is about to propose, already measured and
already declined.

The date is the date the decision was made. It is never touched afterwards.

## Adding one

```sh
cp docs/adr/TEMPLATE.md docs/adr/0014-<slug>.md
```

Then give it `title:` and `sidebar.order:` frontmatter, matching the record
above it. CI fails on a record without both: no frontmatter means the content
collection rejects the file and the build stops, and no `sidebar.order` means
the record lands unplaced in the nav, which is nearly as good as unwritten.

## The records

| # | Title |
| ---: | --- |
| 0001 | [Capture call audio with a Core Audio process tap](0001-core-audio-process-tap.md) |
| 0002 | [Launch only as a certificate-signed bundle through LaunchServices](0002-signed-bundle-launch.md) |
| 0003 | [Two independent IOProcs, realigned on wall clock](0003-two-ioprocs-wall-clock.md) |
| 0004 | [Chunk at VAD-chosen boundaries in 30 s windows](0004-vad-chunking.md) |
| 0005 | [Run ASR on CPU only, with no CoreML execution provider](0005-cpu-not-coreml.md) |
| 0006 | [Hand-write a separate feature front-end per model](0006-separate-frontends.md) |
| 0007 | [Diarization is a separate verb, not part of `record`](0007-diarize-separate-verb.md) |
| 0008 | [`raw.jsonl` is append-only and `transcript.md` is derived](0008-append-only-raw.md) |
| 0009 | [Consent is an Armed menu bar state with per-call opt-in](0009-armed-consent-state.md) |
| 0010 | [Store names, never voiceprints](0010-names-not-voiceprints.md) |
| 0011 | [Sweep track audio after 7 days](0011-audio-retention-sweep.md) |
| 0012 | [CI verifies assembly; behaviour is verified by dedicated binaries](0012-ci-verifies-assembly.md) |
| 0013 | [Nimbus and Cirrus, not mdBook](0013-nimbus-over-mdbook.md) |
| 0014 | [Developer ID and notarisation, not a self-signed identity](0014-developer-id-and-notarization.md) |
| 0015 | [Capture and transcription are separate work, joined by a serial queue](0015-capture-and-transcription-are-separate.md) |
| 0016 | [An `ambient mcp` verb is how other programs read sessions](0016-mcp-verb-for-live-reading.md) |
