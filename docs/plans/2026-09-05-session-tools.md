---
title: "Plan: session tools"
sidebar:
  order: 93
---

# Plan: session tools

**Goal:** give the CLI the verbs a script or a second program needs to read
sessions safely: machine-readable output, a session list, an undo for naming,
a guard on moving the sessions directory, and a `doctor` that says what is
missing.
**Architecture:** every verb is a thin `src/main.rs` arm over a pure function in
`src/session.rs` (or `src/config.rs`) that takes paths, so each has a unit test
against a temporary directory. Output formats are stable: `--json` prints the
same structs `serde_json` already serialises.
**Tech Stack:** Rust 1.85 floor, serde/serde_json, no new crates.

## Global Constraints

- `raw.jsonl` is never rewritten; every change is an appended line in `edits.jsonl`.
- `--json` output is one JSON value on stdout and nothing else on stdout.
- Every new verb is added to `USAGE` in `src/main.rs` and documented in
  `docs/using/commands.md` in the existing per-verb style (heading, fenced
  command, a short paragraph).
- Functions that take a `root` never consult `session::home()`; the CLI arm
  passes `home()` in.
- Tests use a directory unique to the test: `std::env::temp_dir().join(format!("ambient-{}-{}", <test fn name as a string literal>, std::process::id()))`, created at the start and removed at the end of that test only; `cargo test` runs tests in parallel, so two tests never share a directory.
- `scripts/check` does not look at docs. After any docs edit run
  `node .github/scripts/check-links.mjs` and quote its `… 0 broken link(s)` line.
- No `.github/workflows/`, `.claude/`, or `loop/` changes; no editing existing tests.

## Task 1: `show --json`

**Files:**
- Modify: `src/session.rs:1049-1055` (`Line`), `src/session.rs:1127-1145` (`show`)
- Modify: `src/main.rs:141-148` (`show` arm), `USAGE`
- Modify: `docs/using/commands.md` (the `show` section)

**Interfaces:**
- Produces: `#[derive(Serialize)]` on `Line` (field order `track, start_ms, end_ms, speaker, text`;
  `track` serialises as `"room"`/`"call"` exactly as `RawRecord` does);
  `pub fn show(dir: &Path, verbatim: bool, json: bool) -> Result<()>`;
  `ambient show <dir> [--verbatim] [--json]` printing a JSON array of lines.

- [ ] **Step 1:** write the failing test in `src/session.rs`'s test module: create a temp session dir with
  `raw.jsonl` holding two records (`room` 0–1000 "hello", `call` 1000–2000 "hi") and `edits.jsonl` with one
  `speaker` edit naming the first as "Ana"; call `transcript(dir, false)` and assert
  `serde_json::to_string(&lines)` equals
  `[{"track":"room","start_ms":0,"end_ms":1000,"speaker":"Ana","text":"hello"},{"track":"call","start_ms":1000,"end_ms":2000,"speaker":null,"text":"hi"}]`.
- [ ] **Step 2:** run `cargo test session::`, expect FAIL "the trait `Serialize` is not implemented for `Line`".
- [ ] **Step 3:** derive `Serialize` on `Line`; add the `json` parameter to `show`, printing
  `serde_json::to_string_pretty` of the lines when set; parse `--json` in the `show` arm the way
  `--verbatim` is parsed (collect the remaining args into a `Vec` first so both flags work in either order).
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`; run `node .github/scripts/check-links.mjs`,
  expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add src/session.rs src/main.rs docs/using/commands.md && git commit -m "show: --json for scripts"`.

## Task 2: `ambient sessions`

**Files:**
- Modify: `src/session.rs` (new `pub struct SessionSummary`, `pub fn summaries(root: &Path) -> Vec<SessionSummary>`
  next to `list` at `src/session.rs:927-937`; new `pub fn live_session_in(root: &Path) -> Option<PathBuf>`
  with the existing `live_session()` (`src/session.rs:1389-1397`) becoming `live_session_in(&home())`)
- Modify: `src/main.rs` (new `sessions` arm before `show`), `USAGE`
- Modify: `docs/using/commands.md` (new `## sessions` section)

**Interfaces:**
- Consumes: `session::list`, `SessionMeta` (`session.json`), `is_growing`, `live_transcriber`.
- Produces: `#[derive(Serialize)] pub struct SessionSummary { pub id: String, pub dir: PathBuf, pub name: Option<String>, pub started_at: Option<String>, pub duration_s: Option<f64>, pub transcribed: bool, pub live: bool, pub transcribing: bool, pub error: Option<String> }`;
  `ambient sessions [--json]`: one line per session newest first (`id  started_at  duration  name  state`)
  or the JSON array. `state` is one of `live`, `transcribing`, `awaiting transcript`, `done`, `broken`.

