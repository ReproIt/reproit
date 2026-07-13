#!/usr/bin/env bash
# Live capture-diff for the QT operability marker. Compiles the REAL in-process
# Qt agent's demo main (runners/native/qt-agent/qt_agent.cpp, the same build
# line documented in its header), runs it under the Qt "offscreen" platform
# plugin (no display needed), captures the EXPLORE:GROUNDTRUTH line it actually
# emits, and diffs it (sig dropped) against tests/golden/operability/qt.json via
# canonicalize-diff.mjs. Non-zero exit on any drift.
#
# Needs: Linux, g++, pkg-config, Qt 6 dev headers (qt6-base-dev +
# libgl1-mesa-dev on Debian/Ubuntu), node.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../../.." && pwd)"

BUILD_DIR="$(mktemp -d)"
RAW="$(mktemp)"
LIVE="$(mktemp)"
trap 'rm -rf "$BUILD_DIR" "$RAW" "$LIVE"' EXIT

# shellcheck disable=SC2046 # pkg-config output is intentionally word-split
g++ -std=c++17 $(pkg-config --cflags Qt6Widgets Qt6Gui Qt6Core) \
    -DREPROIT_QT_DEMO_MAIN -fPIC "$REPO_ROOT/runners/native/qt-agent/qt_agent.cpp" \
    $(pkg-config --libs Qt6Widgets Qt6Gui Qt6Core) -o "$BUILD_DIR/qt_agent"

QT_QPA_PLATFORM=offscreen "$BUILD_DIR/qt_agent" > "$RAW"

grep -a 'EXPLORE:GROUNDTRUTH' "$RAW" > "$LIVE" || {
  echo "capture-qt: no EXPLORE:GROUNDTRUTH emitted by the qt agent" >&2
  echo "capture-qt: agent stdout follows" >&2
  cat "$RAW" >&2
  exit 1
}

node "$HERE/canonicalize-diff.mjs" qt "$LIVE"
