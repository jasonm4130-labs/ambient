#!/usr/bin/env bash
# Wrap the CLI in a .app bundle. Core Audio taps need TCC to attribute the
# request to a bundle with NSAudioCaptureUsageDescription — an unbundled
# binary is handed silence rather than an error or a prompt.
set -euo pipefail
cd "$(dirname "$0")"

# The settings page is TypeScript built by Vite into assets/settings.html,
# which src/settings.rs embeds. The built file is committed so `cargo build`
# never needs node; this only refreshes it when the toolchain is present.
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

# Do not silently ship an owner path from a dependency or a stale Cargo cache.
./scripts/check-build-paths "$APP/Contents/MacOS/ambient"

# Licenses are part of the signed payload, even for a model-free dev bundle.
cp LICENSE THIRD_PARTY_NOTICES.md "$APP/Contents/Resources/"
ditto licenses "$APP/Contents/Resources/licenses"

# Use the committed icon for reproducible builds. Regeneration is an explicit
# authoring step, not a side effect of having Inkscape installed.
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
# Substituted after the fact because the heredoc is quoted — nothing expands
# inside it, which is what keeps the plist readable. CFBundleVersion did not
# exist before and Gatekeeper expects it; both now come from Cargo.toml rather
# than a literal that drifts the moment someone bumps one and not the other.
VERSION=$(cargo metadata --format-version 1 --no-deps \
          | sed -n 's/.*"name":"ambient","version":"\([^"]*\)".*/\1/p')
[ -n "$VERSION" ] || VERSION=0.0.0
sed -i '' "s/__VERSION__/$VERSION/g" "$APP/Contents/Info.plist"

# Models, when asked for. A release bundle carries them so a downloaded .app
# works with no repo anywhere on the machine; a dev build does not, because the
# copy is ~672 MB. Must happen BEFORE signing — the signature covers Resources.
#
# The `-d models` guard is load-bearing for CI: the rust job runs this script on
# a runner that has no models/ (it is gitignored and never fetched), so an
# unconditional copy would fail the build.
if [ "${AMBIENT_BUNDLE_MODELS:-0}" = "1" ]; then
  [ -d models ] || {
    echo "AMBIENT_BUNDLE_MODELS=1 but no models/ directory — run ./fetch-models.sh" >&2
    exit 1
  }
  # Explicitly enumerated, NOT a copy of models/. Only ~671 MB of that directory
  # is ever loaded, but it accumulates: a second ASR model, and any archive
  # fetched by hand rather than by fetch-models.sh (which does delete its own,
  # at fetch-models.sh:23 and :37). It measured 3.3 GB on the machine this was
  # written on. Copying the lot would multiply the download for no benefit.
  #
  # The ASR model must match session.rs's default, which is the int8 build.
  ASR_MODEL="${AMBIENT_ASR_MODEL:-sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8}"
  echo "copying models into the bundle ..."
  mkdir -p "$APP/Contents/Resources/models"
  case "$ASR_MODEL" in
    sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8) ;;
    *) echo "release notices cover only the default ASR model; update them before bundling $ASR_MODEL" >&2; exit 1 ;;
  esac
  # Copy only runtime files. Upstream test audio is neither needed by the app
  # nor covered by our model attribution, and must not enter a release ZIP.
  for m in "$ASR_MODEL/encoder.int8.onnx" "$ASR_MODEL/decoder.int8.onnx" \
           "$ASR_MODEL/joiner.int8.onnx" "$ASR_MODEL/tokens.txt" \
           pyannote-segmentation-3.0/model.onnx \
           wespeaker_en_voxceleb_resnet34_LM.onnx silero_vad.onnx; do
    [ -e "models/$m" ] || { echo "  missing models/$m — run ./fetch-models.sh" >&2; exit 1; }
    mkdir -p "$APP/Contents/Resources/models/$(dirname "$m")"
    # ditto rather than cp -R: correct metadata for a bundle about to be signed.
    ditto "models/$m" "$APP/Contents/Resources/models/$m"
  done
  echo "  bundled $(du -sh "$APP/Contents/Resources/models" | cut -f1)"
  (cd "$APP/Contents/Resources" && shasum -a 256 -c licenses/model-sha256.txt)
fi

# Signing identity is load-bearing, not cosmetic. An ad-hoc signature's
# designated requirement IS the binary's cdhash, so every rebuild produces a new
# identity and orphans the TCC grant — leaving the row in place still reading
# "allowed" while the system-audio tap silently returns zeros. A stable identity
# keeps the requirement as identifier + certificate leaf, which survives rebuilds.
#
# Preference order: an explicit AMBIENT_SIGN_ID, then a Developer ID Application
# certificate, then the local self-signed 'Ambient Dev'. Developer ID is what a
# downloaded build needs — it is the only one Gatekeeper accepts after
# notarization — but a contributor without an Apple account still gets a working
# local build from the fallback.
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
  "Developer ID Application"*) SIGN_ARGS+=(--timestamp) ;;
  *)                           SIGN_ARGS+=(--timestamp=none) ;;
esac

# stderr is NOT discarded. It used to be, which meant a signing failure aborted
# the build with a nonzero status and no explanation whatsoever — the single
# worst thing to be missing while debugging a notarization rejection.
if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$SIGN_ID"; then
  codesign "${SIGN_ARGS[@]}" --sign "$SIGN_ID" "$APP"
  echo "built $APP (signed: $SIGN_ID)"
else
  codesign "${SIGN_ARGS[@]}" --sign - "$APP"
  echo "built $APP (AD-HOC signed)"
  echo
  echo "  WARNING: no '$SIGN_ID' code-signing identity found, so this is ad-hoc signed."
  echo "  System-audio capture will silently break on every rebuild. Run ./setup-signing.sh"
  echo "  once to fix that permanently."
fi

# An existing app instance ignores new --args. Quit it first rather than
# suggesting a second recorder instance with `open -n`.
echo "run: open -a \"$APP\"   (quit an existing instance before passing --args)"
