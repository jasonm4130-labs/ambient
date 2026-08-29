# Diarization

Diarization answers "who spoke when" on a session that already has a
transcript. It is a separate verb, it runs two more models, and its front-end
is the part that fails silently.

`ambient diarize <session-dir> [--threshold <f>]` assigns speakers to a
recording that already exists. Two models, in sequence:

| stage | model | what it decides |
|---|---|---|
| segment | pyannote-segmentation-3.0 | how many people are talking in each 10 s window, and which parts belong to each |
| embed | WeSpeaker resnet34 (VoxCeleb) | a 256-d vector per (window, speaker) |
| cluster | agglomerative, cosine, average linkage | which of those are the same person |

Segmentation's speaker indices are local to a window and mean nothing across
windows, so nothing tries to stitch windows together by permutation — the
global clustering is what recovers a consistent identity. Each raw record then
takes the speaker holding the most of it, labelled `room-1`, `call-2`: speaker
1 in the room and speaker 1 on the call are different people, and nothing
downstream should be able to assume otherwise.

It is a separate verb rather than part of `record` for three reasons. It
roughly doubles processing time for something not always wanted; `--threshold`
needs tuning against real room audio; and the two graphs are never resident
alongside Parakeet, which matters on the 16 GB target. Re-running appends
`revert` records for the previous run's labels before writing new ones, so a
bad threshold costs an appended revert rather than a lost recording.

## The front-end is the part that fails silently

WeSpeaker wants Kaldi fbank: HTK mel, 80 bins, Povey window, snip-edges,
pre-emphasis, int16-scale samples. `src/features.rs` is Slaney, 128 bins,
centre-padded and per-feature normalised, because that is what NeMo wants.
Almost nothing is shareable, so `src/fbank.rs` is a second front-end rather
than a configurable one.

A near-miss filterbank does not error here. It yields embeddings that still
cluster, only worse — so the damage arrives as confidently mislabelled
speakers, which is exactly the output a reader cannot audit. `bin/embtest.rs`
tests it directly, on two clips each of two `say` voices:

```
           A1     A2     B1     B2
    A1  1.000  0.880  0.192  0.138
    A2  0.880  1.000  0.145  0.132
    B1  0.192  0.145  1.000  0.823
    B2  0.138  0.132  0.823  1.000

same voice 0.851   cross voice 0.152   separation 0.700
```

A wrong front-end collapses that matrix toward uniform. `bin/diartest.rs` is
the companion tool for tuning `--threshold` against a bare wav;
`AMBIENT_DEBUG_DIAR=1` adds the per-window powerset class histogram, which is
how the class layout was confirmed rather than assumed — every known silent gap
decodes to class 0.

## Measured

A 38 s four-turn conversation between two `say` voices, played through the
speakers and captured by both tracks at once:

```
[00:00] call call-1  Right, let's start with the Transcoda backlog...
[00:11] call call-2  That's good news. The chunking fix went in on Tuesday...
[00:20] call call-1  Did anyone confirm whether customers were actually affected...
[00:29] call call-2  Still guessing, honestly, I will pull the request logs...
```

Both tracks alternate correctly across all four turns, and `raw.jsonl` hashes
identically before and after. The room track — a microphone listening to the
laptop's own speakers — over-clusters at the default threshold (three groups
rather than two, five at `--threshold 0.3`), though the surplus groups are
short fragments that never win a whole line. The tap does not have that
problem, which is the same argument as the tap-versus-microphone one.

Peak RSS on a 10.2-minute track: **249 MB**, 5.9 s wall — the sliding window
does not accumulate.

## Swept audio

`diarize` refuses to run on a session whose audio the retention policy has
already swept. The refusal is a guard, not a convenience: a re-run appends
reverts for the previous run's labels *before* it discovers there is nothing to
read, so without it every speaker nobody had named by hand would be silently
unlabelled. See [privacy](privacy.md) for the retention policy itself.

See [ADR-0006](../adr/0006-separate-frontends.md) and [ADR-0007](../adr/0007-diarize-separate-verb.md).
