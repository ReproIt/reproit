#!/usr/bin/env bash
# iOS/Appium RUNTIME smoke for the mobile backends: the XCUITest counterpart of
# appium-android-smoke.sh. The rn-appium, android, and swift-ios platforms all
# route through the same production runner (runners/rn/runner.mjs, spawned by
# the Rust orchestrator's Appium backend); the Android smoke proved the
# UiAutomator2 half, this proves the XCUITest half against a REAL iPhone
# simulator, driving the purpose-built fixture app in
# examples/ios-smoke-fixture (one screen, accessibility-identified controls, a
# button that structurally toggles a labeled detail row). The fixture is
# compiled here with plain swiftc against the simulator SDK (no Xcode project,
# no signing; seconds, while the WebDriverAgent build dominates the job).
#
# WHY A FIXTURE AND NOT SETTINGS: the Android smoke drives the preinstalled
# Settings app, and the first iOS version of this smoke did too. But Settings'
# accessibility tree is iOS-version and boot-timing dependent: on a GitHub
# macos-15 runner the first XCUITest snapshot of Settings returned a VALID
# structural signature with an EMPTY elements list (no tappable rows
# surfaced), so zero taps were attempted and the smoke failed, while the same
# walk passed locally on iOS 18.5. A trivial app we build ourselves surfaces a
# deterministic tree on every runtime, which is exactly what a smoke target
# must do.
#
# WHAT IT PROVES: the Appium backend's runner can (1) create an XCUITest
# session against a booted simulator (including the WebDriverAgent build), (2)
# capture the iOS page source and reduce it to a canonical structural state
# signature (EXPLORE:STATE with a non-empty elements list), (3) resolve a
# structural key:<accessibilityIdentifier> selector and perform a real tap
# that provably changes app state (EXPLORE:EDGE), and (4) finish cleanly with
# the crash oracle ACTIVE and silent (JOURNEY DONE, "All tests passed",
# session deleted). WHAT IT DOES NOT PROVE: the Rust orchestrator spawn path
# (the runner is invoked directly here), the React Native fiber probe (the
# fixture is not an RN app; groundtruth falls back to the native a11y tree),
# the multi-actor conductor protocol (single actor here; scenario.test.mjs
# covers the wire contract), or Android/UiAutomator2 (the android smoke covers
# that).
#
# Unlike the Settings-based smokes we DO pin appium:bundleId: the fixture is a
# single-bundle app we install and control, it never legitimately leaves the
# foreground, and pinning it (a) has XCUITest launch the app itself, so the
# session deterministically starts on the fixture's screen, and (b) gives the
# runner's crash oracle a real target, so this smoke also exercises the
# queryAppState foreground check that Settings (multi-package / cross-app
# navigation) forced us to disable.
#
# The runner intentionally exits 0 even on an internal exception (the exception
# marker is the signal, parsed by the Rust side), so the assertion pass over
# the captured log below is what actually gates the job.
#
# Needs: node, Xcode with an iOS simulator runtime (any macos-14/15 GitHub
# runner has both), and a running Appium server with the xcuitest driver.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOG="${1:-$(mktemp)}"

APPIUM_URL="${REPROIT_APPIUM_URL:-http://127.0.0.1:4723}"
export REPROIT_APPIUM_URL="$APPIUM_URL"

# Wait for the Appium server to answer /status (started in the background by
# the CI step; give it a generous but bounded window).
for _ in $(seq 1 60); do
  if curl -sf "$APPIUM_URL/status" > /dev/null; then break; fi
  sleep 2
done
curl -sf "$APPIUM_URL/status" > /dev/null || {
  echo "appium-ios-smoke: Appium server never became ready at $APPIUM_URL" >&2
  exit 1
}

