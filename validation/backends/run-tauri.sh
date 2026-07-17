#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/Dockerfile" <<'EOF'
FROM rust:1.88-bookworm AS rust-toolchain
FROM node:22-bookworm AS node-toolchain
FROM ubuntu:24.04
COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
COPY --from=node-toolchain /usr/local /usr/local
ENV PATH=/usr/local/cargo/bin:/usr/local/bin:$PATH \
  CARGO_HOME=/usr/local/cargo \
  RUSTUP_HOME=/usr/local/rustup
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config ca-certificates curl \
    libssl-dev libwebkit2gtk-4.1-dev webkit2gtk-driver \
    libayatana-appindicator3-dev librsvg2-dev \
    xvfb dbus dbus-x11 at-spi2-core \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install tauri-driver --locked
EOF

cat > "$WORK/inner.sh" <<'EOF'
set -euo pipefail
cp -R /repo/examples/tauri-fixture /tmp/fixture
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
grep -q '^JOURNEY DONE$' /tmp/run.log
grep -q '^All tests passed$' /tmp/run.log
! grep -q 'EXCEPTION CAUGHT BY REPROIT' /tmp/run.log
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

docker build -t reproit-tauri-backend "$WORK"
docker run --rm -v "$ROOT":/repo:ro -v "$WORK":/work:ro \
  reproit-tauri-backend bash /work/entry.sh
