#!/usr/bin/env bash
# iOS production exchange capture, proven on a real simulator.
#
# Three phases against a booted iPhone simulator, so the capture and replay
# claims rest on device behavior rather than host `swift test` alone:
#
#   capture  the app mounts the SDK with captureExchanges, makes a real
#            URLSession call to a local stub dependency, hits a planted
#            failure, and ships a capture-batch-v1 to a local stub ingest.
#            Asserts the batch carries the response body, the secret field
#            was redacted ON DEVICE, the determinism envelope is present,
#            and the emitted bytes pass the protocol validator.
#   replay   the same app runs under the runner capsule contract with the
#            stub dependency KILLED. Asserts the recorded response is served
#            (CAPSULE:HIT) and the planted failure reproduces.
#   miss     a URL the capsule does not hold. Asserts the call fails closed
#            (CAPSULE:MISS) rather than reaching the network.
#
# Honest note on markers: mobile replay uses the frozen runner contract
# (CAPSULE:HIT / CAPSULE:MISS), NOT the backend SDKs' REPROIT:DIVERGENCE
# marker. The two are different contracts and this script pins the real one.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SDK_SRC="$ROOT/sdk/reproit-ios/Sources/ReproIt"
HARNESS="$(cd "$(dirname "$0")" && pwd)"
DEVICE="${REPROIT_SIM_DEVICE:-iPhone 16 Pro}"
BUNDLE_ID="com.reproit.simharness"
STUB_PORT="${REPROIT_STUB_PORT:-19801}"
INGEST_PORT="${REPROIT_INGEST_PORT:-19802}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-ios-sim.XXXXXX")"
STUB_PID=""
INGEST_PID=""

