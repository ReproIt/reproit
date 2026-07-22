#!/usr/bin/env bash
# Qt6 (desktop) RUNTIME e2e for the Linux AT-SPI backend (runners/linux-atspi.py).
# Builds the minimal REAL Qt Widgets app in examples/qt-fixture/main.cpp inside an
# ubuntu:24.04 container (Xvfb + a session bus + the dbus-activated a11y bus, the
# same harness the AT-SPI barrier validation used), forces Qt's AT-SPI a11y
# bridge on, and drives it with the production runner in single-actor EXPLORE
# mode.
#
# WHAT IT PROVES: reproit drives a native Qt6 UI end to end. The AT-SPI tree
# exposes the Qt widgets (the "Toggle" push button surfaces as a tappable), the
# runner reduces the screen to a canonical structural signature (EXPLORE:STATE),
# a real do_action tap on the Qt button structurally changes the app (the hidden
# "extra" panel appears / disappears -> a new structural state -> EXPLORE:EDGE),
# and the walk finishes cleanly (JOURNEY DONE, "All tests passed").
#
# Needs: docker.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
IMAGE=reproit-qt-atspi-e2e

cp "$HERE/main.cpp" "$WORK/main.cpp"
cp "$HERE/qml-main.cpp" "$WORK/qml-main.cpp"
cp "$HERE/main.qml" "$WORK/main.qml"
cp "$ROOT/examples/wxwidgets-fixture/main.cpp" "$WORK/wx-main.cpp"

