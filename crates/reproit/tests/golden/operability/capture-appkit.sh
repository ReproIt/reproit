#!/usr/bin/env bash
# Live capture-diff for the APPKIT operability marker. Builds + runs the REAL
# in-process AppKit agent (runners/native/appkit-agent/build-and-run.sh; it is
# headless: the view tree is built and walked without entering the run loop or
# touching the window server, so it works over SSH and on a macOS CI runner),
# captures the EXPLORE:GROUNDTRUTH line it actually emits, and diffs it (sig
# dropped) against tests/golden/operability/appkit.json via
# canonicalize-diff.mjs. Non-zero exit on any drift.
#
# Needs: macOS, swiftc, node.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../../.." && pwd)"

RAW="$(mktemp)"
LIVE="$(mktemp)"
trap 'rm -f "$RAW" "$LIVE"' EXIT

# The agent prints the marker on stdout and per-element gap verdicts on stderr;
# let stderr stream through for the CI log.
bash "$REPO_ROOT/runners/native/appkit-agent/build-and-run.sh" > "$RAW"

grep -a 'EXPLORE:GROUNDTRUTH' "$RAW" > "$LIVE" || {
  echo "capture-appkit: no EXPLORE:GROUNDTRUTH emitted by the appkit agent" >&2
  echo "capture-appkit: agent stdout follows" >&2
  cat "$RAW" >&2
  exit 1
}

node "$HERE/canonicalize-diff.mjs" appkit "$LIVE"
