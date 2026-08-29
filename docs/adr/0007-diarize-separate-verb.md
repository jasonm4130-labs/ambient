# 7. Diarization is a separate verb, not part of `record`

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

`ambient record` already attributes every line: room is you, call is them, and
for a one-to-one call that attribution is complete. Several people round a
table or on the same call need speaker separation — pyannote-segmentation-3.0
for speech activity, WeSpeaker resnet34 embeddings, agglomerative clustering —
and the obvious place to put it is at the end of `record`, so a session is
finished when recording stops.

## Considered Options

- **Fold diarization into `record`**, so every session arrives labelled.
- **A separate `ambient diarize <session-dir>` verb** run against a session
  that already exists.

## Decision Outcome

A separate verb, for three reasons that all point the same way.

It roughly doubles processing time for something that is not always wanted —
most one-to-one calls need nothing beyond track attribution. `--threshold`
needs tuning against real room audio rather than being fixed at record time;
the room track over-clusters at the default (three groups rather than two,
five at `--threshold 0.3`) while the tap track does not. And keeping the two
diarization graphs out of memory alongside Parakeet is part of what makes the
16 GB target reachable at all.

## Consequences

Diarization is re-runnable, which is what makes a tunable threshold usable: a
re-run appends `revert` records for the previous run's labels before writing
new ones, so a bad threshold costs an appended revert rather than a lost
recording. Names a person typed are skipped by that revert, so re-running does
not quietly turn Priya back into `call-2`.

Two constraints follow from being separate. A re-run must refuse on a session
whose audio has been swept — it would revert the old labels before discovering
there is nothing to read — see
[ADR-0011](0011-audio-retention-sweep.md). And speaker labels are per-track:
`room-1` and `call-1` are different people, and nothing downstream may assume
otherwise.

Peak RSS for the diarization pass on a 10.2-minute track is 249 MB over 5.9 s
wall, so the sliding window does not accumulate.

## Confirmation

`cargo test --all-targets` in CI runs the clustering tests in `src/diarize.rs`
(`two_tight_groups_become_two_clusters`, `one_voice_stays_one_cluster`,
`a_high_threshold_collapses_everything`, `powerset_covers_every_pair_once`)
and the separation guard in `src/session.rs`,
`diarizing_a_session_whose_audio_was_swept_refuses_rather_than_unlabelling` —
that last one fails if diarization is ever folded back into a path that
assumes audio is present.

Nothing checks the memory argument automatically. A person confirms it by
running `ambient record` and `ambient diarize` under `/usr/bin/time -l`
separately and checking neither peak approaches the combined figure.
`cargo run --release --bin diartest` tunes `--threshold` against a bare wav.
