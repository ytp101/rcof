#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
build_profile=debug
case "${1:-}" in
  --release) build_profile=release; cargo build --release --locked --offline ;;
  "") cargo build --locked --offline ;;
  *) printf 'Usage: sh scripts/package_macos.sh [--release]\n' >&2; exit 2 ;;
esac
mkdir -p target/RCOF.app/Contents/MacOS
cp "target/$build_profile/rcof" target/RCOF.app/Contents/MacOS/rcof
cat > target/RCOF.app/Contents/Info.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>rcof</string>
<key>CFBundleIdentifier</key><string>dev.rcof.local</string>
<key>CFBundleName</key><string>RCOF</string>
<key>CFBundleDisplayName</key><string>RCOF</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSCameraUsageDescription</key><string>Show your camera in a local RCOF test call.</string>
<key>NSMicrophoneUsageDescription</key><string>Send microphone audio to the other local RCOF instance.</string>
</dict></plist>
PLIST
printf 'Created %s/target/RCOF.app\n' "$PWD"
