#!/usr/bin/env bash
# Capsule DELIVERY rate on the crash path, measured on a real emulator.
#
# The capsule is the artifact hermetic replay depends on. It used to be POSTed
# synchronously on the crashing thread, which races the OS killing the process:
# on a Pixel_9a the kill lands 168 to 768 ms after the fatal exception while a
# cold POST took 40 to 316 ms to LOCALHOST, so delivery was intermittent, and
# with a realistic ingest latency it failed outright. The SDK now writes the
# capsule to a bounded on-disk spool during the crash and uploads it on the
# next launch, so this script measures DELIVERY, not just capture.
#
# The ingest stub answers slowly on purpose (INGEST_DELAY_MS, default 2000).
# That is what makes the old race deterministic: without the spool this script
# reports 0 delivered, with it every confirmed crash is delivered.
#
# A run is COUNTED only when the planted crash is confirmed in logcat, so an
# install or launch failure is never miscounted as a lost capsule.
#
#   ./capsule-delivery.sh [runs]        # default 8
set -uo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ADB="$ANDROID_HOME/platform-tools/adb"
AVD="${REPROIT_AVD:-Pixel_9a}"
RUNS="${1:-8}"
INGEST_DELAY_MS="${INGEST_DELAY_MS:-2000}"
SDK_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-capsule-delivery.XXXXXX")"
INGEST_PORT=39990
UPSTREAM_PORT=39991
PKG=com.reproit.proof

# The emulator's /data is often near full, which blocks installs. Lower the
# storage guard for the duration and RESTORE it on exit; never touch another
# project's apps.
lower_storage_guard() {
  PRIOR_MAX="$("$ADB" shell settings get global sys_storage_threshold_max_bytes 2>/dev/null | tr -d '\r')"
  PRIOR_PCT="$("$ADB" shell settings get global sys_storage_threshold_percentage 2>/dev/null | tr -d '\r')"
  "$ADB" shell settings put global sys_storage_threshold_max_bytes 2097152 >/dev/null 2>&1
  "$ADB" shell settings put global sys_storage_threshold_percentage 1 >/dev/null 2>&1
}
restore_storage_guard() {
  if [ "${PRIOR_MAX:-null}" = "null" ] || [ -z "${PRIOR_MAX:-}" ]; then
    "$ADB" shell settings delete global sys_storage_threshold_max_bytes >/dev/null 2>&1
  else
    "$ADB" shell settings put global sys_storage_threshold_max_bytes "$PRIOR_MAX" >/dev/null 2>&1
  fi
  if [ "${PRIOR_PCT:-null}" = "null" ] || [ -z "${PRIOR_PCT:-}" ]; then
    "$ADB" shell settings delete global sys_storage_threshold_percentage >/dev/null 2>&1
  else
    "$ADB" shell settings put global sys_storage_threshold_percentage "$PRIOR_PCT" >/dev/null 2>&1
  fi
}

cleanup() {
  [ -n "${SERVERS_PID:-}" ] && kill "$SERVERS_PID" 2>/dev/null
  restore_storage_guard
  "$ADB" uninstall "$PKG" >/dev/null 2>&1
  "$ADB" reverse --remove tcp:$INGEST_PORT >/dev/null 2>&1
  "$ADB" reverse --remove tcp:$UPSTREAM_PORT >/dev/null 2>&1
  rm -rf "$WORK"
}
trap cleanup EXIT

command -v node >/dev/null || { echo "node is required" >&2; exit 1; }
test -x "$ADB" || { echo "adb not found at $ADB" >&2; exit 1; }
if ! "$ADB" shell true >/dev/null 2>&1; then
  echo "no device: boot one with"
  echo "  $ANDROID_HOME/emulator/emulator -avd $AVD -no-snapshot -no-boot-anim -gpu swiftshader_indirect"
  exit 1
fi

lower_storage_guard

# The sample app is a throwaway host for the SDK, not a shipped artifact, so it
# is generated here rather than committed.
"$(dirname "$0")/make-proof-app.sh" "$WORK" || exit 1

cat > "$WORK/servers.mjs" <<'EOF'
import http from 'node:http';
import fs from 'node:fs';
const OUT = process.env.OUT;
const DELAY = Number(process.env.INGEST_DELAY_MS || 0);
let seq = 0;
http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => (body += c));
  req.on('end', () => setTimeout(() => {
    fs.mkdirSync(OUT, { recursive: true });
    const n = ++seq;
    fs.writeFileSync(`${OUT}/${String(n).padStart(3, '0')}.json`, body);
    fs.appendFileSync(`${OUT}/paths.log`, `${req.url}\n`);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end('{"ok":true}');
  }, DELAY));
}).listen(39990, '127.0.0.1');
http.createServer((_req, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ prices: null, symbol: 'ACME' }));
}).listen(39991, '127.0.0.1');
EOF

OUT="$WORK/received"
OUT="$OUT" INGEST_DELAY_MS="$INGEST_DELAY_MS" node "$WORK/servers.mjs" &
SERVERS_PID=$!
sleep 2

APK="$WORK/app/build/outputs/apk/debug/app-debug.apk"
counted=0; delivered=0; lost=0; skipped=0
for i in $(seq 1 "$RUNS"); do
  mkdir -p "$OUT"; rm -f "$OUT"/*.json "$OUT"/paths.log 2>/dev/null
  "$ADB" shell am force-stop "$PKG" >/dev/null 2>&1
  "$ADB" uninstall "$PKG" >/dev/null 2>&1
  if ! "$ADB" install -r -g "$APK" >/dev/null 2>&1; then
    echo "run $i: SKIP (install failed)"; skipped=$((skipped+1)); continue
  fi
  "$ADB" reverse tcp:$INGEST_PORT tcp:$INGEST_PORT >/dev/null 2>&1
  "$ADB" reverse tcp:$UPSTREAM_PORT tcp:$UPSTREAM_PORT >/dev/null 2>&1
  "$ADB" logcat -c >/dev/null 2>&1
  "$ADB" shell am start -n "$PKG/.MainActivity" >/dev/null 2>&1
  sleep 14
  if [ "$("$ADB" logcat -d 2>/dev/null | grep -c 'FATAL EXCEPTION')" -eq 0 ]; then
    echo "run $i: SKIP (app did not crash)"; skipped=$((skipped+1)); continue
  fi
  # The next healthy session is where a spooled capsule ships.
  "$ADB" shell am start -n "$PKG/.MainActivity" --ez drain_only true >/dev/null 2>&1
  sleep 12
  counted=$((counted+1))
  if grep -q "capture-batches" "$OUT/paths.log" 2>/dev/null; then
    delivered=$((delivered+1)); echo "run $i: DELIVERED"
  else
    lost=$((lost+1)); echo "run $i: LOST"
  fi
done

echo "capsule delivery: $delivered/$counted delivered, $lost lost, $skipped skipped (ingest delay ${INGEST_DELAY_MS}ms)"
if [ "$counted" -eq 0 ]; then
  echo "FAIL: no run reached a confirmed crash, so nothing was measured" >&2
  exit 1
fi
if [ "$lost" -ne 0 ]; then
  echo "FAIL: $lost confirmed crash(es) lost the capsule" >&2
  exit 1
fi
echo "capsule-delivery: every confirmed crash delivered its capsule"
