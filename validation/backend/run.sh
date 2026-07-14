#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
cargo test -p reproit backend:: --locked
cargo test -p reproit-backend --locked
cargo clippy -p reproit --all-targets --locked -- -D warnings
cargo clippy -p reproit-backend --all-targets --locked -- -D warnings
node --test runners/web/backend-transport.test.mjs validation/backend/sdk-node.test.mjs validation/backend/web-e2e.test.mjs validation/backend/cloud-schema-parity.test.mjs
node validation/backend/benchmark.mjs
