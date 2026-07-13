#!/usr/bin/env bash
# Live capture-diff for the FLUTTER operability marker. Runs the REAL Flutter
# in-process agent (the operability fixture under `flutter test`), captures the
# EXPLORE:GROUNDTRUTH line it actually emits, and diffs it (sig dropped) against
# tests/golden/operability/flutter.json via canonicalize-diff.mjs. Non-zero exit
# on any drift -> the CI job fails, naming the platform + the changed field.
#
# This is the LIVE-IN-CI path for flutter: the CI flutter job installs the
# Flutter toolchain, so this re-captures the marker on every run and catches an
# agent that has drifted from the committed golden.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../../.." && pwd)"
EXAMPLE_DIR="$REPO_ROOT/sdk/reproit_flutter/example"

cd "$EXAMPLE_DIR"
flutter pub get >/dev/null

LIVE="$(mktemp)"
RAW="$(mktemp)"
trap 'rm -f "$LIVE" "$RAW"' EXIT

# `flutter test` streams the explorer's stdout through; grab the GROUNDTRUTH line.
set +e
flutter test -r expanded test/operability_fixture_test.dart > "$RAW" 2>&1
STATUS=$?
set -e
grep -a 'EXPLORE:GROUNDTRUTH' "$RAW" > "$LIVE" || {
  echo "capture-flutter: no EXPLORE:GROUNDTRUTH emitted by the flutter agent" >&2
  echo "capture-flutter: flutter test output follows" >&2
  tail -120 "$RAW" >&2
  exit 1
}
if [ "$STATUS" -ne 0 ]; then
  echo "capture-flutter: flutter test failed with exit code $STATUS" >&2
  echo "capture-flutter: flutter test output follows" >&2
  tail -120 "$RAW" >&2
  exit "$STATUS"
fi

node "$HERE/canonicalize-diff.mjs" flutter "$LIVE"
