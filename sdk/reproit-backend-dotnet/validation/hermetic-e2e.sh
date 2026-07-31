#!/usr/bin/env bash
# Hermetic production-to-local acceptance for the .NET SDK: capture a planted 5xx (upstream +
# db exchanges recorded through the Instrument boundary), then re-execute it with
# `reproit check --exec` on a machine state where NO dependency is running. Asserts the
# four-way verdict contract: reproduced (1), fixed (0), reproduced again (1), and diverged (3)
# when the capture is missing an exchange the code makes.
set -euo pipefail

SDK="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$SDK/../.." && pwd)"
CLI="$ROOT/target/debug/reproit"
DOTNET="${DOTNET:-$HOME/.dotnet/dotnet}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-dotnet-hermetic.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

if [[ ! -x "$CLI" ]]; then
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit
fi

"$DOTNET" build "$SDK/HermeticFixture/HermeticFixture.csproj" -v q --nologo >/dev/null
FIXTURE_DLL="$SDK/HermeticFixture/bin/Debug/net8.0/HermeticFixture.dll"
test -f "$FIXTURE_DLL"

MODE=capture CAPTURE_OUT="$WORK/capture.json" "$DOTNET" "$FIXTURE_DLL" >/dev/null
test -s "$WORK/capture.json"

# Case accounting. A harness that stops early looks exactly like one that
# passed everything it printed, so reaching the end is not the pass condition:
# the count of completed cases matching what this script intends to run is.
CASES_RUN=0
EXPECTED_CASES=4

run_case() {
  local capture="$1" prefix="$2" expected="$3" label="$4"
  # Capture the status instead of toggling errexit, so a subject that
  # exits non-zero on purpose cannot leave the shell in the wrong mode.
  local status=0
  "$CLI" check "$capture" --exec "$prefix $DOTNET $FIXTURE_DLL" >"$WORK/out.txt" 2>&1 || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  # A CLI resolution error also exits 1, so pin the verdict line too: an exit
  # code alone would let an unresolved capture masquerade as a reproduction.
  local marker
  case "$expected" in
    1) marker="reproduced by re-execution" ;;
    0) marker="PASS the operation now answers cleanly" ;;
    3) marker="DIVERGED" ;;
    *) marker="" ;;
  esac
  if [[ -n "$marker" ]] && ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: expected the verdict line '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "" 1 "dotnet bug reproduces hermetically"
run_case "$WORK/capture.json" "FIXED=1" 0 "dotnet fix certifies"
run_case "$WORK/capture.json" "" 1 "dotnet revert reproduces again"

python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
capture['events'] = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('protocol') != 'http'
]
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "" 3 "dotnet missing exchange diverges"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "dotnet hermetic-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
