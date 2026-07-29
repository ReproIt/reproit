#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
HERE="$ROOT/validation/field/linux-qt-widgets"
WORK="$(mktemp -d)"
OUTPUT="${REPROIT_QT_WIDGETS_OUTPUT:-$ROOT/target/reproit-validation/linux-qt-widgets/amd64}"
GATEWAY="${REPROIT_QT_WIDGETS_GATEWAY:-black@zgx-5a09.local}"
STRIX="${REPROIT_QT_WIDGETS_STRIX_HOST:-strix}"
CAMPAIGN_ID="qtw-$(uuidgen | tr '[:upper:]' '[:lower:]')"
IMAGE="reproit-field-linux-qt-widgets:$CAMPAIGN_ID"
REMOTE_ROOT="/home/black/reproit-qtwidgets-field-$CAMPAIGN_ID"
CONTAINER_PREFIX="reproit-qtwidgets-field-$CAMPAIGN_ID"
SSH_OPTIONS=(
  -o BatchMode=yes
  -o ConnectTimeout=10
  -o ServerAliveInterval=15
  -o ServerAliveCountMax=4
)

on_strix() {
  local seconds="$1"
  local command="$2"
  local encoded_command
  encoded_command="$(
    printf 'set -euo pipefail\n%s\n' "$command" | base64 | tr -d '\n'
  )"
  ssh "${SSH_OPTIONS[@]}" "$GATEWAY" \
    "ssh -o BatchMode=yes -o ConnectTimeout=10 \
      -o ServerAliveInterval=15 -o ServerAliveCountMax=4 '$STRIX' \
      \"printf %s '$encoded_command' | base64 -d \
        | timeout --signal=TERM --kill-after=30s '${seconds}s' bash\""
}

cleanup() {
  local status=$?
  local remote_status
  local local_status
  trap - EXIT
  set +e
  on_strix 120 \
    "container_ids=\$(docker ps -aq --filter 'name=^${CONTAINER_PREFIX}-'); \
      if test -n \"\$container_ids\"; then \
        docker rm -f \$container_ids >/dev/null; \
      fi; \
      if docker image inspect '$IMAGE' >/dev/null 2>&1; then \
        docker image rm -f '$IMAGE' >/dev/null; \
      fi; \
      if test -d '$REMOTE_ROOT'; then find '$REMOTE_ROOT' -depth -delete; fi; \
      test \"\$(docker ps -aq --filter 'name=^${CONTAINER_PREFIX}-')\" = ''; \
      test \"\$(docker images -q --filter 'reference=$IMAGE')\" = ''; \
      test ! -e '$REMOTE_ROOT'"
  remote_status=$?
  find "$WORK" -depth -delete
  local_status=$?
  if [[ "$remote_status" != 0 || "$local_status" != 0 ]]; then
    exit 1
  fi
  exit "$status"
}
trap cleanup EXIT

archive_revision() {
  local repository="$1"
  local revision="$2"
  local destination="$3"
  mkdir -p "$destination"
  git -C "$repository" archive "$revision" | tar -x -C "$destination"
}

QVIEW_REPOSITORY="${REPROIT_QVIEW_REPOSITORY:?set REPROIT_QVIEW_REPOSITORY}"
KEEPASSXC_REPOSITORY="${REPROIT_KEEPASSXC_REPOSITORY:?set REPROIT_KEEPASSXC_REPOSITORY}"
if [[ -e "$OUTPUT" ]]; then
  [[ -d "$OUTPUT" ]]
  [[ -z "$(find "$OUTPUT" -mindepth 1 -print -quit)" ]]
else
  mkdir -p "$OUTPUT"
fi

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

on_strix 60 "mkdir -p '$REMOTE_ROOT/context' '$REMOTE_ROOT/output'"
COPYFILE_DISABLE=1 tar -C "$WORK" -cf - \
  Dockerfile \
  atspi_helpers.py \
  probe.py \
  qview-affected \
  qview-fixed \
  keepassxc-affected \
  keepassxc-fixed \
  | ssh "${SSH_OPTIONS[@]}" "$GATEWAY" \
    "ssh -o BatchMode=yes -o ConnectTimeout=10 \
      -o ServerAliveInterval=15 -o ServerAliveCountMax=4 '$STRIX' \
      timeout --signal=TERM --kill-after=30s 300s \
      tar -C '$REMOTE_ROOT/context' -xf -"
on_strix 60 \
  "test \"\$(find '$REMOTE_ROOT/context' -type f -name '._*' -print -quit)\" = ''"

on_strix 1800 \
  "date +%s > '$REMOTE_ROOT/output/build-start.epoch'; \
    docker build --platform linux/amd64 -t '$IMAGE' '$REMOTE_ROOT/context'; \
    date +%s > '$REMOTE_ROOT/output/build-finish.epoch'"

for application in qview keepassxc; do
  for revision in affected fixed; do
    for run in 1 2 3; do
      name="${CONTAINER_PREFIX}-${application}-${revision}-${run}"
      file="${application}-${revision}-${run}.json"
      on_strix 180 \
        "docker run --rm --name '$name' --platform linux/amd64 \
          --network none --memory 3g --cpus 4 --pids-limit 512 \
          --tmpfs /tmp:rw,nosuid,nodev,size=512m \
          -e DISPLAY=:99 -v '$REMOTE_ROOT/output:/output:Z' '$IMAGE' \
          --application '$application' --revision '$revision' \
          --run '$run' --output '/output/$file'"
    done
  done
done

on_strix 120 \
  "docker image inspect '$IMAGE' > '$REMOTE_ROOT/output/image-inspect.json'; \
    docker run --rm --name '${CONTAINER_PREFIX}-identity' --network none \
      --entrypoint sh -v '$REMOTE_ROOT/output:/output:Z' '$IMAGE' \
      -c 'cp /opt/reproit/build-packages.tsv /output/; \
          cp /opt/reproit/toolchain.txt /output/'; \
    containers=\$(docker ps -aq --filter 'name=^${CONTAINER_PREFIX}-' | wc -l); \
    docker image rm -f '$IMAGE' >/dev/null; \
    images=\$(docker images -q --filter 'reference=$IMAGE' | wc -l); \
    printf '{\"containersRemaining\":%s,\"imagesRemaining\":%s}\\n' \
      \"\$containers\" \"\$images\" \
      > '$REMOTE_ROOT/output/cleanup.json'; \
    test \"\$containers\" -eq 0; \
    test \"\$images\" -eq 0"

on_strix 300 "tar -C '$REMOTE_ROOT/output' -cf - ." | tar -C "$OUTPUT" -xf -
on_strix 120 \
  "find '$REMOTE_ROOT' -depth -delete; test ! -e '$REMOTE_ROOT'; \
    printf 'remoteRootRemaining=0\\n'" \
  > "$OUTPUT/remote-root-cleanup.txt"
printf 'linux-qt-widgets x86 campaign complete: %s\n' "$OUTPUT"