cleanup() {
  [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null || true
  [[ -n "$INGEST_PID" ]] && kill "$INGEST_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

UDID="$(xcrun simctl list devices available | grep -F "$DEVICE (" | head -1 |
  sed -E 's/.*\(([0-9A-F-]{36})\).*/\1/')"
if [[ -z "$UDID" ]]; then
  echo "FAIL no available simulator named '$DEVICE'" >&2
  exit 1
fi
echo "device: $DEVICE ($UDID)"
xcrun simctl boot "$UDID" 2>/dev/null || true
xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true

# Build the SDK as a real library, then the app against it, exactly as an
# app consumes the package.
SDKROOT="$(xcrun --sdk iphonesimulator --show-sdk-path)"
TARGET="arm64-apple-ios15.0-simulator"
mkdir -p "$WORK/build"
xcrun swiftc -sdk "$SDKROOT" -target "$TARGET" -O \
  -emit-module -emit-library -static -module-name ReproIt \
  -emit-module-path "$WORK/build/ReproIt.swiftmodule" \
  -o "$WORK/build/libReproIt.a" "$SDK_SRC"/*.swift
xcrun swiftc -sdk "$SDKROOT" -target "$TARGET" -O \
  -I "$WORK/build" -L "$WORK/build" -lReproIt \
  -module-name ReproItSimApp -o "$WORK/build/ReproItSim" \
  "$HARNESS/SimHarness/main.swift" 2>/dev/null
echo "PASS built the SDK library and sample app for the simulator"

rm -rf "$WORK/ReproItSim.app"
mkdir -p "$WORK/ReproItSim.app"
cp "$WORK/build/ReproItSim" "$WORK/ReproItSim.app/"
cp "$HARNESS/SimHarness/Info.plist" "$WORK/ReproItSim.app/"
xcrun simctl install "$UDID" "$WORK/ReproItSim.app"
echo "PASS installed the app on the simulator"

mkdir -p "$WORK/received"
python3 "$HARNESS/stub_server.py" "$STUB_PORT" >/dev/null 2>&1 &
STUB_PID="$!"
python3 "$HARNESS/ingest_server.py" "$INGEST_PORT" "$WORK/received" >/dev/null 2>&1 &
INGEST_PID="$!"
sleep 2

DEPENDENCY="http://127.0.0.1:$STUB_PORT/prices?tier=gold"
INGEST="http://127.0.0.1:$INGEST_PORT"

launch() {
  local phase="$1"
  shift
  env "$@" \
    SIMCTL_CHILD_RP_PHASE="$phase" \
    SIMCTL_CHILD_RP_DEPENDENCY="$DEPENDENCY" \
    SIMCTL_CHILD_RP_INGEST="$INGEST" \
    xcrun simctl launch --console-pty --terminate-running-process \
    "$UDID" "$BUNDLE_ID" 2>&1
}

# --- phase 1: capture -------------------------------------------------------
OUT="$(launch capture SIMCTL_CHILD_IGNORE=1)"
grep -q "RP: result=FAILED-AS-PLANTED" <<<"$OUT" || {
  echo "FAIL capture phase did not hit the planted failure" >&2
  echo "$OUT" >&2
  exit 1
}
BATCH="$WORK/received/capture-batch-0.json"
test -s "$BATCH" || { echo "FAIL no capture batch reached the ingest" >&2; exit 1; }
echo "PASS the device shipped a capture batch after a real URLSession call"

python3 - "$BATCH" <<'EOF'
import json, sys
batch = json.load(open(sys.argv[1]))
kinds = [e["event"]["kind"] for e in batch["events"]]
expected = ["operation-start", "trigger", "checkpoint", "dependency",
            "operation-end", "observation"]
assert kinds == expected, f"event sequence {kinds} != {expected}"
events = {e["event"]["kind"]: e["event"] for e in batch["events"]}
envelope = events["checkpoint"]
assert envelope["name"] == "determinism-envelope", envelope["name"]
for field in ("observedAtMs", "tz", "runtime", "os", "arch", "replaySeed"):
    assert field in envelope["attributes"], f"envelope missing {field}"
assert batch["deployment"]["commit"], "deployment commit missing"
exchange = events["dependency"]["value"]["value"]
assert exchange["protocol"] == "http", exchange["protocol"]
body = exchange["response"]["body"]
assert body["prices"] is None, "the failing response value was not captured"
assert body["apiKey"]["$reproit"]["redacted"] is True, "secret was not redacted"
network = [c for c in batch["capabilities"] if c["capability"] == "network"]
assert network and network[0]["completeness"] == "complete", "network capability"
print("batch carries the response body, the envelope, and a redacted secret")
EOF
echo "PASS batch carries the exchange response, envelope, and redaction"

if grep -q "SHOULD-NEVER-LEAVE-DEVICE" "$WORK/received"/*.json; then
  echo "FAIL the secret value left the device" >&2
  exit 1
fi
echo "PASS the raw secret never left the device"

cargo run -q -p reproit-protocol --bin capture-validate \
  --manifest-path "$ROOT/Cargo.toml" <"$BATCH" >/dev/null
echo "PASS the device-emitted batch passes the protocol validator"

# --- phase 2: replay with the dependency down -------------------------------
python3 - "$BATCH" "$WORK/capsule.json" <<'EOF'
import json, sys
batch = json.load(open(sys.argv[1]))
dependency = next(e["event"] for e in batch["events"]
                  if e["event"]["kind"] == "dependency")
exchange = dependency["value"]["value"]
capsule = {"exchanges": [{
    "id": "a-0-0", "actor": "a", "actionIndex": 0, "ordinal": 0,
    "protocol": "http",
    "method": exchange["request"]["method"], "url": exchange["request"]["url"],
    "requestHeaders": {}, "requestBody": None,
    "status": exchange["response"]["status"],
    "responseHeaders": {"Content-Type": "application/json"},
    "responseBody": exchange["response"]["body"],
    "required": True,
}]}
json.dump(capsule, open(sys.argv[2], "w"))
EOF

kill "$STUB_PID" 2>/dev/null || true
STUB_PID=""
sleep 1
if curl -s -m 2 "$DEPENDENCY" >/dev/null 2>&1; then
  echo "FAIL the stub dependency is still reachable" >&2
  exit 1
fi
echo "PASS the stub dependency is down"

CAPSULE="$(cat "$WORK/capsule.json")"
OUT="$(launch replay \
  SIMCTL_CHILD_REPROIT_CAUSAL=1 \
  SIMCTL_CHILD_REPROIT_DEVICE=a \
  SIMCTL_CHILD_REPROIT_CAPSULE_JSON="$CAPSULE")"
grep -q "CAPSULE:HIT" <<<"$OUT" || {
  echo "FAIL replay did not serve the recorded exchange" >&2
  echo "$OUT" >&2
  exit 1
}
grep -q "RP: http-status=200" <<<"$OUT" || {
  echo "FAIL replay did not deliver the recorded response" >&2
  exit 1
}
grep -q "RP: result=FAILED-AS-PLANTED" <<<"$OUT" || {
  echo "FAIL replay did not reproduce the planted failure" >&2
  exit 1
}
echo "PASS replay served the recorded response with the dependency down"

# --- phase 3: an unmatched call must fail closed ----------------------------
OUT="$(launch miss \
  SIMCTL_CHILD_REPROIT_CAUSAL=1 \
  SIMCTL_CHILD_REPROIT_DEVICE=a \
  SIMCTL_CHILD_REPROIT_CAPSULE_JSON="$CAPSULE" \
  SIMCTL_CHILD_RP_UNMATCHED="http://127.0.0.1:$STUB_PORT/unknown-endpoint")"
grep -q "CAPSULE:MISS" <<<"$OUT" || {
  echo "FAIL an unmatched call did not report a capsule miss" >&2
  echo "$OUT" >&2
  exit 1
}
grep -q "RP: result=NETWORK-FAILED" <<<"$OUT" || {
  echo "FAIL an unmatched call did not fail closed" >&2
  exit 1
}
echo "PASS an unmatched call fails closed instead of reaching the network"

echo "ios simulator-e2e: capture, replay, and fail-closed all hold on device"
