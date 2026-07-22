#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP must name the temporary directory}"

APPIUM_LOG="${RUNNER_TEMP}/appium-swiftui.log"
appium --log-level warn --relaxed-security >"$APPIUM_LOG" 2>&1 &
APPIUM_PID=$!

cleanup() {
  kill "$APPIUM_PID" >/dev/null 2>&1 || true
  wait "$APPIUM_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

deadline=$((SECONDS + 30))
until curl --fail --silent --show-error http://127.0.0.1:4723/status >/dev/null; do
  if ! kill -0 "$APPIUM_PID" 2>/dev/null; then
    echo "Appium exited before becoming ready" >&2
    tail -n 100 "$APPIUM_LOG" >&2
    exit 1
  fi
  if ((SECONDS >= deadline)); then
    echo "Appium did not become ready within 30 seconds" >&2
    tail -n 100 "$APPIUM_LOG" >&2
    exit 1
  fi
  sleep 1
done

python3 validation/backends/gate.py swiftui-ios
