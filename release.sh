#!/usr/bin/env bash
# Build, sign, notarize, staple and publish Ambient.app as a GitHub Release.
#
#   op run --env-file .env.op -- ./release.sh [--dry-run]
#
# Must run under `op run`: the App Store Connect key is a secret and does not
# live on disk in this repo. --dry-run does everything except notarize and
# publish, which is the fast way to check the bundle is shippable.
#
# Why each step is here, since several look redundant and are not:
#
#   Hardened runtime   Notarization refuses a bundle without it. make-app.sh
#                      applies it; this script verifies rather than assumes.
#   Notarization       Gatekeeper blocks a downloaded Developer ID app that has
#                      not been notarized. Signing alone is not enough.
#   Stapling           Without it the app only launches while the machine can
#                      reach Apple's notarization service. A stapled ticket
#                      travels with the bundle and works offline.
#   ditto -c -k        The zip must preserve symlinks and resource forks. `zip`
#                      corrupts a signed bundle and the signature stops
#                      verifying.
set -euo pipefail
cd "$(dirname "$0")"

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

VERSION=$(cargo metadata --format-version 1 --no-deps \
          | sed -n 's/.*"name":"ambient","version":"\([^"]*\)".*/\1/p')
[ -n "$VERSION" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }
TAG="v$VERSION"
APP=build/Ambient.app
ZIP="build/Ambient-$VERSION.zip"

echo "==> releasing $TAG"

# --- preflight --------------------------------------------------------------
# Checked up front: notarization takes minutes, and finding out afterwards that
# the identity was wrong wastes all of it.
SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null \
          | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)
[ -n "$SIGN_ID" ] || {
  echo "no Developer ID Application identity in the keychain." >&2
  echo "A build signed with anything else is rejected on every machine that" >&2
  echo "downloads it. See ./setup-signing.sh --developer-id <cert.cer>." >&2
  exit 1
}
echo "    identity: $SIGN_ID"

if [ "$DRY_RUN" = 0 ]; then
  for v in AC_API_KEY_ID AC_API_ISSUER_ID; do
    [ -n "${!v:-}" ] || { echo "$v is unset — run under: op run --env-file .env.op -- $0" >&2; exit 1; }
  done
  command -v gh >/dev/null || { echo "gh CLI not found" >&2; exit 1; }
  gh auth status >/dev/null 2>&1 || { echo "gh is not authenticated — run: gh auth login" >&2; exit 1; }
fi

# --- build ------------------------------------------------------------------
# The models are what make this a usable download rather than a binary that
# errors on first launch.
AMBIENT_BUNDLE_MODELS=1 AMBIENT_SIGN_ID="$SIGN_ID" ./make-app.sh

# --- verify the signature before spending minutes on notarization -----------
echo "==> verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -dv --verbose=4 "$APP" 2>&1 | grep -E "^(Authority|TeamIdentifier)" | sed 's/^/    /'
codesign -dv --verbose=4 "$APP" 2>&1 | grep -q "flags=.*runtime" || {
  echo "bundle is not hardened — notarization would reject it" >&2; exit 1
}
echo "    hardened runtime: yes"

rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "    $ZIP ($(du -h "$ZIP" | cut -f1))"

if [ "$DRY_RUN" = 1 ]; then
  echo "==> dry run: stopping before notarization and publish"
  exit 0
fi

# --- notarize ---------------------------------------------------------------
# notarytool wants the key as a file. It is held in an env var so it never
# touches this repo; write it to a private temp file for the call only.
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
if [ -n "${AC_API_KEY_P8:-}" ]; then
  KEYFILE="$WORK/key.p8"; umask 077; printf '%s\n' "$AC_API_KEY_P8" > "$KEYFILE"
elif [ -n "${AC_API_KEY_PATH:-}" ]; then
  KEYFILE="$AC_API_KEY_PATH"
else
  echo "neither AC_API_KEY_P8 nor AC_API_KEY_PATH is set" >&2; exit 1
fi

echo "==> submitting to Apple (this takes a few minutes)"
set +e
xcrun notarytool submit "$ZIP" \
  --key "$KEYFILE" --key-id "$AC_API_KEY_ID" --issuer "$AC_API_ISSUER_ID" \
  --wait --timeout 45m
NOTARY_STATUS=$?
set -e
if [ "$NOTARY_STATUS" -ne 0 ]; then
  echo >&2
  echo "notarization failed. Read the actual reason rather than guessing:" >&2
  echo "  xcrun notarytool history --key <key> --key-id $AC_API_KEY_ID --issuer $AC_API_ISSUER_ID" >&2
  echo "  xcrun notarytool log <submission-id> --key <key> --key-id ... --issuer ..." >&2
  exit 1
fi

# --- staple and re-zip ------------------------------------------------------
# Staple the .app, then rebuild the zip: the ticket is attached to the bundle,
# and the zip submitted above does not contain it.
echo "==> stapling"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

# The check that actually predicts the download experience.
echo "==> Gatekeeper assessment"
spctl -a -vvv -t install "$APP" 2>&1 | sed 's/^/    /'

# --- publish ----------------------------------------------------------------
echo "==> publishing $TAG"
git rev-parse "$TAG" >/dev/null 2>&1 || git tag -a "$TAG" -m "Ambient $VERSION"
git push origin "$TAG"
gh release create "$TAG" "$ZIP" \
  --title "Ambient $VERSION" \
  --notes "Signed and notarized. Download, unzip, move to /Applications, open.

Models are bundled — no fetch-models.sh, no setup needed.

On first launch macOS asks for System Audio Recording and Microphone. Both are
required: the first is the meeting audio, the second is you."

echo
echo "done: $(gh release view "$TAG" --json url -q .url)"
