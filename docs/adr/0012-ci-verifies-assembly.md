# 12. CI verifies assembly; behaviour is verified by dedicated binaries

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

No hosted runner has an audio device, and none can answer a TCC prompt. So the
three things this project exists to do — capture, transcription, diarization —
cannot be exercised in CI at all. What is left is compilation, and compiling
proves only that the code *says* the right thing. The failures that have
actually cost time here were all of the other kind: a tap that returns zeros
without erroring, a menu bar symbol that does not exist, a settings control
that renders but reaches nothing.

## Considered Options

- **A macOS runner doing everything**, and accept that the capture jobs are
  skipped or fake.
- **Split by what actually needs macOS**, verify assembly in CI, and push
  behaviour into binaries that ask the OS directly.

## Decision Outcome

CI verifies that the thing assembles, and dedicated binaries assert on what
macOS actually resolves and what actually reaches the WKWebView bridge.

`.github/workflows/ci.yml` has three jobs. `hygiene` and `ui` run on Linux
(`blacksmith-4vcpu-ubuntu-2404`, $0.004/min): typos and cargo-deny never
invoke rustc, and `ui/` is plain TypeScript — `pnpm check`, `lint`, `build`,
then `git diff --exit-code assets/settings.html` to catch a committed bundle
that no longer matches its source. `rust` runs on `macos-latest` ($0.062/min)
because process taps, `objc2-app-kit` and `objc2-web-kit` compile nowhere
else: `cargo fmt --check`, `clippy -D warnings`, `cargo test --all-targets`,
`cargo run --bin symbolcheck`, then `./make-app.sh` followed by
`codesign -v`, `plutil -lint` and a test that the bundle's executable exists.

`symbolcheck` is the pattern in miniature: it asks macOS whether each menu bar
icon resolves, because `imageWithSystemSymbolName` returning nothing leaves
the previous icon in place — the app would claim to be recording while merely
armed. It found `waveform.badge.questionmark` does not exist.
`cargo run --bin iconcheck -- <app> <out.png>` asks NSWorkspace what
LaunchServices actually resolves for the bundle, because a `CFBundleIconFile`
key and a file in Resources prove neither that the .icns parses nor that it is
picked up. `cargo run --release --bin uicheck -- out.png` drives the built
page in a real WKWebView, pushes a config in, synthesises every click and
prints what reaches the bridge.

## Consequences

A green CI run means the code compiles, is formatted, is clippy-clean, passes
its unit tests, has resolvable icons and produces a signed bundle. It does not
mean the app records anything. Capture, transcription and diarization stay on
the porting checklist as manual steps, deliberately.

Two details in the workflow are load-bearing and non-obvious: `rust-cache` is
told to keep `~/Library/Caches/ort.pyke.io`, because ort-sys downloads 80 MB
of ONNX Runtime there rather than to `OUT_DIR` under `target/`, and the
toolchain version is read out of `rust-toolchain.toml` by a shell step because
`dtolnay/rust-toolchain` requires it as an input and will not read the file.

## Confirmation

The workflow checks itself: `.github/workflows/ci.yml` fails on any of its own
steps, and `symbolcheck` is one of them, so a menu bar icon that stops
resolving turns the `rust` job red.

What CI cannot check is that this division of labour is still honest — that
`uicheck` and `iconcheck` are still run when the page or the icon changes.
Neither is in the workflow: `uicheck` needs a window server and `iconcheck`
needs the built bundle inspected by hand. A person must run
`cargo run --release --bin uicheck -- out.png` after changing `ui/`, and check
every control reaches the bridge with the right payload.
