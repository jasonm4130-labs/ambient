#!/usr/bin/env bash
# Wrap the CLI in a .app bundle. Core Audio taps need TCC to attribute the
# request to a bundle with NSAudioCaptureUsageDescription — an unbundled
# binary is handed silence rather than an error or a prompt.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
BIN=$(cargo metadata --format-version 1 --no-deps \
      | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])')/release/ambient

APP=build/Ambient.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/ambient"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>ambient</string>
  <key>CFBundleIdentifier</key><string>uk.ambient.cli</string>
  <key>CFBundleName</key><string>Ambient</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.1</string>
  <key>LSMinimumSystemVersion</key><string>14.4</string>
  <key>NSAudioCaptureUsageDescription</key>
  <string>Ambient records meeting audio locally so it can be transcribed on this Mac.</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>Ambient records your side of the conversation locally.</string>
</dict>
</plist>
PLIST

codesign --force --sign - --identifier uk.ambient.cli "$APP" >/dev/null 2>&1
echo "built $APP"
echo "run: $APP/Contents/MacOS/ambient tap /tmp/tap.wav 10"
