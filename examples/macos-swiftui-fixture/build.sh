#!/usr/bin/env bash
# Build the ReproIt macOS SwiftUI smoke fixture (MacFixture.swift + Info.plist)
# into a proper .app bundle, with plain swiftc against the macosx SDK: no Xcode
# project, no scheme, no signing identity (ad-hoc). The desktop counterpart of
# examples/ios-swiftui-fixture/build.sh; -parse-as-library is needed so the
# @main App attribute is honoured.
#
# Usage: build.sh [output-dir]     (default: <this dir>/build)
# Emits: <output-dir>/ReproItMacSwiftUIFixture.app  (bundle id com.reproit.macswiftuifixture)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-$HERE/build}"
APP="$OUT/ReproItMacSwiftUIFixture.app"

ARCH="$(uname -m)"
SDK="$(xcrun --sdk macosx --show-sdk-path)"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
xcrun -sdk macosx swiftc \
  -sdk "$SDK" \
  -target "${ARCH}-apple-macos13.0" \
  -parse-as-library \
  -O \
  -o "$APP/Contents/MacOS/ReproItMacSwiftUIFixture" \
  "$HERE/MacFixture.swift"
cp "$HERE/Info.plist" "$APP/Contents/Info.plist"
# Ad-hoc sign (free, offline) so the bundle launches without a Gatekeeper prompt
# on the machine that built it.
codesign --force --sign - "$APP" > /dev/null 2>&1 || true

echo "$APP"
