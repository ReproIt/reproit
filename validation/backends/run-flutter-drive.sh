#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
FRESH_UDID=""

# This gate owns at most one extra device: the tier-3 fresh simulator. Delete
# it with the same verify-after-delete discipline as with-ios-simulator.sh so
# a leaked device fails the gate instead of accumulating on the runner. The
# wrapper-created device stays owned by the wrapper's own cleanup.
cleanup() {
  local command_status=$?
  local inventory
  local probe_status
  trap - EXIT
  if [[ -n "$FRESH_UDID" ]]; then
    xcrun simctl shutdown "$FRESH_UDID" >/dev/null 2>&1 || true
    xcrun simctl delete "$FRESH_UDID" >/dev/null 2>&1 || true
    if ! inventory="$(xcrun simctl list devices -j)"; then
      echo "FlutterDrive fresh simulator cleanup: could not verify $FRESH_UDID" >&2
      command_status=1
    elif python3 -c '
import json
import sys

target = sys.argv[1]
devices = json.load(sys.stdin).get("devices", {})
found = any(
    device.get("udid") == target
    for runtime_devices in devices.values()
    for device in runtime_devices
)
raise SystemExit(0 if found else 1)
' "$FRESH_UDID" <<<"$inventory"; then
      echo "FlutterDrive fresh simulator cleanup: device still exists $FRESH_UDID" >&2
      command_status=1
    else
      probe_status=$?
      if [[ "$probe_status" -ne 1 ]]; then
        echo "FlutterDrive fresh simulator cleanup: bad inventory for $FRESH_UDID" >&2
        command_status=1
      else
        echo "FlutterDrive fresh simulator cleanup: deleted $FRESH_UDID"
      fi
    fi
  fi
  rm -rf "$WORK"
  exit "$command_status"
}
trap cleanup EXIT

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
cp "$ROOT/fixtures/flutter-fixture/lib/main.dart" "$APP/lib/main.dart"
cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
(cd "$APP" && "$ROOT/target/debug/reproit" init --platform flutter --force --yes)
printf '{"budget":4}' > "$WORK/fuzz.json"

# The measured CI stall (3x on 2026-08-01): the app publishes its Dart VM
# service URI, then flutter drive never connects and the VM-service line is
# the LAST output before 300 silent seconds. flutter drive connecting is the
# next observable step after that line, so bound that specific gap tightly
# (exit 121, named) instead of paying the generic idle timeout, and spend the
# reclaimed time on retry tiers that change something plausibly causal.
# Assertion failures and other nonzero exits remain immediate failures.
#
# Measured again on 2026-08-02 (3 consecutive red runs): the tool ECHOES the
# VM-service URI and then hangs in the connect, so discovery is fine and the
# hang sits in the tool's VM-service/DDS attach. The retry tiers therefore
# disable DDS (a documented mitigation for exactly this attach hang); the
# per-attempt evidence records the dds flag so CI accumulates the A/B data.
# The retry tiers also run with a tighter idle bound: with the build already
# proven, the longest legitimate silent gap is app launch, not compilation.
IDLE_TIMEOUT_SECONDS="${REPROIT_FLUTTER_IDLE_TIMEOUT_SECONDS:-300}"
RETRY_IDLE_TIMEOUT_SECONDS="${REPROIT_FLUTTER_RETRY_IDLE_TIMEOUT_SECONDS:-150}"
VM_CONNECT_TIMEOUT_SECONDS="${REPROIT_FLUTTER_VM_CONNECT_TIMEOUT_SECONDS:-75}"

run_drive() {
  local build_argument="${1:-}"
  local dds_argument="${2:-}"
  local idle_timeout_seconds="${3:-$IDLE_TIMEOUT_SECONDS}"
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
  if [[ "$dds_argument" == "no-dds" ]]; then
    drive_command+=(--no-dds)
  fi
  (
    cd "$APP"
    python3 "$ROOT/validation/backends/run-output-contract.py" \
      --idle-timeout-seconds "$idle_timeout_seconds" \
      --stall-marker 'The Dart VM service is listening on' \
      --stall-timeout-seconds "$VM_CONNECT_TIMEOUT_SECONDS" \
      --stall-name 'vm-service connect' \
      --stall-diagnostic-command \
      "bash '$ROOT/validation/backends/sample-stalled-tools.sh'" \
      --success-marker 'JOURNEY DONE' \
      --success-marker 'All tests passed' \
      -- \
      "${drive_command[@]}"
  ) 2>&1 | tee -a "$WORK/run.log"
  return "${PIPESTATUS[0]}"
}

# 121 is the bounded VM-service connect stall; 124 is the generic idle
# timeout. Both are environmental stall shapes worth a retry; anything else
# is a real failure and stops the gate immediately.
is_stall() { [[ "$1" -eq 121 || "$1" -eq 124 ]]; }

outcome_for() {
  case "$1" in
    0) echo "passed" ;;
    121) echo "vm-service-connect-stall" ;;
    124) echo "output-idle-timeout" ;;
    *) echo "failed-exit-$1" ;;
  esac
}

ATTEMPT_EVIDENCE=""
record_attempt() {
  local dds="true"
  [[ "${3:-}" == "no-dds" ]] && dds="false"
  ATTEMPT_EVIDENCE="${ATTEMPT_EVIDENCE:+$ATTEMPT_EVIDENCE,}"
  ATTEMPT_EVIDENCE+="$(printf '{"tier":"%s","outcome":"%s","dds":%s}' "$1" "$2" "$dds")"
}

