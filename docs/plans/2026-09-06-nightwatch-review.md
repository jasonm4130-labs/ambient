---
title: "Nightwatch review and completion audit"
---

# Nightwatch review and completion audit

The September 5 Nightwatch branch left four UI tasks unfinished. The September
6 follow-up completes those implementation tasks on the review branch. The
independent CLI review subsequently ran after explicit user approval. Terra
reported three defects, reproduced and repaired in the follow-up; the UI suite
now reports `Tests 67 passed (67)`. See [the Terra review record](2026-09-06-terra-review.md)
for the findings, verification and prepared Blacksmith runner change. The first
hosted Blacksmith run awaits cost approval. No merge or publication has been
performed.

The original audit compared `e5e2de5` with `f6acc6d8` and the six specifications
in `~/.local/state/nightwatch/ambient/specs/`. Its initial defect repairs are in
`bc4c467`. The follow-up implements the missing UI and preserves those repairs.

## Completion by specification

| Specification | Finding |
| --- | --- |
| 01: Session API | Dispatcher, CLI verbs, metadata, deletion, six export formats, config, roster and speaker methods delivered. This review repairs API pin ordering, exposes saved notes and makes metadata staging safe. |
| 02: Session tools | `doctor`, ten checks, JSON output and shared model paths delivered. Tasks 1–4 of the source plan were already on main; only Task 5 belonged to this run. |
| 03: UI shell | **Implemented.** Tasks 5–6 now replace the native views with the full-window React page and add shortcuts, loading states, focus rings, reduced motion, light/dark screenshots and documentation. |
| 04: UI features | **Implemented.** Tasks 5–6 now add text/tag filters, fixed-height virtual rows, month groups and Welcome with doctor checks and tested inline fixes. The normal window exposes these features. |
| 05: WER experiments | Resampling, silence-gap, padding and chunk sweeps, calls fixtures and the separate calls gate delivered. Dated measurements retain the shipped defaults. |
| 06: Live-ASR measurements | Replay benchmark, queue arithmetic, drain readiness/counting and dated go/no-go measurements delivered. The spec explicitly excludes implementing live transcription. |

## UI completion evidence

The remaining work is recorded in [the completion spec](2026-09-06-ui-completion.md).
The UI suite now reports `Test Files 17 passed (17)` and `Tests 65 passed (65)`.
The new sidebar tests fail against the pre-completion `SessionList` and pass
against the virtual list. Welcome fixes are checked against troubleshooting
text; integration tests cover empty libraries, failed health checks, shortcuts,
accessible controls, metadata errors and naming refresh after diarization.

The real host reports
`windowcheck: ok — WKWebView, transcript, phase, selection, Stop, Settings, File menu`.
Its Stop click reaches the app delegate; it does not open an audio device.
The activation-policy probe reports `policycheck: ok`, including modal and
other-window guards and reopening after close.

Both appearance runs report
`uicheck: ok — clipboard, live card, settings, 2,000 rows, snapshot`.
Each measured `maxMs: 9`, `maxRows: 29`, `last: true`. The probe includes a
scheduled wait and layout read in each sample. The [UI chapter](../developing/ui.md)
embeds the resulting light and dark screenshots. These are synthetic sessions
in WebKit, separate from the real-dispatcher host check.

UI type checking, lint and bundle generation pass. Lint retains advisory
function/file-length and style warnings; it is not warning-free. Source links
report `56 file(s) scanned, 0 broken link(s)`, and typo and whitespace checks
pass. Native save-panel interaction and recording with microphone/system-audio
grants remain interactive checks in the signed app. They were not simulated.

The final Rust run returned `CHECK OK`. The frozen UI install reported
`Lockfile is up to date, resolution step is skipped`. The signed bundle passed
`codesign --verify --deep --strict`, reporting `valid on disk` and
`satisfies its Designated Requirement`. The rebuilt app was launched from
`build/Ambient.app`; macOS reported PID 86628 and a visible 981 × 832 window.
The real sessions folder passed `doctor: 10/10 checks passed`.

The docs build returned `56 page(s) built` with Pagefind indexing, and the built
route check returned `check-routes: 56 pages, every internal link resolves`.
Astro was invoked directly with `node_modules/.bin` on PATH because pnpm's
automatic reinstall stopped on unapproved dependency installation scripts.
Those scripts were not enabled. Generated package-manager files were moved out
of the checkout; no docs dependency changes are included.

## Additional integration repairs

Capture warnings and retained-audio availability now reach the page through
session summaries. The page explains when audio cannot be diarized. Native
File commands reach the selected transcript, and Reveal with no selection
opens the sessions folder. Failed name, tag and pin writes display an error;
a rejected tag remains available to retry. Speaker naming reports failures and
reloads labels when a diarization job finishes.

The original window's pure regression assertions remain in `src/window_tests.rs`
with their historical fixtures compiled only for tests. Production code no
longer constructs the native session table, transcript or naming views.

## Defects repaired

- The API's session list ignored pin ordering, although the CLI's summary list
  already sorted pins first. Both now preserve newest-first order within pin groups.
- Notes were saved but omitted from summaries, making the editor reopen empty.
  Summaries now carry notes; failed note saves retain the draft and show an error.
- Speaker naming refreshed only the sidebar. It now reloads the displayed transcript.
- The latest-request guard dropped old successes but propagated old failures.
  A failed request for session A can no longer display an error over session B.
- Native naming and diarization bypassed the session lock used by the new API
  and CLI. Both handlers now acquire that lock around their writes.
- Metadata staging followed an existing `session.json.tmp` symlink. A regression
  test demonstrated an unrelated disposable file being overwritten. Updates now
  exclusively create a unique staging file before atomically replacing metadata.

## Initial review validation (before UI completion)

The final Rust verifier returned `CHECK OK`. The UI suite returned
`Test Files 14 passed (14)` and `Tests 55 passed (55)`. New regressions first
failed on the original implementation: saved notes, pin ordering, naming refresh,
stale request errors and the staging symlink overwrite.

The real quality gate returned
`QUALITY OK clean=0.0218 calls=0.1000 der=0.1434`. The doctor CLI produced a JSON
array of ten checks, all passing, against the installed models and disposable
sessions. The replay benchmark completed with
`blocks=11 audio_s=302.11 prep_s=0.921 decode_s=6.051 rtf=0.023 max_block_work_s=0.871 model_load_s=0.658`;
its two-track maximum lag was `1.742` seconds for both the original and repeated workload.
These are a new 30-second replay smoke check, not a rerun of all historical sweeps
or the three drain-overshoot experiments.

The built page's WKWebView probe returned `14 message(s) reached the bridge`,
including export, clipboard and settings requests. The native probe returned
`windowcheck: ok` against disposable sessions. These probes cover different
hosts: the page probe uses canned replies and cannot establish that the normal
window exposes the new page. Native recording and manual save-panel interaction
were not exercised.

UI type checking, lint and build succeeded; lint still emits the branch's
function-length warnings. Dependency policy returned
`advisories ok, bans ok, licenses ok, sources ok`. Typo and hook checks passed.
The documentation link check returned `54 file(s) scanned, 0 broken link(s)`.
Full hosted CI, release packaging and the docs-site build were not run.

The independent CLI review was not dispatched: `op whoami` still returned
`account is not signed in`, and the shared instructions require 1Password
readiness before dispatching long work. The findings above are this session's
source review and observed test results, not an independent review verdict.

Nightwatch's generated PR body covers only its final WER retry. Any eventual PR
must describe the entire branch, include this UI completion and record the
independent review outcome.
