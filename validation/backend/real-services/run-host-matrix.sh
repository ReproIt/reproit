#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
: "${REPROIT_ARM_HOST:?set REPROIT_ARM_HOST to the SSH destination for the ARM Linux host}"
: "${REPROIT_X86_HOST:?set REPROIT_X86_HOST to the SSH destination reachable from the ARM host}"
ARM_LABEL="${REPROIT_ARM_LABEL:-linux-arm64-remote}"
X86_LABEL="${REPROIT_X86_LABEL:-linux-x86_64-remote}"
ARCHIVE="$(mktemp -t reproit-real-backends).tar.gz"
REMOTE_DIR="/tmp/reproit-real-backends"
cleanup() { rm -f "$ARCHIVE"; }
trap cleanup EXIT

COPYFILE_DISABLE=1 tar -C "$REPO" -czf "$ARCHIVE" \
  --exclude='*/artifacts' \
  --exclude='*/node_modules' \
  --exclude='*/target' \
  crates sdk skills runners/web Cargo.toml Cargo.lock \
  validation/backend validation/backends

collect_from_arm() {
  local label="$1"
  mkdir -p "$HERE/artifacts/$label"
  local source_path
  source_path="$REPROIT_ARM_HOST:$REMOTE_DIR/validation/backend/real-services/artifacts/$label/."
  scp -qr "$source_path" "$HERE/artifacts/$label/"
}

REPROIT_HOST_LABEL=macos-local node "$HERE/run.mjs"
scp -q "$ARCHIVE" "$REPROIT_ARM_HOST:/tmp/reproit-real-backends.tar.gz"
ARM_COMMAND="rm -rf '$REMOTE_DIR' && mkdir -p '$REMOTE_DIR' && "
ARM_COMMAND+="tar -xzf /tmp/reproit-real-backends.tar.gz -C '$REMOTE_DIR' && "
ARM_COMMAND+="cd '$REMOTE_DIR' && REPROIT_HOST_LABEL='$ARM_LABEL' "
ARM_COMMAND+="CARGO_TARGET_DIR=/tmp/reproit-real-backend-target "
ARM_COMMAND+="node validation/backend/real-services/run.mjs"
ssh "$REPROIT_ARM_HOST" "$ARM_COMMAND"
collect_from_arm "$ARM_LABEL"

# The x86 host may be reachable only through the ARM host. Stage both the
# source bundle and result archive there without embedding a private route.
X86_COMMAND="rm -rf '$REMOTE_DIR' && mkdir -p '$REMOTE_DIR' && "
X86_COMMAND+="tar -xzf /tmp/reproit-real-backends.tar.gz -C '$REMOTE_DIR' && "
X86_COMMAND+="cd '$REMOTE_DIR' && REPROIT_HOST_LABEL='$X86_LABEL' "
X86_COMMAND+="CARGO_TARGET_DIR=/tmp/reproit-real-backend-target "
X86_COMMAND+="node validation/backend/real-services/run.mjs && "
X86_COMMAND+="tar -C '$REMOTE_DIR/validation/backend/real-services/artifacts' "
X86_COMMAND+="-czf /tmp/reproit-real-backends-x86-results.tar.gz '$X86_LABEL'"
ARM_BRIDGE="scp -q /tmp/reproit-real-backends.tar.gz "
ARM_BRIDGE+="'$REPROIT_X86_HOST:/tmp/reproit-real-backends.tar.gz' && "
ARM_BRIDGE+="ssh '$REPROIT_X86_HOST' \"$X86_COMMAND\" && "
ARM_BRIDGE+="scp -q '$REPROIT_X86_HOST:/tmp/reproit-real-backends-x86-results.tar.gz' "
ARM_BRIDGE+="/tmp/reproit-real-backends-x86-results.tar.gz"
ssh "$REPROIT_ARM_HOST" "$ARM_BRIDGE"
X86_RESULTS="$REPROIT_ARM_HOST:/tmp/reproit-real-backends-x86-results.tar.gz"
scp -q "$X86_RESULTS" /tmp/reproit-real-backends-x86-results.tar.gz
tar -xzf /tmp/reproit-real-backends-x86-results.tar.gz -C "$HERE/artifacts"

echo "macOS, remote ARM64, and remote x86_64 real-service gates passed"
echo "Windows is executed separately through the nested VM route documented in README.md."
