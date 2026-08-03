#!/usr/bin/env bash
# Hermetic production-to-local acceptance for the PHP SDK at capsule parity:
# capture a planted 5xx (a PSR-18 upstream call and a PDO query recorded with
# their responses) on the `php -S` fixture, then re-execute it with
# `check --exec` under the PORTABILITY bar: the replay legs run from a COPY
# of the checkout at a different absolute path, with no database and no
# upstream running (the SDK opens no sockets in replay and the copy's DSN
# names a host that does not exist, so the network is effectively denied).
# Asserts the four-way verdict contract: reproduced (1), fixed (0),
# reproduced again (1), and diverged (3) when the capture is missing an
# exchange the code makes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE_REL="fixtures/php-backend-fixture/app.php"
SDK_REL="sdk/reproit-backend-php"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-php-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# Capture on the "capturing machine": the original checkout.
MODE=capture CAPTURE_OUT="$WORK/capture.json" php "$ROOT/$FIXTURE_REL" >/dev/null
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

# PORTABILITY: the replay legs run from a copy of the checkout at a
# different absolute path. Only the SDK and the fixture are needed; their
# relative layout is preserved so the fixture's require hop still lands.
COPY="$WORK/copy"
mkdir -p "$COPY/sdk" "$COPY/fixtures"
cp -R "$ROOT/$SDK_REL" "$COPY/sdk/"
cp -R "$ROOT/fixtures/php-backend-fixture" "$COPY/fixtures/"

# $PORT is expanded by the `sh -c` the CLI boots the command with, so php -S
# is the direct child and dies with the check.
SERVE='php -S 127.0.0.1:$PORT '"$COPY/$FIXTURE_REL"

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
  "php bug reproduces hermetically from the copied checkout" \
  "reproduced by re-execution"
run_case "$WORK/capture.json" "FIXED=1 $SERVE" 0 \
  "php fix certifies" \
  "the operation now answers cleanly"
run_case "$WORK/capture.json" "$SERVE" 1 \
  "php revert reproduces again" \
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
  "php missing exchange diverges naming the call" \
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
echo "php-hermetic-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