- [ ] **Step 1:** write the failing test: temp root with four session dirs: `a` holding `session.json`
  (id "a", duration 12.5) and `transcript.md`; `b` holding only `session.json` (id "b"); `c` holding a
  `session.json` that is not JSON (`{"id":`); `d` holding no `session.json` but a freshly written
  `audio/room.native.wav` (a capture in progress: `session.json` is written only after the scratch
  wavs close, `src/session.rs:766`); plus a directory `notes` with nothing in it and a plain file
  `README`. `summaries(root)` returns `d`, `c`, `b`, `a` in that order (newest first, by directory
  name, which sorts by time); `a.transcribed == true`; `b.transcribed == false`; `c.error` is `Some`
  and contains "session.json"; `c.id == "c"` (the directory name, when the file cannot say);
  `d.live == true`, `d.error == None`, `d.duration_s == None`; `notes` and `README` are absent;
  `a`, `b`, `c` have `live == false`.
- [ ] **Step 2:** run `cargo test session::summaries`, expect FAIL "cannot find function `summaries`".
- [ ] **Step 3:** implement: a dir is included when `session.json` exists or
  `audio/room.native.wav` is growing; a parse or read failure fills `error` and leaves the metadata
  fields `None` (state `broken`), so a session interrupted mid-write is listed, not hidden; a
  `session.json` whose `symlink_metadata` is not a regular file is not read and fills `error` with
  "session.json is a symlink"; a live
  capture has no `session.json` yet, so its metadata fields are `None` with no error (state `live`). `transcribed` = `transcript.md` is a file; `live` =
  `is_growing(dir/audio/room.native.wav)`; `transcribing` = `live_transcriber(dir).is_some()`.
  Add `live_session_in(root)` by moving the body of `live_session()` and making the old function call it.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`; run `cargo run -- sessions` and quote the first
  line; run `node .github/scripts/check-links.mjs`, expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add src/session.rs src/main.rs docs/using/commands.md && git commit -m "sessions: list what is on disk"`.

## Task 3: `ambient undo`

**Files:**
- Modify: `src/session.rs` (new `pub fn undo_last_naming(dir: &Path) -> Result<usize>` and `pub fn undo_seq(dir: &Path, seq: usize) -> Result<()>` after `name_speaker`, `src/session.rs:1343-1379`)
- Modify: `src/main.rs` (new `undo` arm), `USAGE`
- Modify: `docs/using/commands.md` (new `## undo` section); `docs/using/getting-started.md:174` (replace
  "there is no undo verb" with one sentence pointing at `undo`)

**Interfaces:**
- Consumes: `Edit::Revert { target_seq, by, at }` which `transcript` already folds (`src/session.rs:1059-1121`);
  `USER_BY`; `markdown`.
- Produces: `ambient undo <dir>` reverts the newest batch of `speaker` edits by `user` (every `speaker`
  line sharing the newest `at` among user speaker edits), appending one `revert` per line and rewriting
  `transcript.md`; prints `reverted <n> edits`. `ambient undo <dir> --seq <n>` reverts one edit by its
  zero-based line in `edits.jsonl`. An already-reverted seq, or a seq that names a `revert`, is an error.

- [ ] **Step 1:** write the failing tests: (a) temp session with two raw lines both labelled `SPEAKER_00`
  by `diarize` edits, then `name_speaker(dir, "SPEAKER_00", "Ana")`; `undo_last_naming(dir)` returns 2 and
  `transcript(dir,false)` shows `SPEAKER_00` again; a second `undo_last_naming` returns an error
  "nothing to undo". (b) `undo_seq(dir, 0)` on the diarize edit reverts it; `undo_seq(dir, 0)` again errors
  "already reverted"; `undo_seq(dir, 99)` errors "no edit 99".
- [ ] **Step 2:** run `cargo test session::undo`, expect FAIL "cannot find function".
- [ ] **Step 3:** implement over `edits.jsonl` read as `Vec<Edit>`: the set of already-reverted seqs is
  every `Revert.target_seq`; append `Edit::Revert { target_seq, by: USER_BY, at: now }` per line with one
  `at` for the batch; rewrite `transcript.md` from `markdown(dir)?` exactly as `name_speaker` does.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`; run `node .github/scripts/check-links.mjs`,
  expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add src/session.rs src/main.rs docs/using/commands.md docs/using/getting-started.md && git commit -m "undo: revert the last naming batch"`.

## Task 4: Refuse to move the sessions directory mid-recording

**Files:**
- Modify: `src/config.rs` (new `pub fn refuse_while_live(key: &str, live: Option<&Path>) -> Result<()>` next to `set`, `src/config.rs:114`)
- Modify: `src/main.rs:403-412` (`config` arm: call it before `cfg.set`)
- Modify: `docs/using/settings.md` (one sentence under the sessions directory setting)

