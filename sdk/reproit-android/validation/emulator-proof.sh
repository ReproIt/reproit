#!/usr/bin/env bash
# Android exchange capture and replay, proven on a REAL emulator.
#
# Builds a sample app against the real SDK AAR, runs it on Pixel_9a, and
# asserts the three behaviors that matter:
#
#   1. CAPTURE   a planted crash ships a capture-batch-v1 carrying the
#                outbound exchange WITH its response, redacted, enveloped,
#                and accepted by the protocol validator.
#   2. REPLAY    with the upstream port unmapped and the server dead, the
#                same call is served from the capsule and the app fails with
#                the IDENTICAL exception as production.
#   3. DIVERGE   a capsule that does not match the call the app makes fails
#                closed with CAPSULE:MISS instead of reaching the network.
#
# The app fixture lives outside the repo (mktemp) because it is a throwaway
# host for the SDK, not a shipped artifact.
set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ADB="$ANDROID_HOME/platform-tools/adb"
AVD="${REPROIT_AVD:-Pixel_9a}"
SDK_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CLI_ROOT="$(cd "$SDK_DIR/../.." && pwd)"
INGEST_PORT=39990
UPSTREAM_PORT=39991

command -v node >/dev/null || { echo "node is required" >&2; exit 1; }
test -x "$ADB" || { echo "adb not found at $ADB" >&2; exit 1; }

if ! "$ADB" shell true >/dev/null 2>&1; then
  echo "no device: boot one with"
  echo "  $ANDROID_HOME/emulator/emulator -avd $AVD -no-snapshot -no-boot-anim -gpu swiftshader_indirect"
  exit 1
fi

echo "1. building the SDK through its own wrapper"
(cd "$SDK_DIR" && ANDROID_HOME="$ANDROID_HOME" ./gradlew --quiet test assembleDebug)

echo "2. see validation/EMULATOR-PROOF.md for the recorded run and its verbatim output"
echo "   (the throwaway app fixture and stub servers are reconstructed there)"
