---
title: "Plan: hardening"
sidebar:
  order: 96
---

# Plan: hardening

**Goal:** remove the panics and silent failures a real recording can hit, and
put tests under the small pure functions that have none.
**Architecture:** each task is one class of defect across the files that have
it, fixed the same way everywhere, with a unit test that fails on the old code.
**Tech Stack:** Rust 1.85 floor, no new crates.

## Global Constraints

- A fix changes behaviour only on the failing input; every existing test still passes unchanged.
- No new `unwrap()` on anything that comes from a file, a model, or the user.
- No `.github/workflows/`, `.claude/`, or `loop/` changes; no editing existing tests.

## Task 1: NaN-safe argmax

**Files:**
- Modify: `src/features.rs` (add `pub fn argmax(xs: &[f32]) -> Option<usize>` and `pub fn argmin(xs: &[f32]) -> Option<usize>`)
- Modify: `src/asr.rs:269-273` and `src/asr.rs:278-282` (token and duration argmax),
  `src/diarize.rs:90-94` (per-frame class argmax), `src/vad.rs:248-251` (quietest-frame argmin)

**Interfaces:**
- Produces: `argmax`/`argmin` using `f32::total_cmp`, returning `None` on empty input and the first
  index on ties; a NaN never panics and never wins over a finite value (filter NaN out before comparing).

- [ ] **Step 1:** write the failing tests in `src/features.rs`'s test module: `argmax(&[])` is `None`;
  `argmax(&[1.0, 3.0, 3.0])` is `Some(1)`; `argmax(&[f32::NAN, 2.0, 1.0])` is `Some(1)`;
  `argmax(&[f32::NAN])` is `None`; `argmin(&[2.0, f32::NAN, 1.0])` is `Some(2)`.
- [ ] **Step 2:** run `cargo test features::arg`, expect FAIL "cannot find function `argmax`".
- [ ] **Step 3:** implement, then replace the four `max_by`/`min_by` + `partial_cmp().unwrap()` sites.
  In `asr.rs` take `argmax(...)` then index `logits[tok]` for the winning value; a `None` there is an
  `anyhow` error "empty logits" (the length and finiteness of the joiner output are validated once,
  in Task 6). In `diarize.rs` keep the `unwrap_or(0)` fallback. In `vad.rs` keep the `unwrap_or(target)` fallback.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`.
- [ ] **Step 5:** commit: `git add src/features.rs src/asr.rs src/diarize.rs src/vad.rs && git commit -m "argmax: total_cmp, no panic on NaN"`.

## Task 2: Model-path errors that say what to do

**Files:**
- Modify: `src/session.rs:534` (the `no VAD model at …` bail), `src/session.rs:845` (`vad_path.to_str().unwrap()`)
- Modify: `src/session.rs` (add `pub fn utf8_path(p: &Path) -> Result<&str>` next to `models_root`, `src/session.rs:139`)

**Interfaces:**
- Produces: the VAD bail reads `no VAD model at <path> — run ./fetch-models.sh`, matching the ASR bail
  five lines above it; `utf8_path` errors with `path is not UTF-8: <display>` and is used at `:845` and at
  the two `bad model path` sites in `diarize_session` (`src/session.rs:1194-1197`).

- [ ] **Step 1:** write the failing test in `src/session.rs`'s test module:
  `utf8_path(Path::new("/a/b"))` is `Ok("/a/b")`; on unix,
  `utf8_path(Path::new(std::ffi::OsStr::from_bytes(b"/a/\xff")))` (via `std::os::unix::ffi::OsStrExt`)
  is an error whose message contains "not UTF-8".
- [ ] **Step 2:** run `cargo test session::utf8_path`, expect FAIL "cannot find function".
- [ ] **Step 3:** implement and replace the three sites; add the hint to the VAD bail.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`.
- [ ] **Step 5:** commit: `git add src/session.rs && git commit -m "session: model path errors name the fix"`.

## Task 3: Tests under slugify, mmss and the status line

**Files:**
- Modify: `src/session.rs` (test module only, plus any fix a test forces in `slugify` `:178-191`,
  `mmss` `:1494-1497`, or `Meter::status_line` `:287-305`)

