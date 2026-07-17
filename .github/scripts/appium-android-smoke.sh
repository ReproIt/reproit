#!/usr/bin/env bash
# Android/Appium RUNTIME smoke for the mobile backends. The rn-appium, android,
# and swift-ios platforms all route through the same production runner
# (runners/rn/runner.mjs, spawned by the Rust orchestrator's Appium backend);
# until this smoke existed their only CI coverage was signature parity, never a
# live session. This drives that exact runner against a REAL UiAutomator2
# session on a booted Android emulator, using the emulator's preinstalled
# Settings app as the target (no APK to build, present on every AOSP image),
# with a small action budget so the walk stays bounded.
#
# WHAT IT PROVES: the Appium backend's runner can (1) create a UiAutomator2
# session against a real device, (2) capture the page source and reduce it to a
# canonical structural state signature (EXPLORE:STATE), (3) resolve a structural
# selector and perform a real tap that provably changes app state
# (EXPLORE:EDGE), and (4) finish cleanly (JOURNEY DONE, session deleted).
# WHAT IT DOES NOT PROVE: the Rust orchestrator spawn path (the runner is
# invoked directly here), the React Native fiber probe (Settings is not an RN
# app; groundtruth falls back to the native a11y tree), or iOS/XCUITest.
#
# The runner intentionally exits 0 even on an internal exception (the exception
# marker is the signal, parsed by the Rust side), so the assertion pass over the
# captured log below is what actually gates the job.
#
# Needs: node, a running Appium server with the uiautomator2 driver, and a
# booted Android emulator/device visible to adb.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOG="${1:-$(mktemp)}"

APPIUM_URL="${REPROIT_APPIUM_URL:-http://127.0.0.1:4723}"
export REPROIT_APPIUM_URL="$APPIUM_URL"

# Wait for the Appium server to answer /status (it is started in the background
# by the CI step; give it a generous but bounded window).
for _ in $(seq 1 60); do
  if curl -sf "$APPIUM_URL/status" > /dev/null; then break; fi
  sleep 2
done
curl -sf "$APPIUM_URL/status" > /dev/null || {
  echo "appium-android-smoke: Appium server never became ready at $APPIUM_URL" >&2
  exit 1
}

# Deterministic device state. A freshly booted emulator often still has a
# transient "System UI isn't responding" ANR dialog in the foreground; if the
# runner drove that dialog instead of Settings, a tap could dismiss it and move
# the foreground off the target, which the crash oracle would (correctly, for
# what it saw) read as the app leaving the foreground. So before the session we
# suppress system error dialogs and launch Settings explicitly, so the walk
# starts from a known screen. `adb` comes from the android-emulator-runner PATH.
if command -v adb > /dev/null; then
  adb shell settings put global hide_error_dialogs 1 || true
  adb shell am force-stop com.android.settings || true
  adb shell input keyevent KEYCODE_HOME || true
  adb shell am start -n com.android.settings/.Settings || true
  sleep 3
fi

# Target the preinstalled AOSP Settings app: nothing to build or sideload. We
# deliberately DO NOT pin appium:appPackage; instead we launched Settings via
# adb above and let UiAutomator2 attach to the current foreground. Settings is a
# multi-package app (its search UI lives in a separate intelligence package), so
# pinning the package would make the runner's crash oracle read normal
# cross-package navigation as the app leaving the foreground. With no pinned
# package the crash oracle has no target and correctly stays silent, while the
# structural signature + tap + state-change path (what this smoke proves) is
# unaffected.
REPROIT_APPIUM_CAPS='{"platformName":"Android","appium:automationName":"UiAutomator2",'
REPROIT_APPIUM_CAPS+='"appium:noReset":true,"appium:newCommandTimeout":600,'
REPROIT_APPIUM_CAPS+='"appium:adbExecTimeout":120000}'
export REPROIT_APPIUM_CAPS

# A small map-mode budget keeps the walk to a handful of taps.
FUZZ="$(mktemp)"
trap 'rm -f "$FUZZ"' EXIT
printf '{"budget":6}' > "$FUZZ"
export REPROIT_FUZZ_CONFIG="$FUZZ"

node "$ROOT/runners/rn/runner.mjs" | tee "$LOG"

node "$ROOT/.github/scripts/appium-smoke-assert.mjs" "$LOG"
