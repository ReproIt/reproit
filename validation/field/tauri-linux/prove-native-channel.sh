#!/usr/bin/env bash
# Prove the native-window channel before any campaign depends on it.
#
# One container, Xvfb, a session bus, the a11y bus, and the GTK fixture. The
# channel must: see the fixture's window, press its button through the
# accessible action, see the file chooser that appears, type a path into the
# chooser's own entry, accept it, and have the FIXTURE confirm the selection on
# its stdout. A driver that claims success without the fixture agreeing is not a
# channel, so the fixture's own line is the pass condition.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
FIELD="$ROOT/validation/field/tauri-linux"
IMAGE="${REPROIT_TAURI_FIELD_IMAGE:-reproit-field-tauri-linux:amd64}"

docker run --rm --platform linux/amd64 --network none \
  -v "$FIELD:/field:ro" \
  "$IMAGE" bash -c '
    set -euo pipefail
    export HOME=/tmp/home
    mkdir -p "$HOME"
    Xvfb :99 -screen 0 1280x800x24 >/tmp/xvfb.log 2>&1 &
    export DISPLAY=:99 XDG_RUNTIME_DIR=/tmp/xdg
    mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
    for _ in $(seq 1 100); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.1; done
    exec dbus-run-session -- bash /field/prove-native-channel-inner.sh
  '
