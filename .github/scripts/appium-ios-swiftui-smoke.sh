#!/usr/bin/env bash
# iOS/Appium RUNTIME smoke for a REAL SwiftUI app. The sibling
# appium-ios-smoke.sh proves the XCUITest half of the Appium backend against a
# UIKit fixture; the site marquee claims ReproIt drives SwiftUI, so this smoke
# proves the SAME production runner (runners/rn/runner.mjs) drives a fixture
# written with the modern SwiftUI @main App lifecycle, @State, and declarative
# View tree (examples/ios-swiftui-fixture). It is deliberately identical to
# appium-ios-smoke.sh except for the fixture it builds and installs, so the ONLY
# variable under test is UIKit vs SwiftUI: same runner, same assertion pass,
# same crash-oracle contract.
#
# WHY A SECOND FIXTURE: UIKit and SwiftUI publish DIFFERENT accessibility trees
# for the "same" screen (SwiftUI synthesizes its element hierarchy from the View
# graph rather than a UIView tree), so a UIKit pass does not by itself prove the
# runner canonicalizes a SwiftUI tree into a non-empty structural elements list
# with resolvable key:<accessibilityIdentifier> selectors. This job proves it
# does: session, EXPLORE:STATE with a non-empty elements list, a real
# key:<id> tap that structurally changes the screen (EXPLORE:EDGE), clean finish.
#
# WHAT IT PROVES / DOES NOT PROVE: identical to appium-ios-smoke.sh (see its
# header); the only delta is the UI framework of the target app.
#
# The runner intentionally exits 0 even on an internal exception (the exception
# marker is the signal, parsed by the Rust side), so the assertion pass over the
# captured log below is what actually gates the job.
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
  echo "appium-ios-swiftui-smoke: Appium server never became ready at $APPIUM_URL" >&2
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
  echo "appium-ios-swiftui-smoke: no available iPhone simulator found" >&2
  xcrun simctl list devices available >&2
  exit 1
fi
echo "appium-ios-swiftui-smoke: using simulator $UDID"

# Build the fixture .app (swiftc, simulator SDK, ad-hoc signed; seconds).
BUILD_DIR="$(mktemp -d)"
APP_PATH="$(bash "$ROOT/examples/ios-swiftui-fixture/build.sh" "$BUILD_DIR")"
BUNDLE_ID="com.reproit.swiftuifixture"
echo "appium-ios-swiftui-smoke: built fixture $APP_PATH"

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
  echo "appium-ios-swiftui-smoke: could not resolve iOS version for simulator $UDID" >&2
  exit 1
fi
echo "appium-ios-swiftui-smoke: simulator runtime iOS $IOS_VERSION"

# Pinned bundleId (see appium-ios-smoke.sh header): XCUITest launches the
# fixture and the crash oracle watches it stay foreground. The first session
# builds WebDriverAgent from source, which dominates the runtime; the generous
# wdaLaunchTimeout covers a cold CI runner, and the client-side connect timeout
# must outlive the whole WDA build + retries or webdriverio aborts POST /session.
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
