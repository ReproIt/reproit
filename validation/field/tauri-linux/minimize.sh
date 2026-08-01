#!/usr/bin/env bash
# Minimize phase. The campaign's trigger is already the smallest executable
# action for its scenario. This phase proves it by replaying only that action
# against a freshly staged affected build and requiring the exact identity to
# survive. The container is left running for the neighboring-legal-behavior
# control that follows.
set -euo pipefail

: "${CAMPAIGN_IMAGE:?}" "${CAMPAIGN_MIN_CONTAINER:?}" "${CAMPAIGN_SUBJECT:?}"
: "${CAMPAIGN_FIELD:?}" "${CAMPAIGN_AFFECTED:?}"
: "${CAMPAIGN_PROBE:?}" "${CAMPAIGN_IDENTITY:?}" "${CAMPAIGN_STAGE:?}"
: "${APP_BIN:?}" "${SCENARIO:?}"

docker rm -f "$CAMPAIGN_MIN_CONTAINER" >/dev/null 2>&1 || true

docker run --rm --platform linux/amd64 \
  -e revision="$CAMPAIGN_AFFECTED" \
  -v "$CAMPAIGN_SUBJECT:/work" -v "$CAMPAIGN_FIELD:/field:ro" \
  "$CAMPAIGN_IMAGE" bash "$CAMPAIGN_STAGE" >/dev/null

# bash 3.2 on the macOS host treats an empty array expansion as unset, so the
# mount is built as a plain string rather than an array.
books_mount=""
if [ -n "${CAMPAIGN_BOOKS:-}" ]; then
  books_mount="-v $CAMPAIGN_BOOKS:/books:ro"
fi

# shellcheck disable=SC2086
docker run -d --name "$CAMPAIGN_MIN_CONTAINER" --platform linux/amd64 --network none \
  -e APP_BIN="$APP_BIN" -e SCENARIO="$SCENARIO" -e APP_ARGS="${APP_ARGS:-}" \
  -v "$CAMPAIGN_SUBJECT:/work" -v "$CAMPAIGN_FIELD:/field:ro" \
  -v "$CAMPAIGN_PROBE:/probe:ro" $books_mount \
  "$CAMPAIGN_IMAGE" bash /field/launch.sh >/dev/null

ready=""
for _ in $(seq 1 90); do
  if docker exec "$CAMPAIGN_MIN_CONTAINER" \
      node /probe/probe-tauri.mjs ask readiness >/dev/null 2>&1; then
    ready=yes
    break
  fi
  sleep 2
done
test -n "$ready" || { echo "minimized replay never became ready" >&2; exit 1; }

docker exec "$CAMPAIGN_MIN_CONTAINER" node /probe/probe-tauri.mjs ask trigger >/dev/null
observation="$(docker exec "$CAMPAIGN_MIN_CONTAINER" node /probe/probe-tauri.mjs ask observe)"
echo "$observation"

node -e '
  const observation = JSON.parse(process.argv[1]);
  if (observation.identity !== process.argv[2]) {
    throw new Error(`minimized trigger lost the identity: ${observation.identity}`);
  }
' "$observation" "$CAMPAIGN_IDENTITY"
