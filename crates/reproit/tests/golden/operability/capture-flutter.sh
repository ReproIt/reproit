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
trap 'rm -f "$LIVE"' EXIT

# `flutter test` streams the explorer's stdout through; grab the GROUNDTRUTH line.
flutter test -r expanded test/operability_fixture_test.dart 2>&1 | grep -a 'EXPLORE:GROUNDTRUTH' > "$LIVE" || {
  echo "capture-flutter: no EXPLORE:GROUNDTRUTH emitted by the flutter agent" >&2
  exit 1
}

node "$HERE/canonicalize-diff.mjs" flutter "$LIVE"
