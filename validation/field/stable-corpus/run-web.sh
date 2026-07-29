#!/usr/bin/env bash
set -euo pipefail

engine="${1:?browser engine is required}"
case "$engine" in
  chromium)
    slidev_root=/work/slidev-monaco
    ;;
  firefox|webkit)
    slidev_root=/work/slidev-hash
    ;;
  *)
    echo "unsupported browser engine: $engine" >&2
    exit 2
    ;;
esac

node /field/serve-static-fallback.mjs /work/vert/build 4173 &
vert_server=$!
node /field/serve-static-fallback.mjs "$slidev_root" 4174 &
slidev_server=$!

cleanup() {
  kill "$vert_server" "$slidev_server" >/dev/null 2>&1 || true
  wait "$vert_server" "$slidev_server" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

for _ in $(seq 1 100); do
  if (
    exec 3<>/dev/tcp/127.0.0.1/4173
    exec 4<>/dev/tcp/127.0.0.1/4174
  ) 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.1
done
test "${ready:-0}" = 1 || {
  echo "offline loopback servers did not become ready" >&2
  exit 1
}

xvfb-run -a node /field/stable-corpus/probe-web.mjs \
  "$engine" \
  http://127.0.0.1:4173 \
  http://127.0.0.1:4174
