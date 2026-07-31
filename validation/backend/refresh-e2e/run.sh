#!/usr/bin/env bash
# `keep --refresh` acceptance: a drifted hermetic guard is re-recorded against
# the current code, but ONLY after the old-versus-new exchange diff is shown
# and explicitly confirmed. The properties under test:
#
#   1. a guard whose code drifted reports DIVERGED (the state refresh exists for)
#   2. --refresh without --yes prints the diff and REFUSES to rewrite (exit 3)
#   3. --refresh --yes rewrites, preserving the inbound trigger and the oracle
#   4. the refreshed guard replays cleanly against the drifted code
#   5. --refresh on an unchanged guard reports no drift and rewrites nothing
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$ROOT/validation/backend/refresh-e2e"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-refresh.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

cli() { (cd "$WORK/project" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- "$@"); }

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=5

pass_case() {
  CASES_RUN=$((CASES_RUN + 1))
  echo "$1"
}

mkdir -p "$WORK/project"

# Guard birth: capture the planted failure with the ORIGINAL call sequence.
MODE=capture CAPTURE_OUT="$WORK/capture.json" node "$FIXTURE/app.mjs" >/dev/null
test -s "$WORK/capture.json"

cli keep "$WORK/capture.json" --exec "node $FIXTURE/app.mjs" --as quote-guard > "$WORK/keep.txt"
grep -q "verdict now: reproduced" "$WORK/keep.txt" \
  || { echo "FAIL guard did not land reproducing" >&2; cat "$WORK/keep.txt" >&2; exit 1; }
GUARD_DIR="$WORK/project/.reproit/repros/$(ls "$WORK/project/.reproit/repros")"

# The guard's recipe now drifts: the code makes an extra call.
python3 - "$GUARD_DIR/hermetic.json" << 'EOF'
import json, sys
recipe = json.load(open(sys.argv[1]))
recipe['exec'] = 'DRIFT=1 ' + recipe['exec']
json.dump(recipe, open(sys.argv[1], 'w'))
EOF

# 1. Drift is visible as DIVERGED, not as a pass and not as a regression.
DRIFT_STATUS=0
cli check --repro-id quote-guard > "$WORK/drifted.txt" 2>&1 || DRIFT_STATUS="$?"
test "$DRIFT_STATUS" -eq 3 || { echo "FAIL drifted guard exit: want 3, got $DRIFT_STATUS" >&2; cat "$WORK/drifted.txt" >&2; exit 1; }
grep -q "DIVERGED" "$WORK/drifted.txt" || { echo "FAIL no DIVERGED verdict" >&2; cat "$WORK/drifted.txt" >&2; exit 1; }
pass_case "PASS drifted guard reports DIVERGED (exit 3)"

# The capture as it stands, so step 3 can prove the trigger survived.
TRIGGER_BEFORE="$(python3 -c "
import json,sys
events=json.load(open('$GUARD_DIR/capture.json'))['events']
print(json.dumps([e for e in events if e.get('kind')=='start'][0]['input'],sort_keys=True))
")"

# 2. Refresh WITHOUT --yes: the diff is shown, nothing is rewritten.
BEFORE_HASH="$(shasum -a 256 "$GUARD_DIR/capture.json" | cut -d' ' -f1)"
DRY_STATUS=0
cli keep quote-guard --refresh > "$WORK/dry.txt" 2>&1 || DRY_STATUS="$?"
test "$DRY_STATUS" -eq 3 || { echo "FAIL unconfirmed refresh exit: want 3, got $DRY_STATUS" >&2; cat "$WORK/dry.txt" >&2; exit 1; }
grep -q "NOT rewritten" "$WORK/dry.txt" || { echo "FAIL no refusal notice" >&2; cat "$WORK/dry.txt" >&2; exit 1; }
grep -q "+ http GET /inventory" "$WORK/dry.txt" || { echo "FAIL diff did not name the added call" >&2; cat "$WORK/dry.txt" >&2; exit 1; }
AFTER_DRY="$(shasum -a 256 "$GUARD_DIR/capture.json" | cut -d' ' -f1)"
test "$BEFORE_HASH" = "$AFTER_DRY" || { echo "FAIL an unconfirmed refresh rewrote the guard" >&2; exit 1; }
pass_case "PASS unconfirmed refresh shows the diff and refuses to rewrite (exit 3)"

# 3. Refresh WITH --yes: rewritten, trigger and oracle preserved.
cli --yes keep quote-guard --refresh > "$WORK/refresh.txt" 2>&1 \
  || { echo "FAIL confirmed refresh failed" >&2; cat "$WORK/refresh.txt" >&2; exit 1; }
grep -q "re-recorded" "$WORK/refresh.txt" || { echo "FAIL no re-record notice" >&2; cat "$WORK/refresh.txt" >&2; exit 1; }
python3 - "$GUARD_DIR/capture.json" "$TRIGGER_BEFORE" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
assert capture["oracle"] == "backend-server-error", capture["oracle"]
assert capture["operation"] == "GET /quote", capture["operation"]
start = [e for e in capture["events"] if e.get("kind") == "start"][0]
assert json.dumps(start["input"], sort_keys=True) == sys.argv[2], "the inbound trigger changed"
urls = [
    e["exchange"]["request"].get("url", "")
    for e in capture["events"]
    if e.get("exchange", {}).get("protocol") == "http"
]
assert any("/inventory" in u for u in urls), urls
print("refreshed capture keeps trigger and oracle, and records the new call")
EOF
pass_case "PASS confirmed refresh rewrites, preserving trigger and oracle"

# 4. The refreshed guard now replays cleanly against the drifted code.
AFTER_STATUS=0
cli check --repro-id quote-guard > "$WORK/after.txt" 2>&1 || AFTER_STATUS="$?"
test "$AFTER_STATUS" -eq 1 || { echo "FAIL refreshed guard exit: want 1 (reproduces), got $AFTER_STATUS" >&2; cat "$WORK/after.txt" >&2; exit 1; }
grep -q "reproduced by re-execution" "$WORK/after.txt" || { echo "FAIL refreshed guard did not reproduce" >&2; cat "$WORK/after.txt" >&2; exit 1; }
pass_case "PASS refreshed guard replays hermetically against the drifted code"

# 5. A guard that has not drifted reports no change and rewrites nothing.
STABLE_HASH="$(shasum -a 256 "$GUARD_DIR/capture.json" | cut -d' ' -f1)"
cli --yes keep quote-guard --refresh > "$WORK/nochange.txt" 2>&1 \
  || { echo "FAIL refresh on an unchanged guard errored" >&2; cat "$WORK/nochange.txt" >&2; exit 1; }
grep -q "no change" "$WORK/nochange.txt" || { echo "FAIL no-change refresh did not say so" >&2; cat "$WORK/nochange.txt" >&2; exit 1; }
test "$STABLE_HASH" = "$(shasum -a 256 "$GUARD_DIR/capture.json" | cut -d' ' -f1)" \
  || { echo "FAIL a no-change refresh rewrote the guard anyway" >&2; exit 1; }
pass_case "PASS refresh on an undrifted guard reports no change and rewrites nothing"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "refresh-e2e: drift is re-recorded only after an explicit, diffed confirmation ($CASES_RUN/$EXPECTED_CASES cases)"