# Resolve the target simulator: an explicit REPROIT_IOS_SIM_UDID wins, else the
# first available iPhone simulator (every macOS runner image ships several).
UDID="${REPROIT_IOS_SIM_UDID:-}"
if [ -z "$UDID" ]; then
  UDID_PATTERN='[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-'
  UDID_PATTERN+='[0-9A-F]{4}-[0-9A-F]{12}'
  UDID="$(
    xcrun simctl list devices available | grep -E '^ +iPhone' |
      head -n 1 | grep -oE "$UDID_PATTERN"
  )"
fi
if [ -z "$UDID" ]; then
  echo "appium-ios-smoke: no available iPhone simulator found" >&2
  xcrun simctl list devices available >&2
  exit 1
fi
echo "appium-ios-smoke: using simulator $UDID"

# Build the fixture .app (swiftc, simulator SDK, ad-hoc signed; seconds).
BUILD_DIR="$(mktemp -d)"
APP_PATH="$(bash "$ROOT/examples/ios-smoke-fixture/build.sh" "$BUILD_DIR")"
BUNDLE_ID="com.reproit.smokefixture"
echo "appium-ios-smoke: built fixture $APP_PATH"

# Deterministic device state: boot the simulator (idempotent), wait for it, and
# install a fresh copy of the fixture. XCUITest launches it via the pinned
# bundleId when the session starts, so no simctl launch is needed.
xcrun simctl boot "$UDID" 2> /dev/null || true
xcrun simctl bootstatus "$UDID" -b
xcrun simctl terminate "$UDID" "$BUNDLE_ID" 2> /dev/null || true
xcrun simctl install "$UDID" "$APP_PATH"

# Pin the selected simulator's actual runtime in the W3C capabilities. Leaving
# platformVersion undefined makes recent XCUITest drivers probe both simulator
# and real-device paths during session negotiation and emits an invalid-version
# warning. The UDID is authoritative, so derive the version from the matching
# CoreSimulator runtime instead of guessing from the runner image.
IOS_VERSION="$(xcrun simctl list devices -j | python3 -c '
import json, sys
udid = sys.argv[1]
for runtime, devices in json.load(sys.stdin).get("devices", {}).items():
    if any(device.get("udid") == udid for device in devices):
        print(runtime.rsplit("iOS-", 1)[-1].replace("-", "."))
        break
' "$UDID")"
if [ -z "$IOS_VERSION" ]; then
  echo "appium-ios-smoke: could not resolve iOS version for simulator $UDID" >&2
  exit 1
fi
echo "appium-ios-smoke: simulator runtime iOS $IOS_VERSION"

# Pinned bundleId (see header): XCUITest launches the fixture and the crash
# oracle watches it stay foreground. The first session builds WebDriverAgent
# from source, which dominates the runtime; the generous wdaLaunchTimeout
# covers a cold CI runner, and the client-side connect timeout must outlive the
# whole WDA build + retries or webdriverio aborts POST /session at its 120s
# default (exactly what the first CI run did).
export REPROIT_APPIUM_CONNECT_TIMEOUT_MS=1200000
REPROIT_APPIUM_CAPS='{"platformName":"iOS","appium:automationName":"XCUITest",'
REPROIT_APPIUM_CAPS+="\"appium:platformVersion\":\"$IOS_VERSION\","
REPROIT_APPIUM_CAPS+="\"appium:udid\":\"$UDID\",\"appium:bundleId\":\"$BUNDLE_ID\","
REPROIT_APPIUM_CAPS+='"appium:noReset":true,"appium:newCommandTimeout":600,'
REPROIT_APPIUM_CAPS+='"appium:wdaLaunchTimeout":300000,"appium:wdaStartupRetries":2}'
export REPROIT_APPIUM_CAPS

# A small map-mode budget keeps the walk to a handful of taps.
FUZZ="$(mktemp)"
trap 'rm -f "$FUZZ"; rm -rf "$BUILD_DIR"' EXIT
printf '{"budget":6}' > "$FUZZ"
export REPROIT_FUZZ_CONFIG="$FUZZ"

node "$ROOT/runners/rn/runner.mjs" | tee "$LOG"

node "$ROOT/.github/scripts/appium-smoke-assert.mjs" "$LOG"
