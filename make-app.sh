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
  (cd ui && npm run --silent build >/dev/null) && echo "rebuilt assets/settings.html"
fi

cargo build --release
BIN=$(cargo metadata --format-version 1 --no-deps \
      | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])')/release/ambient

APP=build/Ambient.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ambient"

# Regenerate the icon from assets/icon.svg when Inkscape is around; otherwise
# use the committed .icns, so a machine without it still builds a bundle that
# has an icon.
if command -v inkscape >/dev/null 2>&1; then
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
  <key>CFBundleShortVersionString</key><string>0.0.1</string>
  <key>LSMinimumSystemVersion</key><string>14.4</string>
  <key>LSUIElement</key><true/>
  <key>NSAudioCaptureUsageDescription</key>
  <string>Ambient records meeting audio locally so it can be transcribed on this Mac.</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>Ambient records your side of the conversation locally.</string>
</dict>
</plist>
PLIST

# Signing identity is load-bearing, not cosmetic. An ad-hoc signature's
# designated requirement IS the binary's cdhash, so every rebuild produces a new
# identity and orphans the TCC grant — leaving the row in place still reading
# "allowed" while the system-audio tap silently returns zeros. A stable identity
# keeps the requirement as identifier + certificate leaf, which survives rebuilds.
SIGN_ID="${AMBIENT_SIGN_ID:-Ambient Dev}"
if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$SIGN_ID"; then
  codesign --force --sign "$SIGN_ID" --identifier uk.ambient.cli "$APP" >/dev/null 2>&1
  echo "built $APP (signed: $SIGN_ID)"
else
  codesign --force --sign - --identifier uk.ambient.cli "$APP" >/dev/null 2>&1
  echo "built $APP (AD-HOC signed)"
  echo
  echo "  WARNING: no '$SIGN_ID' code-signing identity found, so this is ad-hoc signed."
  echo "  System-audio capture will silently break on every rebuild. Run ./setup-signing.sh"
  echo "  once to fix that permanently."
fi
echo "run: open -a \"$PWD/$APP\"   (menu bar; add --args for the CLI)"
