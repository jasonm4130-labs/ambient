#!/usr/bin/env bash
# Build, sign, notarize, staple and publish Ambient.app as a GitHub Release.
#
#   op run --env-file .env.op -- ./release.sh [--dry-run]
#
# --dry-run does everything except notarize and publish.
#
# Steps that look redundant and are not:
#   Hardened runtime   notarization refuses a bundle without it; verified here.
#   Notarization       Gatekeeper blocks a downloaded Developer ID app without it.
#   Stapling           without it the app launches only while Apple is reachable.
#   ditto -c -k        `zip` corrupts a signed bundle and the signature breaks.
set -euo pipefail
cd "$(dirname "$0")"

DRY_RUN=0
PUBLISH_ONLY=0
case "${1:-}" in
  --dry-run)      DRY_RUN=1 ;;
  --publish-only) PUBLISH_ONLY=1 ;;
  "")             ;;
  *) echo "usage: $0 [--dry-run | --publish-only]" >&2; exit 1 ;;
esac

VERSION=$(cargo metadata --format-version 1 --no-deps \
          | sed -n 's/.*"name":"ambient","version":"\([^"]*\)".*/\1/p')
[ -n "$VERSION" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }
TAG="v$VERSION"
APP="${AMBIENT_BUILD_DIR:-build}/Ambient.app"
ZIP="${AMBIENT_BUILD_DIR:-build}/Ambient-$VERSION.zip"

# Separate so --publish-only can reach it without rebuilding. Tags are signed,
# so a locked 1Password vault fails here and nowhere else.
publish() {
  echo "==> publishing $TAG"
  if ! git rev-parse "$TAG" >/dev/null 2>&1; then
    git tag -a "$TAG" -m "Ambient $VERSION" || {
      echo >&2
      echo "could not create the tag. If that says \"failed to fill whole buffer\"," >&2
      echo "the 1Password vault holding your git signing key is locked — unlock it" >&2
      echo "and re-run with: $0 --publish-only" >&2
      echo "The notarised bundle on disk is kept, so this costs no Apple round trip." >&2
      exit 1
    }
  fi
  git push origin "$TAG"
  gh release create "$TAG" "$ZIP" \
    --title "Ambient $VERSION" \
    --notes "Signed and notarized. Download, unzip, move to /Applications, open.

Models are bundled — no fetch-models.sh, no setup needed.

Ambient is MIT licensed. Model and third-party licenses and attribution are
included in Ambient.app/Contents/Resources/THIRD_PARTY_NOTICES.md and licenses/.

On first launch macOS asks for System Audio Recording and Microphone. Both are
required: the first is the meeting audio, the second is you."

  echo
  echo "done: $(gh release view "$TAG" --json url -q .url)"
}

echo "==> releasing $TAG"

# Checked up front: notarization takes minutes.
SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null \
          | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)
[ -n "$SIGN_ID" ] || {
  echo "no Developer ID Application identity in the keychain." >&2
  echo "A build signed with anything else is rejected on every machine that" >&2
  echo "downloads it. See ./setup-signing.sh --developer-id <cert.cer>." >&2
  exit 1
}
echo "    identity: $SIGN_ID"

# Only notarization needs credentials: --dry-run stops before it and
# --publish-only resumes after it.
if [ "$DRY_RUN" = 0 ] && [ "$PUBLISH_ONLY" = 0 ]; then
  #   AC_APPLE_ID + AC_APP_PASSWORD   an app-specific password from appleid.apple.com
  #   AC_API_KEY_* (+ issuer)         an App Store Connect API key
  if [ -n "${AC_APPLE_ID:-}" ] && [ -n "${AC_APP_PASSWORD:-}" ]; then
    AUTH_MODE=apple-id
  elif [ -n "${AC_API_KEY_ID:-}" ] && { [ -n "${AC_API_KEY_P8:-}" ] || [ -n "${AC_API_KEY_PATH:-}" ]; }; then
    AUTH_MODE=api-key
  else
    echo "no notarisation credentials. Run under: op run --env-file .env.op -- $0" >&2
    echo "and set EITHER AC_APPLE_ID + AC_APP_PASSWORD, OR AC_API_KEY_ID plus" >&2
    echo "AC_API_KEY_P8 (and AC_API_ISSUER_ID for a team key)." >&2
    exit 1
  fi
  echo "    notarising via: $AUTH_MODE"
fi

if [ "$DRY_RUN" = 0 ]; then
  command -v gh >/dev/null || { echo "gh CLI not found" >&2; exit 1; }
  gh auth status >/dev/null 2>&1 || { echo "gh is not authenticated — run: gh auth login" >&2; exit 1; }
