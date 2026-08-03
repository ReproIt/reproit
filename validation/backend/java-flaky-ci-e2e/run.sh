#!/usr/bin/env bash
# Flaky-CI wedge acceptance for the JAVA SDK, cloned leg for leg from
# validation/backend/flaky-ci-e2e: a planted order-dependent test failure
# fires only under CI-like conditions (the CI legacy matrix leaks state into
# a shared config service), the simulated CI run spools a test-trigger
# capsule, and `reproit check <capsule> --exec "<test command>"` re-executes
# the exact failing run under the PORTABILITY bar: the checkout is a COPY at
# a different absolute path and no recorded dependency runs (the SDK serves
# everything in process; the replay compile uses plain javac, no Maven and
# no downloads). Asserts the flaky-versus-fixed distinction and the four-way
# verdict contract: a plain rerun passes (flaky evidence, proving nothing),
# check reproduces (1), the fix certifies (0), the revert reproduces again
# (1), and a deleted exchange diverges (3).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SDK_REL="sdk/reproit-backend-java"
FIXTURE_REL="fixtures/java-flaky-ci-fixture"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-java-flaky-ci.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# Capture side compiles on the "capturing machine": the original checkout,
# plain javac (the SDK main sources have zero dependencies beyond the JDK).
javac -d "$WORK/classes-capture" \
  -sourcepath "$ROOT/$SDK_REL/src/main/java:$ROOT/$FIXTURE_REL" \
  "$ROOT/$FIXTURE_REL/CheckoutTest.java"

# The failure fires ONLY under CI-like conditions: a plain run of the suite
# from the original checkout passes, which is exactly why the bug reads as
# unreproducible without the capsule.
if ! java -cp "$WORK/classes-capture" CheckoutTest >/dev/null 2>&1; then
  echo "FAIL the planted failure fired outside CI conditions" >&2
  exit 1
fi
echo "PASS plain local run passes (the failure is invisible without CI conditions)"

# Simulated CI run: env-driven, headless. The legacy matrix leaks state into
# the shared config service, the second test fails, and the SDK spools the
# capsule.
SPOOL="$WORK/spool"
if REPROIT_CI_CAPTURE=1 CI_LEGACY_MATRIX=1 REPROIT_CI_SPOOL="$SPOOL" \
  java -cp "$WORK/classes-capture" CheckoutTest >/dev/null 2>&1; then
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
assert exchanges[0]['exchange']['response']['body'] == {'rate': '0.25'}
EOF

# PORTABILITY: the replay side compiles and runs from a COPY of the checkout
# at a different absolute path. Only the SDK and the fixture are copied,
# which is itself evidence of the closure: no jars, no database, no
# upstream.
COPY="$WORK/copy"
mkdir -p "$COPY/sdk" "$COPY/fixtures"
cp -R "$ROOT/$SDK_REL" "$COPY/sdk/"
cp -R "$ROOT/$FIXTURE_REL" "$COPY/fixtures/"
rm -rf "$COPY/$SDK_REL/target"
javac -d "$WORK/classes-replay" \
  -sourcepath "$COPY/$SDK_REL/src/main/java:$COPY/$FIXTURE_REL" \
  "$COPY/$FIXTURE_REL/CheckoutTest.java"
TEST="java -cp $WORK/classes-replay CheckoutTest"

# Flaky versus fixed, stated as measurements: the developer's instinctive
# rerun of the suite from the copy passes WITHOUT any fix. That pass is
# flaky evidence (envelope-dependent), not a fix, and check must still
# reproduce from the capsule afterwards.
if ! $TEST >/dev/null 2>&1; then
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

run_case "$CAPSULE" "$TEST" 1 \
  "capsule reproduces the CI failure from the copy" \
  "reproduced by re-execution (backend-authored-invariant on test:checkout#order total"
run_case "$CAPSULE" "FIXED=1 $TEST" 0 \
  "fix certifies under the recorded envelope" \
  "the test now passes under the recorded envelope"
run_case "$CAPSULE" "$TEST" 1 \
  "revert reproduces again" \
  "reproduced by re-execution"

# Delete the recorded exchange: the replayed test's config call has nothing
# to match and must diverge with the named call, never fall through to a
# live socket.
python3 - "$CAPSULE" "$WORK/tampered.json" << 'EOF'
import json, sys
capsule = json.load(open(sys.argv[1]))
capsule['events'] = [e for e in capsule['events'] if not e.get('exchange')]
json.dump(capsule, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "$TEST" 3 \
  "deleted exchange diverges naming the call" \
  "DIVERGED"
if ! grep -q '"url":"http://127.0.0.1:19992/tax-rate"' "$WORK/out.txt"; then
  echo "FAIL divergence does not name the unmatched call" >&2
  cat "$WORK/out.txt" >&2
  exit 1
fi

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "java-flaky-ci-e2e: flaky-vs-fixed distinguished, all four verdicts hold" \
  "($CASES_RUN/$EXPECTED_CASES cases)"
