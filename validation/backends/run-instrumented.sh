#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

assert_protocol() {
  local name="$1" log="$2"
  grep -q '^EXPLORE:STATE ' "$log"
  grep -q '^EXPLORE:EDGE ' "$log"
  grep -q '^JOURNEY DONE$' "$log"
  grep -q '^All tests passed$' "$log"
  ! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$log"
  echo "$name instrumented runtime passed"
}

clang++ -std=c++17 \
  -I "$ROOT/examples/imgui-headless/imgui" -I "$ROOT/runners" \
  "$ROOT/examples/imgui-headless/main.cpp" \
  "$ROOT"/examples/imgui-headless/imgui/*.cpp \
  -o "$WORK/imgui"
(cd "$WORK" && REPROIT_FUZZ_CONFIG="$ROOT/examples/imgui-headless/fuzz.json" \
  ./imgui > "$WORK/imgui.log")
assert_protocol "Dear ImGui" "$WORK/imgui.log"

clang -std=c11 \
  -I "$ROOT/examples/clay-headless" -I "$ROOT/runners" \
  "$ROOT/examples/clay-headless/main.c" -o "$WORK/clay"
REPROIT_FUZZ_CONFIG="$ROOT/examples/clay-headless/fuzz.json" \
  "$WORK/clay" > "$WORK/clay.log"
assert_protocol "Clay" "$WORK/clay.log"

echo "Instrumented backend passed Dear ImGui and Clay native runtimes"
