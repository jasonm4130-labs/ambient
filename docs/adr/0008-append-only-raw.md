# 8. `raw.jsonl` is append-only and `transcript.md` is derived

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

A session accumulates corrections after it is recorded: a repair pass fixes
proper nouns, `ambient name` renames a speaker, `ambient diarize` relabels
every line and may be re-run with a different threshold. Each of those could
rewrite the transcript in place, which is simplest and destroys the only copy
of what the recogniser actually heard — including the evidence needed to tell
a bad threshold from a bad recording.

## Considered Options

- **Rewrite the transcript in place** on every edit.
- **Append-only `raw.jsonl` with a separate append-only `edits.jsonl`**, folded
  at read time, with `transcript.md` regenerated from the fold.

## Decision Outcome

`raw.jsonl` is what the recogniser heard and is never rewritten. Repairs,
speaker names and reverts layer into `edits.jsonl`, also append-only, and
`ambient show` folds the second over the first. A revert appends a record
naming an earlier line rather than deleting one, so even undoing is an append.

`transcript.md` is derived. `record`, `diarize`, `name` and `export` each
regenerate it, so the file on disk is never stale, and deleting it loses
nothing.

## Consequences

The raw transcript is recoverable byte-for-byte at any point, which is what
makes `diarize` safely re-runnable ([ADR-0007](0007-diarize-separate-verb.md))
and what lets `--verbatim` show the unedited text.

`raw.jsonl` plus `edits.jsonl` are the only source of truth, so anything that
wants to change a session appends rather than writes — including the settings
window, which names speakers through the same `session::name_speaker` the CLI
uses so the edit is recorded as the user's.

Because the markdown is disposable, it is also the one artefact safe to treat
as an export: it carries YAML front matter for the downstream Confluence stage
and can be regenerated whenever that format changes.

## Confirmation

The fold's edge cases are pinned by tests in `src/session.rs` that run in CI —
`a_person_with_a_name_is_no_longer_offered`,
`a_name_that_merely_resembles_a_label_is_left_named`, and
`diarizations_own_labels_are_the_ones_offered_for_naming` — each of which
depends on edits being layered rather than applied destructively.

Nothing automated asserts `raw.jsonl` is byte-identical after an edit. That
was verified by hand, by hashing `raw.jsonl` either side of applying an edit
layer, and by hashing it before and after a `diarize` run; a person repeating
the check runs `shasum` on the file, applies an edit with `ambient name`, and
compares.
