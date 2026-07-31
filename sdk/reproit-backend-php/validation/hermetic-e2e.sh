#!/usr/bin/env bash
# PHP-SDK hermetic acceptance, mirroring validation/backend/hermetic-e2e:
# capture a planted 5xx (database plus upstream exchanges recorded through
# the explicit instrument boundaries), then `reproit check --exec`
# re-executes it with NO dependency running. Asserts the four-way verdict
# contract: reproduced (1), fixed (0), reproduced again (1), diverged (3).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$(cd "$(dirname "$0")" && pwd)/hermetic_fixture.php"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-php-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit
BIN="$ROOT/target/debug/reproit"

MODE=capture CAPTURE_OUT="$WORK/capture.json" php "$FIXTURE" >/dev/null 2>&1
test -s "$WORK/capture.json"

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=4

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4" marker="$5"
  # Capture the status instead of toggling errexit, so a subject that
  # exits non-zero on purpose cannot leave the shell in the wrong mode.
  local status=0
  "$BIN" check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1 || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  # The exit code alone can lie: a capture the CLI cannot even resolve also
  # exits 1, which would let a resolution error masquerade as a
  # reproduction. Pin the verdict line too.
  if ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: output lacks the verdict marker '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "php $FIXTURE" 1 "php bug reproduces hermetically" \
  "reproduced by re-execution"
run_case "$WORK/capture.json" "FIXED=1 php $FIXTURE" 0 "php fix certifies" \
  "the operation now answers cleanly"
run_case "$WORK/capture.json" "php $FIXTURE" 1 "php revert reproduces again" \
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
run_case "$WORK/tampered.json" "php $FIXTURE" 3 "php missing exchange diverges" \
  "DIVERGED"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "php hermetic-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
