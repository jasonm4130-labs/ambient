---
title: "Settings and the UI"
sidebar:
  order: 27
---

# Settings and the UI

Ambient's settings live in one JSON file, reachable from a CLI verb and from a
menu bar window that renders a bundled web page. This chapter covers where that
file lives, how it is resolved, how the page is built, and why three separate
binaries exist purely to check that the assembled app is what it claims to be.

Every setting has code behind it, and nothing is stored that nothing reads. The
full key-by-key list is in [the config reference](../using/settings.md).

They live in `~/Library/Application Support/Ambient/config.json` — deliberately
not under the sessions folder, since that folder is itself a setting and a
config inside the thing it configures cannot be found before it is read.
Precedence runs **environment → CLI flag → config file → default**, so
`AMBIENT_HOME` and `--app` still win and every existing harness is unaffected.
A missing file is an ordinary first run; a corrupt one warns and falls back
rather than failing a recording.

`ambient config` prints the resolved settings and the input devices it can see;
`ambient config <key> <value>` sets one. That exists so every setting is
verifiable without a GUI.

A named input device that has gone away **warns and names the alternatives**
before falling back. Silent fallback is this project's recurring failure and
the one thing a settings layer must not reintroduce.

## The settings window

An `NSWindow` holding a `WKWebView`, rendering the same page the design canvas
draws. The page is embedded with `include_str!`: launched through
LaunchServices the working directory is `/`, and that is the only launch with
the audio-capture grant, so a relative path would break the one path that
matters.

The bridge carries a **JSON string** each way. `WKScriptMessage::body` otherwise
arrives as an `NSDictionary` that Rust would unpick a value at a time; a string
goes straight to serde. The page never edits its own state — it sends a patch,
Rust merges it onto the file and pushes the whole config back, so the file is
the single source of truth rather than the DOM.

## Building the page — `ui/`

TypeScript and [Effect](https://effect.website), bundled by Vite through
`vite-plugin-singlefile` into one self-contained `assets/settings.html`; a
`WKWebView` loaded via `loadHTMLString` has no origin to fetch siblings from,
so nothing may stay external. Linting and formatting are oxlint and oxfmt.

```
cd ui && pnpm install
pnpm run build     # → ../assets/settings.html
pnpm run check     # tsc --noEmit
pnpm run lint      # oxlint
```

The built file is committed, so `cargo build` never needs node. `make-app.sh`
refreshes it only when `ui/node_modules` is present.

Effect earns its place on two counts and is otherwise heavy for a settings
page: `Schema` decodes the payload Rust pushes in rather than trusting it, so a
field of the wrong shape leaves the page as it was instead of rendering a
control that lies about the setting behind it; and the bridge is a real
effectful boundary worth having a testable seam at.

`cargo run --bin symbolcheck` asks macOS whether each menu bar icon actually
resolves. `imageWithSystemSymbolName` returning nothing leaves the previous
icon in place, so a symbol this OS lacks would show "recording" while merely
armed — silent, and only catchable by asking. It runs in CI, and it caught
`waveform.badge.questionmark` not existing.

`cargo run --release --bin uicheck -- out.png` drives the built page in a real
`WKWebView`, pushes a config in, synthesises every click and prints what
reaches the bridge. Compiling and type-checking prove the page *says* the right
thing; only this proves it does anything.

## Why there are three checking binaries

Each of the three asserts something a compiler cannot: that an artefact the
running OS resolves at runtime is the one the code assumed. They differ in
which artefact.

**`src/bin/symbolcheck.rs`** asks `NSImage::imageWithSystemSymbolName` for each
of the four menu bar symbols — `waveform`, `waveform.badge.exclamationmark`,
`waveform.circle.fill`, `hourglass` — and exits non-zero if any is missing,
printing "`{missing} symbol(s) will silently leave the wrong icon showing`".
The silent failure it exists to catch is named in its own header: "`set_state`
only sets an image when `imageWithSystemSymbolName` returns one, so a symbol
this OS does not have leaves the previous icon in place — the menu bar would
then say 'recording' while armed, silently. That is this project's recurring
failure mode, so it gets a check rather than a hope." It caught
`waveform.badge.questionmark` not existing on this OS.

**`src/bin/iconcheck.rs`** takes a built `.app` and an output path, asks
`NSWorkspace::iconForFile` what icon macOS resolves for the bundle, prints the
size and every representation, and writes the result out as a PNG. Its header
states the gap precisely: "The plist key and the file being present prove
neither that the .icns parses nor that LaunchServices picks it up." A wrong or
unparsed icon is not an error anywhere — you simply get the generic one.

**`src/bin/uicheck.rs`** loads the built `assets/settings.html` into a real
`WKWebView`, registers a script message handler, pushes a config in through
`applyConfig`, then on a timer synthesises the clicks a user would make —
toggling diarize, dragging the sensitivity slider, removing an app chip,
toggling ask-before-recording, adding a person, naming a speaker — and prints
every message that reaches the bridge, snapshotting a PNG at the end. Its
header: "Compiling and type-checking prove the page *says* the right thing;
only this proves it does anything."

Two worked examples of the class of bug that only a running check finds.

The menu bar's refresh timer polls twice a second, and the first version
scheduled it the obvious way. `src/menubar.rs` now registers it in the common
run loop modes instead, with the reason in the code: "Added in the common modes
rather than scheduled: `scheduledTimer…` registers only for
NSDefaultRunLoopMode, and AppKit runs the loop in event-tracking mode for as
long as a menu is open. The elapsed time therefore froze exactly while you were
looking at it, and only moved when the menu was closed and reopened." Nothing
errors; the number is simply stale in the one moment anyone reads it.

The settings page's capture-mode selector has the same shape of problem.
`ui/src/main.ts` keeps a `scopeChoice` variable rather than deriving the mode
from the config, because "It cannot be inferred from the config, because
'selected apps, none picked yet' and 'everything' are both an empty `apps`
list. Inferring it meant choosing Selected apps did nothing visible and left
the + Add button hidden, so there was no way to pick a first app at all." A
type-check passes on that page; a click does not. `uicheck` step [8] is exactly
that trap — switch to Everything, switch back, and report whether the chip row
and the + Add button are actually there.

See [ADR-0012](../adr/0012-ci-verifies-assembly.md).
