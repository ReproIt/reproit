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
  xcrun simctl shutdown "$UDID" >/dev/null 2>&1 || true
  xcrun simctl delete "$UDID" >/dev/null 2>&1 || true
  echo "iOS simulator cleanup: deleted $UDID"
}
trap cleanup EXIT

xcrun simctl boot "$UDID"
xcrun simctl bootstatus "$UDID" -b
export REPROIT_IOS_UDID="$UDID"
export REPROIT_IOS_SIM_UDID="$UDID"

echo "iOS simulator reset: created $UDID runtime=$RUNTIME_ID deviceType=$DEVICE_TYPE_ID"
"$@"