**Interfaces:**
- Consumes: `session::live_session()` (`src/session.rs:1389-1397`).
- Produces: `ambient config sessions_dir <path>` fails with
  `a recording is in progress in <dir>; stop it before moving the sessions directory` while a capture is live.

- [ ] **Step 1:** write the failing test in `src/config.rs`'s test module (create one if absent):
  `refuse_while_live("sessions_dir", Some(Path::new("/x")))` is an error containing "/x";
  `refuse_while_live("sessions_dir", None)` and `refuse_while_live("diarize", Some(Path::new("/x")))` are `Ok`.
- [ ] **Step 2:** run `cargo test config::`, expect FAIL "cannot find function `refuse_while_live`".
- [ ] **Step 3:** implement; wire the arm as `ambient::config::refuse_while_live(&k, ambient::session::live_session().as_deref())?;`.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`; run `node .github/scripts/check-links.mjs`,
  expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add src/config.rs src/main.rs docs/using/settings.md && git commit -m "config: keep sessions_dir still while recording"`.

## Task 5: `ambient doctor`

**Files:**
- Create: `src/doctor.rs`; register `pub mod doctor;` in `src/lib.rs` before writing the test
- Modify: `src/session.rs` (extract `pub fn model_files(root: &Path) -> ModelFiles` holding the four paths
  the verbs build at `src/session.rs:520-535` and `src/session.rs:1188-1197`, and make those two sites use it)
- Modify: `src/main.rs` (new `doctor` arm), `USAGE`
- Modify: `docs/using/troubleshooting.md` (a `## Start with doctor` section at the top of the page)

**Interfaces:**
- Consumes: `session::models_root() -> Result<PathBuf>` (an error when no candidate directory exists),
  `config::path`, `session::captured_awaiting_transcript`, `session::live_session_in`, `TRANSCRIBING_LOCK`.
- Produces: `pub struct ModelFiles { pub asr_dir: PathBuf, pub vad: PathBuf, pub segmentation: PathBuf, pub embedding: PathBuf }`;
  `pub struct Check { pub name: String, pub ok: bool, pub detail: String }`;
  `pub fn run(models: Result<PathBuf>, config_file: &Path, sessions: &Path) -> Vec<Check>`;
  `ambient doctor [--json]` prints `ok   <name>  <detail>` / `FAIL <name>  <detail>` per check and exits
  1 if any failed. Check names, in order: `models/root`, `models/asr`, `models/vad`, `models/segmentation`,
  `models/embedding`, `config`, `sessions/writable`, `sessions/awaiting-transcript`, `sessions/live`,
  `sessions/stale-lock`.

- [ ] **Step 1:** write the failing tests: (a) with a temp models dir holding only an empty
  `wespeaker_en_voxceleb_resnet34_LM.onnx`, a temp config file containing `{}`, and a temp sessions
  dir with one session lacking `transcript.md` (`session.json` + `audio/room.wav`): `run` reports
  `models/root` ok, `models/asr` FAIL, `models/vad` FAIL, `models/segmentation` FAIL, `models/embedding` ok,
  `config` ok, `sessions/writable` ok, `sessions/awaiting-transcript` ok with detail "1 session",
  `sessions/live` ok with detail "none", `sessions/stale-lock` ok. (b) `run(Err(anyhow!("no models")), …)`
  reports `models/root` FAIL with detail containing "no models" and the four `models/*` checks FAIL with
  detail "no models directory"; the sessions checks still run. (c) a config file containing `{"diarize":`
  gives `config` FAIL with detail containing "not valid"; a missing config file gives `config` ok with
  detail "defaults"; a config path that is a directory gives `config` FAIL with detail containing
  "could not be read".
- [ ] **Step 2:** run `cargo test doctor::`, expect FAIL "unresolved import".
- [ ] **Step 3:** implement. `config` reads and parses the file itself, because `Config::load_from`
  (`src/config.rs:74-98`) deliberately swallows both failures: `read_to_string` `NotFound` → ok
  "defaults"; any other read error → FAIL "could not be read: <error>"; `serde_json::from_str::<Config>`
  error → FAIL "not valid: <error>". `sessions/live` uses `live_session_in(sessions)`. A `transcribing.lock` whose pid is not alive
  (`libc::kill(pid, 0)` fails) is `sessions/stale-lock` FAIL with the path in the detail.
- [ ] **Step 4:** run `scripts/check`, expect `CHECK OK`; run `cargo run -- doctor` and quote its output;
  run `node .github/scripts/check-links.mjs`, expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add src/doctor.rs src/session.rs src/lib.rs src/main.rs docs/using/troubleshooting.md && git commit -m "doctor: say what is missing"`.

## Open Questions

