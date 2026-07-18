#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${1:-$ROOT/target/release/reproit-perf}"

if [[ ! -x "$BIN" ]]; then
  cargo build --manifest-path "$ROOT/Cargo.toml" --release \
    --features perf-bench --bin reproit-perf
fi

run() {
  "$BIN" "$1" "$2" "$3" 7
}

run frontier 100 100
run frontier 1000 20
run frontier 10000 3
run log 1 20
run log 10 3
run log 100 1
run merge 100 100
run merge 1000 20
run merge 10000 3
run batch 1 20
run batch 10 3
run batch 100 1
run permission 100 100
run permission 1000 20
run permission 10000 3
run persistence 100 5
run persistence 1000 3
run persistence 10000 1
run fingerprint 100 20
