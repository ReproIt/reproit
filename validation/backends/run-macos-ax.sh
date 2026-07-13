#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
cleanup() {
  pkill -x ReproItMacSwiftUIFixture 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

APP="$($ROOT/examples/macos-swiftui-fixture/build.sh "$WORK")"
FUZZ="$WORK/fuzz.json"
LOG="$WORK/run.log"
printf '{"budget":4}' > "$FUZZ"

open "$APP"
for _ in $(seq 1 50); do
  pgrep -x ReproItMacSwiftUIFixture >/dev/null && break
  sleep 0.1
done

REPROIT_TARGET=com.reproit.macswiftuifixture \
REPROIT_FUZZ_CONFIG="$FUZZ" \
swift "$ROOT/runners/macos-ax.swift" | tee "$LOG"

grep -q '^EXPLORE:STATE ' "$LOG"
grep -q '^EXPLORE:EDGE ' "$LOG"
grep -q 'Reveal detail' "$LOG"
grep -q 'Detail revealed' "$LOG"
grep -q '^JOURNEY DONE$' "$LOG"
grep -q '^All tests passed$' "$LOG"
! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$LOG"

echo "macOS DesktopAx backend passed native SwiftUI runtime"
