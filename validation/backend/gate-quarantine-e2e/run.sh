#!/usr/bin/env bash
# Gate-level drift quarantine acceptance: a full `reproit check` backend gate
# (scan + kept guards in one run) with a hermetic guard in three states:
#   healthy guard + clean scan  -> gate exit 0, guard held
#   DRIFTED guard (capture no longer matches the code's outbound calls)
#     -> gate stays GREEN (exit 0) while the drift is reported (quarantine)
#   REPRODUCING guard -> gate exit 1 (the bug is back blocks the merge)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
CLI_E2E="$ROOT/validation/backend/cli-e2e"
MONEY="$ROOT/validation/backend/hermetic-e2e"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-gate-quarantine.XXXXXX")"
PORT=19893
SERVER_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# A backend-only project (no app section, so check runs the backend gate).
mkdir -p "$WORK/project"
# One GET the read-only scan can exercise cleanly (mirrors the fixture
# server's /headless-message under VALID_RESPONSE=1), so the gate has real
# coverage and never fails closed on an inconclusive scan half.
cat > "$WORK/project/openapi.yaml" << 'YAML'
openapi: 3.1.0
info:
  title: Reproit gate quarantine fixture
  version: 1.0.0
paths:
  /headless-message:
    get:
      operationId: getHeadlessMessage
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  id: { type: string }
YAML
cat > "$WORK/project/reproit.yaml" << YAML
backend:
  enabled: true
  schemas: [openapi.yaml]
  target: http://127.0.0.1:$PORT
YAML

# Boot the clean fixture service for the scan half of the gate.
VALID_RESPONSE=1 PORT=$PORT node "$CLI_E2E/server.mjs" >/dev/null 2>&1 &
SERVER_PID="$!"
for _ in $(seq 1 40); do
  if curl -fsS "http://127.0.0.1:$PORT" >/dev/null 2>&1; then break; fi
  sleep 0.2
done

# Capture the planted hermetic failure and keep it as a guard in the project.
MODE=capture CAPTURE_OUT="$WORK/capture.json" node "$MONEY/app.mjs" >/dev/null
run_cli() {
  (cd "$WORK/project" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- "$@")
}
# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=4

run_cli keep "$WORK/capture.json" --exec "node $MONEY/app.mjs" --as gate-guard \
  > "$WORK/keep.txt"
grep -q "verdict now: reproduced" "$WORK/keep.txt"
CASES_RUN=$((CASES_RUN + 1))
echo "PASS guard lands reproducing"

expect_gate() {
  local expected="$1" marker="$2" label="$3"
  # Capture the status instead of toggling errexit, so a gate that exits
  # non-zero on purpose cannot leave the shell in the wrong mode.
  local status=0
  (cd "$WORK/project" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    check > "$WORK/gate.txt" 2>&1) || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected gate exit $expected, got $status" >&2
    cat "$WORK/gate.txt" >&2
    exit 1
  fi
  if ! grep -q "$marker" "$WORK/gate.txt"; then
    echo "FAIL $label: gate output lacks marker '$marker'" >&2
    cat "$WORK/gate.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

# 1. Reproducing guard: the gate is the regression stop.
expect_gate 1 "REPRODUCED hermetically" "reproducing guard blocks the gate"

# 2. Fix the app (recipe-level): the guard holds and the gate is green.
GID="$(ls "$WORK/project/.reproit/repros")"
python3 - "$WORK/project/.reproit/repros/$GID/hermetic.json" << 'EOF'
import json, sys
path = sys.argv[1]
recipe = json.load(open(path))
recipe['exec'] = 'FIXED=1 ' + recipe['exec']
json.dump(recipe, open(path, 'w'))
EOF
expect_gate 0 "held (hermetic re-execution clean)" "held guard passes the gate"

# 3. Drift: the capture no longer matches the code's calls. The gate must
#    stay GREEN while reporting the quarantine; drift is not a regression.
python3 - "$WORK/project/.reproit/repros/$GID/capture.json" << 'EOF'
import json, sys
path = sys.argv[1]
capture = json.load(open(path))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(path, 'w'))
EOF
expect_gate 0 "DRIFTED (quarantined, not blocking)" "drifted guard is quarantined, gate green"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "gate-quarantine-e2e: quarantine semantics hold ($CASES_RUN/$EXPECTED_CASES cases)"
