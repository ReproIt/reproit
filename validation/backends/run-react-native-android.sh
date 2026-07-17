#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export PATH="$ANDROID_HOME/platform-tools:$PATH"
APPIUM_URL="${REPROIT_APPIUM_URL:-http://127.0.0.1:4723}"
default_udid="$(adb devices | awk 'NR > 1 && $2 == "device" { print $1; exit }')"
ANDROID_UDID="${REPROIT_ANDROID_UDID:-$default_udid}"
adb_run() { adb -s "$ANDROID_UDID" "$@"; }

command -v adb >/dev/null || { echo 'adb is required' >&2; exit 1; }
test -n "$ANDROID_UDID" || { echo 'a booted Android device is required' >&2; exit 1; }
adb_run get-state | grep -q device || {
  echo "Android device $ANDROID_UDID is not ready" >&2
  exit 1
}
curl -fsS "$APPIUM_URL/status" >/dev/null || {
  echo "Appium is not ready at $APPIUM_URL" >&2
  exit 1
}

# Pin both the generator and framework. The generated Gradle project is the
# upstream React Native template, not a hand-written native surrogate.
npx --yes @react-native-community/cli@15.1.3 init ReproitRnFixture \
  --version 0.76.9 --directory "$WORK/app" --skip-install --skip-git-init
cp "$ROOT/examples/react-native-fixture/App.tsx" "$WORK/app/App.tsx"
cp "$ROOT/examples/react-native-fixture/index.js" "$WORK/app/index.js"
sed -i.bak 's/^newArchEnabled=true$/newArchEnabled=false/' "$WORK/app/android/gradle.properties"

npm install --prefix "$WORK/app" --no-audit --no-fund
(cd "$WORK/app/android" && ./gradlew --no-daemon \
  -PreactNativeArchitectures=arm64-v8a :app:assembleRelease)

APK="$WORK/app/android/app/build/outputs/apk/release/app-release.apk"
adb_run install -r "$APK" >/dev/null
adb_run shell am force-stop com.reproitrnfixture || true
adb_run shell am start -n com.reproitrnfixture/.MainActivity >/dev/null
sleep 3
adb_run wait-for-device
test "$(adb_run shell getprop sys.boot_completed | tr -d '\r')" = "1"

printf '{"budget":1}' > "$WORK/fuzz.json"
export REPROIT_APPIUM_URL="$APPIUM_URL"
REPROIT_APPIUM_CAPS='{"platformName":"Android","appium:automationName":"UiAutomator2",'
REPROIT_APPIUM_CAPS+="\"appium:udid\":\"$ANDROID_UDID\",\"appium:noReset\":true,"
REPROIT_APPIUM_CAPS+='"appium:newCommandTimeout":600,'
REPROIT_APPIUM_CAPS+='"appium:appPackage":"com.reproitrnfixture",'
REPROIT_APPIUM_CAPS+='"appium:appActivity":".MainActivity"}'
export REPROIT_APPIUM_CAPS
export REPROIT_FUZZ_CONFIG="$WORK/fuzz.json"

node "$ROOT/runners/rn/runner.mjs" | tee "$WORK/run.log"

grep -q '^EXPLORE:STATE ' "$WORK/run.log"
grep -q '^EXPLORE:EDGE ' "$WORK/run.log"
grep -Eq 'key:(toggle|com\.reproitrnfixture:id/toggle)' "$WORK/run.log"
grep -q 'Detail revealed' "$WORK/run.log"
grep -q '^JOURNEY DONE$' "$WORK/run.log"
grep -q '^All tests passed$' "$WORK/run.log"
! grep -q 'EXCEPTION CAUGHT BY RN RUNNER' "$WORK/run.log"
echo 'Appium backend passed native React Native Android runtime'
