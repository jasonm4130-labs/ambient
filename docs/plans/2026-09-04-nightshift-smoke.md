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
