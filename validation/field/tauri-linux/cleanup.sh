#!/usr/bin/env bash
# Cleanup phase. Runs after success, failure, cancellation, and timeout. It
# removes every container this campaign owns and then proves none remain; a
# surviving container is a cleanup failure, not a warning.
set -euo pipefail

: "${CAMPAIGN_CONTAINER:?}" "${CAMPAIGN_MIN_CONTAINER:?}"

for name in "$CAMPAIGN_CONTAINER" "$CAMPAIGN_MIN_CONTAINER"; do
  docker rm -f "$name" >/dev/null 2>&1 || true
done

remaining="$(docker ps -a --format '{{.Names}}' \
  | grep -Fx -e "$CAMPAIGN_CONTAINER" -e "$CAMPAIGN_MIN_CONTAINER" || true)"
if [ -n "$remaining" ]; then
  echo "campaign containers survived cleanup: $remaining" >&2
  exit 1
fi
echo "no campaign containers remain"