fi

# Resume from the notarized bundle on disk: rebuilding discards the staple and
# costs another ~11-minute trip through Apple's queue. Verified, not assumed.
if [ "$PUBLISH_ONLY" = 1 ]; then
  echo "==> publish-only: reusing the notarised bundle on disk"
  ./scripts/check-bundle "$APP" --models
  bundle_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
  [ "$bundle_version" = "$VERSION" ] || { echo "bundle version does not match $TAG; rebuild first" >&2; exit 1; }
  codesign --verify --deep --strict --verbose=2 "$APP"
  xcrun stapler validate "$APP" >/dev/null 2>&1 || {
    echo "$APP is not stapled — it has not been notarised, run a full release" >&2
    exit 1
  }
  SPCTL=$(spctl -a -vvv -t install "$APP" 2>&1) || true
  case "$SPCTL" in
    *"source=Notarized Developer ID"*) echo "    Gatekeeper: accepted, notarised" ;;
    *) echo "Gatekeeper does not accept $APP:" >&2
       printf '%s\n' "$SPCTL" | sed 's/^/    /' >&2; exit 1 ;;
  esac
  # Always zip the bundle just validated: an old ZIP beside a newly notarized
  # app must never be mistaken for it.
  rm -f "$ZIP"
  ditto -c -k --keepParent "$APP" "$ZIP"
  publish
  exit 0
fi

# Not a per-PR check: every routine dependency bump would fail it.
./scripts/check-rust-notices
AMBIENT_BUNDLE_MODELS=1 AMBIENT_SIGN_ID="$SIGN_ID" ./make-app.sh
./scripts/check-bundle "$APP" --models

echo "==> verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP"

# Captured, not piped: under pipefail, `codesign | grep -q` can fail when grep
# exits early and codesign dies of SIGPIPE.
SIGINFO=$(codesign -dv --verbose=4 "$APP" 2>&1)
printf '%s\n' "$SIGINFO" | grep -E "^(Authority|TeamIdentifier)" | sed 's/^/    /'
case "$SIGINFO" in
  *"flags="*"runtime"*) echo "    hardened runtime: yes" ;;
  *) echo "bundle is not hardened — notarization would reject it" >&2
     printf '%s\n' "$SIGINFO" | sed 's/^/    /' >&2
     exit 1 ;;
esac

rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "    $ZIP ($(du -h "$ZIP" | cut -f1))"

if [ "$DRY_RUN" = 1 ]; then
  echo "==> dry run: stopping before notarization and publish"
  exit 0
fi

WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
NOTARY_AUTH=()
if [ "$AUTH_MODE" = "apple-id" ]; then
  NOTARY_AUTH=(--apple-id "$AC_APPLE_ID" --password "$AC_APP_PASSWORD"
               --team-id "${AC_TEAM_ID:-}")
  [ -n "${AC_TEAM_ID:-}" ] || { echo "AC_TEAM_ID is required with an Apple ID" >&2; exit 1; }
else
  if [ -n "${AC_API_KEY_P8:-}" ]; then
    # notarytool wants a file; write the env-held key out private, for this call only.
    KEYFILE="$WORK/key.p8"; (umask 077; printf '%s\n' "$AC_API_KEY_P8" > "$KEYFILE")
  else
    KEYFILE="$AC_API_KEY_PATH"
  fi
  NOTARY_AUTH=(--key "$KEYFILE" --key-id "$AC_API_KEY_ID")
  # Only a team key takes an issuer; passing one for an individual key is an error.
  [ -n "${AC_API_ISSUER_ID:-}" ] && NOTARY_AUTH+=(--issuer "$AC_API_ISSUER_ID")
fi

echo "==> submitting to Apple (this takes a few minutes)"
set +e
xcrun notarytool submit "$ZIP" "${NOTARY_AUTH[@]}" --wait --timeout 45m
NOTARY_STATUS=$?
set -e
if [ "$NOTARY_STATUS" -ne 0 ]; then
  echo >&2
  echo "notarization failed. Read the actual reason rather than guessing:" >&2
  echo "  xcrun notarytool history <same auth flags as above>" >&2
  echo "  xcrun notarytool log <submission-id> <same auth flags>" >&2
  exit 1
fi

# Rebuild the zip after stapling: the ticket attaches to the bundle, and the
# zip submitted above does not contain it.
echo "==> stapling"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> Gatekeeper assessment"
spctl -a -vvv -t install "$APP" 2>&1 | sed 's/^/    /'

publish
