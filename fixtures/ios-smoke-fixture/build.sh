#!/usr/bin/env bash
# Build the ReproIt iOS smoke fixture (main.swift + Info.plist) into a
# simulator .app bundle, with plain swiftc against the iphonesimulator SDK: no
# Xcode project, no scheme, no signing identity (ad-hoc), so it is fast enough
# to run inside the CI smoke job itself.
#
# Usage: build.sh [output-dir]     (default: <this dir>/build)
# Emits: <output-dir>/ReproItSmokeFixture.app  (bundle id com.reproit.smokefixture)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-$HERE/build}"
APP="$OUT/ReproItSmokeFixture.app"

# Host arch == simulator arch (arm64 on Apple Silicon runners, x86_64 on Intel).
ARCH="$(uname -m)"
SDK="$(xcrun --sdk iphonesimulator --show-sdk-path)"

rm -rf "$APP"
mkdir -p "$APP"
xcrun -sdk iphonesimulator swiftc \
  -sdk "$SDK" \
  -target "${ARCH}-apple-ios16.0-simulator" \
  -O \
  -o "$APP/ReproItSmokeFixture" \
  "$HERE/main.swift"
cp "$HERE/Info.plist" "$APP/Info.plist"
# Ad-hoc sign (free, offline); the simulator accepts ad-hoc signed bundles.
codesign --force --sign - "$APP" > /dev/null 2>&1 || true

echo "$APP"
