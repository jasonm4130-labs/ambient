---
title: "Nightwatch review and completion audit"
---

# Nightwatch review and completion audit

The September 5 Nightwatch branch did not complete all six specifications.
This record identifies what a maintainer can use and what remains before the
planned UI is finished. The audit compares `e5e2de5` with `f6acc6d8` and the six
specifications in `~/.local/state/nightwatch/ambient/specs/`.

The review fixes defects in delivered code and verifies them with regression
tests and the existing runners. Implementing the missing UI tasks is separate
work; this review does not mark those tasks complete or merge the branch.

## Completion by specification

| Specification | Finding |
| --- | --- |
| 01: Session API | Dispatcher, CLI verbs, metadata, deletion, six export formats, config, roster and speaker methods delivered. This review repairs API pin ordering, exposes saved notes and makes metadata staging safe. |
| 02: Session tools | `doctor`, ten checks, JSON output and shared model paths delivered. Tasks 1–4 of the source plan were already on main; only Task 5 belonged to this run. |
| 03: UI shell | **Incomplete.** React components and bridge delivered, but source-plan Task 5 (native host replacement) and Task 6 (polish and documentation) remain. |
| 04: UI features | **Incomplete.** Search, editing, exports, live transcript polling and the doctor API delivered. Task 5 (filters and virtualised sidebar) and Task 6 (Welcome page) remain. The normal window still uses the native browser. |
| 05: WER experiments | Resampling, silence-gap, padding and chunk sweeps, calls fixtures and the separate calls gate delivered. Dated measurements retain the shipped defaults. |
| 06: Live-ASR measurements | Replay benchmark, queue arithmetic, drain readiness/counting and dated go/no-go measurements delivered. The spec explicitly excludes implementing live transcription. |

## Unfinished acceptance criteria

UI-shell acceptance 8 requires the page to be the window's content view and
`windowcheck` to query page selection. The actual probe still reports native
table rows and `settings pane: hidden`. `src/window.rs` still constructs the
native sidebar, transcript, naming strip and live pane. Its new phase events
do not replace those views.

UI-shell acceptance 5 requires empty-state and shortcut tests. The full planned
polish pass, light/dark screenshots and `docs/developing/ui.md` are outstanding.
Acceptance 9 also requires the host architecture and getting-started docs to be
updated. A passing link checker does not establish that those edits happened.

UI-features acceptance 5 requires the 2,000-session virtualisation test, fixed
56 px rows, month headers, text and tag filters, and Welcome checks whose fix
sentences match troubleshooting documentation. `SessionList` currently renders
every session with `sessions.map`; `App` routes only to Sessions and Settings.
The doctor API exists, but that does not deliver the Welcome page.

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

## Validation

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
must describe the entire branch and explicitly retain these unfinished tasks.
