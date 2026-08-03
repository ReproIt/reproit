#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$ROOT/validation/backend/cli-e2e"
LOG="$(mktemp "${TMPDIR:-/tmp}/reproit-backend-cli.XXXXXX")"
cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  rm -f "$LOG"
  rm -rf "$FIXTURE/.reproit"
}
trap cleanup EXIT

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=11

run_case() {
  local valid="$1" expected="$2"
  rm -rf "$FIXTURE/.reproit"
  VALID_RESPONSE="$valid" node "$FIXTURE/server.mjs" >"$LOG" 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 30); do
    if curl -fsS http://127.0.0.1:19877 >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  set +e
  OUTPUT="$(cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    --config "$FIXTURE/reproit.yaml" --json internal scan --budget 4 2>&1)"
  STATUS="$?"
  set -e
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  printf '%s\n' "$OUTPUT"
  if [[ "$STATUS" -ne "$expected" ]]; then
    echo "expected scan exit $expected, got $STATUS" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
}

run_case 1 0
printf '%s\n' "$OUTPUT" | grep -q '"issues":0'
run_case 0 1
printf '%s\n' "$OUTPUT" | grep -q 'backend-response-shape'
printf '%s\n' "$OUTPUT" | grep -Fq "\$output.id is required"

run_headless_case() {
  local valid="$1" expected="$2"
  rm -rf "$FIXTURE/.reproit"
  VALID_RESPONSE="$valid" node "$FIXTURE/server.mjs" >"$LOG" 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 30); do
    if curl -fsS http://127.0.0.1:19877/headless-message >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  set +e
  OUTPUT="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
    -p reproit -- --json internal scan headless-openapi.yaml \
    --service http://127.0.0.1:19877 2>&1)"
  STATUS="$?"
  set -e
  if [[ "$STATUS" -ne "$expected" ]]; then
    printf '%s\n' "$OUTPUT" >&2
    echo "expected headless scan exit $expected, got $STATUS" >&2
    exit 1
  fi
  if [[ "$valid" == "0" ]]; then
    FINDING_ID="$(printf '%s\n' "$OUTPUT" | jq -r '.findings[0].id')"
    [[ "$FINDING_ID" == fnd_* ]]
    set +e
    REPLAY="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
      -p reproit -- --json "$FINDING_ID" 2>&1)"
    REPLAY_STATUS="$?"
    set -e
    [[ "$REPLAY_STATUS" -eq 1 ]]
    printf '%s\n' "$REPLAY" | jq -e '.reproduced == true' >/dev/null
  else
    CONFIGURED="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
      -p reproit -- --config backend-only.yaml --json internal scan \
      --service http://127.0.0.1:19877 2>&1)"
    printf '%s\n' "$CONFIGURED" | \
      jq -e '.complete == true and .findings == [] and .exercised == 1' >/dev/null
  fi
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  CASES_RUN=$((CASES_RUN + 1))
}

run_headless_case 1 0
printf '%s\n' "$OUTPUT" |
  jq -e '.complete == true and .findings == [] and .exercised == 1' >/dev/null
run_headless_case 0 1
printf '%s\n' "$OUTPUT" | jq -e '.findings | length == 1' >/dev/null
printf '%s\n' "$OUTPUT" | grep -Fq "\$output.id is required"

run_server_error_case() {
  rm -rf "$FIXTURE/.reproit"
  SERVER_ERROR=1 node "$FIXTURE/server.mjs" >"$LOG" 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 30); do
    if curl -sS http://127.0.0.1:19877/headless-message >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  set +e
  OUTPUT="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
    -p reproit -- --json internal scan headless-openapi.yaml \
    --service http://127.0.0.1:19877 2>&1)"
  STATUS="$?"
  set -e
  [[ "$STATUS" -eq 1 ]]
  FINDING_ID="$(printf '%s\n' "$OUTPUT" | jq -r '.findings[0].id')"
  printf '%s\n' "$OUTPUT" | \
    jq -e \
      '.complete == true and .exercised == 1 and .rejected == 1 and
       (.findings | length) == 1 and .findings[0].kind == "server-error"' \
      >/dev/null
  set +e
  REPLAY="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
    -p reproit -- --json "$FINDING_ID" 2>&1)"
  REPLAY_STATUS="$?"
  set -e
  [[ "$REPLAY_STATUS" -eq 1 ]]
  printf '%s\n' "$REPLAY" | jq -e '.reproduced == true' >/dev/null
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  CASES_RUN=$((CASES_RUN + 1))
}

run_server_error_case

