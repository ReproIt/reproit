#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

UDID="${REPROIT_IOS_UDID:-}"
APPIUM_URL="${REPROIT_APPIUM_URL:-http://127.0.0.1:4723}"
test -n "$UDID" || { echo "REPROIT_IOS_UDID is required" >&2; exit 1; }
xcrun simctl list devices -j | python3 -c '
import json
import os
import sys

udid = os.environ["REPROIT_IOS_UDID"]
devices = json.load(sys.stdin).get("devices", {})
match = next((d for values in devices.values() for d in values if d.get("udid") == udid), None)
if not match or match.get("state") != "Booted":
    raise SystemExit(f"iOS simulator {udid} is not booted")
'
curl -fsS "$APPIUM_URL/status" >/dev/null

npx --yes @react-native-community/cli@15.1.3 init ReproitRnFixture \
  --version 0.76.9 --directory "$WORK/app" --skip-install --skip-git-init
cp "$ROOT/fixtures/react-native-fixture/App.tsx" "$WORK/app/App.tsx"
cp "$ROOT/fixtures/react-native-fixture/index.js" "$WORK/app/index.js"
npm install --prefix "$WORK/app" --no-audit --no-fund
npm ci --prefix "$ROOT/runners/rn" --no-audit --no-fund

export RCT_NEW_ARCH_ENABLED=0
(cd "$WORK/app/ios" && pod install)
xcodebuild \
  -quiet \
  -workspace "$WORK/app/ios/ReproitRnFixture.xcworkspace" \
  -scheme ReproitRnFixture \
  -configuration Release \
  -sdk iphonesimulator \
  -destination "platform=iOS Simulator,id=$UDID" \
  -derivedDataPath "$WORK/derived" \
  ARCHS=arm64 \
  ONLY_ACTIVE_ARCH=YES \
  COMPILER_INDEX_STORE_ENABLE=NO \
  CODE_SIGNING_ALLOWED=NO \
  build

APP="$WORK/derived/Build/Products/Release-iphonesimulator/ReproitRnFixture.app"
test -d "$APP" || { echo "React Native iOS application was not built" >&2; exit 1; }
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Info.plist")"
test -n "$BUNDLE_ID" || { echo "React Native iOS bundle id is empty" >&2; exit 1; }
xcrun simctl install "$UDID" "$APP"
xcrun simctl launch "$UDID" "$BUNDLE_ID"

IOS_VERSION="$(xcrun simctl list devices -j | python3 -c '
import json
import sys

udid = sys.argv[1]
for runtime, devices in json.load(sys.stdin).get("devices", {}).items():
    if any(device.get("udid") == udid for device in devices):
        print(runtime.rsplit("iOS-", 1)[-1].replace("-", "."))
        raise SystemExit(0)
raise SystemExit(f"could not resolve iOS runtime for simulator {udid}")
' "$UDID")"
WDA_PORT="$(python3 -c '
import socket

for port in range(18201, 18233):
    with socket.socket() as candidate:
        try:
            candidate.bind(("127.0.0.1", port))
        except OSError:
            continue
        print(port)
        raise SystemExit(0)
raise SystemExit("no free WebDriverAgent port in bounded range 18201-18232")
')"
echo "React Native iOS runtime: iOS $IOS_VERSION, WebDriverAgent port: $WDA_PORT"

printf '{"replay":["tap:key:toggle"],"budget":1}' > "$WORK/fuzz.json"
export REPROIT_APPIUM_URL="$APPIUM_URL"
export REPROIT_APPIUM_CONNECT_TIMEOUT_MS=1200000
export REPROIT_APPIUM_CAPS
printf -v REPROIT_APPIUM_CAPS '%s%s%s%s%s' \
  '{"platformName":"iOS","appium:automationName":"XCUITest",' \
  "\"appium:platformVersion\":\"$IOS_VERSION\"," \
  "\"appium:udid\":\"$UDID\",\"appium:bundleId\":\"$BUNDLE_ID\"," \
  "\"appium:noReset\":true,\"appium:newCommandTimeout\":600," \
  "\"appium:wdaLocalPort\":$WDA_PORT,\"appium:wdaLaunchTimeout\":300000," \
  "\"appium:derivedDataPath\":\"$WORK/wda-derived\"," \
  '"appium:useNewWDA":true,"appium:shouldUseSingletonTestManager":true,' \
  '"appium:wdaStartupRetries":2,"appium:wdaStartupRetryInterval":2000}'
export REPROIT_FUZZ_CONFIG="$WORK/fuzz.json"

node "$ROOT/runners/rn/runner.mjs" | tee "$WORK/run.log"

grep -q "^EXPLORE:STATE " "$WORK/run.log"
grep -q "^EXPLORE:EDGE " "$WORK/run.log"
grep -Eq "key:.*(toggle|Toggle)" "$WORK/run.log"
grep -q "Detail revealed" "$WORK/run.log"
grep -q "^JOURNEY DONE$" "$WORK/run.log"
grep -q "^All tests passed$" "$WORK/run.log"
if grep -q "EXCEPTION CAUGHT BY RN RUNNER" "$WORK/run.log"; then
  exit 1
fi

printf '{"replay":["tap:key:flicker-positive"],"budget":1}' > "$WORK/flicker-positive.json"
positive_flicker_captured=0
for attempt in 1 2; do
  positive_log="$WORK/flicker-positive-attempt-$attempt.log"
  REPROIT_FUZZ_CONFIG="$WORK/flicker-positive.json" REPROIT_FLICKER_PIXELS=1 \
    REPROIT_FLICKER_DIAGNOSTICS=1 \
    node "$ROOT/runners/rn/runner.mjs" | tee "$positive_log"
  if grep -q '^EXPLORE:FLICKER ' "$positive_log"; then
    positive_flicker_captured=1
    break
  fi
  if ! grep -q '"reason":"short-capture"' "$positive_log"; then
    echo "positive flicker did not produce a finding or an exact short-capture abstention" >&2
    exit 1
  fi
  echo "positive flicker capture attempt $attempt abstained with short-capture" >&2
done
if [[ "$positive_flicker_captured" != "1" ]]; then
  echo "positive flicker capture exhausted its bounded two-attempt budget" >&2
  exit 1
fi

printf '{"replay":["tap:key:flicker-fixed"],"budget":1}' > "$WORK/flicker-fixed.json"
REPROIT_FUZZ_CONFIG="$WORK/flicker-fixed.json" REPROIT_FLICKER_PIXELS=1 \
  REPROIT_FLICKER_DIAGNOSTICS=1 \
  node "$ROOT/runners/rn/runner.mjs" | tee "$WORK/flicker-fixed.log"
! grep -q '^EXPLORE:FLICKER ' "$WORK/flicker-fixed.log"

printf '{"replay":["tap:key:flicker-one-way"],"budget":1}' > "$WORK/flicker-one-way.json"
REPROIT_FUZZ_CONFIG="$WORK/flicker-one-way.json" REPROIT_FLICKER_PIXELS=1 \
  REPROIT_FLICKER_DIAGNOSTICS=1 \
  node "$ROOT/runners/rn/runner.mjs" | tee "$WORK/flicker-one-way.log"
! grep -q '^EXPLORE:FLICKER ' "$WORK/flicker-one-way.log"

echo "Appium backend passed native React Native iOS simulator runtime"
