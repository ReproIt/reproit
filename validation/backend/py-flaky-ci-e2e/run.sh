#!/usr/bin/env bash
# Flaky-CI wedge acceptance for the PYTHON SDK, cloned leg for leg from
# flaky-ci-e2e (the Node gate): a planted order-dependent test failure fires
# only under CI-like conditions (the CI legacy matrix leaks state into a
# shared upstream), the simulated CI run spools a test-trigger capsule, and
# `reproit check <capsule> --exec "<pytest command>"` re-executes the exact
# failing run under the PORTABILITY bar: the checkout is a COPY at a
# different absolute path and no recorded dependency runs (the SDK's replay
# wrappers serve everything in process; a real socket attempt would be a
# divergence, not a connection). Asserts the flaky-versus-fixed distinction
# and the four-way verdict contract: a plain rerun passes (flaky evidence,
# proving nothing), check reproduces (1), the fix certifies (0), the revert
# reproduces again (1), and a deleted exchange diverges (3).
#
# pytest runs with `-s` throughout: the REPROIT:CI-TEST and
# REPROIT:DIVERGENCE stderr markers `reproit check` parses must not be
# swallowed by pytest's output capture.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-py-flaky-ci.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# PORTABILITY: the replay side runs from a COPY of the checkout at a
# different absolute path. Only what the fixture needs is copied, which is
# itself evidence of the closure: the SDK and the fixture. No virtualenv, no
# database, no upstream.
COPY="$WORK/copy"
mkdir -p "$COPY/sdk" "$COPY/fixtures"
cp -R "$ROOT/sdk/reproit-backend-py" "$COPY/sdk/"
rm -rf "$COPY/sdk/reproit-backend-py/.venv"
cp -R "$ROOT/fixtures/py-flaky-ci-fixture" "$COPY/fixtures/"
TEST_REL="fixtures/py-flaky-ci-fixture/tests/test_checkout.py"
PYTEST_ORIG="uv run --project $ROOT/sdk/reproit-backend-py --group test \
python -m pytest -q -s -p no:cacheprovider $ROOT/$TEST_REL"
PYTEST_COPY="uv run --project $COPY/sdk/reproit-backend-py --group test \
python -m pytest -q -s -p no:cacheprovider $COPY/$TEST_REL"

# The failure fires ONLY under CI-like conditions: a plain run of the suite
# from the original checkout passes, which is exactly why the bug reads as
# unreproducible without the capsule.
if ! $PYTEST_ORIG >/dev/null 2>&1; then
  echo "FAIL the planted failure fired outside CI conditions" >&2
  exit 1
fi
echo "PASS plain local run passes (the failure is invisible without CI conditions)"

# Simulated CI run: env-driven, headless. The legacy matrix leaks state into
# the shared config service, the second test fails, and the SDK spools the
# capsule.
SPOOL="$WORK/spool"
if REPROIT_CI_CAPTURE=1 CI_LEGACY_MATRIX=1 REPROIT_CI_SPOOL="$SPOOL" \
  $PYTEST_ORIG >/dev/null 2>&1; then
  echo "FAIL the simulated CI run did not fail" >&2
  exit 1
fi
CAPSULE="$(/bin/ls "$SPOOL"/capsule-*.json)"
test -s "$CAPSULE"

# The capsule must carry the test trigger identity in the existing operation
# field, the existing authored-invariant oracle, and the recorded legacy
# exchange, or the legs below exercise a weaker capsule than claimed.
python3 - "$CAPSULE" << 'EOF'
import json, sys
capsule = json.load(open(sys.argv[1]))
assert capsule['format'] == 'reproit-backend-capture', capsule['format']
assert capsule['version'] == 2, capsule['version']
operation = capsule['operation']
assert operation == 'test:checkout#order total applies the configured tax rate', operation
assert capsule['oracle'] == 'backend-authored-invariant', capsule['oracle']
assert 'replaySeed' in capsule['envelope'], capsule['envelope']
exchanges = [e for e in capsule['events'] if e.get('exchange')]
assert len(exchanges) == 1, len(exchanges)
assert exchanges[0]['exchange']['response']['body'] == {'rate': '25', 'unit': 'percent'}
EOF

# Flaky versus fixed, stated as measurements: the developer's instinctive
# rerun of the suite from the copy passes WITHOUT any fix. That pass is
# flaky evidence (envelope-dependent), not a fix, and check must still
# reproduce from the capsule afterwards.
if ! $PYTEST_COPY >/dev/null 2>&1; then
  echo "FAIL the plain rerun from the copy did not pass" >&2
  exit 1
fi
echo "PASS plain rerun passes without a fix (flaky evidence, not Fixed)"

CASES_RUN=0
EXPECTED_CASES=4

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4" marker="$5"
  local status=0
  cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1 || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  if ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: output lacks the verdict marker '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

run_case "$CAPSULE" "$PYTEST_COPY" 1 \
  "capsule reproduces the CI failure from the copy" \
  "reproduced by re-execution (backend-authored-invariant on test:checkout#order total"
run_case "$CAPSULE" "FIXED=1 $PYTEST_COPY" 0 \
  "fix certifies under the recorded envelope" \
  "the test now passes under the recorded envelope"
run_case "$CAPSULE" "$PYTEST_COPY" 1 \
  "revert reproduces again" \
  "reproduced by re-execution"

# Delete the recorded exchange: the replayed test's upstream call has
# nothing to match and must diverge with the named call, never fall through
# to a live socket.
python3 - "$CAPSULE" "$WORK/tampered.json" << 'EOF'
import json, sys
capsule = json.load(open(sys.argv[1]))
capsule['events'] = [e for e in capsule['events'] if not e.get('exchange')]
json.dump(capsule, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "$PYTEST_COPY" 3 \
  "deleted exchange diverges naming the call" \
  "DIVERGED"
if ! grep -q '"url":"http://127.0.0.1:19995/tax-rate"' "$WORK/out.txt"; then
  echo "FAIL divergence does not name the unmatched call" >&2
  cat "$WORK/out.txt" >&2
  exit 1
fi

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "py-flaky-ci-e2e: flaky-vs-fixed distinguished, all four verdicts hold" \
  "($CASES_RUN/$EXPECTED_CASES cases)"
