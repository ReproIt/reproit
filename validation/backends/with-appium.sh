#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: with-appium.sh COMMAND [ARG ...]" >&2
  exit 2
fi

APPIUM_PORT="${REPROIT_APPIUM_PORT:-}"
if [[ -z "$APPIUM_PORT" ]]; then
  APPIUM_PORT="$(
    python3 - <<'PY'
import socket

for port in range(4723, 4755):
    with socket.socket() as candidate:
        try:
            candidate.bind(("127.0.0.1", port))
        except OSError:
            continue
        print(port)
        break
else:
    raise SystemExit("no free Appium port in bounded range 4723-4754")
PY
  )"
fi
if [[ ! "$APPIUM_PORT" =~ ^[0-9]+$ ]] \
  || ((APPIUM_PORT < 1024 || APPIUM_PORT > 65535)); then
  echo "with-appium: REPROIT_APPIUM_PORT must be an integer from 1024 to 65535" >&2
  exit 2
fi
readonly APPIUM_PORT
readonly APPIUM_URL="http://127.0.0.1:$APPIUM_PORT"
APPIUM_LOG="$(mktemp "${TMPDIR:-/tmp}/reproit-appium.XXXXXX")"
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

appium --address 127.0.0.1 --port "$APPIUM_PORT" --log-level debug \
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
