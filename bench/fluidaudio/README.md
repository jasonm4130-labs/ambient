# FluidAudio spike (issue #168)

Scripts behind the 2026-11-25 comparison of the shipped ONNX pipeline with
FluidAudio (Core ML / Neural Engine) speech-to-text and diarisation candidates.
They are not part of the build.

- `amiref.py` builds word-level, speaker-attributed references for the 16 AMI
  test meetings from the AMI manual annotations 1.6.2.
- `run.sh` runs every candidate over both the headset mix and the single
  far-field mic (Array1-01).
- `score.py` reports WER (Whisper English normaliser), speaker counts, and WDER:
  the share of aligned words given the wrong speaker.
- `wer.py` scores single files, with `--prefix` for clips cut from longer
  references.
- `voxwav.py` extracts VoxConverse test files. Nemotron 3 was trained on
  VoxConverse test, so do not use it to score Nemotron.

The datasets are on brok under `/mnt/user/data/datasets/`
(`ami-corpus`, `ami-diarization-setup`, `voxconverse`, `notsofar-1`,
`earnings22`), each with `PROVENANCE` and `SHA256SUMS`. `src/bin/der.rs`
gained `--rttm-out` and `--hyp-dir`, so other diarisers are scored with the
same scorer.
