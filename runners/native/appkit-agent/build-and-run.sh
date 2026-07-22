#!/usr/bin/env bash
# Build + run the in-process AppKit operability agent. Headless: it builds the
# demo view tree and walks it without showing a window, so it works in CI.
# Prints the EXPLORE:GROUNDTRUTH marker on stdout and per-element gap
# verdicts on stderr. Exit 0 on success.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="${TMPDIR:-/tmp}/reproit-appkit-agent"
swiftc -O "$here/main.swift" -o "$out"
exec "$out"
