#!/usr/bin/env bash
# Neighboring-legal-behavior control. On the same affected build the campaign
# just reproduced, the same pointer press on a preset reached without the search
# still selects it. This separates "a preset reached through the search cannot
# be selected by pointer" from "this harness cannot select a preset at all".
set -euo pipefail

: "${CAMPAIGN_MIN_CONTAINER:?}"

result="$(docker exec "$CAMPAIGN_MIN_CONTAINER" node /probe/probe-tauri.mjs ask control)"
echo "$result"

node -e '
  const control = JSON.parse(process.argv[1]);
  if (control.legal !== true) {
    throw new Error(`neighboring legal behavior failed: ${JSON.stringify(control)}`);
  }
' "$result"
