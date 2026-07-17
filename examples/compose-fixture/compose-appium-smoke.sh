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
# Needs: node, a running Appium server with the uiautomator2 driver, the Android
# SDK, and a booted emulator or device. The gate builds and installs its fixture.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOG="${1:-$(mktemp)}"
ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
ADB="${ADB:-$ANDROID_HOME/platform-tools/adb}"
UDID="${REPROIT_ANDROID_UDID:-${ANDROID_SERIAL:-}}"

if [[ ! -x "$ADB" ]]; then
  ADB="$(command -v adb || true)"
fi
[[ -n "$ADB" && -x "$ADB" ]] || {
  echo "compose-appium-smoke: adb was not found" >&2
  exit 1
}

if [[ -z "$UDID" ]]; then
  UDID="$($ADB devices | awk 'NR > 1 && $2 == "device" { print $1; exit }')"
fi
[[ -n "$UDID" ]] || {
  echo "compose-appium-smoke: no booted Android device was found" >&2
  exit 1
}

adb_device() {
  "$ADB" -s "$UDID" "$@"
}

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

# Deterministic device state: build and install the fixture, suppress system
# error dialogs, and launch its Activity explicitly so the walk starts from a
# known screen. Pinning the package, Activity, and device prevents a false pass
# against an unrelated foreground app.
(
  cd "$ROOT/examples/compose-fixture"
  ANDROID_HOME="$ANDROID_HOME" ./gradlew --no-daemon :app:assembleDebug
)
adb_device install -r "$ROOT/examples/compose-fixture/app/build/outputs/apk/debug/app-debug.apk"
adb_device shell settings put global hide_error_dialogs 1 || true
adb_device shell setprop debug.reproit.capsule __reproit_none__ || true
adb_device shell am force-stop com.reproit.composefixture
adb_device shell input keyevent KEYCODE_HOME
adb_device shell am start -W -n com.reproit.composefixture/.MainActivity
sleep 3

REPROIT_APPIUM_CAPS='{"platformName":"Android","appium:automationName":"UiAutomator2",'
REPROIT_APPIUM_CAPS+="\"appium:udid\":\"$UDID\","
REPROIT_APPIUM_CAPS+='"appium:appPackage":"com.reproit.composefixture",'
REPROIT_APPIUM_CAPS+='"appium:appActivity":".MainActivity","appium:noReset":true,'
REPROIT_APPIUM_CAPS+='"appium:newCommandTimeout":600,"appium:adbExecTimeout":120000}'
export REPROIT_APPIUM_CAPS

FUZZ="$(mktemp)"
trap 'rm -f "$FUZZ"' EXIT
printf '{"budget":3}' > "$FUZZ"
export REPROIT_FUZZ_CONFIG="$FUZZ"

node "$ROOT/runners/rn/runner.mjs" | tee "$LOG"

node "$ROOT/.github/scripts/appium-smoke-assert.mjs" "$LOG"
