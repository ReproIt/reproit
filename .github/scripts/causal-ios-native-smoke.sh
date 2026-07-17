#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
default_udid="$(
  xcrun simctl list devices booted | grep -E 'iPhone|App-' |
    head -1 | grep -oE '[0-9A-F-]{36}'
)"
UDID="${REPROIT_IOS_SIM_UDID:-$default_udid}"
[ -n "$UDID" ] || { echo "no booted iOS simulator" >&2; exit 1; }

BUILD="$(mktemp -d)"
CAPSULE="$(mktemp)"
SERVER_PID=""
cleanup() {
  [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$BUILD"; rm -f "$CAPSULE"
}
trap cleanup EXIT

APP="$(bash "$ROOT/examples/ios-swiftui-fixture/build.sh" "$BUILD")"
xcrun simctl install "$UDID" "$APP"
DATA="$(xcrun simctl get_app_container "$UDID" com.reproit.swiftuifixture data)"
NETWORK="$DATA/tmp/reproit-network.jsonl"
CAPTURE_RESULT="$DATA/tmp/reproit-capture-result.json"
REPLAY_RESULT="$DATA/tmp/reproit-replay-result.json"
rm -f "$NETWORK" "$CAPTURE_RESULT" "$REPLAY_RESULT"
python3 -c 'from http.server import BaseHTTPRequestHandler,HTTPServer
class H(BaseHTTPRequestHandler):
 def do_GET(self):
  b=b"{\"ok\":true,\"email\":\"native@example.test\"}"
  self.send_response(200)
  self.send_header("content-type","application/json")
  self.end_headers()
  self.wfile.write(b)
 def log_message(self,*a): pass
HTTPServer(("127.0.0.1",18766),H).serve_forever()' &
SERVER_PID=$!
SIMCTL_CHILD_REPROIT_CAUSAL=1 SIMCTL_CHILD_REPROIT_DEVICE=a \
  SIMCTL_CHILD_REPROIT_NETWORK_FILE="$NETWORK" \
  SIMCTL_CHILD_REPROIT_NATIVE_RESULT_FILE="$CAPTURE_RESULT" \
  xcrun simctl launch --terminate-running-process \
  "$UDID" com.reproit.swiftuifixture >/dev/null
sleep 3
xcrun simctl terminate "$UDID" com.reproit.swiftuifixture
grep -q 'bootstrap' "$NETWORK"
grep -q '<reproit:string:length=19>' "$NETWORK"
! grep -q 'native@example.test' "$NETWORK"
grep -q 'native@example.test' "$CAPTURE_RESULT"

kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
cat >"$CAPSULE" <<'JSON'
{
  "exchanges": [{
    "id": "a-0-0", "actor": "a", "actionIndex": 0, "ordinal": 0,
    "protocol": "http", "method": "GET",
    "url": "http://127.0.0.1:18766/bootstrap", "requestHeaders": {},
    "status": 200, "responseHeaders": {"content-type": "application/json"},
    "responseBody": {"ok": true, "source": "capsule"}, "required": true
  }]
}
JSON
RAW="$(tr -d '\n' <"$CAPSULE")"
SIMCTL_CHILD_REPROIT_CAUSAL=1 SIMCTL_CHILD_REPROIT_DEVICE=a \
  SIMCTL_CHILD_REPROIT_CAPSULE_JSON="$RAW" \
  SIMCTL_CHILD_REPROIT_NATIVE_RESULT_FILE="$REPLAY_RESULT" \
  xcrun simctl launch --terminate-running-process \
  "$UDID" com.reproit.swiftuifixture >/dev/null
sleep 3
xcrun simctl terminate "$UDID" com.reproit.swiftuifixture
grep -q '"source":"capsule"' "$REPLAY_RESULT"
echo "native iOS causal capture/replay passed"
