#!/usr/bin/env bash
# Occurrence-pull hermetic routing acceptance: a PULLED occurrence whose
# capture carries recorded dependency exchanges must re-execute HERMETICALLY
# (`reproit occ_<id>` boots backend.exec, or --exec, under REPROIT_REPLAY and
# verdicts from live re-execution with every dependency down), while a pulled
# capture with NO exchanges keeps the offline log re-evaluation it gets today,
# honestly labeled as not-hermetic. Also pins the verify surface: a kept
# hermetic guard reports through verify with mode `hermetic-exec`, and a
# drifted guard lands in the new `diverged` bucket without blocking.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$ROOT/validation/backend/hermetic-e2e"
SDK="$ROOT/sdk/reproit-backend-node"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-occ-routing.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

if [[ ! -d "$SDK/node_modules" ]]; then
  (cd "$SDK" && npm ci --silent)
fi

reproit() {
  cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- "$@"
}

# A fabricated "pulled" occurrence: the money-test fixture records a real
# version-2 capture (exchanges + envelope), collect/import lands it as a local
# occurrence, and the capture projection is placed exactly where the Cloud
# pull writes it (backend-capture.json). No cloud is contacted.
MODE=capture CAPTURE_OUT="$WORK/capture.json" node "$FIXTURE/app.mjs" >/dev/null
test -s "$WORK/capture.json"

PROJECT="$WORK/project"
mkdir -p "$PROJECT"
cat > "$PROJECT/reproit.yaml" << EOF
backend:
  enabled: true
  exec: "node $FIXTURE/app.mjs"
EOF

(cd "$PROJECT" && reproit internal collect --output "$WORK/bundle.rpb" \
  --product demo --component quote --summary "quote 500s on gold tier" \
  --artifact "$WORK/capture.json" >/dev/null)
(cd "$PROJECT" && reproit internal capture --bundle "$WORK/bundle.rpb" > "$WORK/import.txt")
OCC="$(grep -o 'occ_[0-9a-f]*' "$WORK/import.txt" | head -1)"
test -n "$OCC"
cp "$WORK/capture.json" "$PROJECT/.reproit/occurrences/$OCC/backend-capture.json"

CASES_RUN=0
EXPECTED_CASES=6

# Run one occurrence/verify case and pin BOTH the exit code and a verdict
# marker: the exit code alone can lie (a resolution error also exits 1).
run_case() {
  local expected="$1" label="$2"
  shift 2
  local markers=()
  while [[ "$1" != "--" ]]; do
    markers+=("$1")
    shift
  done
  shift
  local status=0
  (cd "$PROJECT" && reproit "$@" >"$WORK/out.txt" 2>&1) || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  for marker in "${markers[@]}"; do
    if ! grep -q "$marker" "$WORK/out.txt"; then
      echo "FAIL $label: output lacks the marker '$marker'" >&2
      cat "$WORK/out.txt" >&2
      exit 1
    fi
  done
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

# 1. The pulled occurrence re-executes hermetically off backend.exec: verdict
#    from re-executed code with the upstream and the database both absent
#    (the fixture's pg stand-in throws if any live query gets through).
run_case 1 "pulled occurrence reproduces hermetically" \
  "hermetic re-execution with the recorded exchanges" \
  "reproduced by re-execution" \
  -- "$OCC"

# 2. --exec overrides backend.exec on the occurrence path; the fixed code
#    certifies the fix from re-execution (exit 0).
run_case 0 "--exec override certifies the fix" \
  "the operation now answers cleanly" \
  -- "$OCC" --exec "FIXED=1 node $FIXTURE/app.mjs"

# 3. A pulled capture with NO exchanges keeps today's non-hermetic path,
#    labeled honestly: offline log re-evaluation, never presented as
#    re-execution.
python3 - "$WORK/capture.json" "$PROJECT/.reproit/occurrences/$OCC/backend-capture.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
for event in capture['events']:
    event.pop('exchange', None)
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case 1 "no-exchanges occurrence stays on the labeled non-hermetic path" \
  "offline log re-evaluation" \
  "NOT hermetic re-execution" \
  -- "$OCC"
if grep -q "reproduced by re-execution" "$WORK/out.txt"; then
  echo "FAIL: the no-exchanges path claimed re-execution" >&2
  exit 1
fi

# 4. The verify surface reports hermetic guards with their mode: keep the
#    exchange-carrying capture as a hermetic guard, then verify. The guard
#    still reproduces, so verify blocks (exit 1) and names the hermetic mode.
(cd "$PROJECT" && reproit keep "$WORK/capture.json" \
  --exec "node $FIXTURE/app.mjs" >/dev/null)
run_case 1 "verify replays the hermetic guard and blocks on reproduction" \
  "still reproduces on GET /quote (hermetic re-execution)" \
  -- internal verify

# 5. A drifted guard lands in verify's diverged bucket: quarantined (exit 0,
#    never blocks) and never counted as held.
GUARD_CAPTURE="$(ls "$PROJECT"/.reproit/repros/*/capture.json | head -1)"
python3 - "$GUARD_CAPTURE" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(sys.argv[1], 'w'))
EOF
run_case 0 "a drifted guard diverges in verify without blocking" \
  "DIVERGED" \
  "never proof" \
  -- internal verify

# 6. The JSON surface is additive: counts.diverged and the per-entry mode
#    exist for machine consumers (the MCP bridge reads exactly this).
status=0
(cd "$PROJECT" && reproit --json internal verify >"$WORK/verify.json" 2>/dev/null) || status="$?"
if [[ "$status" -ne 0 ]]; then
  echo "FAIL json verify: expected exit 0, got $status" >&2
  cat "$WORK/verify.json" >&2
  exit 1
fi
python3 - "$WORK/verify.json" << 'EOF'
import json, sys
report = json.load(open(sys.argv[1]))
assert report['counts']['diverged'] == 1, report['counts']
assert report['counts']['held'] == 0, report['counts']
assert report['diverged'][0]['mode'] == 'hermetic-exec', report['diverged']
assert report['diverged'][0]['divergences'], 'divergence report must name the drift'
EOF
CASES_RUN=$((CASES_RUN + 1))
echo "PASS verify --json carries counts.diverged and the hermetic mode (exit 0)"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "occ-routing-e2e: hermetic occurrence routing and the verify surface hold ($CASES_RUN/$EXPECTED_CASES cases)"