wait_booted() {
  local udid="$1"
  local _
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
' "$udid"; then
      return 0
    fi
    sleep 2
  done
  echo "simulator $udid did not report Booted within the bound; continuing" >&2
}

collect_stall_diagnostics() {
  # Runner alone is not enough: run 30747023317 stalled with the app never
  # launching, so the launch plumbing processes are part of the evidence.
  python3 "$ROOT/validation/backends/run-output-contract.py" \
    --idle-timeout-seconds 30 \
    -- \
    xcrun simctl spawn "$UDID" log show \
    --last 10m \
    --style compact \
    --predicate 'process == "Runner" OR process == "installd"
      OR process == "SpringBoard" OR process == "launchd_sim"' \
    2>&1 | tail -n 300 || true

  local app_plist="$APP/build/ios/iphonesimulator/Runner.app/Info.plist"
  local bundle_id
  if [[ -f "$app_plist" ]]; then
    bundle_id="$(plutil -extract CFBundleIdentifier raw "$app_plist" 2>/dev/null || true)"
    if [[ -n "$bundle_id" ]]; then
      xcrun simctl terminate "$UDID" "$bundle_id" >/dev/null 2>&1 || true
    fi
  fi
}

# Tier 2 reset: erase keeps the same UDID, so the outer cleanup and its
# deletion audit still apply. Measured on CI: retrying with --no-build alone
# rebuilt in 7 seconds and stalled the full window again, because the stall is
# device state (libCoreFSCache "Errors found! Invalidating cache" precedes it
# in the simulator log), not the build.
erase_reboot() {
  xcrun simctl shutdown "$UDID" > /dev/null 2>&1 || true
  xcrun simctl erase "$UDID" > /dev/null 2>&1 || true
  xcrun simctl boot "$UDID" > /dev/null 2>&1 || true
  wait_booted "$UDID"
}

# Tier 3 reset: erase reuses the same device record; a device whose backing
# state survives erase (the measured double-stall shape) needs a genuinely new
# device. Create one (new UDID), point the drive at it, and drop the fixture's
# build tree so the final attempt rebuilds from clean state.
fresh_simulator() {
  local runtime="${REPROIT_IOS_RUNTIME_ID:-}"
  local device_type="${REPROIT_IOS_DEVICE_TYPE_ID:-}"
  local spec
  if [[ -z "$runtime" || -z "$device_type" ]]; then
    spec="$(xcrun simctl list devices -j | python3 -c '
import json
import sys

devices_by_runtime = json.load(sys.stdin).get("devices", {})
for runtime, devices in devices_by_runtime.items():
    if "iOS" not in runtime:
        continue
    for device in devices:
        device_type = device.get("deviceTypeIdentifier", "")
        if device.get("isAvailable", True) and ".iPhone-" in device_type:
            print(runtime, device_type)
            raise SystemExit(0)
raise SystemExit("no available iPhone simulator runtime and device type")
')"
    runtime="${spec%% *}"
    device_type="${spec#* }"
  fi
  xcrun simctl shutdown "$UDID" > /dev/null 2>&1 || true
  FRESH_UDID="$(xcrun simctl create "Reproit-Gate-Fresh-$$" "$device_type" "$runtime")"
  echo "FlutterDrive fresh simulator: created $FRESH_UDID" \
    "runtime=$runtime deviceType=$device_type"
  xcrun simctl boot "$FRESH_UDID" > /dev/null 2>&1 || true
  wait_booted "$FRESH_UDID"
  UDID="$FRESH_UDID"
  rm -rf "$APP/build"
}

set +e
run_drive
drive_status=$?
set -e
succeeded_tier="initial"
record_attempt initial "$(outcome_for "$drive_status")"

if is_stall "$drive_status"; then
  echo "Flutter drive stalled before its output contract; collecting simulator logs"
  collect_stall_diagnostics
  echo "Erasing and rebooting the simulator before the retry"
  erase_reboot
  echo "Retrying the built Flutter application without DDS after the bounded stall"
  set +e
  run_drive no-build no-dds "$RETRY_IDLE_TIMEOUT_SECONDS"
  drive_status=$?
  set -e
  succeeded_tier="erase-reboot"
  record_attempt erase-reboot "$(outcome_for "$drive_status")" no-dds
fi

if is_stall "$drive_status"; then
  echo "Second stall on the same device; creating a fresh simulator for the final attempt"
  collect_stall_diagnostics
  fresh_simulator
  echo "Retrying with a rebuilt application, still without DDS, on fresh simulator $UDID"
  set +e
  run_drive "" no-dds
  drive_status=$?
  set -e
  succeeded_tier="fresh-simulator"
  record_attempt fresh-simulator "$(outcome_for "$drive_status")" no-dds
fi

if [[ "$drive_status" -eq 0 ]]; then
  succeeded_tier_json="\"$succeeded_tier\""
else
  succeeded_tier_json="null"
fi
printf 'FLUTTER_GATE_ATTEMPTS {"attempts":[%s],"succeededTier":%s}\n' \
  "$ATTEMPT_EVIDENCE" "$succeeded_tier_json"

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
