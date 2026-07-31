#!/usr/bin/env bash
# React Native production exchange capture, proven on a real simulator.
#
# WHAT THIS EXERCISES ON DEVICE: the SDK's REAL built dist modules
# (exchange, capture-batch, causal) running inside a WKWebView on a booted
# iPhone simulator, wrapping WebKit's genuine fetch. Device networking,
# bounds, at-source redaction, envelope construction, capture-batch-v1
# emission, capsule replay, and the fail-closed miss path are all real.
#
# WHAT IT DOES NOT EXERCISE: the React provider component (the only module
# that imports react-native) and the NativeModules capsule bridge. Those
# need a full React Native app with CocoaPods, which downloads roughly a
# gigabyte and would mostly prove React Native's plumbing rather than this
# SDK's capture logic. The capsule is injected through the SDK's own
# documented `globalThis.__reproit_capsule` embedded-host override, which is
# the same value `nativeCausalCapsule()` reads before it ever touches
# NativeModules.
#
# Honest note on markers: mobile replay uses the frozen runner contract
# (CAPSULE:HIT / CAPSULE:MISS), NOT the backend SDKs' REPROIT:DIVERGENCE
# marker. This script pins the contract that actually exists.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SDK="$(cd "$(dirname "$0")/.." && pwd)"
HARNESS="$SDK/validation/SimHarness"
DEVICE="${REPROIT_SIM_DEVICE:-iPhone 16 Pro}"
BUNDLE_ID="com.reproit.rnsimharness"
STUB_PORT="${REPROIT_STUB_PORT:-19801}"
INGEST_PORT="${REPROIT_INGEST_PORT:-19802}"
PAGE_PORT="${REPROIT_PAGE_PORT:-19803}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-rn-sim.XXXXXX")"
STUB_PID=""
INGEST_PID=""
PAGE_PID=""

cleanup() {
  for pid in "$STUB_PID" "$INGEST_PID" "$PAGE_PID"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
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

# Build the published dist, then embed those exact modules in the page.
(cd "$SDK" && npm run build >/dev/null 2>&1)
echo "PASS built the publishable dist"

python3 - "$SDK/dist" "$WORK" <<'EOF'
import json, os, sys
dist, work = sys.argv[1], sys.argv[2]
mods = {}
for name in ("exchange.js", "capture-batch.js", "causal.js"):
    mods["./" + name[:-3]] = open(os.path.join(dist, name)).read()
loader = """
var __mods = %s;
var __cache = {};
function require(name) {
  if (name === 'react-native') { throw new Error('no react-native on this host'); }
  var key = name.replace(/\\.js$/, '');
  if (__cache[key]) return __cache[key].exports;
  var src = __mods[key];
  if (!src) throw new Error('module not found: ' + name);
  var module = { exports: {} };
  __cache[key] = module;
  new Function('require', 'module', 'exports', src)(require, module, module.exports);
  return module.exports;
}
""" % json.dumps(mods)
open(os.path.join(work, "loader.js"), "w").write(loader)
EOF
cp "$HARNESS/harness.js" "$WORK/harness.js"
echo "PASS embedded the real dist modules in the device harness"

SDKROOT="$(xcrun --sdk iphonesimulator --show-sdk-path)"
mkdir -p "$WORK/build"
xcrun swiftc -sdk "$SDKROOT" -target arm64-apple-ios15.0-simulator -O \
  -module-name ReproItRNSimApp -o "$WORK/build/ReproItRNSim" \
  "$HARNESS/main.swift" 2>/dev/null
rm -rf "$WORK/ReproItRNSim.app"
mkdir -p "$WORK/ReproItRNSim.app"
cp "$WORK/build/ReproItRNSim" "$WORK/ReproItRNSim.app/"
cp "$HARNESS/Info.plist" "$WORK/ReproItRNSim.app/"
xcrun simctl install "$UDID" "$WORK/ReproItRNSim.app"
echo "PASS installed the webview host on the simulator"

mkdir -p "$WORK/received"
python3 "$SDK/validation/stub_server.py" "$STUB_PORT" >/dev/null 2>&1 &
STUB_PID="$!"
python3 "$SDK/validation/ingest_server.py" "$INGEST_PORT" "$WORK/received" >/dev/null 2>&1 &
INGEST_PID="$!"
python3 "$HARNESS/page_server.py" "$PAGE_PORT" >/dev/null 2>&1 &
PAGE_PID="$!"
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
    SIMCTL_CHILD_RP_BUNDLE="$WORK" \
    SIMCTL_CHILD_RP_PAGE_PORT="$PAGE_PORT" \
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
grep -q "RP: recorded-exchanges=1" <<<"$OUT" || {
  echo "FAIL the SDK did not record the outbound exchange" >&2
  echo "$OUT" >&2
  exit 1
}
BATCH="$WORK/received/capture-batch-0.json"
test -s "$BATCH" || { echo "FAIL no capture batch reached the ingest" >&2; exit 1; }
echo "PASS the device shipped a capture batch after a real fetch"

python3 - "$BATCH" <<'EOF'
import json, sys
batch = json.load(open(sys.argv[1]))
kinds = [e["event"]["kind"] for e in batch["events"]]
expected = ["operation-start", "trigger", "checkpoint", "dependency",
            "operation-end", "observation"]
assert kinds == expected, f"event sequence {kinds} != {expected}"
events = {e["event"]["kind"]: e["event"] for e in batch["events"]}
envelope = events["checkpoint"]["attributes"]
for field in ("observedAtMs", "tz", "runtime", "os", "replaySeed"):
    assert field in envelope, f"envelope missing {field}"
assert envelope["runtime"] == "react-native", envelope["runtime"]
assert batch["deployment"]["commit"], "deployment commit missing"
exchange = events["dependency"]["value"]["value"]["exchange"]
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
exchange = dependency["value"]["value"]["exchange"]
capsule = {"exchanges": [{
    "id": "a-0-0", "actor": "a", "actionIndex": 0, "ordinal": 0,
    "protocol": "http",
    "method": exchange["request"]["method"], "url": exchange["request"]["url"],
    "requestHeaders": {}, "requestBody": None,
    "status": exchange["response"]["status"],
    "responseHeaders": {"content-type": "application/json"},
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
OUT="$(launch replay SIMCTL_CHILD_RP_CAPSULE="$CAPSULE")"
grep -q "RP: http-status=200" <<<"$OUT" || {
  echo "FAIL replay did not serve the recorded response" >&2
  echo "$OUT" >&2
  exit 1
}
grep -q "RP: result=FAILED-AS-PLANTED" <<<"$OUT" || {
  echo "FAIL replay did not reproduce the planted failure" >&2
  exit 1
}
echo "PASS replay served the recorded response with the dependency down"

# --- phase 3: an unmatched call must fail closed ----------------------------
OUT="$(launch miss \
  SIMCTL_CHILD_RP_CAPSULE="$CAPSULE" \
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

echo "rn simulator-e2e: capture, replay, and fail-closed all hold on device"
