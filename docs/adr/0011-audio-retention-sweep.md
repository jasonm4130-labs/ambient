# 11. Sweep track audio after 7 days

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

The room track is the most sensitive artefact this tool produces — it carries
the whole room, including people who never joined the meeting — and it is the
least useful artefact once the transcript exists. Keeping wav files
indefinitely accumulates the exact material that would be most damaging to
retain and least often wanted.

## Considered Options

- **Keep audio forever**, and let the user delete it by hand.
- **A launchd agent** sweeping on a schedule independently of the app.
- **Sweep at the end of every recording**, with a configurable window.

## Decision Outcome

`audio_retention_days` defaults to 7. The sweep runs at the end of every
recording — the app is by definition running whenever a recording happens, so
no launchd agent is needed — and it deletes only track wavs, never
`raw.jsonl`, `edits.jsonl`, `session.json` or `transcript.md`. `0` deletes the
wavs as soon as the transcript is written; `forever` keeps them.

Two guards authorise the deletion, and both are required
(`sweep_audio_at` in `src/session.rs`):

1. **`transcript.md` must exist.** A session without one is still being
   written, and its audio is the only copy of what was said.
2. **`has_transcribed_words` must be true** — `raw.jsonl` must hold at least
   one non-blank line. This is the interesting half: `markdown` writes a
   transcript file even when the recogniser heard nothing, so gating on the
   file merely existing would delete exactly the recordings where the audio is
   the only surviving copy. An earlier version did precisely that.

The sweep also skips symlinks rather than following one out of the sessions
folder, and leaves directories that are not sessions alone.

## Consequences

Text outlives audio by default, which is the intended asymmetry: the
transcript is the durable artefact and the recording is not.

`ambient diarize` must therefore refuse on a session whose audio has been
swept. A re-run reverts the previous run's labels before discovering there is
nothing to read, which would silently unlabel every speaker nobody had named
by hand — see [ADR-0007](0007-diarize-separate-verb.md).

## Confirmation

Both guards and the window are pinned by unit tests in `src/session.rs` that
run in CI under `cargo test --all-targets`:
`a_transcript_with_no_words_in_it_does_not_authorise_deleting_the_audio` is
the one that fails if the second guard is dropped,
`a_session_without_a_transcript_is_never_swept` covers the first, and
`keeping_audio_forever_deletes_nothing`, `audio_within_the_window_is_left_alone`,
`zero_days_removes_the_audio_and_keeps_the_words` and
`a_directory_that_is_not_a_session_is_left_alone` cover the rest. The sweep
takes `now` as an argument (`sweep_audio_at`) precisely so the window is
testable without waiting seven days.

The end-to-end behaviour was also checked by hand against a real recording:
audio gone, `transcript.md` and `raw.jsonl` intact.
