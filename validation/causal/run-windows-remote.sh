#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMMIT="${1:-$(git -C "$ROOT" rev-parse HEAD)}"
OUTPUT_DIR="${REPROIT_GATE_OUTPUT_DIR:-$ROOT/target/reproit-validation}"
WINDOWS_CHECKOUT="${REPROIT_WINDOWS_CHECKOUT:-C:\\reproit}"
GATEWAY="black@zgx-5a09.local"
LINUX_HOST="strix"
GUEST="reproit@localhost"
GUEST_PORT="2223"
GUEST_KEY="${REPROIT_WINDOWS_GUEST_KEY:-winlab/vmkey}"

if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "expected a full lowercase Git commit" >&2
  exit 2
fi
mkdir -p "$OUTPUT_DIR"
WORK="$(mktemp -d)"
cleanup() {
  rm -rf "$WORK"
}
trap cleanup EXIT

cat > "$WORK/run.ps1" <<POWERSHELL
\$ErrorActionPreference = "Stop"
\$ProgressPreference = "SilentlyContinue"
\$checkout = "${WINDOWS_CHECKOUT}"
\$commit = "${COMMIT}"
if (-not (Test-Path (Join-Path \$checkout ".git"))) {
    throw "native checkout is missing: \$checkout"
}
Set-Location \$checkout
if (git status --porcelain) {
    throw "native checkout has local changes"
}
git fetch --no-tags origin \$commit
if (\$LASTEXITCODE -ne 0) { throw "fetch failed" }
git checkout --detach \$commit
if (\$LASTEXITCODE -ne 0) { throw "checkout failed" }
\$actual = git rev-parse HEAD
if (\$actual -ne \$commit) { throw "exact commit mismatch: \$actual" }
\$evidence = Join-Path \$env:TEMP "reproit-windows-exact-evidence"
Remove-Item -Recurse -Force \$evidence -ErrorAction SilentlyContinue
python validation/backends/gate.py windows-uia --output-dir \$evidence
if (\$LASTEXITCODE -ne 0) { throw "Windows UIA gate failed" }
\$archive = Join-Path \$env:TEMP "reproit-windows-exact-evidence.zip"
Remove-Item -Force \$archive -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path \$evidence "*") -DestinationPath \$archive
\$bytes = [System.IO.File]::ReadAllBytes(\$archive)
Write-Output "REPROIT_EVIDENCE_BEGIN"
Write-Output ([Convert]::ToBase64String(\$bytes))
Write-Output "REPROIT_EVIDENCE_END"
POWERSHELL

ENCODED="$(
  iconv -f UTF-8 -t UTF-16LE "$WORK/run.ps1" | base64 | tr -d '\r\n'
)"
ssh "$GATEWAY" \
  ssh "$LINUX_HOST" \
  ssh -i "$GUEST_KEY" -p "$GUEST_PORT" "$GUEST" \
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
  -EncodedCommand "$ENCODED" | tee "$WORK/remote.log"

python3 - "$WORK/remote.log" "$WORK/evidence.zip" <<'PY'
import base64
import sys
from pathlib import Path

source = Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
begin = source.find("REPROIT_EVIDENCE_BEGIN")
end = source.find("REPROIT_EVIDENCE_END")
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