**Interfaces:**
- Consumes: `slugify(&str) -> String`, `mmss(u64) -> String`, `Meter::status_line(&self) -> String`,
  `MeterPhase` (`src/session.rs:220-228`).

- [ ] **Step 1:** write the tests: `slugify("Hello, World!")` is `"hello-world"`;
  `slugify("  --x--  ")` is `"x"`; `slugify("")` is `""`; `slugify("Ünïcode ok")` is `"ncode-ok"` (only
  ASCII alphanumerics survive; the test names that as the current rule);
  `mmss(0)` is `"00:00"`; `mmss(61_000)` is `"01:01"`; `mmss(3_600_000)` is `"60:00"`;
  `mmss(999)` is `"00:00"`. For `status_line`: read `Meter` and `status_line` first, then construct a
  `Meter` in each `MeterPhase` and assert the exact string the current code produces, so the test pins
  the format the menu bar parses; every phase gets one assertion.
- [ ] **Step 2:** run `cargo test session::`, expect the new tests to PASS against unchanged code, or
  FAIL with a message that names a real defect; a failing test here is a finding, fix the function and
  say so in the report. Do not weaken the test.
- [ ] **Step 3:** run `scripts/check`, expect `CHECK OK`.
- [ ] **Step 4:** commit: `git add src/session.rs && git commit -m "session: pin slugify, mmss and status line"`.

## Task 4: VAD hysteresis as a pure function with tests

**Files:**
- Modify: `src/vad.rs:177` (`fn segments_from(&self, …)` becomes `fn segments_from(probs: &[f32], n_samples: usize) -> Vec<Segment>`, an associated function; update the call at `src/vad.rs:173`)
- Modify: `src/vad.rs:186-194` (a non-finite probability reads as `0.0`, silence)
- Modify: `src/vad.rs` (test module: new tests; create the module if absent)

**Interfaces:**
- Consumes: the constants at `src/vad.rs:18-24` (`ON` 0.50, `OFF` 0.35, `MIN_SPEECH_MS` 250,
  `MIN_SILENCE_MS` 400, `PAD_MS` 200), `SR` 16 000, `FRAME` 512, `Segment { start, end }` in samples.
- Produces: `Vad::segments_from` callable without a model; a NaN or infinite probability is treated
  as `0.0`; tests build a probability array with a helper
  `fn probs(pattern: &[(f32, u32)]) -> Vec<f32>` that emits, for each `(value, ms)`,
  `(ms * SR / 1000).div_ceil(FRAME)` frames of `value`, the ceiling the production frame grid uses.

- [ ] **Step 1:** write the failing tests: (a) all 0.0 for 2 s: no segments. (b) 1.0 for 1000 ms in the
  middle of 3 s of 0.0: one segment whose start is `PAD_MS` before the speech and end `PAD_MS` after,
  within one frame. (c) 1.0 for 100 ms: no segments (shorter than `MIN_SPEECH_MS`). (d) 1.0 for 500 ms,
  0.0 for 200 ms, 1.0 for 500 ms: one segment (gap under `MIN_SILENCE_MS`). (e) the same with a 600 ms
  gap: two segments. (f) exactly `MIN_SILENCE_MS` of gap: assert whichever the code does today and
  name it in the test as the rule. (g) 0.45 for 1000 ms: no segments (never crosses `ON`). (h) 1.0 for
  500 ms then 0.40 for 1000 ms: the segment continues through the 0.40 run (above `OFF`) and ends at
  its end. (i) speech running to the last frame: `end == n_samples`, never past it. (j) 1.0 for 500 ms
  then `f32::NAN` for 1000 ms then 0.0 for 1000 ms: the segment ends where the speech ended plus
  `PAD_MS`, not at the end of the recording.
- [ ] **Step 2:** run `cargo test vad::segments`, expect FAIL ("cannot find" while `segments_from`
  still takes `&self`; (j) fails on the NaN policy; any other failure names a real defect, fixed in the
  function and reported).
