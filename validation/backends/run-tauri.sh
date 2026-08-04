#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
IMAGE="${REPROIT_TAURI_GATE_IMAGE:-reproit-tauri-backend}"
IMAGE_READY="${REPROIT_TAURI_GATE_IMAGE_READY:-0}"
VOLUME_LABEL="${REPROIT_DOCKER_VOLUME_LABEL:-}"
[[ "$VOLUME_LABEL" == "" || "$VOLUME_LABEL" == ",z" || "$VOLUME_LABEL" == ",Z" ]] || {
  echo "REPROIT_DOCKER_VOLUME_LABEL must be empty, ,z, or ,Z" >&2
  exit 2
}
[[ "$IMAGE_READY" == "0" || "$IMAGE_READY" == "1" ]] || {
  echo "REPROIT_TAURI_GATE_IMAGE_READY must be 0 or 1" >&2
  exit 2
}
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/inner.sh" <<'EOF'
set -euo pipefail
cp -R /repo/fixtures/tauri-fixture /tmp/fixture
cp -R /repo/runners /tmp/runners
mkdir -p /tmp/fixture/src-tauri/icons
base64 -d > /tmp/fixture/src-tauri/icons/icon.png <<'PNG'
iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScL6WQAAAABJRU5ErkJggg==
PNG
npm install --prefix /tmp/runners --no-audit --no-fund webdriverio@9
cargo build --manifest-path /tmp/fixture/src-tauri/Cargo.toml

tauri-driver --native-driver /usr/bin/WebKitWebDriver > /tmp/tauri-driver.log 2>&1 &
DRIVER_PID=$!
trap 'kill "$DRIVER_PID" 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do
  curl -fsS http://127.0.0.1:4444/status >/dev/null 2>&1 && break
  sleep 0.1
done

printf '{"budget":4}' > /tmp/fuzz.json
REPROIT_APP=/tmp/fixture/src-tauri/target/debug/reproit-tauri-fixture \
REPROIT_FUZZ_CONFIG=/tmp/fuzz.json \
node /tmp/runners/tauri.mjs | tee /tmp/run.log

grep -q '^EXPLORE:STATE ' /tmp/run.log
grep -q '^EXPLORE:EDGE ' /tmp/run.log
grep -q 'key:testid:toggle' /tmp/run.log
grep -q 'Detail revealed' /tmp/run.log
grep -q '^EXPLORE:OVERFLOW ' /tmp/run.log
grep -q 'key:id:overflow-message' /tmp/run.log
grep -q '^JOURNEY DONE$' /tmp/run.log
grep -q '^All tests passed$' /tmp/run.log
! grep -q 'EXCEPTION CAUGHT BY REPROIT' /tmp/run.log

printf '{"replay":["tap:key:testid:flicker-positive"],"budget":1}' > /tmp/flicker-positive.json
REPROIT_APP=/tmp/fixture/src-tauri/target/debug/reproit-tauri-fixture \
REPROIT_FUZZ_CONFIG=/tmp/flicker-positive.json \
REPROIT_FLICKER_PIXELS=1 \
REPROIT_FLICKER_DIAGNOSTICS=1 \
node /tmp/runners/tauri.mjs | tee /tmp/flicker-positive.log
grep -q '^EXPLORE:FLICKER ' /tmp/flicker-positive.log

printf '{"replay":["tap:key:testid:flicker-fixed"],"budget":1}' > /tmp/flicker-fixed.json
REPROIT_APP=/tmp/fixture/src-tauri/target/debug/reproit-tauri-fixture \
REPROIT_FUZZ_CONFIG=/tmp/flicker-fixed.json \
REPROIT_FLICKER_PIXELS=1 \
node /tmp/runners/tauri.mjs | tee /tmp/flicker-fixed.log
! grep -q '^EXPLORE:FLICKER ' /tmp/flicker-fixed.log

printf '{"replay":["tap:key:testid:flicker-one-way"],"budget":1}' > /tmp/flicker-one-way.json
REPROIT_APP=/tmp/fixture/src-tauri/target/debug/reproit-tauri-fixture \
REPROIT_FUZZ_CONFIG=/tmp/flicker-one-way.json \
REPROIT_FLICKER_PIXELS=1 \
node /tmp/runners/tauri.mjs | tee /tmp/flicker-one-way.log
! grep -q '^EXPLORE:FLICKER ' /tmp/flicker-one-way.log
echo 'WebCdp backend passed native Tauri/WebKit runtime'
EOF

cat > "$WORK/entry.sh" <<'EOF'
set -euo pipefail
Xvfb :99 -screen 0 1280x800x24 >/tmp/xvfb.log 2>&1 &
export DISPLAY=:99
export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
for _ in $(seq 1 50); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.1; done
exec dbus-run-session -- bash /work/inner.sh
EOF

if [[ "$IMAGE_READY" == "1" ]]; then
  docker image inspect "$IMAGE" >/dev/null
else
  bash "$ROOT/validation/backends/docker-build-retry.sh" \
    -f "$ROOT/validation/backends/tauri.Dockerfile" \
    -t "$IMAGE" \
    "$ROOT/validation/backends"
fi
docker run --rm -v "$ROOT:/repo:ro$VOLUME_LABEL" -v "$WORK:/work:ro$VOLUME_LABEL" \
  "$IMAGE" bash /work/entry.sh
exit 0
