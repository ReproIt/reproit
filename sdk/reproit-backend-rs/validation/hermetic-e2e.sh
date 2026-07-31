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

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=4

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4"
  # Capture the status instead of toggling errexit, so a subject that
  # exits non-zero on purpose cannot leave the shell in the wrong mode.
  local status=0
  "$BIN" check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1 || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
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

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "rust hermetic-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
