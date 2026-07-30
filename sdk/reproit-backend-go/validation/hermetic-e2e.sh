#!/usr/bin/env bash
# Go-SDK hermetic acceptance, mirroring validation/backend/hermetic-e2e and
# the Rust SDK's: capture a planted net/http 5xx (pg + upstream exchanges
# recorded by the instrument boundaries), then `reproit check --exec`
# re-executes it with NO dependency running. Asserts the four-way verdict
# contract: reproduced (1), fixed (0), reproduced again (1), diverged (3).
set -euo pipefail

SDK="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$SDK/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-go-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

FIXTURE="$WORK/hermeticfixture"
(cd "$SDK" && go build -o "$FIXTURE" ./hermeticfixture)
cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit
BIN="$ROOT/target/debug/reproit"

MODE=capture CAPTURE_OUT="$WORK/capture.json" "$FIXTURE" >/dev/null
test -s "$WORK/capture.json"

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4"
  set +e
  "$BIN" check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1
  local status="$?"
  set -e
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "$FIXTURE" 1 "go bug reproduces hermetically"
run_case "$WORK/capture.json" "FIXED=1 $FIXTURE" 0 "go fix certifies"
run_case "$WORK/capture.json" "$FIXTURE" 1 "go revert reproduces again"

python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "$FIXTURE" 3 "go missing exchange diverges"

echo "go hermetic-e2e: all four verdicts hold"
