---
title: "What CI checks"
sidebar:
  order: 30
---

# What CI checks

`.github/workflows/ci.yml` runs on pushes to `main` and pull requests. A path
filter selects the affected jobs. The final `gate` check passes only when every
required job succeeds or is legitimately skipped.

CI verifies code, generated assets and bundle assembly. It does not verify
microphone permissions or record live audio.

## Jobs and runners

| Job | Runs when | Checks |
| --- | --- | --- |
| `changes` | Always | Selects Rust, UI and docs work from changed paths |
| `hygiene` | Always | Existing hook tests, spelling and dependency licenses/advisories |
| `ui` | UI, generated bundle or workflow changes | TypeScript, lint, all UI tests, build and generated-bundle drift |
| `rust` | Rust, UI, packaging, license or workflow changes | Formatting, Clippy, all-target tests, SF Symbols and signed bundle assembly |
| `docs` | Docs, contributor/security guidance, licenses or workflow changes | Typecheck, static build, Markdown links, built routes, Mermaid and ADR navigation |
| `gate` | Always | Fails if a required job failed or was cancelled |

Maintainer branches use Blacksmith's Linux and Apple Silicon runners. Fork PRs
select standard GitHub-hosted `ubuntu-24.04` and `macos-15` runners. Only the Rust
job needs macOS. Job timeouts and per-ref concurrency limit wasted work.

The fork runner selection avoids ordinary fork builds consuming Blacksmith
capacity. It is not an authorization boundary: a contributor can propose a
workflow change. Maintainers must review outside-contributor workflow runs and
configure runner access at the organization level before opening contributions.
See GitHub's [runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Permissions and supply chain

The workflow starts with `contents: read`. `changes` additionally needs
`pull-requests: read` for the path filter. Checkout does not persist credentials
into Git configuration. Actions are pinned to commit SHAs, with version comments
for review; Dependabot proposes updates.

No job notarizes, publishes a release or deploys the documentation. Do not expose
signing keys, deployment tokens or privileged caches to untrusted PR code. Use
repository settings to require approval for outside contributors. Verify those
settings and require `gate` in a branch ruleset when changing repository visibility.

## Run checks locally

From the repository root:

```sh
scripts/check
```

This runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --all-targets` and `cargo run --bin symbolcheck`. It stops at the
first failure and prints `CHECK OK` on success. It does not run UI or docs checks;
the [contributor guide](https://github.com/jasonm4130-labs/ambient/blob/main/CONTRIBUTING.md)
lists those commands.

For workflow edits, run `actionlint`. For dependency changes, run the relevant
package audit as well as its build and tests. `cargo deny check` checks the Rust
graph against `deny.toml`; the allow-list is not a replacement for bundled
third-party license notices.

## Checks that need context

`assets/settings.html` is committed and embedded by Rust. The UI job rebuilds it
and fails if Git detects a difference, preventing a stale UI from shipping.

The Rust cache includes `~/Library/Caches/ort.pyke.io`. ONNX Runtime downloads
live outside Cargo's target directory, so a target-only cache misses them.

The docs checkout includes Git history so page timestamps reflect their source
commits. The source-link and built-route checks test different things: a Markdown
link can work on GitHub and still be broken after rendering. See
[building these docs](docs.md).

`symbolcheck` asks macOS to resolve the menu bar's SF Symbols. `make-app.sh`
assembles and signs a bundle, includes license notices, and refuses a binary that
still contains the builder's home path. CI checks the signature and `Info.plist`.
It does not download the model set or prove a recording will work.

## Manual release acceptance

A downloadable release still needs a fresh-machine check: download, open, grant
permissions, record a short permitted test, and verify both tracks and transcript.
Keep that evidence separate from CI. See [release preparation](releasing.md) and
[ADR-0012](../adr/0012-ci-verifies-assembly.md).
