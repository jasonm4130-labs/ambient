#!/usr/bin/env bash
# Wrap the CLI in a .app bundle. Core Audio taps need TCC to attribute the
# request to a bundle with NSAudioCaptureUsageDescription — an unbundled
# binary is handed silence rather than an error or a prompt.
set -euo pipefail
cd "$(dirname "$0")"

# assets/settings.html is committed so `cargo build` never needs node; it is
# refreshed only when the toolchain is present.
if [ -d ui/node_modules ]; then
  (cd ui && pnpm run --silent build >/dev/null) && echo "rebuilt assets/settings.html"
fi

./scripts/build-release
# Read from cargo rather than assuming ./target: a global cargo config may
# redirect build.target-dir, and this must not depend on python3 being present.
BIN=$(cargo metadata --format-version 1 --no-deps \
      | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/ambient

APP="${AMBIENT_BUILD_DIR:-build}/Ambient.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ambient"

ENGINE=$(./scripts/build-engine)
cp "$ENGINE" "$APP/Contents/MacOS/ambient-engine"

./scripts/check-build-paths "$APP/Contents/MacOS/ambient"
./scripts/check-build-paths --helper "$APP/Contents/MacOS/ambient-engine"

# Licenses are part of the signed payload, even for a model-free dev bundle.
cp LICENSE THIRD_PARTY_NOTICES.md "$APP/Contents/Resources/"
ditto licenses "$APP/Contents/Resources/licenses"

# The committed icon keeps builds reproducible; regeneration is opt-in.
if [ "${AMBIENT_REBUILD_ICON:-0}" = "1" ]; then
  command -v inkscape >/dev/null 2>&1 || {
    echo "AMBIENT_REBUILD_ICON=1 requires Inkscape" >&2
    exit 1
  }
  ICONSET=$(mktemp -d)/Ambient.iconset
  mkdir -p "$ICONSET"
  for pair in "16 icon_16x16" "32 icon_16x16@2x" "32 icon_32x32" "64 icon_32x32@2x" \
              "128 icon_128x128" "256 icon_128x128@2x" "256 icon_256x256" \
              "512 icon_256x256@2x" "512 icon_512x512" "1024 icon_512x512@2x"; do
    sz="${pair%% *}"; nm="${pair#* }"
    inkscape assets/icon.svg -o "$ICONSET/$nm.png" -w "$sz" -h "$sz" 2>/dev/null
  done
  iconutil -c icns "$ICONSET" -o assets/Ambient.icns && echo "rebuilt assets/Ambient.icns"
fi
cp assets/Ambient.icns "$APP/Contents/Resources/Ambient.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>ambient</string>
  <key>CFBundleIconFile</key><string>Ambient</string>
  <key>CFBundleIdentifier</key><string>uk.ambient.cli</string>
  <key>CFBundleName</key><string>Ambient</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>__VERSION__</string>
  <key>CFBundleVersion</key><string>__VERSION__</string>
  <key>LSMinimumSystemVersion</key><string>14.4</string>
  <key>LSUIElement</key><true/>
  <key>NSAudioCaptureUsageDescription</key>
  <string>Ambient records meeting audio locally so it can be transcribed on this Mac.</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>Ambient records your side of the conversation locally.</string>
</dict>
</plist>
PLIST
# Substituted afterwards because the quoted heredoc expands nothing. Gatekeeper
# expects CFBundleVersion; both versions come from Cargo.toml.
VERSION=$(cargo metadata --format-version 1 --no-deps \
          | sed -n 's/.*"name":"ambient","version":"\([^"]*\)".*/\1/p')
[ -n "$VERSION" ] || VERSION=0.0.0
sed -i '' "s/__VERSION__/$VERSION/g" "$APP/Contents/Info.plist"

# Release bundles carry the models (~0.8 GB) so a downloaded .app needs no
# repo; dev builds do not. Must happen BEFORE signing: the signature covers
# Resources.
if [ "${AMBIENT_BUNDLE_MODELS:-0}" = "1" ]; then
  [ -d models ] || {
    echo "AMBIENT_BUNDLE_MODELS=1 but no models/ directory — run ./fetch-models.sh" >&2
    exit 1
  }
  # The checksum list is the bundle's inventory: exactly the files the models
  # load, nothing else that has accumulated in models/.
  echo "copying models into the bundle ..."
  while read -r _ m; do
    [ -e "$m" ] || { echo "  missing $m — run ./fetch-models.sh" >&2; exit 1; }
    mkdir -p "$APP/Contents/Resources/$(dirname "$m")"
    # ditto rather than cp -R: correct metadata for a bundle about to be signed.
    ditto "$m" "$APP/Contents/Resources/$m"
  done < <(grep -v '^#' licenses/model-sha256.txt)
  echo "  bundled $(du -sh "$APP/Contents/Resources/models" | cut -f1)"
  # An `if`, not `&&`: set -e does not stop a script at a failed `a && b`.
  if ! (cd "$APP/Contents/Resources" && shasum -a 256 -c --quiet licenses/model-sha256.txt); then
    echo "  a bundled model does not match licenses/model-sha256.txt" >&2
    exit 1
  fi
  echo "  model checksums: verified"
fi

# A stable identity is load-bearing: an ad-hoc signature's designated
# requirement is the cdhash, so each rebuild orphans the TCC grant and the
# system-audio tap silently returns zeros while the row still reads "allowed".
#
# Order: AMBIENT_SIGN_ID, then Developer ID Application (the only one
# Gatekeeper accepts after notarization), then the self-signed 'Ambient Dev'.
SIGN_ID="${AMBIENT_SIGN_ID:-}"
if [ -z "$SIGN_ID" ]; then
  SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null \
            | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)
fi
[ -n "$SIGN_ID" ] || SIGN_ID="Ambient Dev"

# The hardened runtime is required for notarization, and it is what makes the
# entitlements file necessary — without com.apple.security.device.audio-input
# the microphone is refused even with the TCC grant in place.
SIGN_ARGS=(--force --identifier uk.ambient.cli
           --options runtime --entitlements Ambient.entitlements)
# A secure timestamp is required for notarization but needs a network round trip
# and only works for a real Apple-issued certificate, so it is off for the
# self-signed fallback.
case "$SIGN_ID" in
  "Developer ID Application"*) TIMESTAMP=--timestamp ;;
  *)                           TIMESTAMP=--timestamp=none ;;
esac
SIGN_ARGS+=("$TIMESTAMP")

# Sign the helper first, with its own identifier and no entitlements: it never
# touches a microphone.
ENGINE_ARGS=(--force --identifier uk.ambient.engine --options runtime "$TIMESTAMP")
if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$SIGN_ID"; then
  codesign "${ENGINE_ARGS[@]}" --sign "$SIGN_ID" "$APP/Contents/MacOS/ambient-engine"
  codesign "${SIGN_ARGS[@]}" --sign "$SIGN_ID" "$APP"
  echo "built $APP (signed: $SIGN_ID)"
else
  codesign "${ENGINE_ARGS[@]}" --sign - "$APP/Contents/MacOS/ambient-engine"
  codesign "${SIGN_ARGS[@]}" --sign - "$APP"
  echo "built $APP (AD-HOC signed)"
  echo
  echo "  WARNING: no '$SIGN_ID' code-signing identity found, so this is ad-hoc signed."
  echo "  System-audio capture will silently break on every rebuild. Run ./setup-signing.sh"
  echo "  once to fix that permanently."
fi

# A running instance ignores new --args; quit it rather than suggest `open -n`.
echo "run: open -a \"$APP\"   (quit an existing instance before passing --args)"
