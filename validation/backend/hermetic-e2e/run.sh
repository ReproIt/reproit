#!/usr/bin/env bash
# Hermetic production-to-local acceptance: capture a planted 5xx (upstream +
# pg exchanges recorded by the SDK), then re-execute it with `check --exec`
# on a machine state where NO dependency is running. Asserts the four-way
# verdict contract: reproduced (1), fixed (0), reproduced again (1), and
# diverged (3) when the capture is missing an exchange the code makes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$ROOT/validation/backend/hermetic-e2e"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

MODE=capture CAPTURE_OUT="$WORK/capture.json" node "$FIXTURE/app.mjs" >/dev/null
test -s "$WORK/capture.json"

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=4

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4" marker="$5"
  # Capture the status instead of toggling errexit, so a subject that exits
  # non-zero on purpose cannot leave the shell in the wrong mode.
  local status=0
  cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1 || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  # The exit code alone can lie: a capture the CLI cannot even resolve also
  # exits 1, which would let a resolution error masquerade as a reproduction.
  # Pin the verdict line too.
  if ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: output lacks the verdict marker '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "node $FIXTURE/app.mjs" 1 "bug reproduces hermetically" \
  "reproduced by re-execution"
run_case "$WORK/capture.json" "FIXED=1 node $FIXTURE/app.mjs" 0 "fix certifies" \
  "the operation now answers cleanly"
run_case "$WORK/capture.json" "node $FIXTURE/app.mjs" 1 "revert reproduces again" \
  "reproduced by re-execution"

python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "node $FIXTURE/app.mjs" 3 "missing exchange diverges" \
  "DIVERGED"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "hermetic-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
