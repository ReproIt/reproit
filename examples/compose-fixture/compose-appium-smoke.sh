#!/usr/bin/env bash
# Jetpack Compose (Android) RUNTIME smoke for the mobile Appium backend
# (runners/rn/runner.mjs, the same production runner the android / rn-appium /
# swift-ios platforms route through). Unlike appium-android-smoke.sh, which
# drives the preinstalled Settings app, this drives a REAL minimal Jetpack
# Compose app (examples/compose-fixture) so the Compose framework claimed on the
# marquee is actually exercised end to end.
#
# The fixture is a single Composable: a Button whose Modifier.testTag("toggle")
# maps (via testTagsAsResourceId) to an Android resource-id the runner reads as a
# stable `key:` selector, plus semantics contentDescriptions. Tapping the button
# conditionally emits an extra Text node, so the tap moves the app to a
# structurally different state.
#
# WHAT IT PROVES: the Appium runner can (1) create a UiAutomator2 session against
# the real Compose Activity, (2) reduce the Compose semantics tree to a canonical
# structural signature (EXPLORE:STATE, non-empty elements), (3) resolve a
# selector and perform a real tap that provably changes app state
# (EXPLORE:EDGE), and (4) finish cleanly with the crash oracle silent
# (JOURNEY DONE, "All tests passed") while the app stays in the foreground.
#
# Needs: node, a running Appium server with the uiautomator2 driver, a booted
# Android emulator/device with the fixture APK installed (build + install:
#   ./gradlew :app:assembleDebug   &&   adb install -r app/.../app-debug.apk ).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOG="${1:-$(mktemp)}"

APPIUM_URL="${REPROIT_APPIUM_URL:-http://127.0.0.1:4723}"
export REPROIT_APPIUM_URL="$APPIUM_URL"

for _ in $(seq 1 60); do
  if curl -sf "$APPIUM_URL/status" > /dev/null; then break; fi
  sleep 2
done
curl -sf "$APPIUM_URL/status" > /dev/null || {
  echo "compose-appium-smoke: Appium server never became ready at $APPIUM_URL" >&2
  exit 1
}

# Deterministic device state: suppress system error dialogs and launch the
# fixture Activity explicitly via adb, so the walk starts from a known screen.
# We deliberately DO NOT pin appium:appPackage (same reasoning as
# appium-android-smoke.sh): the explorer's frontier walk includes a BACK press,
# and pressing BACK from the Compose root Activity exits to the launcher (normal
# Android behavior). With a pinned package the crash oracle would (correctly, for
# what it saw) read that as the app leaving the foreground; with no pin it has no
# target and correctly stays silent, while the structural signature + key-tap +
# EXPLORE:EDGE path this smoke proves is unaffected.
if command -v adb > /dev/null; then
  adb shell settings put global hide_error_dialogs 1 || true
  adb shell setprop debug.reproit.capsule __reproit_none__ || true
  adb shell am force-stop com.reproit.composefixture || true
  adb shell input keyevent KEYCODE_HOME || true
  adb shell am start -n com.reproit.composefixture/.MainActivity || true
  sleep 3
fi

export REPROIT_APPIUM_CAPS='{"platformName":"Android","appium:automationName":"UiAutomator2","appium:noReset":true,"appium:newCommandTimeout":600,"appium:adbExecTimeout":120000}'

FUZZ="$(mktemp)"
trap 'rm -f "$FUZZ"' EXIT
printf '{"budget":3}' > "$FUZZ"
export REPROIT_FUZZ_CONFIG="$FUZZ"

node "$ROOT/runners/rn/runner.mjs" | tee "$LOG"

node "$ROOT/.github/scripts/appium-smoke-assert.mjs" "$LOG"
