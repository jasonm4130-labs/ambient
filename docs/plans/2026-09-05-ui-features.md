---
title: "Plan: the window, fully featured"
sidebar:
  order: 99
---

# Plan: the window, fully featured

**Goal:** search across sessions, session management, export and share, and a
live view of the current recording, in the page the ui-shell plan built.
**Architecture:** each feature is a component in `ui/src` over an `api`
method the ui-api plan already landed and tested; Rust changes are limited to
bridge-only actions (clipboard, reveal) and the live transcript state.
**Tech Stack:** React 19, shadcn/ui, Tailwind v4, vitest + Testing Library;
Rust objc2 for the clipboard.

## Global Constraints

- This plan lands after the ui-api and ui-shell plans.
- Every feature has a test with the `FakeBridge` and a `data-testid` on its entry control so
  `uicheck` can drive it.
- The page keeps no state the API does not have; after every mutation it re-requests.
- Look and shortcuts follow the ui-shell plan's constraints (system font, neutral theme, 8 pt
  grid, `prefers-color-scheme`).
- No `.github/workflows/`, `.claude/`, or `loop/` changes; no editing existing tests.

## Task 1: Search with a command palette

**Files:**
- Create: `ui/src/components/SearchPalette.tsx`, `ui/src/components/SearchPalette.test.tsx`
- Modify: `ui/src/App.tsx` (`⌘K` opens it; a search icon button in the sidebar header), `ui/src/components/Transcript.tsx` (accepts a `highlightIndex` prop and scrolls that line into view)

**Interfaces:**
- Consumes: `api` `search` (`query`, `limit`) returning `Hit[]`
  (`{session, index, track, start_ms, speaker, text}`), `sessions` for names.
- Produces: a shadcn `Command` dialog: typing runs `search` after 150 ms of quiet with `limit: 50`,
  results grouped by session (name, date) with the hit text and `mm:ss`; `Enter` selects the session
  and scrolls the transcript to `index`, highlighted for two seconds; an empty result shows
  "Nothing matches"; `Esc` closes.

- [ ] **Step 1:** write the failing test: typing "budget" calls `search` with `{query:"budget",limit:50}`
  once after the debounce (fake timers); two hits render grouped under two session names; `Enter` on
  the first calls `onOpen` with `(session, index)`.
- [ ] **Step 2:** run `pnpm test`, expect FAIL "cannot find module ./SearchPalette".
- [ ] **Step 3:** implement, including the `highlightIndex` scroll in `Transcript`.
- [ ] **Step 4:** run `scripts/check`, `pnpm test`, `pnpm check`, `pnpm lint`, `pnpm build`.
- [ ] **Step 5:** commit: `git add ui assets/settings.html && git commit -m "ui: search across sessions"`.

## Task 2: Session management

**Files:**
- Create: `ui/src/components/SessionHeader.tsx`, `ui/src/components/SessionHeader.test.tsx`; shadcn `dialog`, `alert-dialog`, `textarea`, `popover`
- Modify: `ui/src/pages/Sessions.tsx` (renders `SessionHeader` above `Transcript` for the selected session and reloads `sessions` after every header action), `ui/src/components/SessionList.tsx` (pinned group at the top, tag chips, context menu)

**Interfaces:**
- Consumes: `api` `session.update` (`session`, optional `name`, `notes`, `pinned`, `add_tag`,
  `remove_tag`; tags are never sent as a whole array, so a tag added by the CLI or MCP while the
  page was open survives), `session.delete` (`session`), `sessions` (with `tags`, `pinned`),
  bridge `reveal`.
- Produces: the transcript header shows the name as an inline-editable field (click or `Enter`
  to edit, `Esc` cancels), a pin toggle, tag chips with an add popover, a notes textarea that saves
  on blur, and a `…` menu with Reveal in Finder and Delete; Delete opens an `AlertDialog` naming
  the session and its duration, and calls `session.delete` only on confirm; a live session's header
  shows "Recording" and disables everything but Reveal.

- [ ] **Step 1:** write the failing tests: editing the name and pressing `Enter` calls
  `session.update {session, name}`; adding a tag calls `session.update {session, add_tag}` and the
  chip's × calls `{session, remove_tag}`; the header re-renders from the `sessions` reply, not from
  local state;
  Delete then Confirm calls `session.delete`; Delete then Cancel calls nothing; a `live` summary
  renders the disabled state.
- [ ] **Step 2:** run `pnpm test`, expect FAIL "cannot find module ./SessionHeader".
- [ ] **Step 3:** implement; `SessionList` renders pinned sessions first under a "Pinned" label and
  tag chips under the name.
