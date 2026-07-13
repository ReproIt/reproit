#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="$(mktemp -d -t reproit-backend-bug-zoo)"
export REPROIT_BUG_ZOO_TMP="$WORK/captured"
mkdir -p "$REPROIT_BUG_ZOO_TMP/buggy" "$REPROIT_BUG_ZOO_TMP/fixed"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

REPO="$WORK/fastapi"
BUGGY="7cea84b74ca3106a7f861b774e9d215e5228728f"
FIXED="75a07f24bf01a31225ee687f3e2b3fc1981b67ab"
IMAGE="python@sha256:1d52838af602b4b5a831beb13a0e4d073280665ea7be7f69ce2382f29c5a613f"

git clone -q --filter=blob:none https://github.com/fastapi/fastapi.git "$REPO"
for revision in buggy fixed; do
  commit="$BUGGY"
  if [[ "$revision" == fixed ]]; then commit="$FIXED"; fi
  git -C "$REPO" checkout -q "$commit"
  docker run --rm --platform linux/amd64 \
    -v "$REPO:/src:ro" \
    -v "$ROOT/validation/backend/bug-zoo/fastapi-889-probe.py:/probe.py:ro" \
    -v "$REPROIT_BUG_ZOO_TMP/$revision:/out" \
    -e PYTHONPATH=/src \
    "$IMAGE" sh -c \
    'pip install -q --disable-pip-version-check pydantic==1.5.1 starlette==0.12.9 requests==2.23.0 && python /probe.py /out'
done

NULL_BUGGY="12f60cac7a2262231374404c538f0b227f9b9496"
NULL_FIXED="634cf22584fc4fd9ee53cfdf0ad6d48a2830ac34"
mkdir -p \
  "$REPROIT_BUG_ZOO_TMP/fastapi-2719/buggy" \
  "$REPROIT_BUG_ZOO_TMP/fastapi-2719/fixed"
for revision in buggy fixed; do
  commit="$NULL_BUGGY"
  if [[ "$revision" == fixed ]]; then commit="$NULL_FIXED"; fi
  git -C "$REPO" checkout -q "$commit"
  docker run --rm --platform linux/amd64 \
    -v "$REPO:/src:ro" \
    -v "$ROOT/validation/backend/bug-zoo/fastapi-2719-probe.py:/probe.py:ro" \
    -v "$REPROIT_BUG_ZOO_TMP/fastapi-2719/$revision:/out" \
    -e PYTHONPATH=/src \
    "$IMAGE" sh -c \
    'pip install -q --disable-pip-version-check anyio==3.6.2 pydantic==1.9.2 starlette==0.19.1 requests==2.28.1 && python /probe.py /out'
done

cargo run --locked --quiet --manifest-path "$ROOT/validation/backend/bug-zoo/Cargo.toml"
echo "backend BugZoo gate passed"
