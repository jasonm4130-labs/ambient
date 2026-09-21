# Contributing to Ambient

Start with an issue describing the behavior you want to change. For a bug,
include the Ambient version, macOS version, Mac architecture, reproduction steps
and expected result. Use synthetic examples; do not attach real recordings,
transcripts, credentials or unredacted local paths.

Ambient is a Rust macOS app with a React UI embedded in a WebKit window.
The [developer guide](docs/developing/index.md) explains the capture and session
architecture. The [decision records](docs/adr/README.md) preserve constraints that
are easy to miss, including signed-bundle launch and separate audio clocks.

## Set up

Use macOS with Xcode Command Line Tools and `rustup`. The repository pins Rust
in `rust-toolchain.toml`. UI changes use Node 24 and pnpm 11; docs use Node 24
and npm. Follow [Getting started](docs/using/getting-started.md) for models and
local signing. Tests that use synthetic fixtures do not require recording audio.

```sh
git switch -c my-change
scripts/check
```

`scripts/check` runs Rust formatting, Clippy, all-target tests and the macOS
SF Symbol check. Set `CARGO_TARGET_DIR` to an isolated directory if you use a
shared cache and suspect stale artifacts.

## Check the parts you change

For UI changes:

```sh
cd ui
pnpm install --frozen-lockfile
pnpm run check
pnpm run lint
pnpm run test
pnpm run build
```

Commit the updated `assets/settings.html` with UI source changes. Rust embeds
that generated file so users can build without Node.

For docs changes, from the repository root:

```sh
npm ci --prefix docs-site
npm run typecheck --prefix docs-site
npm run build --prefix docs-site
npm run lint:docs --prefix docs-site
npm ci --prefix .github/scripts
node .github/scripts/check-links.mjs
node .github/scripts/check-mermaid.mjs
node .github/scripts/check-routes.mjs
```

Edit canonical Markdown in `docs/`. Keep its relative links working on both
GitHub and the built site. [Building these docs](docs/developing/docs.md) explains
the renderer and link checks.

For packaging changes, run `node --test scripts/packaging.test.mjs`, then build
a local bundle and verify its signature, notices and model layout. Do not run
`release.sh` as a routine check: the normal path
notarizes and publishes. [Release preparation](docs/developing/releasing.md)
describes local checks and the separate publication step.

## Open a pull request

Explain the problem, the resulting behavior and the checks you ran. Include
screenshots with synthetic data for UI changes. Distinguish automated checks
from any live microphone or system-audio test; never record people to exercise CI.

All required jobs must pass the `gate` check before a merge commit. Fork PRs use
standard GitHub-hosted runners; maintainer branches use Blacksmith. Outside
contributor runs require the repository's configured approval policy. See
[CI](docs/developing/ci.md) and [branching](docs/developing/branching.md).

## Security and licensing

Follow [SECURITY.md](SECURITY.md) for sensitive reports. Contributions to Ambient
are under its [MIT license](LICENSE). External models and vendored assets keep
their own terms; update [third-party notices](THIRD_PARTY_NOTICES.md) when changing
them. Never commit real credentials, model binaries or capture data.
