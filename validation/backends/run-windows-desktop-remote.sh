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
  crates/llm crates/reproit crates/tui-sig sdk/reproit-backend-rs \
  sdk/reproit-tui-rs templates \
  runners/web skills examples/wpf-fixture examples/avalonia-fixture \
  examples/winui-fixture validation/backends/run-windows-desktop.ps1 \
  validation/backends/run-windows-desktop-interactive.ps1
scp -O -q "$ARCHIVE" "$WINDOWS_HOST:$NAME"

POWERSHELL_COMMAND="$(cat <<EOF
\$d = Join-Path \$env:TEMP 'reproit-backend-validation'
Get-CimInstance Win32_Process |
  Where-Object {
    \$_.Name -eq 'powershell.exe' -and
      \$_.CommandLine -match '-File .*run-windows-desktop'
  } |
  ForEach-Object {
    Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue
  }
\$fixtures = 'reproit','WpfFixture','AvaloniaFixture','WinUiFixture'
Stop-Process -Name \$fixtures -Force -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force \$d -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path \$d | Out-Null
tar -xzf (Join-Path \$HOME '$NAME') -C \$d
\$state = Join-Path \$env:TEMP 'reproit-windows-backends'
New-Item -ItemType Directory -Force -Path \$state | Out-Null
\$result = Join-Path \$state 'interactive.exit'
\$log = Join-Path \$state 'interactive.log'
Remove-Item \$result,\$log -Force -ErrorAction SilentlyContinue
\$script = Join-Path \$d 'validation/backends/run-windows-desktop-interactive.ps1'
\$taskArgs = '-NoProfile -ExecutionPolicy Bypass -File ' + \$script
\$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument \$taskArgs
\$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
\$principalArgs = @{
  UserId = \$user
  LogonType = 'Interactive'
  RunLevel = 'Highest'
}
\$principal = New-ScheduledTaskPrincipal @principalArgs
\$task = @{
  TaskName = 'ReproitBackendGate'
  Action = \$action
  Principal = \$principal
  Force = \$true
}
Register-ScheduledTask @task | Out-Null
Start-ScheduledTask -TaskName 'ReproitBackendGate'
\$deadline = [DateTime]::UtcNow.AddMinutes(15)
while (-not (Test-Path \$result) -and [DateTime]::UtcNow -lt \$deadline) {
  Start-Sleep -Seconds 2
}
if (-not (Test-Path \$result)) {
  throw 'interactive Windows backend gate timed out'
}
Get-Content \$log
\$exit = [int](Get-Content -Raw \$result)
if (\$exit -ne 0) { throw 'interactive Windows backend gate failed' }
EOF
)"
ENCODED="$(printf '%s' "$POWERSHELL_COMMAND" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')"
REMOTE_POWERSHELL="powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass"
REMOTE_POWERSHELL+=" -OutputFormat Text"
ssh "$WINDOWS_HOST" "$REMOTE_POWERSHELL -EncodedCommand $ENCODED"

echo "remote Windows WPF/Avalonia/WinUI UIA matrix passed"
