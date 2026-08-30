---
title: "6. Hand-write a separate feature front-end per model"
sidebar:
  order: 46
---

# 6. Hand-write a separate feature front-end per model

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

Two models in this pipeline each need mel features, and ONNX provides neither.
Parakeet was trained on NeMo's front-end: Slaney filterbank, 128 bins,
centre-padded, pre-emphasised, per-feature normalised. WeSpeaker was trained on
Kaldi fbank: HTK mel, 80 bins, Povey window, snip-edges, pre-emphasis, and
int16-scale samples.

They look like the same computation and are not: the mel scale, the bin count,
the padding convention, the window and the input scaling all differ. Almost
nothing is shareable.

The danger is that a near-miss does not error. A wrong filterbank yields
embeddings that still cluster, only worse — so the damage arrives as
confidently mislabelled speakers, which is exactly the output a reader cannot
audit.

## Considered Options

- **One configurable front-end** parameterised over mel scale, bin count,
  window and padding.
- **Two hand-written front-ends**, `src/features.rs` for NeMo and
  `src/fbank.rs` for Kaldi.

## Decision Outcome

Write two. `src/features.rs` is Slaney/128/centre-padded/per-feature
normalised because that is what NeMo wants; `src/fbank.rs` is the Kaldi
front-end WeSpeaker wants. Neither is a configuration of the other.

A configurable front-end would concentrate every one of those differences into
flags whose wrong combination fails silently. Duplicated code that can be read
against its reference implementation beats shared code whose failure mode is
plausible output — an unauditable failure is worse than duplication.

## Consequences

Two files to maintain, and a new model means a third rather than a new config
struct. In exchange each front-end is directly comparable to the paper or
implementation it copies.

The failure mode still exists, so it is measured rather than assumed:
`bin/embtest.rs` runs the WeSpeaker path over two clips each of two `say`
voices and prints the similarity matrix, currently same voice 0.851, cross
voice 0.152, **separation 0.700**. A wrong front-end collapses that matrix
toward uniform.

## Confirmation

The Kaldi half is pinned by unit tests in `src/fbank.rs` —
`mel_scale_is_htk_not_slaney` is the direct guard against the two front-ends
being conflated, alongside `frame_count_snips_edges`,
`too_short_yields_nothing` and `mean_normalised_per_dimension` — and these run
in CI under `cargo test --all-targets`.

Those tests do not prove the embeddings still separate, because that needs the
model weights, which CI does not have. A person confirms that by running
`cargo run --release --bin embtest` against two voices and checking the
separation figure has not collapsed from ~0.700 toward zero.
