#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: with-appium.sh COMMAND [ARG ...]" >&2
  exit 2
fi

readonly APPIUM_URL="http://127.0.0.1:4723"
APPIUM_LOG="$(mktemp -t reproit-appium)"
readonly APPIUM_LOG
APPIUM_PID=""

# shellcheck disable=SC2329
# Invoked indirectly by the EXIT trap.
cleanup() {
  if [[ -n "$APPIUM_PID" ]]; then
    pkill -TERM -P "$APPIUM_PID" >/dev/null 2>&1 || true
    kill "$APPIUM_PID" >/dev/null 2>&1 || true
    wait "$APPIUM_PID" >/dev/null 2>&1 || true
    pkill -KILL -P "$APPIUM_PID" >/dev/null 2>&1 || true
  fi
  rm -f "$APPIUM_LOG"
}
trap cleanup EXIT

if curl --fail --silent "$APPIUM_URL/status" >/dev/null 2>&1; then
  echo "with-appium: port 4723 is already owned by another Appium server" >&2
  exit 1
fi

appium --address 127.0.0.1 --port 4723 --log-level debug \
  --relaxed-security >"$APPIUM_LOG" 2>&1 &
APPIUM_PID=$!

deadline=$((SECONDS + 30))
until curl --fail --silent "$APPIUM_URL/status" >/dev/null 2>&1; do
  if ! kill -0 "$APPIUM_PID" 2>/dev/null; then
    echo "with-appium: server exited before readiness" >&2
    tail -n 100 "$APPIUM_LOG" >&2
    exit 1
  fi
  if ((SECONDS >= deadline)); then
    echo "with-appium: server did not become ready within its startup bound" >&2
    tail -n 100 "$APPIUM_LOG" >&2
    exit 1
  fi
  sleep 1
done

export REPROIT_APPIUM_URL="$APPIUM_URL"
set +e
"$@"
child_status=$?
set -e
if [[ "$child_status" -ne 0 ]]; then
  tail -n 300 "$APPIUM_LOG" >&2
fi
exit "$child_status"
