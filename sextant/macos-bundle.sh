#!/usr/bin/env bash
# Wrap the sextant binary in a minimal .app bundle so macOS treats it
# as a proper app and (a) shows a mic-permission prompt the first time
# it tries to open the input device, and (b) lists "Sextant" under
# System Settings > Privacy & Security > Microphone, so permission can
# be toggled per-app rather than inheriting Terminal's permissions.
#
# Usage:
#   ./sextant/macos-bundle.sh [--release]
#   open target/Sextant.app
#
# Rebuild-and-launch loop:
#   ./sextant/macos-bundle.sh && open target/Sextant.app
set -euo pipefail

PROFILE="debug"
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    cargo build --release -p sextant
else
    cargo build -p sextant
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO_ROOT/target/$PROFILE/sextant"
APP_DIR="$REPO_ROOT/target/Sextant.app"

if [[ ! -x "$BIN" ]]; then
    echo "error: binary not found at $BIN" >&2
    exit 1
fi

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp "$BIN" "$APP_DIR/Contents/MacOS/sextant"

cat > "$APP_DIR/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>sextant</string>
    <key>CFBundleIdentifier</key>
    <string>dev.swiftraccoon.sextant</string>
    <key>CFBundleName</key>
    <string>Sextant</string>
    <key>CFBundleDisplayName</key>
    <string>Sextant</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>Sextant captures microphone audio to encode AMBE voice frames for D-STAR transmission.</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

echo "Built $APP_DIR"
echo
echo "First launch:"
echo "  open $APP_DIR"
echo
echo "macOS will prompt for mic access the first time the app opens the"
echo "default input device.  To re-check later:"
echo "  System Settings > Privacy & Security > Microphone"