- [ ] **Step 4:** run `scripts/check`, `pnpm test`, `pnpm check`, `pnpm lint`, `pnpm build`.
- [ ] **Step 5:** commit: `git add ui assets/settings.html && git commit -m "ui: rename, pin, tag, note and delete sessions"`.

## Task 3: Export and share

**Files:**
- Create: `ui/src/components/ExportMenu.tsx`, `ui/src/components/ExportMenu.test.tsx`
- Modify: `ui/src/components/SessionHeader.tsx` (renders `ExportMenu` in its toolbar for every non-live session; the ui-shell plan's Copy Markdown button in `Transcript` stays as it is, and the menu does not repeat it)
- Modify: `src/settings.rs` (bridge-only method `save {session, format}` via `NSSavePanel` writing
  `api` `export`'s output, reply `{result: {path: string|null}}`, null when cancelled; `clipboard.write`
  already exists from the ui-shell plan's Task 3)
- Modify: `docs/using/commands.md` (the `export` section mentions the window's menu)

**Interfaces:**
- Consumes: `api` `export` (`session`, `format` in `markdown|text|json|srt|vtt|assistant`).
- Produces: an Export button with a dropdown: Copy for an assistant, Save as… →
  submenu of Markdown / Plain text / JSON / SRT / VTT; Copy actions call `export` then
  `clipboard.write` and show a shadcn toast "Copied"; Save calls `save` and toasts the path; a
  rejected `export`, `clipboard.write` or `save` shows a destructive toast "Export failed: <message>"
  with the bridge's message, and the menu stays usable.

- [ ] **Step 1:** write the failing tests: "Copy for an assistant" calls `export {format:"assistant"}`
  then `clipboard.write` with the returned text; "Save as… → SRT" calls `save {session, format:"srt"}`
  with the selected session's id; a `{path: null}` reply shows no toast; the "Copied" toast appears;
  an `export` that rejects with "no such session" shows "Export failed: no such session".
- [ ] **Step 2:** run `pnpm test`, expect FAIL "cannot find module ./ExportMenu".
- [ ] **Step 3:** implement both sides; `uicheck` gains a step that opens the menu, clicks Copy for
  an assistant and asserts the bridge received `export {format: "assistant"}` then `clipboard.write`.
- [ ] **Step 4:** run `scripts/check`, `pnpm test`, `pnpm check`, `pnpm lint`, `pnpm build`;
  `node .github/scripts/check-links.mjs`, expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add ui assets/settings.html src/settings.rs src/bin/uicheck.rs docs/using/commands.md && git commit -m "ui: export and copy for an assistant"`.

## Task 4: The live view, as live as the transcript is

**Files:**
- Create: `ui/src/components/LiveTranscript.tsx`, `ui/src/components/LiveTranscript.test.tsx`
- Modify: `ui/src/pages/Sessions.tsx` (renders `<LiveTranscript key={session}>` instead of `Transcript` while the selected session's summary is `live` or `transcribing`; the `key` remounts it on every selection change, so the cursor restarts at 0 and the previous session's timer is cleared on unmount, and its polls go through `useLatest`, so a delayed reply for the previous session is dropped); `LivePane` and its test are untouched

**Interfaces:**
- Consumes: event `phase` (`live.id`, `live.elapsed_s`, levels, `status_line`), `api` `transcript`
  (`session`, `since`, `verbatim`) whose reply is `{session, state, next, lines}` with `state` in
  `live|transcribing|pending|done` and `next` the append-order cursor, as the MCP plan defines it and
  the ui-api plan's Task 1 restates; a missing `raw.jsonl` during `live` is `state: "live"` with no
  lines, not an error.
- Produces: selecting the live session shows the live card full-width with a transcript area
  beneath it; while `state` is `live` or `transcribing` the page polls `transcript` every 2 s with
  the last `next` as `since`, appends new lines with a fade-in, and shows "Listening…" (live) or
  "Transcribing…" (transcribing) when there are no lines yet; when `state` becomes `done` the view
  swaps to the normal transcript. Today `raw.jsonl` is written after capture ends, so during `live`
  the area shows only "Listening…" and the meters; the polling makes the page ready for the
  live-writer plan without changes.

- [ ] **Step 1:** write the failing test: with a `FakeBridge` whose `transcript` returns
  `{state:"transcribing", next:1, lines:[…one]}` then `{state:"transcribing", next:2, lines:[…one]}`
  then `{state:"done", next:2, lines:[]}`, fake timers advance 2 s twice and `LiveTranscript` shows
  two lines then calls `onDone`, and `Sessions` swaps to `Transcript`; `since` is `0`, `1`, `2` in
  that order; a first reply `{state:"live", next:0, lines:[]}` renders "Listening…"; selecting live
  session B after A had reached cursor 5 issues `transcript {session: B, since: 0}`, and A's reply
  released afterwards adds nothing to B's view.
- [ ] **Step 2:** run `pnpm test`, expect FAIL on the new assertions.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** run `scripts/check`, `pnpm test`, `pnpm check`, `pnpm lint`, `pnpm build`.
- [ ] **Step 5:** commit: `git add ui assets/settings.html && git commit -m "ui: live view polls the transcript cursor"`.

## Task 5: Filters and the sidebar at scale

**Files:**
- Create: `ui/src/components/SidebarFilters.tsx`, `ui/src/components/SidebarFilters.test.tsx`
- Modify: `ui/src/components/SessionList.tsx` (virtualised list, no library: every session row is a
  fixed 56 px, every month header a fixed 28 px, so the offset of any row is arithmetic; the name is one
  line, truncated with an ellipsis (`truncate`), and the tags render as a single non-wrapping line that truncates with an ellipsis and a "+N" chip, never a second line;
  the list renders a top spacer, the rows intersecting the viewport plus 20 of overscan, and a bottom
  spacer, recomputed on `scroll` and on `ResizeObserver`)

**Interfaces:**
- Consumes: `sessions` (`tags`, `started_at`, `name`, `duration_s`).
- Produces: a filter row above the list: a text filter over name and tags, a tag chip picker
  (chips from every tag seen), and a date group header per month; the list stays under 16 ms per
  frame at 2,000 sessions (measured in the test with a fake list and `performance.now`, asserting
  fewer than 300 rendered rows).

- [ ] **Step 1:** write the failing tests: 2,000 fake summaries with a 600 px viewport render fewer
  than 60 row elements; setting `scrollTop` to the bottom and firing `scroll` renders the last
  session and the top spacer's height equals the arithmetic offset; a session with six tags renders
  one row of 56 px, and so does one with a 300-character name in a 200 px wide list; typing "standup" leaves only the matching names; picking the tag "1:1" leaves
  only sessions with it; month headers appear once per month.
- [ ] **Step 2:** run `pnpm test`, expect FAIL "cannot find module ./SidebarFilters".
- [ ] **Step 3:** implement.
- [ ] **Step 4:** run `scripts/check`, `pnpm test`, `pnpm check`, `pnpm lint`, `pnpm build`.
- [ ] **Step 5:** commit: `git add ui assets/settings.html && git commit -m "ui: filters and a sidebar that scales"`.

## Task 6: Onboarding and health in the page

**Files:**
- Create: `ui/src/pages/Welcome.tsx`, `ui/src/pages/Welcome.test.tsx`
- Modify: `ui/src/App.tsx` (show Welcome when `doctor` reports any FAIL or there are no sessions), `src/api.rs` (method `doctor` returning the checks from `doctor::run`, session-tools plan Task 5), `docs/developing/api.md`

**Interfaces:**
- Consumes: `api` `doctor` → `[{name, ok, detail}]`, `sessions`.
- Produces: a welcome page listing the checks as a checklist with the detail under each failed
  one and the fix from `docs/using/troubleshooting.md` as a sentence (models: run `./fetch-models.sh`;
  permissions: open System Settings), and a "Start recording" call to action that calls
  `record.start`. No link out: the docs site is not deployed (`docs-site/astro.config.ts`,
  `docs/developing/docs.md`), so the fix text is carried inline and kept in step with
  `docs/using/troubleshooting.md` by a test that asserts each check name's fix sentence appears in
  that file.

- [ ] **Step 1:** write the failing tests: with two FAIL checks the page renders both details and no
  call to action; with all ok and no sessions it renders the call to action, and clicking it calls
  `record.start`; every fix sentence the page can show is a substring of
  `docs/using/troubleshooting.md` (read in the test with `fs`).
- [ ] **Step 2:** run `pnpm test`, expect FAIL "cannot find module ./pages/Welcome".
- [ ] **Step 3:** implement, including the `doctor` api method and its test in `src/api.rs`.
- [ ] **Step 4:** run `scripts/check`, `pnpm test`, `pnpm check`, `pnpm lint`, `pnpm build`;
  `node .github/scripts/check-links.mjs`, expect `0 broken link(s)`.
- [ ] **Step 5:** commit: `git add ui assets/settings.html src/api.rs docs/developing/api.md && git commit -m "ui: welcome page from doctor"`.

## Open Questions

