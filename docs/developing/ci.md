---
title: "What CI checks"
sidebar:
  order: 30
---

# What CI checks

`.github/workflows/ci.yml` runs on every push to `main` and every pull
request: a `changes` job decides which of four jobs the change needs, and a
`gate` job at the end passes only when every job succeeded or was skipped.
`gate` is the one check anything waits on — `merge-pr.sh` and the landing
loop's merge workflow both read it. It is worth being explicit about the
split, because the most important thing this project does is the one thing
CI cannot test.

## The jobs

| Job | Runner | Runs when | What it does |
| --- | --- | --- | --- |
| `hygiene` | `blacksmith-4vcpu-ubuntu-2404` | always | `typos`, and `cargo-deny check` |
| `ui` | `blacksmith-4vcpu-ubuntu-2404` | `ui/**` or the bundle changed | `tsc`, `oxlint`, `vite build`, and a drift check on the committed bundle |
| `rust` | `blacksmith-6vcpu-macos-latest` | code changed | `fmt`, `clippy -D warnings`, `test --all-targets`, `symbolcheck`, and `make-app.sh` |
| `docs` | `blacksmith-4vcpu-ubuntu-2404` | `docs/**`, `docs-site/**` or `README.md` changed | the site build and its six guards — see [docs](docs.md) |
| `gate` | `blacksmith-4vcpu-ubuntu-2404` | always | fails unless every job above succeeded or was skipped |

Only `rust` needs macOS. The crate does not compile anywhere else — process
taps, `objc2-app-kit` and `objc2-web-kit` have no other target — so the other
jobs were deliberately kept off it. `cargo-deny` resolves the dependency
graph from `Cargo.lock` without invoking rustc, so it reads the macOS-only
dependencies rather than building them, and runs on Linux.

`changes` is `dorny/paths-filter`, pinned by commit. Until 2026-09-04 the
docs build was its own workflow with a `paths:` filter, because filters are
per-workflow; the cost is that no single workflow's conclusion then covered a
change, and `merge-pr.sh` waited on a docs check that a code-only pull
request never registers. One workflow with a skip-aware `gate` keeps the
macOS saving and removes that failure.

## Before CI: the format gate runs at commit time

`cargo fmt --check` is the first step of the `rust` job, and it stayed red on
`main` for six pushes before anyone looked: pushes go straight to `main`, the
repo's plan has no branch protection, and nothing on the machine watches CI.
So the check that matters runs before the commit exists. `.githooks/pre-commit`
refuses a commit whose *staged* Rust files rustfmt would change — staged
content, not the working tree, so formatting after `git add` does not slip
through — and says which files and what to run. Only staged files are checked,
so drift elsewhere never blocks an unrelated commit. There is no bypass flag.

The global `core.hooksPath` would normally hide a repo's own hooks; the global
`pre-commit` dispatches to `.githooks/pre-commit` when it is executable, so
this works for every clone on a machine with that config and is a no-op
elsewhere. CI remains the check of record for anyone without the hook. The
same hook refuses a commit on `main`; the path a change takes instead is
[how changes reach main](branching.md).

Keeping jobs that do not need a Mac on Linux limits macOS usage. The Rust job
now uses Blacksmith's 6-vCPU Apple Silicon runner, which follows GitHub's latest
macOS image, with a 15-minute job timeout to bound runner usage. The previous
GitHub-hosted job measured 2m43s cold and 1m51s warm;
Blacksmith timing still needs a hosted run. Runner labels and billing details
are in [Blacksmith's runner reference](https://docs.blacksmith.sh/blacksmith-runners/overview).

## Before pushing: `scripts/check`

`scripts/check` runs the `rust` job's steps locally — `fmt --check`, `clippy -D
warnings`, `test --all-targets`, `symbolcheck` — and prints one `✓` line per
step and `CHECK OK` last. On the first failure it prints `ERROR <step>`, then
that step's full output, and exits 1. Quiet on success on purpose: the
unattended landing loop ([landing](landing.md)) runs it before every commit and
again afterwards, and reads only the last line. It is narrower than CI by
design; `typos`, `cargo-deny`, the `ui` job and `make-app.sh` still run only on
the pull request, which is the cheap form of a holdout suite.

`.claude/settings.json` allow-lists `scripts/check` and `merge-pr.sh` for
Claude Code and denies `gh pr merge` and force pushes. Auto mode's classifier
denied `./merge-pr.sh` once with no rule in place; an allow rule bypasses the
classifier, and a denial mid-run stops an unattended session dead.

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
which is otherwise invisible until someone opens the settings page and sees an
old one.

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