run_finance_case() {
  local valid="$1" expected="$2"
  VALID_RESPONSE="$valid" node "$FIXTURE/server.mjs" >"$LOG" 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 30); do
    if curl -fsS http://127.0.0.1:19877/finance >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  set +e
  OUTPUT="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
    -p reproit -- --config finance-backend.yaml --json internal scan 2>&1)"
  STATUS="$?"
  set -e
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  [[ "$STATUS" -eq "$expected" ]]
  CASES_RUN=$((CASES_RUN + 1))
}

run_finance_case 1 0
printf '%s\n' "$OUTPUT" | jq -e '.findings == []' >/dev/null
run_finance_case 0 1
printf '%s\n' "$OUTPUT" | jq -e \
  '[.findings[].kind] == ["authored-invariant", "authored-invariant"]' >/dev/null

run_stateful_fuzz_case() {
  local valid="$1" expected="$2"
  rm -rf "$FIXTURE/.reproit"
  VALID_RESPONSE="$valid" node "$FIXTURE/server.mjs" >"$LOG" 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 30); do
    if curl -fsS http://127.0.0.1:19877/headless-message >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  set +e
  OUTPUT="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
    -p reproit -- --json internal fuzz stateful-openapi.yaml --runs 1 \
    --service http://127.0.0.1:19877 \
    --reset http://127.0.0.1:19877/__reproit/reset 2>&1)"
  STATUS="$?"
  set -e
  if [[ "$STATUS" -ne "$expected" ]]; then
    printf '%s\n' "$OUTPUT" >&2
    echo "expected stateful fuzz exit $expected, got $STATUS" >&2
    exit 1
  fi
  if [[ "$valid" == "0" ]]; then
    FINDING_ID="$(printf '%s\n' "$OUTPUT" | jq -r '.findings[0].id')"
    [[ "$FINDING_ID" == fnd_* ]]
    set +e
    REPLAY="$(cd "$FIXTURE" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
      -p reproit -- --json "$FINDING_ID" 2>&1)"
    REPLAY_STATUS="$?"
    set -e
    [[ "$REPLAY_STATUS" -eq 1 ]]
    printf '%s\n' "$REPLAY" | jq -e '.reproduced == true' >/dev/null
  fi
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  CASES_RUN=$((CASES_RUN + 1))
}

run_stateful_fuzz_case 1 0
printf '%s\n' "$OUTPUT" |
  jq -e \
    '.complete == true and .findings == [] and .exercised == 3 and .rejected == 1' \
    >/dev/null
run_stateful_fuzz_case 0 1
printf '%s\n' "$OUTPUT" |
  jq -e '.exercised == 3 and .rejected == 1 and (.findings | length) == 1' \
    >/dev/null
printf '%s\n' "$OUTPUT" | grep -Fq "\$output.name is required"

run_proof_case() {
  local valid="$1" expected="$2"
  rm -rf "$FIXTURE/.reproit"
  VALID_RESPONSE="$valid" node "$FIXTURE/server.mjs" >"$LOG" 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 30); do
    if curl -fsS http://127.0.0.1:19877/proof >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  set +e
  OUTPUT="$(cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    --config "$FIXTURE/proof-backend.yaml" --json internal scan --budget 3 2>>"$LOG")"
  STATUS="$?"
  set -e
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  if [[ "$STATUS" -ne "$expected" ]]; then
    printf '%s\n' "$OUTPUT" >&2
    echo "expected proof scan exit $expected, got $STATUS" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
}

run_proof_case 1 0
printf '%s\n' "$OUTPUT" | jq -e \
  '.command == "scan" and .complete == true and .issues == 0 and .results == []' >/dev/null
run_proof_case 0 1
printf '%s\n' "$OUTPUT" | jq -e \
  '.command == "scan" and .complete == true and .issues == 1 and
   (.results | length) == 1 and .results[0].screen == "backend:getOrder" and
   (.results[0].findings | length) == 1 and
   .results[0].findings[0].oracle == "backend-authorization-matrix"' >/dev/null
PROOF_EVIDENCE=("$FIXTURE"/.reproit/runs/*/backend-evidence.json)
[[ -f "${PROOF_EVIDENCE[0]}" ]]
jq -s -e \
  '[.[] | .nodes[]? | .payload.violations[]? |
    select(.oracle == "authorization-matrix")] | length >= 1' \
  "${PROOF_EVIDENCE[@]}" >/dev/null
if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "real reproit scan backend contract gate passed ($CASES_RUN/$EXPECTED_CASES cases)"
