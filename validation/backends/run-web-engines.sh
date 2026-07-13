#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

for engine in firefox webkit; do
  echo "=== Playwright $engine runtime ==="
  REPROIT_ENGINE="$engine" \
  REPROIT_WEB_GATE_PORT="$([[ "$engine" == firefox ]] && echo 18766 || echo 18767)" \
    "$ROOT/validation/backends/run-web-cdp.sh"
done

echo "Playwright Firefox/WebKit browser runtime matrix passed"
