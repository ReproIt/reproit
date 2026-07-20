#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${REPROIT_WINDOWS_HOST:?set REPROIT_WINDOWS_HOST to an OpenSSH host alias}"
WINDOWS_HOST="$REPROIT_WINDOWS_HOST"
ARCHIVE="$(mktemp -t reproit-backend-windows).tar.gz"
NAME="reproit-backend-windows.tar.gz"
cleanup() { rm -f "$ARCHIVE"; }
trap cleanup EXIT

tar -czf "$ARCHIVE" -C "$ROOT" \
  --exclude='runners/web/node_modules' \
  --exclude='**/target' \
  Cargo.toml Cargo.lock crates templates skills validation runners/web \
  sdk/reproit-backend-rs sdk/reproit-tui-rs \
  signature_vectors.json tui_signature_vectors.json
scp -O -q "$ARCHIVE" "$WINDOWS_HOST:$NAME"

POWERSHELL_COMMAND="$(cat <<EOF
\$d = Join-Path \$env:TEMP 'reproit-backend-validation'
Remove-Item -Recurse -Force \$d -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path \$d | Out-Null
tar -xzf (Join-Path \$HOME '$NAME') -C \$d
& (Join-Path \$d 'validation/backend/run.ps1')
EOF
)"
ENCODED="$(printf '%s' "$POWERSHELL_COMMAND" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')"
REMOTE_POWERSHELL="powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass"
REMOTE_POWERSHELL+=" -OutputFormat Text"
ssh "$WINDOWS_HOST" "$REMOTE_POWERSHELL -EncodedCommand $ENCODED"

echo "remote Windows x86 backend structural gate passed"
