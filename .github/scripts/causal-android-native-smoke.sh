#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ADB="${ADB:-${ANDROID_HOME:-$HOME/Library/Android/sdk}/platform-tools/adb}"
"$ADB" get-state >/dev/null

CAPTURE_LOG="$(mktemp)"
REPLAY_LOG="$(mktemp)"
CAPSULE="$(mktemp)"
SERVER_PID=""
cleanup() {
  [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true
  rm -f "$CAPTURE_LOG" "$REPLAY_LOG" "$CAPSULE"
}
trap cleanup EXIT

python3 -c 'from http.server import BaseHTTPRequestHandler,HTTPServer
class H(BaseHTTPRequestHandler):
 def do_GET(self):
  b=b"{\"ok\":true,\"token\":\"native-secret\"}"; self.send_response(200); self.send_header("content-type","application/json"); self.send_header("content-length",str(len(b))); self.end_headers(); self.wfile.write(b)
 def log_message(self,*a): pass
HTTPServer(("0.0.0.0",18765),H).serve_forever()' &
SERVER_PID=$!

cd "$ROOT/examples/compose-fixture"
./gradlew :app:assembleDebug >/dev/null
"$ADB" install -r app/build/outputs/apk/debug/app-debug.apk >/dev/null
"$ADB" shell setprop debug.reproit.fuzz 1
"$ADB" shell setprop debug.reproit.action 0
"$ADB" shell setprop debug.reproit.capsule __reproit_none__
"$ADB" logcat -c
"$ADB" shell am force-stop com.reproit.composefixture
"$ADB" shell am start -n com.reproit.composefixture/.MainActivity >/dev/null
sleep 3
PID="$("$ADB" shell pidof com.reproit.composefixture | tr -d '\r' | awk '{print $1}')"
"$ADB" logcat -d --pid="$PID" -s reproit:D '*:S' >"$CAPTURE_LOG"
grep -q 'REPROIT:CAPABILITIES.*http_replay' "$CAPTURE_LOG"
grep -q 'REPROIT:EXCHANGE.*bootstrap' "$CAPTURE_LOG"
grep -q '<reproit:string:length=13>' "$CAPTURE_LOG"
! grep -q 'native-secret' "$CAPTURE_LOG"

kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
cat >"$CAPSULE" <<'JSON'
{"exchanges":[{"id":"a-0-0","actor":"a","actionIndex":0,"ordinal":0,"protocol":"http","method":"GET","url":"http://10.0.2.2:18765/bootstrap","requestHeaders":{},"status":200,"responseHeaders":{"content-type":"application/json"},"responseBody":{"ok":true,"source":"capsule"},"required":true}]}
JSON
"$ADB" push "$CAPSULE" /data/local/tmp/reproit-capsule.json >/dev/null
"$ADB" shell chmod 0644 /data/local/tmp/reproit-capsule.json
"$ADB" shell setprop debug.reproit.capsule /data/local/tmp/reproit-capsule.json
"$ADB" logcat -c
"$ADB" shell am force-stop com.reproit.composefixture
"$ADB" shell am start -n com.reproit.composefixture/.MainActivity >/dev/null
sleep 3
PID="$("$ADB" shell pidof com.reproit.composefixture | tr -d '\r' | awk '{print $1}')"
"$ADB" logcat -d --pid="$PID" -s reproit:D '*:S' >"$REPLAY_LOG"
grep -q 'CAPSULE:HIT a-0-0' "$REPLAY_LOG"
! grep -q 'REPROIT:EXCHANGE' "$REPLAY_LOG"
echo "native Android causal capture/replay passed"
