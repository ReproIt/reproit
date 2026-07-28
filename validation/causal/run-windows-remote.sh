#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMMIT="${1:-$(git -C "$ROOT" rev-parse HEAD)}"
OUTPUT_DIR="${REPROIT_GATE_OUTPUT_DIR:-$ROOT/target/reproit-validation}"
GATEWAY="black@zgx-5a09.local"
LINUX_HOST="strix"
GUEST="reproit@localhost"
GUEST_PORT="2223"
GUEST_KEY="${REPROIT_WINDOWS_GUEST_KEY:-winlab/vmkey}"

if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "expected a full lowercase Git commit" >&2
  exit 2
fi
SHORT_COMMIT="${COMMIT:0:12}"
mkdir -p "$OUTPUT_DIR"
WORK="$(mktemp -d)"
cleanup() {
  if [[ "${REPROIT_WINDOWS_KEEP_WORK:-0}" == "1" ]]; then
    echo "Windows collector work retained: $WORK" >&2
    return
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

if ! ssh "$GATEWAY" \
  ssh "$LINUX_HOST" \
  ssh -i "$GUEST_KEY" -p "$GUEST_PORT" "$GUEST" \
  powershell.exe -NoProfile -NonInteractive -Command "exit 0" >/dev/null 2>&1; then
  if ! ssh "$GATEWAY" ssh "$LINUX_HOST" pgrep -f qemu-system-x86_64 >/dev/null 2>&1; then
    ssh "$GATEWAY" ssh "$LINUX_HOST" bash winlab/run-vm.sh
  fi
  ssh "$GATEWAY" ssh "$LINUX_HOST" bash winlab/gwait.sh
fi

cat > "$WORK/run.ps1" <<POWERSHELL
\$ErrorActionPreference = "Stop"
\$ProgressPreference = "SilentlyContinue"
\$commit = "${COMMIT}"
\$shortCommit = "${SHORT_COMMIT}"
\$ownedRoot = "C:\lab"
\$checkout = Join-Path \$ownedRoot "reproit-\$shortCommit"
\$evidence = Join-Path \$ownedRoot "evidence-\$shortCommit"
\$archive = Join-Path \$ownedRoot "evidence-\$shortCommit.zip"
\$batch = Join-Path \$ownedRoot "gate-\$shortCommit.bat"
\$done = Join-Path \$ownedRoot "gate-\$shortCommit.done"
\$runLog = Join-Path \$ownedRoot "gate-\$shortCommit.log"
\$task = "reproit-uia-\$shortCommit"

try {
    New-Item -ItemType Directory -Force \$ownedRoot | Out-Null
    foreach (\$path in @(\$checkout, \$evidence, \$archive, \$batch, \$done, \$runLog)) {
        Remove-Item -Recurse -Force \$path -ErrorAction SilentlyContinue
    }
    \$git = (Get-Command git.exe -ErrorAction SilentlyContinue).Source
    if (-not \$git) {
        \$git = "C:\Program Files\Git\cmd\git.exe"
    }
    if (-not (Test-Path \$git)) {
        throw "Git executable is unavailable"
    }
    \$python = (Get-Command python.exe -ErrorAction SilentlyContinue).Source
    if (-not \$python) {
        \$python = Get-ChildItem "\$env:LOCALAPPDATA\Programs\Python\Python*\python.exe" |
            Sort-Object FullName -Descending |
            Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not \$python -or -not (Test-Path \$python)) {
        throw "Python executable is unavailable"
    }

    & \$git clone --filter=blob:none --no-checkout "https://github.com/ReproIt/reproit.git" \$checkout
    if (\$LASTEXITCODE -ne 0) { throw "clone failed" }
    Set-Location \$checkout
    & \$git fetch --no-tags origin \$commit
    if (\$LASTEXITCODE -ne 0) { throw "exact commit fetch failed" }
    & \$git checkout --detach \$commit
    if (\$LASTEXITCODE -ne 0) { throw "exact commit checkout failed" }
    \$actual = (& \$git rev-parse HEAD).Trim()
    if (\$actual -ne \$commit) { throw "exact commit mismatch: \$actual" }
    if (& \$git status --porcelain) { throw "exact checkout has local changes" }

    \$batchText = @"
@echo off
cd /d "\$checkout"
set "REPROIT_GATE_OUTPUT_DIR=\$evidence"
"\$python" validation\backends\gate.py windows-uia > "\$runLog" 2>&1
>"\$done" echo %ERRORLEVEL%
"@
    [System.IO.File]::WriteAllText(
        \$batch,
        \$batchText,
        [System.Text.Encoding]::ASCII
    )
    if (Get-ScheduledTask -TaskName \$task -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName \$task -Confirm:\$false
    }
    & schtasks.exe /create /tn \$task /tr \$batch /sc once /st 00:00 /it /rl highest /f
    if (\$LASTEXITCODE -ne 0) { throw "interactive task creation failed" }
    & schtasks.exe /run /tn \$task
    if (\$LASTEXITCODE -ne 0) { throw "interactive task start failed" }

    \$completed = \$false
    for (\$attempt = 0; \$attempt -lt 240; \$attempt++) {
        if (Test-Path \$done) {
            \$completed = \$true
            break
        }
        Start-Sleep -Seconds 10
    }
    if (-not \$completed) { throw "Windows UIA gate exceeded its bounded wait" }
    \$gateExit = [int](Get-Content \$done -Raw).Trim()
    if (\$gateExit -ne 0) {
        Get-Content \$runLog -Tail 200 -ErrorAction SilentlyContinue
        throw "Windows UIA gate failed with exit \$gateExit"
    }
    Compress-Archive -Path (Join-Path \$evidence "*") -DestinationPath \$archive
    \$bytes = [System.IO.File]::ReadAllBytes(\$archive)
    Write-Output "REPROIT_EVIDENCE_BEGIN"
    Write-Output ([Convert]::ToBase64String(\$bytes))
    Write-Output "REPROIT_EVIDENCE_END"
} finally {
    if (Get-ScheduledTask -TaskName \$task -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName \$task -Confirm:\$false
    }
    Set-Location \$ownedRoot
    foreach (\$path in @(\$checkout, \$evidence, \$archive, \$batch, \$done, \$runLog)) {
        Remove-Item -Recurse -Force \$path -ErrorAction SilentlyContinue
    }
}

POWERSHELL

ssh "$GATEWAY" \
  ssh "$LINUX_HOST" \
  ssh -i "$GUEST_KEY" -p "$GUEST_PORT" "$GUEST" \
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
  -File - <"$WORK/run.ps1" 2>&1 | tee "$WORK/remote.log"

python3 - "$WORK/remote.log" "$WORK/evidence.zip" <<'PY'
import base64
import sys
from pathlib import Path

source = Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
begin = source.rfind("REPROIT_EVIDENCE_BEGIN")
end = source.rfind("REPROIT_EVIDENCE_END")
if begin < 0 or end <= begin:
    raise SystemExit("remote Windows run did not return an evidence archive")
payload = source[begin + len("REPROIT_EVIDENCE_BEGIN"):end]
encoded = "".join(payload.split())
Path(sys.argv[2]).write_bytes(base64.b64decode(encoded, validate=True))
PY

python3 -m zipfile --test "$WORK/evidence.zip"
python3 -m zipfile --extract "$WORK/evidence.zip" "$OUTPUT_DIR"
python3 validation/release/check-native-evidence.py \
  --commit "$COMMIT" \
  --dir windows="$OUTPUT_DIR" \
  --out "$OUTPUT_DIR/windows-exact-commit.json" \
  --only-gate windows-uia
echo "Windows exact-commit evidence collected: $OUTPUT_DIR"