- [ ] **Step 3:** make the function associated; map non-finite `p` to `0.0` at the top of the loop; fix
  anything (i) exposed.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`.
- [ ] **Step 5:** commit: `git add src/vad.rs && git commit -m "vad: hysteresis is a pure function with tests"`.

## Task 5: Settings patch parsing pinned by tests

**Files:**
- Modify: `src/settings.rs` (a `#[cfg(test)] mod tests` at the end; `Patch` `:39-57` and `Assign`
  `:59-67` stay private, the tests live in the same module)

**Interfaces:**
- Consumes: `serde_json::from_str::<Patch>`.

- [ ] **Step 1:** write the tests: `{}` gives a `Patch` with every field `None`;
  `{"threshold":0.6,"diarize":false}` sets exactly those two; `{"assign":{"label":"SPEAKER_00","name":"Ana","session":"2026-09-05-1200"}}` sets all three `Assign` fields;
  `{"assign":{"label":"a"}}` is an error mentioning `name`; `{"threshold":"0.6"}` is an error (a string is
  not a number); `{"audio_retention":"forever"}` gives `Some("forever")`; `{"unknown":1}` parses (unknown
  keys are ignored today; the test names that as the rule so a change to `deny_unknown_fields` is deliberate).
- [ ] **Step 2:** run `cargo test settings::`, expect PASS on unchanged code, or a FAIL that names a
  real defect to fix and report.
- [ ] **Step 3:** run `scripts/check`, expect `CHECK OK`.
- [ ] **Step 4:** commit: `git add src/settings.rs && git commit -m "settings: pin the patch shape"`.

## Task 6: Empty and damaged inputs to the recogniser

**Files:**
- Modify: `src/features.rs:91` (`log_mel` on empty input), `src/asr.rs:66-72` (`tokens.txt` read),
  `src/asr.rs:265-284` (joiner output validation before argmax)
- Modify: `src/asr.rs` (add `pub fn check_joiner(logits: &[f32], n_tok: usize, n_dur: usize) -> anyhow::Result<()>`
  and `pub fn parse_tokens(text: &str, source: &str) -> anyhow::Result<Vec<String>>`, the loop at
  `src/asr.rs:69-72` moved out of `Recognizer::load`, which then calls it)

**Interfaces:**
- Produces: `log_mel(&[])` returns `(Vec::new(), 0)` and `Recognizer::transcribe` on empty samples
  returns `Ok` with empty text (find the callers of `log_mel` and make each handle zero frames);
  `parse_tokens` errors with `<source> is empty` when no token lines are read, so `Recognizer::load`
  fails there instead of `tokens.len() - 1` underflowing; `check_joiner` errors with `joiner output has <n> values, expected at
  least <n_tok + n_dur>` on a short output and `joiner output is not finite` when any value is NaN or
  infinite, and is called once per decode step before the argmax and the softmax.

- [ ] **Step 1:** write the failing tests: in `src/features.rs`, `log_mel(&[])` is `(vec![], 0)` (today it
  panics on `samples[0]`); in `src/asr.rs`'s test module, `check_joiner(&[0.0; 5], 4, 2)` errors with a
  message containing "expected at least 6", `check_joiner(&[0.0, f32::NAN, 0.0], 2, 1)` errors with
  "not finite", `check_joiner(&[0.0; 6], 4, 2)` is `Ok`; `parse_tokens("", "x/tokens.txt")` errors with
  "x/tokens.txt is empty", `parse_tokens("\n\n", "x/tokens.txt")` errors the same way, and
  `parse_tokens` on the first three lines of the real `tokens.txt` layout (read `src/asr.rs:69-72` for
  the line format before writing this) yields three tokens.
- [ ] **Step 2:** run `cargo test features::log_mel`, expect FAIL (a panic); run `cargo test asr::`,
  expect FAIL "cannot find function" for `check_joiner` and `parse_tokens`.
- [ ] **Step 3:** implement all three guards.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`.
- [ ] **Step 5:** commit: `git add src/features.rs src/asr.rs && git commit -m "asr: empty and damaged inputs fail with a message"`.

## Open Questions

