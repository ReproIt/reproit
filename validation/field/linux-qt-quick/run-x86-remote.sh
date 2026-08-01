#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
HERE="$ROOT/validation/field/linux-qt-quick"
WORK="$(mktemp -d)"
OUTPUT="${REPROIT_QT_QUICK_OUTPUT:-$ROOT/target/reproit-validation/linux-qt-quick/amd64}"
GATEWAY="${REPROIT_QT_QUICK_GATEWAY:-black@zgx-5a09.local}"
STRIX="${REPROIT_QT_QUICK_STRIX_HOST:-strix}"
CAMPAIGN_ID="qtq-$(uuidgen | tr '[:upper:]' '[:lower:]')"
IMAGE="reproit-field-linux-qt-quick:$CAMPAIGN_ID"
REMOTE_ROOT="/home/black/reproit-qtq-field-$CAMPAIGN_ID"
CONTAINER_PREFIX="reproit-qtq-field-$CAMPAIGN_ID"
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

KALK_REPOSITORY="${REPROIT_KALK_REPOSITORY:?set REPROIT_KALK_REPOSITORY}"
ELISA_REPOSITORY="${REPROIT_ELISA_REPOSITORY:?set REPROIT_ELISA_REPOSITORY}"
if [[ -e "$OUTPUT" ]]; then
  [[ -d "$OUTPUT" ]]
  [[ -z "$(find "$OUTPUT" -mindepth 1 -print -quit)" ]]
else
  mkdir -p "$OUTPUT"
fi

archive_revision \
  "$KALK_REPOSITORY" \
  67e5d3dd76425bf946687586288463c6df9508fe \
  "$WORK/kalk-affected"
archive_revision \
  "$KALK_REPOSITORY" \
  662aa91d58160fca8538f01d7a35253d351214c4 \
  "$WORK/kalk-fixed"
archive_revision \
  "$ELISA_REPOSITORY" \
  8286818ff1c55e9f45c0f64d4600e11655898a90 \
  "$WORK/elisa-affected"
archive_revision \
  "$ELISA_REPOSITORY" \
  cf0f8b41917ec2de61fe6fc89335cf0939568600 \
  "$WORK/elisa-fixed"
cp "$HERE/Dockerfile" "$WORK/Dockerfile"
cp "$HERE/probe.py" "$WORK/probe.py"
cp "$HERE/atspi_helpers.py" "$WORK/atspi_helpers.py"

on_strix 60 "mkdir -p '$REMOTE_ROOT/context' '$REMOTE_ROOT/output'"
COPYFILE_DISABLE=1 tar -C "$WORK" -cf - \
  Dockerfile \
  atspi_helpers.py \
  probe.py \
  kalk-affected \
  kalk-fixed \
  elisa-affected \
  elisa-fixed \
  | ssh "${SSH_OPTIONS[@]}" "$GATEWAY" \
    "ssh -o BatchMode=yes -o ConnectTimeout=10 \
      -o ServerAliveInterval=15 -o ServerAliveCountMax=4 '$STRIX' \
      timeout --signal=TERM --kill-after=30s 300s \
      tar -C '$REMOTE_ROOT/context' -xf -"
on_strix 60 \
  "test \"\$(find '$REMOTE_ROOT/context' -type f -name '._*' -print -quit)\" = ''"

on_strix 2400 \
  "date +%s > '$REMOTE_ROOT/output/build-start.epoch'; \
    docker build --platform linux/amd64 -t '$IMAGE' \
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

for application in kalk elisa; do
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
  "kalk default" \
  "kalk explicit-backspace" \
  "elisa default" \
  "elisa track-title"; do
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
printf 'linux-qt-quick x86 campaign complete: %s\n' "$OUTPUT"
