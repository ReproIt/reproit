#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP must name the evidence directory parent}"

export REPROIT_GATE_OUTPUT_DIR="${RUNNER_TEMP}/native-gates"
APPIUM_LOG="${RUNNER_TEMP}/appium-android.log"
appium --log-level warn --relaxed-security >"$APPIUM_LOG" 2>&1 &
APPIUM_PID=$!

cleanup() {
  kill "$APPIUM_PID" >/dev/null 2>&1 || true
  wait "$APPIUM_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

ready=false
for _ in $(seq 1 30); do
  if curl --fail --silent --show-error http://127.0.0.1:4723/status >/dev/null; then
    ready=true
    break
  fi
  sleep 1
done
if [[ "$ready" != true ]]; then
  echo "Appium did not become ready within 30 seconds" >&2
  tail -200 "$APPIUM_LOG" >&2
  exit 1
fi

python3 validation/backends/gate.py compose-android
if [[ "${GITHUB_EVENT_NAME:-}" == workflow_dispatch ]]; then
  python3 validation/backends/gate.py react-native-android
fi
