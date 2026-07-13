#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/validation/contracts/output.jsonl"
node "$root/validation/contracts/server.mjs" >/dev/null 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT

set +e
cargo run --quiet --manifest-path "$root/Cargo.toml" -p reproit -- \
  --config "$root/validation/contracts/reproit.yaml" --json --yes \
  fuzz --runs 1 --budget 1 >"$out" 2>&1
status=$?
set -e

test "$status" -eq 0
grep -q 'sent-message-becomes-visible' "$out"
find "$root/validation/contracts/.reproit/runs" -name 'contract-evidence-*.json' -print -quit | grep -q .
id="$(sed -n 's/.*"id": "\(fnd_[0-9a-f]*\)".*/\1/p' "$out" | tail -1)"
test -n "$id"
set +e
cargo run --quiet --manifest-path "$root/Cargo.toml" -p reproit -- \
  --config "$root/validation/contracts/reproit.yaml" --json --yes \
  "$id" >"$root/validation/contracts/replay-output.jsonl" 2>&1
replay_status=$?
set -e
test "$replay_status" -eq 1
grep -q '"outcome".*"fail"' "$root/validation/contracts/replay-output.jsonl"
echo "contract validation passed"
