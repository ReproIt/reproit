#!/usr/bin/env bash
# Rust-SDK hermetic acceptance, mirroring validation/backend/hermetic-e2e:
# capture a planted axum 5xx (pg + upstream exchanges recorded by the
# instrument boundaries), then `reproit check --exec` re-executes it with NO
# dependency running. Asserts the four-way verdict contract:
# reproduced (1), fixed (0), reproduced again (1), diverged (3).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-rs-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit-backend \
  --features axum,instrument --example hermetic_fixture
cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit
FIXTURE="$ROOT/target/debug/examples/hermetic_fixture"
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

run_case "$WORK/capture.json" "$FIXTURE" 1 "rust bug reproduces hermetically"
run_case "$WORK/capture.json" "FIXED=1 $FIXTURE" 0 "rust fix certifies"
run_case "$WORK/capture.json" "$FIXTURE" 1 "rust revert reproduces again"

python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "$FIXTURE" 3 "rust missing exchange diverges"

echo "rust hermetic-e2e: all four verdicts hold"
