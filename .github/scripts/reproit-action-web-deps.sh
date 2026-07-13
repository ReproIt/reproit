#!/usr/bin/env bash
# Fetch chromium + its system libraries for the reproit web runner on a CI
# image. install.sh already installs the runner bundle and does a plain chromium
# fetch, but headless Chrome on a clean Linux runner also needs system libs
# (--with-deps), which only the CI job can install. Idempotent.
set -euo pipefail

# The runner bundle lands in the same data dir the binary self-heals into
# (see config::web_runner_data_dir and install.sh); resolve it identically.
case "$(uname -s)" in
  Darwin) data_base="$HOME/Library/Application Support" ;;
  *)      data_base="${XDG_DATA_HOME:-$HOME/.local/share}" ;;
esac
web_dir="$data_base/reproit/web"
cli="$web_dir/node_modules/playwright/cli.js"

if ! command -v node >/dev/null 2>&1; then
  echo "error: node is required for the reproit web runner (install Node 18+ before this step)" >&2
  exit 1
fi

# Prefer the runner's own pinned playwright so the browser build matches the
# runner; fall back to npx if the bundle layout differs.
if [ -f "$cli" ]; then
  node "$cli" install --with-deps chromium
else
  echo "note: bundled playwright cli not found at $cli; falling back to npx"
  npx --yes playwright install --with-deps chromium
fi
