#!/usr/bin/env bash
# Hermetic production-to-local acceptance for the Rust SDK at capsule
# parity: capture a planted 5xx (a reqwest upstream call and a REAL
# tokio-postgres query recorded with their responses) on the loopback
# fixture, then re-execute it with `check --exec` under the PORTABILITY bar:
# the replay legs run from a COPY of the checkout at a different absolute
# path, with the database container STOPPED and the upstream never started
# (the SDK opens no sockets in replay, so the network is effectively
# denied; pg::connect is a stub, so the app boots with the DB down).
# Asserts the four-way verdict contract: reproduced (1), fixed (0),
# reproduced again (1), and diverged (3) when the capture is missing an
# exchange the code makes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE_REL="examples/rs-backend-fixture"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-rs-hermetic.XXXXXX")"
PG_NAME="reproit-rs-hermetic-pg"
PG_PORT=15499
cleanup() {
  docker rm -f "$PG_NAME" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# The capturing machine's database: a disposable postgres that exists ONLY
# for the capture leg and is stopped before any replay leg runs.
docker rm -f "$PG_NAME" >/dev/null 2>&1 || true
docker run -d --name "$PG_NAME" -e POSTGRES_PASSWORD=reproit \
  -p "127.0.0.1:$PG_PORT:5432" postgres:16-alpine >/dev/null
for _ in $(seq 1 60); do
  if docker exec "$PG_NAME" pg_isready -U postgres >/dev/null 2>&1; then break; fi
  sleep 1
done
docker exec "$PG_NAME" psql -U postgres -q -c \
  "CREATE TABLE issuers (id int, symbol text); INSERT INTO issuers VALUES (7, 'ACME');"

# Capture on the "capturing machine": the original checkout, live deps up.
MODE=capture CAPTURE_OUT="$WORK/capture.json" \
  REPROIT_PG="host=127.0.0.1 port=$PG_PORT user=postgres password=reproit dbname=postgres" \
  cargo run --quiet --manifest-path "$ROOT/$FIXTURE_REL/Cargo.toml" >/dev/null
test -s "$WORK/capture.json"

# The capture must carry BOTH dependency exchanges (http and pg), or the
# replay legs below would be exercising a weaker capsule than claimed.
python3 - "$WORK/capture.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
protocols = sorted(
    (event.get('exchange') or {}).get('protocol')
    for event in capture['events']
    if event.get('exchange')
)
assert protocols == ['http', 'pg'], protocols
assert capture['envelope']['replaySeed'], 'envelope must carry a replay seed'
EOF

# Every recorded dependency is now DOWN: the database container is gone and
# the upstream only ever existed inside the capture process.
docker rm -f "$PG_NAME" >/dev/null

# PORTABILITY: the replay legs run from a copy of the checkout at a
# different absolute path (build artifacts and VCS state excluded; the copy
# builds its own).
COPY="$WORK/copy"
mkdir -p "$COPY"
(cd "$ROOT" && tar cf - \
  --exclude=./target --exclude=./.git --exclude='./examples/*/target' \
  --exclude='./sdk/*/node_modules' --exclude='./sdk/*/.venv' .) | (cd "$COPY" && tar xf -)
cargo build --quiet --manifest-path "$COPY/$FIXTURE_REL/Cargo.toml"
SERVE="$COPY/$FIXTURE_REL/target/debug/rs-backend-fixture"

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass
# condition: the count of completed cases matching intent is.
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
  # exits 1. Pin the verdict line too.
  if ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: output lacks the verdict marker '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "$SERVE" 1 \
  "rust bug reproduces hermetically from the copied checkout" \
  "reproduced by re-execution"
run_case "$WORK/capture.json" "FIXED=1 $SERVE" 0 \
  "rust fix certifies" \
  "the operation now answers cleanly"
run_case "$WORK/capture.json" "$SERVE" 1 \
  "rust revert reproduces again" \
  "reproduced by re-execution"

# Delete the recorded http exchange: the code still makes the call, so the
# replay must DIVERGE naming it, never silently reach a live upstream.
python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "$SERVE" 3 \
  "rust missing exchange diverges naming the call" \
  "DIVERGED"
if ! grep -q '"protocol":"http"' "$WORK/out.txt"; then
  echo "FAIL divergence report does not name the missing http call" >&2
  cat "$WORK/out.txt" >&2
  exit 1
fi

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "rs-hermetic-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
