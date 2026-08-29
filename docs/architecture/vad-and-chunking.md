# Voice activity detection

Voice activity detection sits between capture and the recogniser: it decides
which audio is worth sending to [ASR](asr.md), and — more consequentially —
where the chunk boundaries fall.

Silero VAD (`models/silero_vad.onnx`, 629 KB) runs before transcription and
does two jobs. It skips silence, and — more valuably — it decides where to cut.

Chunk boundaries were previously chosen by local energy, which sliced through
words: a 127 s file came back containing "Chunking keeps the memory flat. flat
regardless of how long the meeting actually runs." Cutting on VAD boundaries
instead, the same file yields 375 words with **zero adjacent duplicates**.

| File | Speech / total | Chunks | Realtime | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| 127 s continuous | 127.5 / 127.5 s | 6 | 39x | 2255 MB |
| mic track | 3.6 / 9.7 s | 1 | 14x | — |

The VAD pass costs throughput (39x against 54x without) because it runs one
inference per 32 ms frame. Worth it: memory stays flat, silence is never sent
to the encoder, and the seams stop mangling words.

**One behaviour to know.** VAD also discarded the television audible in the
background of the mic recording — it fell below the speech threshold. Good for
noise, but quiet speech that genuinely matters would go the same way, so the
thresholds in `vad.rs` are a knob to revisit against real room recordings
rather than settled.

## Chunks versus turns

One record per turn, not per recogniser call. `Vad::chunks` merges neighbouring
speech into few large calls, which is right when the output is one block of
text and wrong here — a record spanning two people cannot carry a speaker, and
`diarize` can only label whole records. `record` uses `Vad::turns` instead,
which splits over-long speech but never merges across a silence.

See [ADR-0004](../adr/0004-vad-chunking.md).
