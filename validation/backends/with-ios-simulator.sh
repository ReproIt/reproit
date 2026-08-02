#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: with-ios-simulator.sh COMMAND [ARG ...]" >&2
  exit 2
fi

DEVICE_SPEC="$(xcrun simctl list devices -j | python3 -c '
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
RUNTIME_ID="${DEVICE_SPEC%% *}"
DEVICE_TYPE_ID="${DEVICE_SPEC#* }"
DEVICE_NAME="Reproit-Gate-$$"
UDID="$(xcrun simctl create "$DEVICE_NAME" "$DEVICE_TYPE_ID" "$RUNTIME_ID")"

cleanup() {
  local command_status=$?
  local inventory
  local probe_status
  trap - EXIT

  xcrun simctl terminate "$UDID" \
    com.facebook.WebDriverAgentRunner.xctrunner >/dev/null 2>&1 || true
  xcrun simctl terminate "$UDID" \
    com.facebook.WebDriverAgentRunner.xctrunner.xctrunner >/dev/null 2>&1 || true
  xcrun simctl shutdown "$UDID" >/dev/null 2>&1 || true
  xcrun simctl delete "$UDID" >/dev/null 2>&1 || true

  if ! inventory="$(xcrun simctl list devices -j)"; then
    echo "iOS simulator cleanup: could not verify deletion of $UDID" >&2
    exit 1
  fi
  if python3 -c '
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
' "$UDID" <<<"$inventory"
  then
    echo "iOS simulator cleanup: device still exists $UDID" >&2
    exit 1
  else
    probe_status=$?
    if [[ "$probe_status" -ne 1 ]]; then
      echo "iOS simulator cleanup: could not parse deletion inventory for $UDID" >&2
      exit 1
    fi
  fi

  echo "iOS simulator cleanup: deleted $UDID"
  exit "$command_status"
}
trap cleanup EXIT

xcrun simctl boot "$UDID"
xcrun simctl bootstatus "$UDID" -b
export REPROIT_IOS_UDID="$UDID"
export REPROIT_IOS_SIM_UDID="$UDID"
# The wrapped gate may need to create its own replacement device (fresh-
# simulator retry tier); hand it the already-selected spec so it cannot pick
# a different runtime mid-gate.
export REPROIT_IOS_RUNTIME_ID="$RUNTIME_ID"
export REPROIT_IOS_DEVICE_TYPE_ID="$DEVICE_TYPE_ID"

echo "iOS simulator reset: created $UDID runtime=$RUNTIME_ID deviceType=$DEVICE_TYPE_ID"
"$@"
