---
title: "Plan: Nightshift smoke"
sidebar:
  order: 90
---

# Plan: Nightshift smoke

The first plan the overnight loop lands. Each task is small, real, and
checkable by reading the diff, so what is being tested is the loop, not the
task. Landed tasks are the merge commits on `main` naming
`land/2026-09-04-nightshift-smoke-tN`.

# Task 1: The docs say notarisation has never run, and it has

`docs/index.md` and `docs/adr/0014-developer-id-and-notarization.md` both
state that notarisation has never been run. Release v0.0.2 was notarised:
`gh release view v0.0.2` shows notes that begin "Signed and notarized", and
`release.sh` only publishes a bundle that passes `xcrun stapler validate`.

Confirm that with `gh release view v0.0.2 --json body -q .body` before editing
anything. Then:

1. In `docs/index.md`, replace the sentence that says notarisation has never
   been run with one that says it first ran for v0.0.2 and what that took
   (`release.sh` records the round trip as about eleven minutes). Keep the
   surrounding paragraph's point, which is what the release script does and
   does not prove.
2. In the ADR, the "Not yet exercised" paragraph becomes a record of the first
   exercise: v0.0.2, notarised and stapled, published from `release.sh`. An ADR
   is history, so leave the decision and its alternatives untouched and change
   only the paragraph that made a claim about the future.
3. Search `docs/` and `README.md` for any other sentence that says notarisation
   is untested or has not run, and fix each the same way. If there are none,
   say so in your report.

Do not touch `release.sh`. Do not add a section; the change is a few
sentences. `scripts/check` does not build the docs, so also run
`node .github/scripts/check-links.mjs` and make sure every link you touch
still resolves.

# Task 2: troubleshooting.md describes log lines the app does not write

`docs/using/troubleshooting.md` names two `app.log` lines that do not exist in
`src/`. Confirm each with `grep -rn` before editing.

1. Around line 97 it says a completed recording is logged as
   `session written:`. The app logs `audio written: <dir>`
   (`src/menubar.rs:589`) when the wav files are closed and
   `transcript written: <dir>` (`src/menubar.rs:609`) when the transcript is
   exported. Say that, keeping the point that the line appears whether or not
   the call track held anything.
2. Around line 203 it says a failure appends `recording FAILED:` with the
   outermost message only, because the error is formatted with `Display`
   rather than `{e:#}`. `fn fail` in `src/menubar.rs:316-319` does the
   opposite: the line is `FAILED — <context>: <error>` and it is built with
   `{e:#}`, so the cause chain is in the log line. Replace the claim with the
   true one; do not delete the sentence, because the paragraph's point (stderr
   is invisible under `open -a`, so the log line is the only report) still
   stands.

Quote the strings exactly as the code has them. Touch nothing outside that
file, and run `node .github/scripts/check-links.mjs` before committing.

# Task 3: log_mel has no tests

`src/features.rs` computes the Parakeet input features and has no test
module. Add one that pins the two properties a refactor is most likely to
break, using a synthetic signal built in the test (a sum of two sine waves at
16 kHz, one second long, is enough; never silence, because a constant row has
zero variance).

1. Shape: `log_mel(&signal)` returns `(out, frames)` with
   `out.len() == N_MELS * frames`, and `frames` equals what the centre-padded
   framing gives for that length: `(len + 2 * (N_FFT / 2) - N_FFT) / HOP_LENGTH + 1`.
   Assert both for at least two input lengths.
2. Normalisation: every mel row `out[m * frames..(m + 1) * frames]` has a mean
   within `1e-3` of zero and an unbiased standard deviation within `1e-2` of
   one. Write the row check as a helper so the assertion reads as one line
   per property.

Do not change `log_mel` or the constants; if a property does not hold, the
test is wrong or the code has a bug, and either way the report says which and
the task stops. Tests use the public constants from the same file, so nothing
new becomes `pub`.
