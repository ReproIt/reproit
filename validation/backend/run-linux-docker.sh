#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

docker run --rm --platform linux/amd64 \
  -v "$ROOT:/work:ro" \
  -v reproit-backend-cargo-registry:/usr/local/cargo/registry \
  -v reproit-backend-linux-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -w /work \
  rust:1.88-bookworm \
  sh -c 'apt-get update -qq && \
apt-get install -y -qq libatspi2.0-dev >/dev/null && \
cargo test -p reproit "backend::" --locked'

echo "Linux x86 backend structural gate passed"
