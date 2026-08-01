#!/usr/bin/env bash
# Retain phase. Records the exact environment the campaign ran in so a reviewer
# can audit the result without rerunning it: worker image digest, subject
# repository state, and the proof that no owned container survived.
set -euo pipefail

: "${CAMPAIGN_IMAGE:?}" "${CAMPAIGN_SUBJECT:?}" "${CAMPAIGN_CONTAINER:?}"
: "${CAMPAIGN_MIN_CONTAINER:?}" "${CAMPAIGN_RETAIN:?}"

mkdir -p "$(dirname "$CAMPAIGN_RETAIN")"
{
  echo "worker-image: $CAMPAIGN_IMAGE"
  echo "worker-digest: $(docker image inspect --format '{{.Id}}' "$CAMPAIGN_IMAGE")"
  echo "worker-arch: $(docker image inspect --format '{{.Architecture}}' "$CAMPAIGN_IMAGE")"
  echo "subject-head: $(git -C "$CAMPAIGN_SUBJECT" rev-parse HEAD)"
  echo "subject-dirty: $(git -C "$CAMPAIGN_SUBJECT" status --porcelain -- src src-tauri \
    | wc -l | tr -d ' ')"
  echo "containers-remaining: $(docker ps -a --format '{{.Names}}' \
    | grep -Fx -e "$CAMPAIGN_CONTAINER" -e "$CAMPAIGN_MIN_CONTAINER" | wc -l | tr -d ' ')"
} > "$CAMPAIGN_RETAIN"
cat "$CAMPAIGN_RETAIN"
