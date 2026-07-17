#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPROIT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPROIT_BIN="${1:-${REPROIT_BIN:-}}"

if [[ -z "$REPROIT_BIN" ]]; then
  cargo build \
    --manifest-path "$REPROIT_ROOT/Cargo.toml" \
    --package reproit \
    --locked
  REPROIT_BIN="$REPROIT_ROOT/target/debug/reproit"
fi

if [[ "$REPROIT_BIN" != /* ]]; then
  REPROIT_BIN="$(cd "$(dirname "$REPROIT_BIN")" && pwd)/$(basename "$REPROIT_BIN")"
fi

if [[ ! -x "$REPROIT_BIN" ]]; then
  echo "reproit executable not found: $REPROIT_BIN" >&2
  exit 2
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/reproit-flutter-scaffold.XXXXXX")"
APP_DIR="$WORK_DIR/app"
HEADLESS_LOG="$WORK_DIR/headless.log"
trap 'find "$WORK_DIR" -depth -delete' EXIT

flutter create \
  --platforms=linux \
  --project-name reproit_flutter_fixture \
  "$APP_DIR" >/dev/null
cp "$REPROIT_ROOT/examples/flutter-fixture/lib/main.dart" "$APP_DIR/lib/main.dart"
rm "$APP_DIR/test/widget_test.dart"

(
  cd "$APP_DIR"
  "$REPROIT_BIN" init --platform flutter --force --yes
  flutter pub get

  test -f test/fuzz_headless_test.dart
  test -f integration_test/journey_explore.dart
  test -f integration_test/reproit_explorer.dart

  dart format \
    --output=none \
    --set-exit-if-changed \
    lib \
    test \
    integration_test \
    test_driver
  flutter analyze \
    --no-pub \
    lib \
    test \
    integration_test \
    test_driver

  if ! flutter test \
    --no-pub \
    --reporter=expanded \
    test/fuzz_headless_test.dart >"$HEADLESS_LOG" 2>&1; then
    sed -n '1,240p' "$HEADLESS_LOG" >&2
    exit 1
  fi
)

for marker in \
  "EXPLORE:STATE " \
  "EXPLORE:EDGE " \
  "JOURNEY DONE" \
  "All tests passed"; do
  if ! grep -Fq "$marker" "$HEADLESS_LOG"; then
    echo "generated Flutter headless test omitted marker: $marker" >&2
    sed -n '1,240p' "$HEADLESS_LOG" >&2
    exit 1
  fi
done

echo "Generated Flutter scaffold passed format, analysis, and headless runtime checks."
