#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${REPROIT_WINDOWS_HOST:?set REPROIT_WINDOWS_HOST to an OpenSSH host alias}"
WINDOWS_HOST="$REPROIT_WINDOWS_HOST"
ARCHIVE="$(mktemp -t reproit-windows-native).tar.gz"
NAME="reproit-windows-native.tar.gz"
cleanup() { rm -f "$ARCHIVE"; }
trap cleanup EXIT

COPYFILE_DISABLE=1 tar -czf "$ARCHIVE" -C "$ROOT" \
  sdk/reproit-windows signature_vectors.json validation/causal/run-windows.ps1
scp -O -q "$ARCHIVE" "$WINDOWS_HOST:$NAME"
POWERSHELL_COMMAND="$(cat <<EOF
\$d = Join-Path \$env:TEMP 'reproit-native-validation'
Remove-Item -Recurse -Force \$d -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path \$d | Out-Null
tar -xzf (Join-Path \$HOME '$NAME') -C \$d
& (Join-Path \$d 'validation/causal/run-windows.ps1')
EOF
)"
ENCODED="$(printf '%s' "$POWERSHELL_COMMAND" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')"
REMOTE_POWERSHELL="powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass"
REMOTE_POWERSHELL+=" -OutputFormat Text"
ssh "$WINDOWS_HOST" "$REMOTE_POWERSHELL -EncodedCommand $ENCODED"

echo "remote Windows x86-family causal matrix passed"
