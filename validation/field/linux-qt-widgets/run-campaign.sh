#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
HERE="$ROOT/validation/field/linux-qt-widgets"
WORK="$(mktemp -d)"
CAMPAIGN_ID="qtw-$(uuidgen | tr '[:upper:]' '[:lower:]')"
IMAGE="reproit-field-linux-qt-widgets:$CAMPAIGN_ID"
PLATFORM="${REPROIT_QT_WIDGETS_PLATFORM:-linux/arm64}"
OUTPUT="${REPROIT_QT_WIDGETS_OUTPUT:-$ROOT/target/reproit-validation/linux-qt-widgets/arm64}"
CONTAINER_PREFIX="reproit-qtwidgets-field-$CAMPAIGN_ID"

cleanup() {
  local status=$?
  local cleanup_failed=0
  local container_ids
  local containers_remaining
  local images_remaining
  trap - EXIT
  set +e
  container_ids="$(docker ps -aq --filter "name=^${CONTAINER_PREFIX}-")"
  if [[ -n "$container_ids" ]]; then
    while IFS= read -r container_id; do
      docker rm -f "$container_id" >/dev/null || cleanup_failed=1
    done <<< "$container_ids"
  fi
  if docker image inspect "$IMAGE" >/dev/null 2>&1; then
    docker image rm -f "$IMAGE" >/dev/null || cleanup_failed=1
  fi
  containers_remaining="$(
    docker ps -aq --filter "name=^${CONTAINER_PREFIX}-" | wc -l | tr -d ' '
  )"
  images_remaining="$(
    docker images -q --filter "reference=$IMAGE" | wc -l | tr -d ' '
  )"
  printf \
    '{"containersRemaining":%s,"imagesRemaining":%s}\n' \
    "$containers_remaining" \
    "$images_remaining" \
    > "$OUTPUT/cleanup.json" \
    || cleanup_failed=1
  find "$WORK" -depth -delete || cleanup_failed=1
  if [[ "$containers_remaining" != 0 || "$images_remaining" != 0 ]]; then
    cleanup_failed=1
  fi
  if [[ "$cleanup_failed" != 0 ]]; then
    exit 1
  fi
  exit "$status"
}
trap cleanup EXIT

bounded() {
  local seconds="$1"
  shift
  perl -e 'alarm shift; exec @ARGV' "$seconds" "$@"
}

archive_revision() {
  local repository="$1"
  local revision="$2"
  local destination="$3"
  git -C "$repository" archive "$revision" | tar -x -C "$destination"
}

mkdir -p \
  "$WORK/qview-affected" \
  "$WORK/qview-fixed" \
  "$WORK/keepassxc-affected" \
  "$WORK/keepassxc-fixed"
if [[ -e "$OUTPUT" ]]; then
  [[ -d "$OUTPUT" ]]
  [[ -z "$(find "$OUTPUT" -mindepth 1 -print -quit)" ]]
else
  mkdir -p "$OUTPUT"
fi

QVIEW_REPOSITORY="${REPROIT_QVIEW_REPOSITORY:?set REPROIT_QVIEW_REPOSITORY}"
KEEPASSXC_REPOSITORY="${REPROIT_KEEPASSXC_REPOSITORY:?set REPROIT_KEEPASSXC_REPOSITORY}"
archive_revision \
  "$QVIEW_REPOSITORY" \
  9f6c225451bb060af8fafd948839432a6de32f4a \
  "$WORK/qview-affected"
archive_revision \
  "$QVIEW_REPOSITORY" \
  e28cbe7b8521959777f40ad6a43b62b4ee243b28 \
  "$WORK/qview-fixed"
archive_revision \
  "$KEEPASSXC_REPOSITORY" \
  caa7d1476134d86c1cf769081d8460933f4cd11c \
  "$WORK/keepassxc-affected"
archive_revision \
  "$KEEPASSXC_REPOSITORY" \
  58a2919650f814e042daf0f51fe7c76705f0288c \
  "$WORK/keepassxc-fixed"
cp "$HERE/Dockerfile" "$WORK/Dockerfile"
cp "$HERE/probe.py" "$WORK/probe.py"
cp "$HERE/atspi_helpers.py" "$WORK/atspi_helpers.py"

bounded 1800 docker build --platform "$PLATFORM" -t "$IMAGE" "$WORK"

for application in qview keepassxc; do
  for revision in affected fixed; do
    for run in 1 2 3; do
      output_file="$OUTPUT/${application}-${revision}-${run}.json"
      bounded 180 docker run --rm \
        --name "${CONTAINER_PREFIX}-${application}-${revision}-${run}" \
        --platform "$PLATFORM" \
        --network none \
        --memory 3g \
        --cpus 4 \
        --pids-limit 512 \
        --tmpfs /tmp:rw,nosuid,nodev,size=512m \
        -e DISPLAY=:99 \
        -v "$OUTPUT:/output" \
        "$IMAGE" \
        --application "$application" \
        --revision "$revision" \
        --run "$run" \
        --output "/output/$(basename "$output_file")"
    done
  done
done

docker image inspect "$IMAGE" > "$OUTPUT/image-inspect.json"
bounded 60 docker run --rm \
  --name "${CONTAINER_PREFIX}-identity" \
  --network none \
  --entrypoint sh \
  -v "$OUTPUT:/output" \
  "$IMAGE" \
  -c 'cp /opt/reproit/build-packages.tsv /output/; \
      cp /opt/reproit/toolchain.txt /output/'
printf 'linux-qt-widgets campaign complete: %s\n' "$PLATFORM"
