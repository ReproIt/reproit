#!/usr/bin/env bash
# Build the ReproIt iOS SwiftUI smoke fixture (Fixture.swift + Info.plist) into
# a simulator .app bundle, with plain swiftc against the iphonesimulator SDK: no
# Xcode project, no scheme, no signing identity (ad-hoc), so it is fast enough
# to run inside the CI smoke job itself. The SwiftUI counterpart of
# examples/ios-smoke-fixture/build.sh; the only difference is -parse-as-library,
# needed so the @main App attribute is honoured (a SwiftUI App has no top-level
# entry statement the way the UIKit fixture's main.swift does).
#
# Usage: build.sh [output-dir]     (default: <this dir>/build)
# Emits: <output-dir>/ReproItSwiftUIFixture.app  (bundle id com.reproit.swiftuifixture)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${1:-$HERE/build}"
APP="$OUT/ReproItSwiftUIFixture.app"

# Host arch == simulator arch (arm64 on Apple Silicon runners, x86_64 on Intel).
ARCH="$(uname -m)"
SDK="$(xcrun --sdk iphonesimulator --show-sdk-path)"

rm -rf "$APP"
mkdir -p "$APP"
xcrun -sdk iphonesimulator swiftc \
  -sdk "$SDK" \
  -target "${ARCH}-apple-ios16.0-simulator" \
  -parse-as-library \
  -O \
  -o "$APP/ReproItSwiftUIFixture" \
  "$ROOT"/sdk/reproit-ios/Sources/ReproIt/*.swift \
  "$HERE/Fixture.swift"
cp "$HERE/Info.plist" "$APP/Info.plist"
# Ad-hoc sign (free, offline); the simulator accepts ad-hoc signed bundles.
codesign --force --sign - "$APP" > /dev/null 2>&1 || true

echo "$APP"
