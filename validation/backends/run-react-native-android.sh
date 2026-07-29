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
DEVICE_ABI="$(adb_run shell getprop ro.product.cpu.abi | tr -d '\r')"
case "$DEVICE_ABI" in
  arm64-v8a | x86_64) ;;
  *)
    echo "unsupported Android device ABI: $DEVICE_ABI" >&2
    exit 1
    ;;
esac
curl -fsS "$APPIUM_URL/status" >/dev/null || {
  echo "Appium is not ready at $APPIUM_URL" >&2
  exit 1
}

# Pin both the generator and framework. The generated Gradle project is the
# upstream React Native template, not a hand-written native surrogate.
if [[ -n "${REPROIT_RN_TEMPLATE_DIR:-}" ]]; then
  template_sha256_file="${REPROIT_RN_TEMPLATE_SHA256_FILE:-}"
  test -d "$REPROIT_RN_TEMPLATE_DIR"
  test -f "$template_sha256_file"
  expected_template_sha256="$(cat "$template_sha256_file")"
  actual_template_sha256="$(
    cd "$REPROIT_RN_TEMPLATE_DIR"
    tar --sort=name --mtime="@0" --owner=0 --group=0 --numeric-owner \
      -cf - . | sha256sum | awk '{print $1}'
  )"
  test "$actual_template_sha256" = "$expected_template_sha256"
  test "$(
    node -p "require('$REPROIT_RN_TEMPLATE_DIR/package.json').dependencies['react-native']"
  )" = "0.76.9"
  cp -a "$REPROIT_RN_TEMPLATE_DIR" "$WORK/app"
  echo "React Native template cache verified: $actual_template_sha256"
else
  npx --yes @react-native-community/cli@15.1.3 init ReproitRnFixture \
    --version 0.76.9 --directory "$WORK/app" --skip-install --skip-git-init
fi
cp "$ROOT/examples/react-native-fixture/App.tsx" "$WORK/app/App.tsx"
cp "$ROOT/examples/react-native-fixture/index.js" "$WORK/app/index.js"
sed -i.bak 's/^newArchEnabled=true$/newArchEnabled=false/' "$WORK/app/android/gradle.properties"

npm install --prefix "$WORK/app" --no-audit --no-fund
(cd "$WORK/app/android" && ./gradlew --no-daemon \
  -PreactNativeArchitectures="$DEVICE_ABI" :app:assembleRelease)

APK="$WORK/app/android/app/build/outputs/apk/release/app-release.apk"
adb_run install -r "$APK" >/dev/null
adb_run shell am force-stop com.reproitrnfixture || true
adb_run shell am start -n com.reproitrnfixture/.MainActivity >/dev/null
sleep 3
adb_run wait-for-device
test "$(adb_run shell getprop sys.boot_completed | tr -d '\r')" = "1"

printf '{"budget":1}' > "$WORK/fuzz.json"
export REPROIT_APPIUM_URL="$APPIUM_URL"
export REPROIT_APPIUM_CAPS
printf -v REPROIT_APPIUM_CAPS '%s%s%s%s%s' \
  '{"platformName":"Android","appium:automationName":"UiAutomator2",' \
  "\"appium:udid\":\"$ANDROID_UDID\",\"appium:noReset\":true," \
  '"appium:forceAppLaunch":true,' \
  '"appium:newCommandTimeout":600,"appium:appPackage":"com.reproitrnfixture",' \
  '"appium:appActivity":".MainActivity"}'
export REPROIT_FUZZ_CONFIG="$WORK/fuzz.json"

node "$ROOT/runners/rn/runner.mjs" | tee "$WORK/run.log"

grep -q '^EXPLORE:STATE ' "$WORK/run.log"
grep -q '^EXPLORE:EDGE ' "$WORK/run.log"
grep -Eq 'key:(toggle|com\.reproitrnfixture:id/toggle)' "$WORK/run.log"
grep -q 'Detail revealed' "$WORK/run.log"
grep -q '^JOURNEY DONE$' "$WORK/run.log"
grep -q '^All tests passed$' "$WORK/run.log"
if grep -q 'EXCEPTION CAUGHT BY RN RUNNER' "$WORK/run.log"; then
  exit 1
fi
echo 'Appium backend passed native React Native Android runtime'
