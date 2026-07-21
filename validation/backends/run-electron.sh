#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cp -R "$ROOT/examples/electron-fixture/." "$WORK/app"
printf '{"budget":4}' > "$WORK/fuzz.json"

# The fixture deliberately does not vendor Electron. Pin the runtime used by
# this native gate while leaving product consumers free to use their own build.
npm install --prefix "$WORK/app" --no-save --no-audit --no-fund electron@31

REPROIT_APP_DIR="$WORK/app" \
REPROIT_FUZZ_CONFIG="$WORK/fuzz.json" \
node "$ROOT/runners/electron.mjs" | tee "$WORK/run.log"

grep -q '^EXPLORE:STATE ' "$WORK/run.log"
grep -q '^EXPLORE:EDGE ' "$WORK/run.log"
grep -q 'key:testid:toggle' "$WORK/run.log"
grep -q 'Detail revealed' "$WORK/run.log"
grep -q '^EXPLORE:OVERFLOW ' "$WORK/run.log"
grep -q 'key:id:overflow-message' "$WORK/run.log"
grep -q '^JOURNEY DONE$' "$WORK/run.log"
grep -q '^All tests passed$' "$WORK/run.log"
! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$WORK/run.log"

echo "WebCdp backend passed native Electron runtime"
