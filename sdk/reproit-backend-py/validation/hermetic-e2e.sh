#!/usr/bin/env bash
# Hermetic production-to-local acceptance for the Python SDK: capture a
# planted 5xx (upstream HTTP plus a database call recorded with their
# responses), then re-execute it with `reproit check --exec` on a machine
# state where NO dependency is running. Asserts the four-way verdict
# contract: reproduced (1), fixed (0), reproduced again (1), and diverged (3)
# when the capture is missing an exchange the code makes.
set -euo pipefail

SDK="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$SDK/../.." && pwd)"
CLI="$ROOT/target/debug/reproit"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-py-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

if [[ ! -x "$CLI" ]]; then
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit
fi

MODE=capture CAPTURE_OUT="$WORK/capture.json" \
  uv run --project "$SDK" --group e2e python "$SDK/validation/app.py" >/dev/null
test -s "$WORK/capture.json"

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4"
  set +e
  "$CLI" check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1
  local status="$?"
  set -e
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  echo "PASS $label (exit $status)"
}

SERVE="uv run --project $SDK --group e2e python $SDK/validation/app.py"

run_case "$WORK/capture.json" "$SERVE" 1 "python bug reproduces hermetically"
run_case "$WORK/capture.json" "FIXED=1 $SERVE" 0 "python fix certifies"
run_case "$WORK/capture.json" "$SERVE" 1 "python revert reproduces again"

python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json
import sys

capture = json.load(open(sys.argv[1]))
capture["events"] = [
    event
    for event in capture["events"]
    if (event.get("exchange") or {}).get("protocol") != "http"
]
json.dump(capture, open(sys.argv[2], "w"))
EOF
run_case "$WORK/tampered.json" "$SERVE" 3 "python missing exchange diverges"

echo "python hermetic-e2e: all four verdicts hold"
