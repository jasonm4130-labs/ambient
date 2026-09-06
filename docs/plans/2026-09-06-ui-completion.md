---
title: "Complete the Nightwatch UI"
---

# Complete the Nightwatch UI

Finish UI-shell tasks 5–6 and UI-features tasks 5–6 on the review branch,
including their integration with the features already delivered. Completion
means the normal Ambient window hosts the React page and the specified
behaviours pass through the real macOS host, not only a fake bridge.

1. Add session filters, month headers and fixed-geometry virtualisation.
   Test 2,000 sessions, scrolling, keyboard selection, long names and tags.
2. Add Welcome from the doctor checks and empty-session state, with inline
   fixes checked against troubleshooting documentation. Keep Settings reachable
   so a user can repair a failing configuration and retry health checks.
3. Replace native session views with the existing WKWebView, preserving
   activation policy, native dialogs, recording controls and File menu actions.
   Keep existing regression assertions; retain obsolete pure test fixtures
   outside the production host if their implementation is removed.
4. Complete loading and empty states, focus rings, reduced motion, typography
   and shortcuts. Capture the real built page in light and dark appearances.
5. Run Rust, UI, bundle drift, native window and activation-policy checks.
   Review the full branch once, resolve confirmed findings, and update the
   completion audit and UI documentation with observed outputs.

No live-transcription engine is added: its Nightwatch specification requested
measurements only. No capture is started by automated verification. Test
sessions and native-host probes use disposable directories.

The user authorised completing these remaining tasks on September 6, 2026.
Builds, commits and the final local app relaunch are part of delivering the
clickable result. Publishing or merging is not required for this local work.

## Native production functions removed

`session`, `of`, `clock`, `verb`, `row_lines`, `capturing_line`, `stamp_of`, `meta_of`, `started_at`, `relative`, `mmss`, `audio_present`, `describe`, `attrs`, `run`, `row_text`, `live_row_text`, `transcript_text`, `number_of_rows`, `view_for_row`, `is_group_row`, `selection_did_change`, `toggle_mode`, `stop_live_recording`, `name_speaker_clicked`, `separate_voices_clicked`, `selected_dir`, `cell_view`, `repaint`, `poll_diarize`, `refresh_rows`, `reselect`, `refresh_pane`, `refresh_live`, `refresh_naming`, `refresh_separate`, `layout`.

The existing pure projection tests remain unchanged in `src/window_tests.rs`;
their historical fixtures are compiled only for tests.

## Result

All four implementation tasks are complete. `scripts/check` returned `CHECK OK`;
the UI suite returned `Tests 65 passed (65)`. The real host and activation-policy
probes passed. Both WebKit appearance runs measured at most 29 mounted session
rows and 9 ms per update across 2,000 sessions. Documentation and screenshots
are in [the UI chapter](../developing/ui.md).

The signed app is built and running from the primary checkout. The real doctor
checks passed 10/10. The full completion evidence and limits are in the
[audit](2026-09-06-nightwatch-review.md). Desktop-integrated 1Password sign-in
and a vault-access check subsequently passed. The user then approved the
independent Terra review. Its three findings were reproduced and repaired;
the expanded UI suite reports `Tests 67 passed (67)`. The [follow-up record](2026-09-06-terra-review.md)
also tracks the Blacksmith migration. The user subsequently approved hosted CI
and merging; the pull request records the hosted result.
