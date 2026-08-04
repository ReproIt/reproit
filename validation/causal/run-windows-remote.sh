#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMMIT="${1:-$(git -C "$ROOT" rev-parse HEAD)}"
OUTPUT_DIR="${REPROIT_GATE_OUTPUT_DIR:-$ROOT/target/reproit-validation}"
MAX_SOURCE_ARCHIVE_BYTES=$((512 * 1024 * 1024))
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

if [[ -n "$(git -C "$ROOT" status --porcelain=v1)" ]]; then
  echo "Windows exact-commit validation requires a clean worktree" >&2
  exit 2
fi
if [[ "$(git -C "$ROOT" rev-parse HEAD)" != "$COMMIT" ]]; then
  echo "Windows source HEAD does not match the requested commit" >&2
  exit 2
fi
if [[ ! -d "$ROOT/.git" ]]; then
  echo "Windows source packaging requires a regular Git checkout" >&2
  exit 2
fi
SOURCE_ARCHIVE="$WORK/source.tar.gz"
git -C "$ROOT" ls-files -z >"$WORK/source-files"
(
  cd "$ROOT"
  COPYFILE_DISABLE=1 tar --no-xattrs --null -T "$WORK/source-files" \
    -czf "$SOURCE_ARCHIVE" .git
)
SOURCE_ARCHIVE_BYTES="$(wc -c <"$SOURCE_ARCHIVE" | tr -d ' ')"
if ((SOURCE_ARCHIVE_BYTES > MAX_SOURCE_ARCHIVE_BYTES)); then
  echo "Windows source archive exceeds the 512 MiB bound" >&2
  exit 2
fi
SOURCE_ARCHIVE_SHA256="$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')"

if ! ssh "$GATEWAY" \
  ssh "$LINUX_HOST" \
  ssh -i "$GUEST_KEY" -p "$GUEST_PORT" "$GUEST" \
  powershell.exe -NoProfile -NonInteractive -Command "exit 0" >/dev/null 2>&1; then
  if ! ssh "$GATEWAY" ssh "$LINUX_HOST" pgrep -f qemu-system-x86_64 >/dev/null 2>&1; then
    ssh "$GATEWAY" ssh "$LINUX_HOST" bash winlab/run-vm.sh
  fi
  ssh "$GATEWAY" ssh "$LINUX_HOST" bash winlab/gwait.sh
fi

# The exact commit may not be published yet. Upload the clean local checkout
# instead of making native validation depend on an external Git host.
UPLOAD_COMMAND="$(python3 - "$SHORT_COMMIT" <<'PY'
import base64
import sys

short_commit = sys.argv[1]
script = rf'''
$ErrorActionPreference = "Stop"
$ownedRoot = "C:\lab"
$sourceArchive = Join-Path $ownedRoot "source-{short_commit}.tar.gz"
New-Item -ItemType Directory -Force $ownedRoot | Out-Null
Remove-Item -Force $sourceArchive -ErrorAction SilentlyContinue
$encoded = [Console]::In.ReadToEnd()
[System.IO.File]::WriteAllBytes(
    $sourceArchive,
    [Convert]::FromBase64String($encoded)
)
'''
print(base64.b64encode(script.encode("utf-16le")).decode("ascii"))
PY
)"
base64 <"$SOURCE_ARCHIVE" | ssh "$GATEWAY" \
  ssh "$LINUX_HOST" \
  ssh -i "$GUEST_KEY" -p "$GUEST_PORT" "$GUEST" \
  powershell.exe -NoProfile -NonInteractive -EncodedCommand "$UPLOAD_COMMAND"

cat > "$WORK/run.ps1" <<POWERSHELL
\$ErrorActionPreference = "Stop"
\$ProgressPreference = "SilentlyContinue"
\$commit = "${COMMIT}"
\$shortCommit = "${SHORT_COMMIT}"
\$ownedRoot = "C:\lab"
\$checkout = Join-Path \$ownedRoot "reproit-\$shortCommit"
\$sourceArchive = Join-Path \$ownedRoot "source-\$shortCommit.tar.gz"
\$sourceArchiveSha256 = "${SOURCE_ARCHIVE_SHA256}"
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

    if (-not (Test-Path \$sourceArchive)) {
        throw "uploaded source archive is unavailable"
    }
    if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
        throw "tar executable is unavailable"
    }
    \$actualArchiveSha256 = (Get-FileHash -Algorithm SHA256 \$sourceArchive).Hash.ToLowerInvariant()
    if (\$actualArchiveSha256 -ne \$sourceArchiveSha256) {
        throw "uploaded source archive digest mismatch"
    }
    New-Item -ItemType Directory -Force \$checkout | Out-Null
    & tar.exe -xzf \$sourceArchive -C \$checkout
    if (\$LASTEXITCODE -ne 0) { throw "source archive extraction failed" }
    Set-Location \$checkout
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
    \$ownedPaths = @(
        \$checkout, \$sourceArchive, \$evidence, \$archive,
        \$batch, \$done, \$runLog
    )
    foreach (\$path in \$ownedPaths) {
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
