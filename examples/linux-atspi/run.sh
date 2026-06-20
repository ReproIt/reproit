#!/usr/bin/env bash
# Entrypoint for the reproit linux-atspi validation harness.
#
# Brings up a headless desktop with the AT-SPI accessibility bus, launches the
# GTK3 fixture app, then drives it two ways:
#   PHASE A: the raw runner (runners/linux-atspi.py) - live AT-SPI capture+drive
#   PHASE B: the reproit binary (reproit map / fuzz) orchestrating that runner
#
# Args: pass "shell" to drop into bash after setup (interactive debugging),
#       "runner" to run only phase A, "reproit" for only phase B. Default: all.
set -u

MODE="${1:-all}"

log() { printf '\n=== %s ===\n' "$*"; }

# --- a11y session bus + headless X ------------------------------------------
log "Starting Xvfb on $DISPLAY"
Xvfb "$DISPLAY" -screen 0 1280x900x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
XVFB_PID=$!
sleep 1.5

log "Starting a private D-Bus session bus"
eval "$(dbus-launch --sh-syntax)"
export DBUS_SESSION_BUS_ADDRESS DBUS_SESSION_BUS_PID
echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"

log "Launching the AT-SPI accessibility bus (at-spi-bus-launcher)"
# at-spi2-core ships the bus launcher + registry daemon. Launch the bus, then
# the registry, so apps can register their accessible trees.
ATSPI_LAUNCHER=""
for p in /usr/libexec/at-spi-bus-launcher \
         /usr/lib/at-spi2-core/at-spi-bus-launcher \
         /usr/libexec/at-spi2-core/at-spi-bus-launcher; do
  [ -x "$p" ] && ATSPI_LAUNCHER="$p" && break
done
if [ -z "$ATSPI_LAUNCHER" ]; then
  ATSPI_LAUNCHER="$(command -v at-spi-bus-launcher || true)"
fi
if [ -n "$ATSPI_LAUNCHER" ]; then
  "$ATSPI_LAUNCHER" --launch-immediately >/tmp/atspi-bus.log 2>&1 &
  echo "at-spi-bus-launcher: $ATSPI_LAUNCHER (pid $!)"
else
  echo "WARN: at-spi-bus-launcher not found; relying on D-Bus activation"
fi

ATSPI_REGISTRY=""
for p in /usr/libexec/at-spi2-registryd \
         /usr/lib/at-spi2-core/at-spi2-registryd \
         /usr/libexec/at-spi2-core/at-spi2-registryd; do
  [ -x "$p" ] && ATSPI_REGISTRY="$p" && break
done
if [ -n "$ATSPI_REGISTRY" ]; then
  "$ATSPI_REGISTRY" --use-gnome-session >/tmp/atspi-reg.log 2>&1 &
  echo "at-spi2-registryd: $ATSPI_REGISTRY (pid $!)"
fi

# Tell GTK + the runner accessibility is on.
export GTK_MODULES="${GTK_MODULES:-gail:atk-bridge}"
export NO_AT_BRIDGE=0
sleep 1.5

if [ "$MODE" = "shell" ]; then
  log "Dropping into shell"
  exec bash
fi

# --- parity gate (runs anywhere, no a11y needed) ----------------------------
log "PHASE 0: signature parity gate (python runners)"
python3 /work/runners/test_signature.py
PARITY=$?
echo "parity exit=$PARITY"

# --- launch the fixture app -------------------------------------------------
log "Launching GTK3 fixture app"
python3 /work/fixture_app.py >/tmp/fixture.log 2>&1 &
FIXTURE_PID=$!
sleep 3
if ! kill -0 "$FIXTURE_PID" 2>/dev/null; then
  echo "FATAL: fixture app died on launch"
  cat /tmp/fixture.log
  exit 1
fi
echo "fixture running (pid $FIXTURE_PID)"

# The runner finds the app by AT-SPI application name. GTK reports the app
# name from the program; PyGObject scripts register as "python3" unless
# g_set_prgname is set. The runner matches a substring, so target either the
# window title fragment or "python3". We use the window/app name "Fixture".
export REPROIT_TARGET="${REPROIT_TARGET:-Fixture}"

rc_runner=0
rc_reproit=0

if [ "$MODE" = "all" ] || [ "$MODE" = "runner" ]; then
  log "PHASE A: raw runner live AT-SPI capture + drive (linux-atspi.py)"
  echo "REPROIT_TARGET=$REPROIT_TARGET"
  python3 /work/runners/linux-atspi.py
  rc_runner=$?
  echo "runner exit=$rc_runner"
fi

if [ "$MODE" = "all" ] || [ "$MODE" = "reproit" ]; then
  log "PHASE B: reproit binary orchestration (reproit map)"
  # reproit spawns the runner via `uv run`; the uv shim (see below) maps that
  # to system python3 so apt's python3-gi is used.
  cd /work
  export REPROIT_RUNNERS=/work/runners
  # First build the map (drives the app), then render it for humans.
  reproit map 2>&1 | tee /tmp/reproit-map.log
  rc_reproit=${PIPESTATUS[0]}
  echo "reproit map exit=$rc_reproit"
  if [ -f /work/.reproit/appmap.json ]; then
    log "PHASE B1: reproit map --show (render built graph)"
    reproit map --show 2>&1 | tee /tmp/reproit-map-show.log
  fi
  log "PHASE B2: reproit fuzz (dead-end / invariant oracle, multi-seed)"
  # --frontier paths to the least-visited state (Help) then explores from it,
  # so the explorer reaches the planted dead-end behind "Get Stuck".
  # Default multi-seed batching: reproit writes a {"batch":[...]} config and the
  # linux-atspi.py runner now parses it, emitting SEED:BEGIN/SEED:END per seed
  # (same contract as the Flutter/web/RN runners), so frontier steering and
  # per-seed coverage work without the legacy --batch 1 workaround.
  reproit fuzz --frontier --runs 6 --budget 60 2>&1 | tee /tmp/reproit-fuzz.log
  echo "reproit fuzz exit=${PIPESTATUS[0]}"
  if ls /work/.reproit/runs/*/fuzz.md >/dev/null 2>&1; then
    log "PHASE B3: fuzz finding report (planted dead-end bug)"
    cat /work/.reproit/runs/*/fuzz.md
  fi
fi

log "DONE  parity=$PARITY runner=$rc_runner reproit-map=$rc_reproit"
kill "$FIXTURE_PID" 2>/dev/null
exit 0
