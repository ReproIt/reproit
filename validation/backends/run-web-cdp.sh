#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
PORT="${REPROIT_WEB_GATE_PORT:-18765}"
cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

printf '{"budget":4}' > "$WORK/fuzz.json"
python3 -m http.server "$PORT" --bind 127.0.0.1 \
  --directory "$ROOT/examples/web-fixture" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do
  curl -fsS "http://127.0.0.1:$PORT/" >/dev/null && break
  sleep 0.1
done

REPROIT_URL="http://127.0.0.1:$PORT/" \
REPROIT_FUZZ_CONFIG="$WORK/fuzz.json" \
node "$ROOT/runners/web/runner.mjs" | tee "$WORK/run.log"

grep -q '^EXPLORE:STATE ' "$WORK/run.log"
grep -q '^EXPLORE:EDGE ' "$WORK/run.log"
grep -q 'key:testid:toggle' "$WORK/run.log"
grep -q 'Detail revealed' "$WORK/run.log"
grep -q '^JOURNEY DONE$' "$WORK/run.log"
grep -q '^All tests passed$' "$WORK/run.log"
! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$WORK/run.log"

echo "Web backend passed native ${REPROIT_ENGINE:-chromium}/DOM runtime"
