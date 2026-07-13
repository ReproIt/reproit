#!/usr/bin/env bash
# Live capture-diff for the GTK operability marker. Compiles the REAL in-process
# GTK4 agent's demo main (runners/native/gtk-agent/gtk_agent.c, the same build
# line documented in its header), runs it under a virtual display when no real
# one is present (GTK4 needs a display to realize widgets; xvfb-run provides
# it), captures the EXPLORE:GROUNDTRUTH line it actually emits, and diffs it
# (sig dropped) against tests/golden/operability/gtk.json via
# canonicalize-diff.mjs. Non-zero exit on any drift.
#
# Needs: Linux, gcc, pkg-config, GTK 4 dev headers + xvfb (libgtk-4-dev + xvfb
# on Debian/Ubuntu), node.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../../.." && pwd)"

BUILD_DIR="$(mktemp -d)"
RAW="$(mktemp)"
LIVE="$(mktemp)"
trap 'rm -rf "$BUILD_DIR" "$RAW" "$LIVE"' EXIT

# shellcheck disable=SC2046 # pkg-config output is intentionally word-split
gcc $(pkg-config --cflags gtk4) -DREPROIT_GTK_DEMO_MAIN \
    "$REPO_ROOT/runners/native/gtk-agent/gtk_agent.c" \
    $(pkg-config --libs gtk4) -o "$BUILD_DIR/gtk_agent"

if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
  "$BUILD_DIR/gtk_agent" > "$RAW"
else
  xvfb-run -a "$BUILD_DIR/gtk_agent" > "$RAW"
fi

grep -a 'EXPLORE:GROUNDTRUTH' "$RAW" > "$LIVE" || {
  echo "capture-gtk: no EXPLORE:GROUNDTRUTH emitted by the gtk agent" >&2
  echo "capture-gtk: agent stdout follows" >&2
  cat "$RAW" >&2
  exit 1
}

node "$HERE/canonicalize-diff.mjs" gtk "$LIVE"
