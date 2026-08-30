---
title: "10. Store names, never voiceprints"
sidebar:
  order: 50
---

# 10. Store names, never voiceprints

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

Diarization discovers speakers rather than identifying them, so every session
starts with `room-1` and `call-2` and someone has to type the names again.
The pipeline already computes a 256-d WeSpeaker embedding per speaker, so
keeping those embeddings and matching them against the next recording is a
small change that would remove the retyping entirely.

## Considered Options

- **Store an embedding per person** and auto-label future recordings.
- **Store a list of names** in `roster.json` beside the config, offered as a
  dropdown after a recording.

## Decision Outcome

Names only. `ambient roster add <name>` keeps the list; after a recording the
settings window lists each speaker diarization could not name alongside the
first thing that voice said, and a dropdown of roster names puts a name on
every line of that speaker.

The compliance argument is the decisive one. An embedding kept in order to
recognise someone later is biometric data under Article 9 of the UK GDPR — a
different regime, requiring explicit consent and a DPIA — where a text file of
names is not. For a tool whose whole point is being local-first and low
ceremony, that is a disproportionate obligation to take on for a convenience.

The accuracy argument points the same way. A confidently misattributed turn is
a lie in your notes, and worse than an honest `call-2`. Storing nothing that
could be matched means the system never guesses. The roster removes the
retyping, not the choosing.

## Consequences

Naming still costs one interaction per speaker per session, and it goes
through the same `session::name_speaker` the CLI uses, so the edit is recorded
as the user's and survives a re-diarize
([ADR-0008](0008-append-only-raw.md)).

Embeddings are computed during `ambient diarize` and discarded when it
finishes; nothing writes them to disk. Any future feature that wants
cross-session recognition is a new decision with a DPIA attached, not an
extension of this one.

## Confirmation

The roster's own behaviour is covered in CI by the tests in `src/roster.rs` —
`it_round_trips_through_the_file`, `the_same_person_is_not_added_twice`,
`a_blank_name_is_refused`, `a_corrupt_roster_does_not_stop_the_app`.

Those tests prove what the roster stores, not what it does not. Nothing
automated asserts that no embedding is ever persisted. A person confirms that
by running a full `record` and `diarize` cycle and then checking that neither
the session directory nor `~/Library/Application Support/Ambient` has gained
any file of floating-point vectors — `roster.json` should hold names and
nothing else.
