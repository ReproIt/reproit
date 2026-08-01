#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
HERE="$ROOT/validation/field/linux-wxwidgets"
WORK="$(mktemp -d)"
OUTPUT="${REPROIT_WXWIDGETS_OUTPUT:-$ROOT/target/reproit-validation/linux-wxwidgets/amd64}"
GATEWAY="${REPROIT_WXWIDGETS_GATEWAY:-black@zgx-5a09.local}"
STRIX="${REPROIT_WXWIDGETS_STRIX_HOST:-strix}"
CAMPAIGN_ID="wxw-$(uuidgen | tr '[:upper:]' '[:lower:]')"
IMAGE="reproit-field-linux-wxwidgets:$CAMPAIGN_ID"
REMOTE_ROOT="/home/black/reproit-wxwidgets-field-$CAMPAIGN_ID"
CONTAINER_PREFIX="reproit-wxwidgets-field-$CAMPAIGN_ID"
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

WXMAXIMA_REPOSITORY="${REPROIT_WXMAXIMA_REPOSITORY:?set REPROIT_WXMAXIMA_REPOSITORY}"
POEDIT_REPOSITORY="${REPROIT_POEDIT_REPOSITORY:?set REPROIT_POEDIT_REPOSITORY}"
if [[ -e "$OUTPUT" ]]; then
  [[ -d "$OUTPUT" ]]
  [[ -z "$(find "$OUTPUT" -mindepth 1 -print -quit)" ]]
else
  mkdir -p "$OUTPUT"
fi

archive_revision \
  "$WXMAXIMA_REPOSITORY" \
  e5c410e884c3f1b24c54ac1179c16c3cf0283247 \
  "$WORK/wxmaxima-affected"
archive_revision \
  "$WXMAXIMA_REPOSITORY" \
  684d2ed4e106fc0fc174f5537129bfd13ba68b93 \
  "$WORK/wxmaxima-fixed"
archive_revision \
  "$POEDIT_REPOSITORY" \
  c4fe890ef72c8c8cbc6b9b8cc1784dba10447798 \
  "$WORK/poedit-affected"
archive_revision \
  "$POEDIT_REPOSITORY" \
  f261986fe726a5c2a0ca0717179740c31875de57 \
  "$WORK/poedit-fixed"
cp "$HERE/Dockerfile" "$WORK/Dockerfile"
cp "$HERE/probe.py" "$WORK/probe.py"
cp "$HERE/atspi_helpers.py" "$WORK/atspi_helpers.py"

on_strix 60 "mkdir -p '$REMOTE_ROOT/context' '$REMOTE_ROOT/output'"
COPYFILE_DISABLE=1 tar -C "$WORK" -cf - \
  Dockerfile \
  atspi_helpers.py \
  probe.py \
  wxmaxima-affected \
  wxmaxima-fixed \
  poedit-affected \
  poedit-fixed \
  | ssh "${SSH_OPTIONS[@]}" "$GATEWAY" \
    "ssh -o BatchMode=yes -o ConnectTimeout=10 \
      -o ServerAliveInterval=15 -o ServerAliveCountMax=4 '$STRIX' \
      timeout --signal=TERM --kill-after=30s 300s \
      tar -C '$REMOTE_ROOT/context' -xf -"
on_strix 60 \
  "test \"\$(find '$REMOTE_ROOT/context' -type f -name '._*' -print -quit)\" = ''"

on_strix 2400 \
  "date +%s > '$REMOTE_ROOT/output/build-start.epoch'; \
    docker build --no-cache --platform linux/amd64 -t '$IMAGE' \
      '$REMOTE_ROOT/context'; \
    date +%s > '$REMOTE_ROOT/output/build-finish.epoch'"

bounded_run() {
  local name="$1"
  local file="$2"
  shift 2
  on_strix 300 \
    "docker run --rm --name '$name' --platform linux/amd64 \
      --network none --memory 3g --cpus 4 --pids-limit 512 \
      --tmpfs /tmp:rw,nosuid,nodev,size=512m \
      -e DISPLAY=:99 -v '$REMOTE_ROOT/output:/output:Z' '$IMAGE' \
      $* --output '/output/$file'"
}

for application in wxmaxima poedit; do
  for revision in affected fixed; do
    for run in 1 2 3; do
      bounded_run \
        "${CONTAINER_PREFIX}-${application}-${revision}-${run}" \
        "${application}-${revision}-${run}.json" \
        "--application '$application' --revision '$revision' --run '$run'"
    done
  done
done

# Corpus subjects. Every case runs the fixed revision, so a reported identity
# would be a false positive rather than the campaign defect.
for subject in \
  "wxmaxima default" \
  "wxmaxima neighboring-panes" \
  "poedit default" \
  "poedit missing-source"; do
  # shellcheck disable=SC2086
  set -- $subject
  bounded_run \
    "${CONTAINER_PREFIX}-corpus-$1-$2" \
    "corpus-$1-$2.json" \
    "--application '$1' --revision fixed --run 1 --variant '$2'"
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
printf 'linux-wxwidgets x86 campaign complete: %s\n' "$OUTPUT"
