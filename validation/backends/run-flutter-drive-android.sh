#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export PATH="$ANDROID_HOME/platform-tools:$PATH"
default_udid="$(adb devices | awk 'NR > 1 && $2 == "device" { print $1; exit }')"
ANDROID_UDID="${REPROIT_ANDROID_UDID:-$default_udid}"
adb_run() { adb -s "$ANDROID_UDID" "$@"; }

command -v adb >/dev/null || { echo "adb is required" >&2; exit 1; }
test -n "$ANDROID_UDID" || { echo "a booted Android emulator is required" >&2; exit 1; }
adb_run get-state | grep -q device
test "$(adb_run shell getprop sys.boot_completed | tr -d '\r')" = "1"

APP="$WORK/app"
flutter create --platforms=android --project-name reproit_flutter_fixture "$APP"
cp "$ROOT/examples/flutter-fixture/lib/main.dart" "$APP/lib/main.dart"
cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
(cd "$APP" && "$ROOT/target/debug/reproit" init --platform flutter --force --yes)
printf '{"budget":4}' > "$WORK/fuzz.json"

(cd "$APP" && flutter drive \
  --driver=test_driver/integration_driver.dart \
  --target=integration_test/journey_explore.dart \
  -d "$ANDROID_UDID" \
  --dart-define=REPROIT_FUZZ_CONFIG="$WORK/fuzz.json" \
  --dart-define=REPROIT_DEVICE=a) | tee "$WORK/run.log"

grep -q "EXPLORE:STATE " "$WORK/run.log"
grep -q "EXPLORE:EDGE " "$WORK/run.log"
grep -q "key:s:toggle" "$WORK/run.log"
grep -q "Detail revealed" "$WORK/run.log"
grep -q "JOURNEY DONE" "$WORK/run.log"
grep -q "All tests passed" "$WORK/run.log"
! grep -q "EXCEPTION CAUGHT BY REPROIT" "$WORK/run.log"

echo "FlutterDrive backend passed native Flutter/Android emulator runtime"
