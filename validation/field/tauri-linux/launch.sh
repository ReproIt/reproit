#!/usr/bin/env bash
# Launch phase. Tauri needs a real display, a session bus, and the WebDriver
# stack before the application exists, so this phase brings up Xvfb, dbus, and
# tauri-driver, proves the driver answers, and only then hands the application
# to the probe.
#
# The application under test is supplied by the campaign, not hard-coded here.
#
# The screen is larger than any window the probe asks for: the cc-switch window
# is 1000x650 by configuration, which leaves most of the preset grid outside the
# viewport, and the probe resizes to 1600x1100 so the same preset is pressed at
# the same place every run.
#
# HOME is redirected into the container's own temporary directory. cc-switch
# rewrites ~/.claude and ~/.codex, and a campaign must never write outside the
# disposable worker.
set -euo pipefail

: "${APP_BIN:?}" "${SCENARIO:?}"

export HOME=/tmp/home
mkdir -p "$HOME"

Xvfb :99 -screen 0 1920x1200x24 >/tmp/xvfb.log 2>&1 &
export DISPLAY=:99
export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
for _ in $(seq 1 100); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.1; done
test -e /tmp/.X11-unix/X99 || { echo "Xvfb never came up" >&2; exit 1; }

exec dbus-run-session -- bash -c '
  set -euo pipefail
  tauri-driver --native-driver /usr/bin/WebKitWebDriver > /tmp/tauri-driver.log 2>&1 &
  for _ in $(seq 1 200); do
    curl -fsS http://127.0.0.1:4444/status >/dev/null 2>&1 && break
    sleep 0.1
  done
  curl -fsS http://127.0.0.1:4444/status >/dev/null \
    || { echo "tauri-driver never answered" >&2; cat /tmp/tauri-driver.log >&2; exit 1; }

  exec node /probe/probe-tauri.mjs serve \
    --app "$APP_BIN" \
    --scenario "$SCENARIO" \
    --app-args "${APP_ARGS:-}" \
    --variant "${VARIANT:-default}" \
    --webdriverio /wd/node_modules/webdriverio/build/index.js \
    --driver-url http://127.0.0.1:4444 \
    --port 8931
'
