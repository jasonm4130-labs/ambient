---
title: "Speaker separation"
sidebar:
  order: 25
---

# Speaker separation

Speaker separation answers "who spoke when" within one track. It runs as the
second stage of the same job as [transcription](asr.md), after every track has
been transcribed, and only when the `diarize` setting is on. With it off, a
transcript has no speaker labels, and the track is the only attribution: on a
one-to-one call, room is you and call is them.

## The model

`ambient-engine diarize` runs Nemotron 3 Diarization with FluidAudio's
`fast32` preset over one 16 kHz mono track. It reads the file from disk in
100 ms pieces and feeds them through the model's streaming path, so the
speaker model's memory does not grow with the length of the meeting. It writes
a list of turns, each a speaker index with a start and an end, and nothing
else. The model decides how many people there are; there is no threshold to
set.

The helper's turns are used as they are. Filling short gaps between one
speaker's turns and padding their edges lowers DER on AMI and makes the
transcript worse, because padding hands the edge words of a turn to whoever
spoke next. The figures are in
[measurements](measurements.md#the-core-ml-helper-adr-0020).

## The phantom-speaker filter

Nemotron sometimes splits one voice into a second speaker who is active only
briefly. Before any word is attributed, `pipeline::assemble` adds up each
speaker's turn time over the whole track and drops every speaker holding less
than 5 s (`pipeline::PHANTOM_SECONDS`). If nobody reaches 5 s, which happens on
a short recording, the speaker with the most speech stays. On issue #168's
benchmark this took exact speaker counts from 15/16 to 16/16 on AMI and from
50/57 to 57/57 on PriMock57, without moving word attribution.

## Words to speakers

The helper's words and turns come from two separate passes, so the Rust side
joins them in `pipeline::assemble`:

- Each word goes to the kept speaker whose turns overlap it most. A word that
  overlaps no kept turn goes to the speaker whose turn is nearest. The words a
  dropped speaker held therefore go to a neighbour.
- A new line starts at a change of speaker, at a pause of a second or more, at
  a sentence end once the line is 20 s long, and at 40 s regardless. Pauses
  matter because on a call each track is mostly one voice, and breaking only at
  speaker changes would make the whole track one line.
- Speakers are numbered from 1 within each track in order of first appearance
  and labelled `room-1`, `call-2` and so on. Speaker 1 in the room and speaker 1
  on the call are different people, and nothing downstream may assume
  otherwise.

The labels are written as `speaker` edits by `diarize` in `edits.jsonl`, the
same kind of edit `ambient name` appends. A name a person types wins the fold,
and `ambient rerun` carries it to the new speaker that overlaps it most (see
[re-running](asr.md#re-running)).

## When it fails

A failed diarize stage fails the job, and the retry resumes from the
transcribe checkpoint. On the last allowed attempt the words are written
without labels instead, with a warning; see [retries](asr.md#retries).

`ambient name` on a session transcribed without labels lists none and says to
run `ambient rerun` with diarize on.
