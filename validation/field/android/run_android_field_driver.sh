#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
XVFB_PID=""

stop_process() {
  local process_id="$1"
  kill "$process_id" >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    if ! kill -0 "$process_id" 2>/dev/null; then
      wait "$process_id" >/dev/null 2>&1 || true
      return
    fi
    sleep 1
  done
  kill -KILL "$process_id" >/dev/null 2>&1 || true
  wait "$process_id" >/dev/null 2>&1 || true
}

cleanup() {
  if [[ -n "$XVFB_PID" ]]; then
    stop_process "$XVFB_PID"
  fi
}
trap cleanup EXIT INT TERM

case "${REPROIT_FIELD_DRIVER:-}" in
  nextplayer_permission_loop.py|greenstash_currency_rotation.py) ;;
  localsend_receive_link.py|gopeed_proxy_credentials.py) ;;
  *)
    echo "unsupported Android field driver: ${REPROIT_FIELD_DRIVER:-}" >&2
    exit 2
    ;;
esac

Xvfb :99 -screen 0 1280x800x24 >"$REPROIT_FIELD_EVIDENCE/xvfb.log" 2>&1 &
XVFB_PID=$!
export DISPLAY=:99
ready=0
for _ in $(seq 1 30); do
  if glxinfo -B >"$REPROIT_FIELD_EVIDENCE/glxinfo.log" 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
if [[ "$ready" != 1 ]]; then
  echo "Xvfb did not expose a GL renderer within its 30-second bound" >&2
  exit 1
fi

python3 "$SCRIPT_DIR/$REPROIT_FIELD_DRIVER" \
  --sdk /android-sdk \
  --avd-home "$REPROIT_FIELD_AVD_HOME" \
  --affected-apk "$REPROIT_FIELD_AFFECTED_APK" \
  --fixed-apk "$REPROIT_FIELD_FIXED_APK" \
  --evidence "$REPROIT_FIELD_EVIDENCE" \
  --cli-commit "$REPROIT_FIELD_CLI_COMMIT" \
  --runs "${REPROIT_FIELD_RUNS:-3}"
