#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$ROOT/validation/backend/cli-e2e"
LOG="$(mktemp -t reproit-backend-cli)"
cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  rm -f "$LOG"
  rm -rf "$FIXTURE/.reproit"
}
trap cleanup EXIT

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
    --config "$FIXTURE/reproit.yaml" --json scan --budget 4 2>&1)"
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
}

run_case 1 0
printf '%s\n' "$OUTPUT" | grep -q '"issues":0'
run_case 0 1
printf '%s\n' "$OUTPUT" | grep -q 'backend-contract'
printf '%s\n' "$OUTPUT" | grep -q '\$output.id is required'

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
  OUTPUT="$(cd "$FIXTURE" && REPROIT_BACKEND_URL=http://127.0.0.1:19877 \
    cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    --json scan headless-openapi.yaml 2>&1)"
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
    CONFIGURED="$(cd "$FIXTURE" && REPROIT_BACKEND_URL=http://127.0.0.1:19877 \
      cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
      --config backend-only.yaml --json scan 2>&1)"
    printf '%s\n' "$CONFIGURED" | \
      jq -e '.complete == true and .findings == [] and .exercised == 1' >/dev/null
  fi
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}

run_headless_case 1 0
printf '%s\n' "$OUTPUT" | jq -e '.complete == true and .findings == [] and .exercised == 1' >/dev/null
run_headless_case 0 1
printf '%s\n' "$OUTPUT" | jq -e '.findings | length == 1' >/dev/null
printf '%s\n' "$OUTPUT" | grep -q '\$output.id is required'

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
    -p reproit -- --config finance-backend.yaml --json scan 2>&1)"
  STATUS="$?"
  set -e
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  [[ "$STATUS" -eq "$expected" ]]
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
  OUTPUT="$(cd "$FIXTURE" && REPROIT_BACKEND_URL=http://127.0.0.1:19877 \
    REPROIT_BACKEND_RESET_URL=http://127.0.0.1:19877/__reproit/reset \
    cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    --json fuzz stateful-openapi.yaml --runs 1 2>&1)"
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
    REPLAY="$(cd "$FIXTURE" && REPROIT_BACKEND_RESET_URL=http://127.0.0.1:19877/__reproit/reset \
      cargo run --quiet --manifest-path "$ROOT/Cargo.toml" \
      -p reproit -- --json "$FINDING_ID" 2>&1)"
    REPLAY_STATUS="$?"
    set -e
    [[ "$REPLAY_STATUS" -eq 1 ]]
    printf '%s\n' "$REPLAY" | jq -e '.reproduced == true' >/dev/null
  fi
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}

run_stateful_fuzz_case 1 0
printf '%s\n' "$OUTPUT" | jq -e '.complete == true and .findings == [] and .exercised == 2' >/dev/null
run_stateful_fuzz_case 0 1
printf '%s\n' "$OUTPUT" | jq -e '.findings | length == 1' >/dev/null
printf '%s\n' "$OUTPUT" | grep -q '\$output.name is required'
echo "real reproit scan backend contract gate passed"
