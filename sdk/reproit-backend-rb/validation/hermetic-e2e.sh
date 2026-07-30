#!/usr/bin/env bash
# Ruby-SDK hermetic acceptance, mirroring validation/backend/hermetic-e2e:
# capture a planted Rack 5xx (database plus upstream exchanges recorded by
# the Net::HTTP hook and the db helper), then `reproit check --exec`
# re-executes it with NO dependency running. Asserts the four-way verdict
# contract: reproduced (1), fixed (0), reproduced again (1), diverged (3).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$(cd "$(dirname "$0")" && pwd)/hermetic_fixture.rb"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-rb-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit
BIN="$ROOT/target/debug/reproit"

MODE=capture CAPTURE_OUT="$WORK/capture.json" ruby "$FIXTURE" >/dev/null 2>&1
test -s "$WORK/capture.json"

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4" marker="$5"
  set +e
  "$BIN" check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1
  local status="$?"
  set -e
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
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "ruby $FIXTURE" 1 "ruby bug reproduces hermetically" \
  "reproduced by re-execution"
run_case "$WORK/capture.json" "FIXED=1 ruby $FIXTURE" 0 "ruby fix certifies" \
  "the operation now answers cleanly"
run_case "$WORK/capture.json" "ruby $FIXTURE" 1 "ruby revert reproduces again" \
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
run_case "$WORK/tampered.json" "ruby $FIXTURE" 3 "ruby missing exchange diverges" \
  "DIVERGED"

echo "ruby hermetic-e2e: all four verdicts hold"
