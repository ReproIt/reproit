#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIELD="$ROOT/validation/field/stable-corpus"
OUTPUT="$ROOT/validation/field/corpus"
WORK="$(mktemp -d)"
IMAGE="reproit-stable-corpus-${BASHPID}:amd64"
CONTAINER_PREFIX="reproit-stable-corpus-${BASHPID}"
mode="${1:-all}"

case "$mode" in
  all|web|tui) ;;
  *)
    echo "usage: run-stable-corpus.sh [all|web|tui]" >&2
    exit 2
    ;;
esac

cleanup() {
  for engine in chromium firefox webkit tui; do
    docker rm -f "$CONTAINER_PREFIX-$engine" >/dev/null 2>&1 || true
  done
  if docker image inspect "$IMAGE" >/dev/null 2>&1; then
    docker run --rm \
      -v "$WORK:/work:z" \
      "$IMAGE" \
      chown -R "$(id -u):$(id -g)" /work >/dev/null 2>&1 || true
  fi
  docker image rm -f "$IMAGE" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

test "$(uname -m)" = x86_64 || {
  echo "stable corpus requires native x86_64 Linux" >&2
  exit 2
}
test "$(uname -s)" = Linux || {
  echo "stable corpus requires native x86_64 Linux" >&2
  exit 2
}
test "$(docker info --format '{{.Architecture}}/{{.OSType}}')" = x86_64/linux || {
  echo "stable corpus requires a native x86_64 Linux Docker engine" >&2
  exit 2
}

docker build --platform linux/amd64 -t "$IMAGE" -f "$FIELD/Dockerfile" "$ROOT"
image_id="$(docker image inspect --format '{{.Id}}' "$IMAGE")"

docker run --rm --platform linux/amd64 \
  -v "$WORK:/work:z" \
  -v "$ROOT/validation/field:/field:ro,z" \
  "$IMAGE" \
  bash /field/stable-corpus/prepare.sh "$mode"

write_args=(--image "reproit-stable-corpus:amd64@$image_id" --output "$OUTPUT")
targets=()
if [[ "$mode" != tui ]]; then
  for engine in chromium firefox webkit; do
    docker run --rm \
      --name "$CONTAINER_PREFIX-$engine" \
      --platform linux/amd64 \
      --network none \
      -v "$WORK:/work:z" \
      -v "$ROOT/validation/field:/field:ro,z" \
      "$IMAGE" \
      bash /field/stable-corpus/run-web.sh "$engine" \
      >"$WORK/$engine.json"
    write_args+=(--web "$WORK/$engine.json")
    targets+=("web-$engine")
  done
fi

if [[ "$mode" != web ]]; then
  docker run --rm \
    --name "$CONTAINER_PREFIX-tui" \
    --platform linux/amd64 \
    --network none \
    -v "$WORK:/work:z" \
    -v "$ROOT/validation/field:/field:ro,z" \
    "$IMAGE" \
    python3 /field/stable-corpus/probe-tui.py \
    >"$WORK/tui.json"
  write_args+=(--tui "$WORK/tui.json")
  targets+=(tui)
fi

for engine in chromium firefox webkit tui; do
  docker inspect "$CONTAINER_PREFIX-$engine" >/dev/null 2>&1 && {
    echo "owned corpus container survived: $CONTAINER_PREFIX-$engine" >&2
    exit 1
  }
done

python3 "$FIELD/write-records.py" "${write_args[@]}"

for target in "${targets[@]}"; do
  python3 "$ROOT/validation/field/check-corpus.py" "$OUTPUT/$target.json"
done
