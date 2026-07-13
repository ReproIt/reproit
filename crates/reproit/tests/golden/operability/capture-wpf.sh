#!/usr/bin/env bash
# Live capture-diff for the WPF operability marker. Builds + runs the REAL
# in-process WPF agent (runners/native/wpf-agent; it is a console-style emitter
# that drives its own STA Dispatcher instead of a WPF Application, so it needs
# no interactive window and runs on a Windows CI runner or over SSH), captures
# the EXPLORE:GROUNDTRUTH line it actually emits, and diffs it (sig dropped)
# against tests/golden/operability/wpf.json via canonicalize-diff.mjs. Non-zero
# exit on any drift.
#
# Needs: Windows, the .NET 8 SDK, node, and a bash (Git Bash on the GitHub
# windows runners).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../../.." && pwd)"

RAW="$(mktemp)"
LIVE="$(mktemp)"
trap 'rm -f "$RAW" "$LIVE"' EXIT

# dotnet run interleaves restore/build chatter on stdout; the grep below picks
# the single marker line out of it.
dotnet run --project "$REPO_ROOT/runners/native/wpf-agent/WpfOperabilityAgent.csproj" \
  -c Release > "$RAW"

grep -a 'EXPLORE:GROUNDTRUTH' "$RAW" > "$LIVE" || {
  echo "capture-wpf: no EXPLORE:GROUNDTRUTH emitted by the wpf agent" >&2
  echo "capture-wpf: dotnet run output follows" >&2
  cat "$RAW" >&2
  exit 1
}

node "$HERE/canonicalize-diff.mjs" wpf "$LIVE"
