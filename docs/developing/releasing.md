---
title: "Release preparation"
sidebar:
  order: 33
---

# Release preparation

A release is a signed, notarized app with its models and license notices. A green
build verifies only part of that result. Publishing and notarization are separate
maintainer actions; do not run them to test a documentation change.

## Check a local bundle

Run the checks in the
[contributor guide](https://github.com/jasonm4130-labs/ambient/blob/main/CONTRIBUTING.md),
then build into a separate directory to preserve any existing release candidate:

```sh
AMBIENT_BUILD_DIR=build/preview AMBIENT_BUNDLE_MODELS=1 ./make-app.sh
codesign --verify --deep --strict --verbose=2 build/preview/Ambient.app
plutil -lint build/preview/Ambient.app/Contents/Info.plist
```

`make-app.sh` invokes `scripts/build-release`, which remaps the repository and
builder's home paths in Rust compiler output. It checks the executable for the
builder's home path before packaging. The exact generic ONNX Runtime upstream
CI prefix `/Users/runner/work/ort-artifacts/ort-artifacts/` is exempt: it is
already embedded in the downloaded native archive, not an Ambient owner path. This applies to newly built executables;
it cannot remove paths from an already-published ZIP.

The committed icon is reused by default. Set `AMBIENT_REBUILD_ICON=1` only when
regenerating it from the SVG with Inkscape.

The bundle includes `LICENSE`, `THIRD_PARTY_NOTICES.md` and `licenses/` in
`Contents/Resources/`. Its model payload contains only the seven default runtime
files. Upstream test WAVs are omitted. An alternative ASR model needs its own
provenance and notices before the packaging script accepts it.

Check model integrity against the recorded inventory:

```sh
cd build/preview/Ambient.app/Contents/Resources
shasum -a 256 -c licenses/model-sha256.txt
```

The hashes identify the reviewed files. A changed hash requires investigation;
do not refresh it merely to make the check pass.

## Prepare a version

1. Choose the version and update `Cargo.toml` and `Cargo.lock`.
2. Review model provenance, licenses and hashes.
3. Run local checks and the required hosted CI jobs.
4. Commit and review the exact release source.
5. Obtain authorization to notarize and publish that version.

Use 1Password references in `.env.op` for notarization credentials. Replace
reference paths with your own vault/item names; never replace them with secrets.
For local signing and certificates, read the comments in `setup-signing.sh`.

`release.sh --dry-run` still builds and signs a bundle with a Developer ID
identity. It stops before notarization and GitHub publication. The normal command
submits to Apple, staples the ticket, creates a tag and uploads a GitHub release.
`--publish-only` validates the existing notarized candidate, its version, notices
and model hashes, then recreates the ZIP from that exact app before uploading.
It never trusts an older ZIP beside the app.

## Before making the repository public

- Review the latest source, all advertised Git refs, PR descriptions, Actions
  logs/artifacts and every release asset that will remain available.
- Use read-only default workflow permissions. Require `gate`, prevent force
  pushes and require review through the branch ruleset available for the repo.
- Require approval for outside-contributor workflow runs and restrict access to
  privileged runners. Verify the policy after visibility changes.
- Enable private vulnerability reporting and secret scanning/protection where
  the repository's plan supports them.
- Replace or retire older release assets that lack notices or retain personal
  build paths. Rebuilding a new version does not repair an old asset.

Deleting source lines does not erase old commits, PR revisions or cached copies.
If a real credential is discovered, revoke it before planning a history rewrite.
Owner-directory redaction alone does not remove author names or commit emails.

## Verify the download

Download the published ZIP through a browser on a separate Mac. Confirm that
Gatekeeper accepts it, both requested audio permissions work, and a permitted
short recording produces both tracks and a useful transcript. Record the version,
architecture and macOS version with the result. A check on the signing machine
alone does not establish the downloader's experience.
