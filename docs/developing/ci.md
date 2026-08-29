# What CI checks

`.github/workflows/ci.yml` runs three jobs on every push to `main` and every
pull request. It is worth being explicit about the split, because the most
important thing this project does is the one thing CI cannot test.

## The three jobs

| Job | Runner | What it does |
| --- | --- | --- |
| `hygiene` | `blacksmith-4vcpu-ubuntu-2404` | `typos`, and `cargo-deny check` |
| `ui` | `blacksmith-4vcpu-ubuntu-2404` | `tsc`, `oxlint`, `vite build`, and a drift check on the committed bundle |
| `rust` | `macos-latest` | `fmt`, `clippy -D warnings`, `test --all-targets`, `symbolcheck`, and `make-app.sh` |

Only `rust` needs macOS. The crate does not compile anywhere else — process
taps, `objc2-app-kit` and `objc2-web-kit` have no other target — so the other
two jobs were deliberately kept off it. `cargo-deny` resolves the dependency
graph from `Cargo.lock` without invoking rustc, so it reads the macOS-only
dependencies rather than building them, and runs on Linux.

That split is a cost decision. Blacksmith's Linux runner is $0.004/min against
`macos-latest` at $0.062/min, roughly fifteen times cheaper, and the two jobs
that do not need a Mac are most of the wall clock. The `rust` job measured
2m43s cold and 1m51s warm.

## The caching line that is not obvious

```yaml
- uses: Swatinem/rust-cache@v2.9.2
  with:
    cache-directories: ~/Library/Caches/ort.pyke.io
```

`ort-sys` downloads ONNX Runtime into its own cache directory rather than
`$OUT_DIR` under `target/`. Without that line every run re-downloads 80 MB from
`cdn.pyke.io`, warm cache included.

## The drift check

`assets/settings.html` is committed but generated, so `cargo build` never needs
node. `git diff --exit-code assets/settings.html` after `pnpm run build` catches
a committed bundle that no longer matches the TypeScript it was built from —
which is otherwise invisible until someone opens the settings window and sees an
old page.

## What CI deliberately does not do

**It never records anything.** No hosted runner has an audio device, and none
has a way to answer a TCC prompt, so capture, transcription and diarization
cannot be exercised on one. CI verifies that the bundle assembles, is signed,
has a valid `Info.plist` and contains an executable — not that it records.

That leaves a gap, and the gap is filled by binaries that assert on what macOS
actually does rather than on what the code says:

- `cargo run --bin symbolcheck` asks the OS whether each menu bar SF Symbol
  resolves, and runs in CI. `imageWithSystemSymbolName` returning nothing leaves
  the previous icon in place, so a symbol this OS lacks would show "recording"
  while merely armed. It caught `waveform.badge.questionmark` not existing.
- `cargo run --release --bin uicheck -- out.png` drives the built settings page
  in a real `WKWebView`, pushes a config in, synthesises every click and prints
  what reaches the bridge. It does not run in CI, because it needs a window
  server.

See [ADR-0012](../adr/0012-ci-verifies-assembly.md) for the reasoning, and
[settings and the UI](settings-and-ui.md) for what each checking
binary has actually caught.

## Keeping the pins fresh

Every action is pinned to an exact tag, which only stays safe if something
proposes the bumps. `.github/dependabot.yml` runs weekly against three
ecosystems: `github-actions`, `cargo`, and `npm` in `/ui`.

## Licences

`deny.toml` targets `aarch64-apple-darwin` with `all-features = true`, and its
allow-list was enumerated from `cargo metadata` rather than guessed. One entry
is worth knowing about: **CDLA-Permissive-2.0**, which arrives through
`ort-sys` → `ureq` → `webpki-root-certs`. A new dependency introducing an
unlisted licence fails `hygiene` rather than landing quietly.
