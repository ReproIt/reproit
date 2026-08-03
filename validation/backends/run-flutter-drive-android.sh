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
FLUTTER_CREATE_ARGS=(
  --platforms=android
  --project-name reproit_flutter_fixture
)
if [[ "${REPROIT_OFFLINE:-0}" == 1 ]]; then
  FLUTTER_CREATE_ARGS+=(--no-pub)
fi
flutter create "${FLUTTER_CREATE_ARGS[@]}" "$APP"
cp "$ROOT/fixtures/flutter-fixture/lib/main.dart" "$APP/lib/main.dart"
cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
(cd "$APP" && "$CARGO_TARGET_DIR/debug/reproit" init --platform flutter --force --yes)
if [[ "${REPROIT_OFFLINE:-0}" == 1 ]]; then
  (cd "$APP" && flutter pub get --offline)
fi
printf '{"budget":4}' > "$WORK/fuzz.json"

# Flutter 3.41 can fail to establish its API 36 log filter, then parse a stale
# Dart VM service announcement from the device-wide log buffer. The stale
# authentication token points the driver at the current app's port but can
# never authenticate. Clear the buffer immediately before launch so the only
# VM service announcement belongs to this fresh application.
adb_run logcat -c

FLUTTER_DRIVE_ARGS=(
  --driver=test_driver/integration_driver.dart
  --target=integration_test/journey_explore.dart
  -d "$ANDROID_UDID"
  --dart-define=REPROIT_FUZZ_CONFIG="$WORK/fuzz.json"
  --dart-define=REPROIT_DEVICE=a
)
if [[ "${REPROIT_OFFLINE:-0}" == 1 ]]; then
  FLUTTER_DRIVE_ARGS+=(--no-pub)
fi
(cd "$APP" && flutter drive "${FLUTTER_DRIVE_ARGS[@]}") | tee "$WORK/run.log"

grep -q "EXPLORE:STATE " "$WORK/run.log"
grep -q "EXPLORE:EDGE " "$WORK/run.log"
grep -q "key:s:toggle" "$WORK/run.log"
grep -q "Detail revealed" "$WORK/run.log"
grep -q "JOURNEY DONE" "$WORK/run.log"
grep -q "All tests passed" "$WORK/run.log"
if grep -q "EXCEPTION CAUGHT BY REPROIT" "$WORK/run.log"; then
  echo "FlutterDrive backend reported an explorer exception" >&2
  exit 1
fi

echo "FlutterDrive backend passed native Flutter/Android emulator runtime"
