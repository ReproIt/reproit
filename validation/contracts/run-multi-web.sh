#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$(mktemp)"
node "$root/validation/contracts/server.mjs" >/dev/null 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true; rm -f "$out"' EXIT

cargo run --quiet --manifest-path "$root/Cargo.toml" -p reproit -- \
  --config "$root/validation/contracts/reproit-multi.yaml" --json --yes \
  journey multi >"$out" 2>&1

grep -q '"outcome".*"pass"' "$out"
echo "multi-actor contract validation passed"
