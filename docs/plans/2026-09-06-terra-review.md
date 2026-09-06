---
title: "Terra review and CI runner follow-up"
---

# Terra review and CI runner follow-up

The user approved sending the branch to Codex/Terra on September 6. One
read-only review ran with `gpt-5.6-terra`, medium effort, over
`e5e2de5..a12695d`. The reviewer did not receive the author's completion audit
and did not execute the app or tests. It reported three actionable findings.

| Finding | Reproduction and repair |
| --- | --- |
| P1: one failed live-transcript poll stops updates permanently | A regression test failed after the second request rejected. Polling now retries with the unchanged cursor and clears the error after recovery. The test checks both lines, one completion callback and no polling after completion. |
| P2: the benchmark cache confuses inputs with the same filename | The actual benchmark reported `audio_s=1.00` for both one-second and two-second WAVs in different directories. Cache identity now includes the canonical path and file contents; the same runs report 1.00 and 2.00 seconds. Replacing the latter at the same path with three seconds reports 3.00. |
| P2: a refused deletion has no visible explanation | A regression test failed on a rejected `session.delete`. The dialog now displays the error, retains confirmation for retry and disables duplicate submissions while pending. The test retries successfully after the lock clears. |

The two new UI regressions failed before the repairs and the full suite now
reports `Test Files 18 passed (18)` and `Tests 67 passed (67)`. The benchmark's
cache-identity unit test also passes. Type checking, lint and bundle generation
succeeded; lint retains advisory warnings. These are locally verified repairs
to Terra's findings, not a second independent review of the fixes.

The full Rust verifier returned `CHECK OK`, and the signed app was rebuilt.
The native probe initially exposed its own reliance on foreground activation:
AppKit reported no key or main window and `File dispatch sent: false`. The
probe now establishes its main-window responder chain before dispatch. The
rerun reports `File dispatch sent: true` and
`windowcheck: ok — WKWebView, transcript, phase, selection, Stop, Settings, File menu`.
No production menu-routing change was needed.

## CI runner

The last failed GitHub macOS job had no steps. Its annotation says the job was
not started because recent account payments failed or the spending limit
needed increasing. See [the failed Rust job](https://github.com/jasonm4130-labs/ambient/actions/runs/33956752545/job/101281382947).
The successful neighbouring workflow skipped Rust, so its green result did
not prove the macOS build could run.

The user requested moving that job to Blacksmith. The workflow now selects
`blacksmith-6vcpu-macos-latest` and caps it at 15 minutes. This is a supported
Apple Silicon runner in [Blacksmith's reference](https://docs.blacksmith.sh/blacksmith-runners/overview).
The custom runner labels are declared for actionlint; validation reports
`actionlint: passed`. The user approved a hosted CI run and merging on September 6. The [listed macOS rate](https://www.blacksmith.sh/pricing) is $0.08/minute, so the Mac job is capped at
$1.20 per run before credits, plus the existing Linux jobs. The pull request records the hosted result; the local measurements above do
not establish release approval.
