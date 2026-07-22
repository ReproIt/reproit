#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${REPROIT_WINDOWS_HOST:?set REPROIT_WINDOWS_HOST to an OpenSSH host alias}"
WINDOWS_HOST="$REPROIT_WINDOWS_HOST"
ARCHIVE="$(mktemp -t reproit-backends).tar.gz"
NAME="reproit-backends.tar.gz"
cleanup() { rm -f "$ARCHIVE"; }
trap cleanup EXIT

COPYFILE_DISABLE=1 tar -czf "$ARCHIVE" -C "$ROOT" \
  --exclude='._*' --exclude='*/._*' --exclude='.DS_Store' --exclude='*/.DS_Store' \
  --exclude='*/node_modules' --exclude='*/target' \
  Cargo.toml Cargo.lock signature_vectors.json tui_signature_vectors.json \
  crates/llm crates/reproit-protocol crates/reproit crates/tui-sig \
  sdk/reproit-backend-rs \
  sdk/reproit-tui-rs \
  runners/web skills examples/wpf-fixture examples/avalonia-fixture \
  examples/winui-fixture validation/backends/invoke-windows-desktop.ps1 \
  validation/backends/run-windows-desktop.ps1 \
  validation/backends/run-windows-desktop-interactive.ps1
scp -O -q "$ARCHIVE" "$WINDOWS_HOST:$NAME"

POWERSHELL_COMMAND="$(cat <<EOF
\$d = Join-Path \$env:TEMP 'reproit-backend-validation'
\$taskName = 'ReproitBackendGate'
Get-CimInstance Win32_Process |
  Where-Object {
    \$_.Name -eq 'powershell.exe' -and
      \$_.ProcessId -ne \$PID -and
      \$_.CommandLine -match '-EncodedCommand'
  } |
  ForEach-Object {
    Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue
  }
Stop-ScheduledTask -TaskName \$taskName -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Unregister-ScheduledTask -TaskName \$taskName -Confirm:\$false -ErrorAction SilentlyContinue
Get-CimInstance Win32_Process |
  Where-Object {
    \$_.Name -eq 'powershell.exe' -and
      \$_.CommandLine -match '-File .*run-windows-desktop'
  } |
  ForEach-Object {
    Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue
  }
\$fixtures = 'cargo','rustc','reproit','WpfFixture','AvaloniaFixture','WinUiFixture'
Stop-Process -Name \$fixtures -Force -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force \$d -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path \$d | Out-Null
tar -xzf (Join-Path \$HOME '$NAME') -C \$d
Remove-Item (Join-Path \$HOME '$NAME') -Force
& (Join-Path \$d 'validation/backends/invoke-windows-desktop.ps1')
EOF
)"
ENCODED="$(printf '%s' "$POWERSHELL_COMMAND" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')"
REMOTE_POWERSHELL="powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass"
REMOTE_POWERSHELL+=" -OutputFormat Text"
ssh "$WINDOWS_HOST" "$REMOTE_POWERSHELL -EncodedCommand $ENCODED"

echo "remote Windows WPF/Avalonia/WinUI UIA matrix passed"