cat > "$WORK/Dockerfile" <<'EOF'
FROM rust:1.88-bookworm
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    g++ pkg-config qt6-base-dev qt6-declarative-dev \
    qml6-module-qtqml-workerscript qml6-module-qtquick \
    qml6-module-qtquick-controls qml6-module-qtquick-templates \
    qml6-module-qtquick-window libwxgtk3.2-dev \
    libxcb-cursor0 libxkbcommon-x11-0 \
    xvfb dbus at-spi2-core \
    libatspi2.0-0 libatspi2.0-dev \
    && rm -rf /var/lib/apt/lists/*
EOF

cat > "$WORK/assert.py" <<'EOF'
import json
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
lines = text.splitlines()
failures = []


def ok(cond, what):
    print("qt ok: " + what if cond else "qt FAILED: " + what)
    if not cond:
        failures.append(what)


ok("EXCEPTION CAUGHT BY REPROIT" not in text, "no runner exception marker")
ok(any("JOURNEY claimed role=a" in l for l in lines), "session started (JOURNEY claimed)")

states = []
for l in lines:
    if l.startswith("EXPLORE:STATE "):
        try:
            states.append(json.loads(l[len("EXPLORE:STATE "):]))
        except Exception:
            pass
ok(len(states) >= 1, "at least one EXPLORE:STATE captured")
ok(any(isinstance(s.get("sig"), str) and re.fullmatch(r"[0-9a-f]{8}", s["sig"]) for s in states),
   "a state carries a well-formed 8-hex canonical signature")
ok(any(s.get("labels") for s in states),
   "a state carries display labels from the accessibility tree")

# The Qt widgets actually surfaced in the AT-SPI tree.
all_labels = [lbl for s in states for lbl in (s.get("labels") or [])]
ok("toggle" in all_labels, "Qt push button (accessibleName 'toggle') exposed via AT-SPI")
ok("status" in all_labels, "Qt status label (accessibleName 'status') exposed via AT-SPI")
# The hidden 'extra' panel must be ABSENT until the toggle reveals it, then present.
ok(any("extra" not in (s.get("labels") or []) for s in states)
   and any("extra" in (s.get("labels") or []) for s in states),
   "hidden 'extra' panel absent pre-tap and present post-tap (structural toggle)")

actions = []
for l in lines:
    if l.startswith("REPROIT/1 contract") and " runner " in l:
        try:
            event = json.loads(l.split(" runner ", 1)[1])
            if event.get("kind") == "action":
                actions.append(event.get("action", ""))
        except Exception:
            pass
taps = [action for action in actions if action.startswith("tap:")]
misses = [l for l in lines if l.startswith("FUZZ:MISS ")]
ok(len(taps) >= 1, "at least one tap attempted (%d attempted)" % len(taps))
ok(len(taps) > len(misses),
   "at least one tap resolved and clicked (%d attempted, %d missed)" % (len(taps), len(misses)))

ok(any(l.startswith("EXPLORE:EDGE ") for l in lines),
   "a tap structurally changed the Qt app (EXPLORE:EDGE recorded)")

ok(any("JOURNEY DONE" in l for l in lines), "walk finished (JOURNEY DONE)")
ok("All tests passed" in text, 'clean finish ("All tests passed")')

if failures:
    print("qt-atspi-e2e: %d assertion(s) failed" % len(failures))
    sys.exit(1)
print("qt-atspi-e2e: all assertions passed")
EOF

# Runs INSIDE the container, inside dbus-run-session (a fresh session bus).
cat > "$WORK/inner.sh" <<'EOF'
set -euo pipefail
cd /work

# shellcheck disable=SC2046 # pkg-config output is intentionally word-split
g++ -std=c++17 -fPIC main.cpp $(pkg-config --cflags --libs Qt6Widgets) -o fixture || {
    echo "qt build failed" >&2; exit 1;
}

export QT_QPA_PLATFORM=xcb
# Force Qt's AT-SPI a11y bridge on (headless sessions otherwise leave it off).
export QT_ACCESSIBILITY=1
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1

export REPROIT_TARGET=/work/fixture
FUZZ="$(mktemp)"
printf '{"budget":8}' > "$FUZZ"
export REPROIT_FUZZ_CONFIG="$FUZZ"

# Build and run the production AT-SPI backend INSIDE Linux. Building on the host
# made this gate accidentally depend on a Linux host and produced a Mach-O
# binary on macOS; the container is the native runtime boundary for both CPU
# architectures.
cargo build -p reproit --manifest-path /repo/Cargo.toml --target-dir /tmp/reproit-target || {
    echo "reproit Linux build failed" >&2; exit 1;
}
timeout 180 /tmp/reproit-target/debug/reproit __atspi > run.log 2> run.err || true

echo "=== run.log ==="
cat run.log
echo "=== run.err (tail) ==="
tail -30 run.err

python3 assert.py run.log

# Qt Quick/QML has a separate scene graph and accessibility bridge from
# Qt Widgets. Build and drive it independently so Widgets evidence cannot mask
# a QML regression.
g++ -std=c++17 -fPIC qml-main.cpp \
    $(pkg-config --cflags --libs Qt6Quick Qt6Qml) -o qml-fixture || {
    echo "Qt Quick/QML build failed" >&2; exit 1;
}
export REPROIT_TARGET=/work/qml-fixture
timeout 180 /tmp/reproit-target/debug/reproit __atspi > qml.log 2> qml.err || true
echo "=== qml.log ==="
cat qml.log
echo "=== qml.err (tail) ==="
tail -30 qml.err
python3 assert.py qml.log
echo "Qt Quick/QML AT-SPI runtime passed"

g++ -std=c++17 wx-main.cpp $(wx-config --cxxflags --libs) -o wx-fixture
export REPROIT_TARGET=/work/wx-fixture
timeout 180 /tmp/reproit-target/debug/reproit __atspi > wx.log 2> wx.err || true
echo "=== wx.log ==="
cat wx.log
grep -q '^EXPLORE:STATE ' wx.log
grep -q '^EXPLORE:EDGE ' wx.log
grep -qi 'toggle' wx.log
grep -qi 'extra' wx.log
grep -q '^JOURNEY DONE$' wx.log
grep -q '^All tests passed$' wx.log
! grep -q 'EXCEPTION CAUGHT BY REPROIT' wx.log
echo "wxWidgets AT-SPI runtime passed"
EOF

# Container entrypoint: virtual display, then a fresh session bus (the a11y bus
# is dbus-activated by the first AT-SPI client), then the harness.
cat > "$WORK/entry.sh" <<'EOF'
set -uo pipefail
Xvfb :99 -screen 0 1280x800x24 > /dev/null 2>&1 &
export DISPLAY=:99
export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
for _ in $(seq 1 50); do
  [ -e /tmp/.X11-unix/X99 ] && break
  sleep 0.1
done
exec dbus-run-session -- bash /work/inner.sh
EOF

docker build -t "$IMAGE" "$WORK"
docker run --rm \
  -v "$ROOT":/repo:ro \
  -v "$WORK":/work \
  "$IMAGE" bash /work/entry.sh
