#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

UDID="${REPROIT_IOS_UDID:-$(xcrun simctl list devices booted -j | python3 -c '
import json,sys
j=json.load(sys.stdin)
for runtime, devices in j.get("devices", {}).items():
    if "iOS" not in runtime: continue
    for d in devices:
        if d.get("state") == "Booted" and d.get("isAvailable", True):
            print(d["udid"]); raise SystemExit
')}"
if [[ -z "$UDID" ]]; then
  echo "FlutterDrive gate needs a booted iOS simulator (or REPROIT_IOS_UDID)" >&2
  exit 2
fi

APP="$WORK/app"
flutter create --platforms=ios --project-name reproit_flutter_fixture "$APP"
cp "$ROOT/examples/flutter-fixture/lib/main.dart" "$APP/lib/main.dart"
cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
(cd "$APP" && "$ROOT/target/debug/reproit" init --platform flutter --force --yes)
printf '{"budget":4}' > "$WORK/fuzz.json"

# Flutter's iOS launcher waits indefinitely when a launched app never publishes
# its VM-service URI. Bound output inactivity here so the outer release gate
# retains time to collect simulator diagnostics. Retry only that specific stall
# with the already-built app; assertion failures and other nonzero exits remain
# immediate failures.
run_drive() {
  local build_argument="${1:-}"
  local -a drive_command=(
    flutter drive
    --driver=test_driver/integration_driver.dart
    --target=integration_test/journey_explore.dart
    -d "$UDID"
    --dart-define=REPROIT_FUZZ_CONFIG="$WORK/fuzz.json"
    --dart-define=REPROIT_DEVICE=a
  )
  if [[ "$build_argument" == "no-build" ]]; then
    drive_command+=(--no-build)
  fi
  (
    cd "$APP"
    python3 "$ROOT/validation/backends/run-output-contract.py" \
      --idle-timeout-seconds 300 \
      --success-marker 'JOURNEY DONE' \
      --success-marker 'All tests passed' \
      -- \
      "${drive_command[@]}"
  ) 2>&1 | tee -a "$WORK/run.log"
  return "${PIPESTATUS[0]}"
}

set +e
run_drive
drive_status=$?
set -e

if [[ "$drive_status" -eq 124 ]]; then
  echo "Flutter drive stalled before its output contract; collecting simulator logs"
  python3 "$ROOT/validation/backends/run-output-contract.py" \
    --idle-timeout-seconds 30 \
    -- \
    xcrun simctl spawn "$UDID" log show \
    --last 10m \
    --style compact \
    --predicate 'process == "Runner"' 2>&1 | tail -n 200 || true

  app_plist="$APP/build/ios/iphonesimulator/Runner.app/Info.plist"
  if [[ -f "$app_plist" ]]; then
    bundle_id="$(plutil -extract CFBundleIdentifier raw "$app_plist" 2>/dev/null || true)"
    if [[ -n "$bundle_id" ]]; then
      xcrun simctl terminate "$UDID" "$bundle_id" >/dev/null 2>&1 || true
    fi
  fi

  # Reset the SIMULATOR before retrying, not just the build. Measured on CI:
  # the retry rebuilt in 7 seconds and then sat silent for the full 300 again,
  # because --no-build re-tests the variable that was never at fault while
  # holding the wedged device fixed. The stall is the launched app never
  # publishing its VM-service URI, and it is preceded by libCoreFSCache
  # "Errors found! Invalidating cache" in the simulator log, so the device
  # state is the thing to clear. Erase keeps the same UDID, so the outer
  # cleanup and its deletion audit still apply.
  echo "Erasing and rebooting the simulator before the retry"
  xcrun simctl shutdown "$UDID" > /dev/null 2>&1 || true
  xcrun simctl erase "$UDID" > /dev/null 2>&1 || true
  xcrun simctl boot "$UDID" > /dev/null 2>&1 || true
  # Bounded wait for the device to finish booting; an unbooted device would
  # fail the retry for a different reason and hide the one being fixed.
  booted=""
  for _ in $(seq 1 60); do
    if xcrun simctl list devices -j | python3 -c '
import json,sys
udid=sys.argv[1]
j=json.load(sys.stdin)
for devices in j.get("devices", {}).values():
    for d in devices:
        if d.get("udid") == udid and d.get("state") == "Booted":
            raise SystemExit(0)
raise SystemExit(1)
' "$UDID"; then
      booted="yes"
      break
    fi
    sleep 2
  done
  if [[ -z "$booted" ]]; then
    echo "simulator $UDID did not reboot within the bound; retrying anyway" >&2
  fi

  echo "Retrying the built Flutter application after the bounded VM-service stall"
  set +e
  run_drive no-build
  drive_status=$?
  set -e
fi

if [[ "$drive_status" -ne 0 ]]; then
  exit "$drive_status"
fi

grep -q 'EXPLORE:STATE ' "$WORK/run.log"
grep -q 'EXPLORE:EDGE ' "$WORK/run.log"
grep -q 'key:s:toggle' "$WORK/run.log"
grep -q 'Detail revealed' "$WORK/run.log"
grep -q 'JOURNEY DONE' "$WORK/run.log"
grep -q 'All tests passed' "$WORK/run.log"
! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$WORK/run.log"

echo "FlutterDrive backend passed native Flutter/iOS simulator runtime"
