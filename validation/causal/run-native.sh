#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

bash .github/scripts/causal-android-native-smoke.sh
bash .github/scripts/causal-ios-native-smoke.sh

docker run --rm --platform linux/amd64 -v "$ROOT:/work" -w /work rust:1.88-bookworm \
  sh -c 'test "$(uname -m)" = x86_64; cc -std=c11 -Wall -Wextra -Werror runners/test_causal.c -o /tmp/reproit-causal && /tmp/reproit-causal; cargo test --manifest-path sdk/reproit-tui-rs/Cargo.toml'
docker run --rm --platform linux/amd64 -v "$ROOT:/work" -w /work/sdk/reproit-linux python:3.13-slim \
  sh -c 'python -m pip install -q pytest && pytest -q'
docker run --rm --platform linux/amd64 -v "$ROOT:/work" -w /work/sdk/reproit-tui-py python:3.13-slim \
  sh -c 'python -m pip install -q pytest && PYTHONPATH=. pytest -q'
docker run --rm --platform linux/amd64 -v "$ROOT:/work" -w /work/sdk/reproit-tui-go golang:1.26-bookworm \
  sh -c 'go test ./...'
docker run --rm --platform linux/amd64 -v "$ROOT:/work" -w /work/sdk/reproit-tui-ts node:22-bookworm \
  sh -c 'npm run test:all && npm run typecheck'

echo "native simulator and x86_64 Linux causal matrix passed"
