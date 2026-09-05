# 0017. The window is one web page; Rust keeps the menu bar, the bridge and the state

**Status:** accepted · **Date:** 2026-09-05 · **Supersedes:** —

## Context and Problem Statement

The main window is native AppKit (`src/window.rs`, 2,600 lines of
NSTableView, NSTextView and NSLevelIndicator driven by `render(&Phase)`), and
only the Settings pane is a web page (`ui/`, vanilla TypeScript, one inlined
HTML file that `src/settings.rs` embeds). The window works and is plain. We
want it beautiful and fully featured: search across sessions, session
management, a live view, export in several formats. Every visual change to the
native window is objc2 code that the landing loop cannot see, and
`windowcheck` asserts state, not looks. Two UI stacks for one window is the
worst of both.

## Considered Options

- **One web page for the whole window.** Sessions, transcript, live view,
  naming, search and settings in a single WKWebView; the native window becomes
  a host. CSS is what the loop can land and test; `uicheck` already
  screenshots the page.
- **Polish the native window.** Most Mac-native, but layout and typography
  are Rust the loop lands blind, and there is no component library to lean on.
- **Hybrid.** Web transcript inside native chrome. Two stacks to keep in sync
  for ever.

## Decision Outcome

The window's content view is one WKWebView showing one page built from `ui/`
with React 19, shadcn/ui components copied into the repo, Tailwind v4 compiled
by Vite, lucide icons, and a Mac-native look (system font, HIG spacing,
system light/dark). The page is still a single inlined HTML file with no
origin, so nothing loads from the network at runtime. Rust keeps the menu bar,
the consent state machine, capture, the queue, and every file; the page holds
no state of its own beyond what is on screen. The bridge becomes
request/response plus events: the page posts `{id, method, params}`, Rust
answers `window.ambient.reply(id, …)`, and Rust pushes `window.ambient.event(name, …)`
whenever `Phase` or a meter changes. The methods are the same functions the
`ambient mcp` verb serves ([0016](0016-mcp-verb-for-live-reading.md)), in one
`src/api.rs`, so the window, the CLI and an assistant cannot disagree about
what a session is.

## Consequences

`src/window.rs` shrinks to hosting, and `windowcheck` becomes a check on the
host plus a script over the page through `uicheck`. The bundle grows from a
few KB to a few hundred, which a local WKWebView does not notice. The `ui`
job's drift check keeps `assets/settings.html` as the committed bundle name
(renaming it is a workflow edit, a daytime change). Actions that touch the
consent state (start, stop, decline) go through the same `Phase` transitions
the menu uses; the page can request them but never owns them.

## Confirmation

`pnpm test` under `ui/` (vitest + Testing Library) and the `ui` CI job's
`tsc`, `oxlint`, build and drift check; `cargo run --bin uicheck` drives the
built page in a real WKWebView and snapshots it; `cargo test api::` pins the
method contracts.
