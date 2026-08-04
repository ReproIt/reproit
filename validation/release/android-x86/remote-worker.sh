#!/usr/bin/env bash
set -u -o pipefail

BASE="$1"
MODE="$2"
COMMIT="$3"
ARCHIVE_SHA256="$4"
GATE_CSV="$5"
if [[ ! "$BASE" =~ ^\.cache/reproit-android-validation/reproit-android-[0-9]{8}T[0-9]{6}Z-[0-9]+$ ]]; then
  echo "invalid owned remote directory: $BASE" >&2
  exit 2
fi
BASE="$HOME/$BASE"
SOURCE="$BASE/source"
EVIDENCE="$BASE/evidence"
RESULT="$BASE/result.tar.gz"
IMAGE=""
PREP_CONTAINER="reproit-android-x86-prep-${ARCHIVE_SHA256:0:12}-$$"
RUN_CONTAINER="reproit-android-x86-run-${ARCHIVE_SHA256:0:12}-$$"
OWNERSHIP_CONTAINER="reproit-android-x86-owner-${ARCHIVE_SHA256:0:12}-$$"
SDK_ROOT="/home/black/reproit-validation/android-sdk"
DOWNLOADS="/home/black/reproit-validation/downloads"
CACHE="/home/black/reproit-validation/cache/android-x86"
AVD="$BASE/avd"
OVERALL=0

cleanup() {
  docker rm -f \
    "$PREP_CONTAINER" "$RUN_CONTAINER" "$OWNERSHIP_CONTAINER" \
    >/dev/null 2>&1 || true
  if [[ -n "$IMAGE" ]] && docker image inspect "$IMAGE" >/dev/null 2>&1; then
    docker run --rm \
      --network none \
      --volume "$BASE:/owned:Z" \
      "$IMAGE" \
      rm -rf /owned/source /owned/avd >/dev/null 2>&1 || true
  else
    rm -rf "$SOURCE" "$AVD"
  fi
}
trap cleanup EXIT INT TERM

mkdir -p "$SOURCE" "$EVIDENCE" "$CACHE" "$AVD"
tar -xzf "$BASE/source.tar.gz" -C "$SOURCE"
ACTUAL_ARCHIVE_SHA256="$(sha256sum "$BASE/source.tar.gz" | awk '{print $1}')"
if [[ "$ACTUAL_ARCHIVE_SHA256" != "$ARCHIVE_SHA256" ]]; then
  echo "uploaded archive digest mismatch" >&2
  exit 2
fi
DOCKERFILE_SHA256="$(
  sha256sum "$SOURCE/validation/release/android-x86/Dockerfile" | awk '{print $1}'
)"
IMAGE="reproit-android-x86-${DOCKERFILE_SHA256:0:20}"
if [[ "$(git -C "$SOURCE" rev-parse HEAD)" != "$COMMIT" ]]; then
  echo "source commit mismatch" >&2
  exit 2
fi
if [[ "$MODE" == exact && -n "$(git -C "$SOURCE" status --porcelain=v1)" ]]; then
  echo "remote exact source is not clean" >&2
  exit 2
fi
if [[ "$(uname -m)" != x86_64 ]]; then
  echo "remote host is not native x86_64" >&2
  exit 2
fi
if [[ "$(docker info --format '{{.Architecture}}/{{.OSType}}')" != x86_64/linux ]]; then
  echo "remote Docker engine is not native x86_64 Linux" >&2
  exit 2
fi
if [[ ! -c /dev/kvm ]]; then
  echo "remote host does not expose KVM" >&2
  exit 2
fi

python3 - "$EVIDENCE/run-metadata.json" "$MODE" "$COMMIT" \
  "$ARCHIVE_SHA256" "$GATE_CSV" <<'PY'
import datetime
import json
import platform
import socket
import subprocess
import sys

path, mode, commit, archive_sha256, gates = sys.argv[1:]
metadata = {
    "schema": 1,
    "route": "black@zgx-5a09.local -> strix",
    "host": socket.gethostname(),
    "hostOs": platform.system().lower(),
    "hostArchitecture": platform.machine().lower(),
    "docker": subprocess.check_output(
        ["docker", "info", "--format", "{{.Architecture}}/{{.OSType}}"],
        text=True,
        timeout=20,
    ).strip(),
    "sourceMode": mode,
    "baseCommit": commit,
    "sourceArchiveSha256": archive_sha256,
    "gates": gates.split(","),
    "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "processOwnership": "two bounded named containers and one run-scoped AVD directory",
    "readiness": "archive, source, host, Docker, KVM, toolchain, AVD, boot, API, and ABI",
    "reset": "new run-scoped AVD plus -wipe-data and -no-snapshot",
    "networkPolicy": "dependency preparation online, gate runtime Docker network none",
    "cleanup": "container, emulator, ADB, Xvfb, AVD, and remote run-directory traps",
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(metadata, output, indent=2)
    output.write("\n")
PY

docker build \
  --tag "$IMAGE" \
  --file "$SOURCE/validation/release/android-x86/Dockerfile" \
  "$SOURCE/validation/release/android-x86" \
  2>&1 | tee "$EVIDENCE/image-build.log" || OVERALL=1

if ((OVERALL == 0)); then
  docker image inspect "$IMAGE" --format '{{.Id}}' >"$EVIDENCE/image-id.txt"
  python3 - "$EVIDENCE/run-metadata.json" "$DOCKERFILE_SHA256" \
    "$(cat "$EVIDENCE/image-id.txt")" <<'PY'
import json
import sys

path, dockerfile_sha256, image_id = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    metadata = json.load(source)
metadata["workerDockerfileSha256"] = dockerfile_sha256
metadata["workerImageId"] = image_id
with open(path, "w", encoding="utf-8") as output:
    json.dump(metadata, output, indent=2)
    output.write("\n")
PY
  docker run --rm \
    --name "$PREP_CONTAINER" \
    --network bridge \
    --env ANDROID_HOME=/android-sdk \
    --env ANDROID_SDK_ROOT=/android-sdk \
    --env CARGO_HOME=/cache/cargo \
    --env CARGO_TARGET_DIR=/cache/cargo-target \
    --env GRADLE_USER_HOME=/cache/gradle \
    --env PUB_CACHE=/cache/pub \
    --env npm_config_cache=/cache/npm \
    --env REPROIT_ANDROID_GATES="$GATE_CSV" \
    --env REPROIT_SOURCE_ROOT=/repo \
    --volume "$SOURCE:/repo:Z" \
    --volume "$SDK_ROOT:/android-sdk:ro,Z" \
    --volume "$CACHE:/cache:Z" \
    "$IMAGE" \
    bash /repo/validation/release/android-x86/prepare-dependencies.sh \
    2>&1 | tee "$EVIDENCE/dependency-preparation.log" || OVERALL=1
fi

if ((OVERALL == 0)); then
  DEVICE_ARGS=(--device /dev/kvm)
  if [[ -c /dev/dri/renderD128 ]]; then
    DEVICE_ARGS+=(--device /dev/dri/renderD128)
  fi
  if [[ -c /dev/dri/card1 ]]; then
    DEVICE_ARGS+=(--device /dev/dri/card1)
  fi
  # The runtime has no network. Select the installed stable toolchain so
  # rustup does not try to refresh rust-toolchain.toml before resolving rustc.
  docker run --rm \
    --name "$RUN_CONTAINER" \
    --network none \
    "${DEVICE_ARGS[@]}" \
    --env ANDROID_AVD_HOME=/android-avd \
    --env ANDROID_HOME=/android-sdk \
    --env ANDROID_SDK_ROOT=/android-sdk \
    --env CARGO_HOME=/cache/cargo \
    --env CARGO_TARGET_DIR=/cache/cargo-target \
    --env GRADLE_USER_HOME=/cache/gradle \
    --env PUB_CACHE=/cache/pub \
    --env npm_config_cache=/cache/npm \
    --env REPROIT_ANDROID_GATES="$GATE_CSV" \
    --env REPROIT_GATE_OUTPUT_DIR=/evidence \
    --env REPROIT_HOST_GID="$(id -g)" \
    --env REPROIT_HOST_UID="$(id -u)" \
    --env REPROIT_OFFLINE=1 \
    --env REPROIT_RN_TEMPLATE_DIR=/cache/react-native-template-0.76.9 \
    --env REPROIT_RN_TEMPLATE_SHA256_FILE=/cache/react-native-template-0.76.9.sha256 \
    --env REPROIT_SOURCE_ROOT=/repo \
    --env RUSTUP_TOOLCHAIN=stable \
    --volume "$SOURCE:/repo:Z" \
    --volume "$EVIDENCE:/evidence:Z" \
    --volume "$SDK_ROOT:/android-sdk:ro,Z" \
    --volume "$DOWNLOADS:/android-downloads:ro,Z" \
    --volume "$CACHE:/cache:Z" \
    --volume "$AVD:/android-avd:Z" \
    "$IMAGE" \
    bash /repo/validation/release/android-x86/run-isolated.sh \
    2>&1 | tee "$EVIDENCE/isolated-worker.log" || OVERALL=1
  docker run --rm \
    --name "$OWNERSHIP_CONTAINER" \
    --network none \
    --volume "$EVIDENCE:/owned:Z" \
    "$IMAGE" \
    chown -R "$(id -u):$(id -g)" /owned || OVERALL=1
fi

python3 - "$EVIDENCE" "$OVERALL" "$MODE" "$ARCHIVE_SHA256" <<'PY'
import datetime
import json
import sys
from pathlib import Path

directory, status, mode, archive_sha256 = sys.argv[1:]
directory = Path(directory)
path = directory / "run-metadata.json"
with path.open(encoding="utf-8") as source:
    metadata = json.load(source)
evidence_revision = metadata["baseCommit"]
if mode == "current-tree":
    evidence_revision = archive_sha256[:40]
    for result_path in directory.glob("*.json"):
        if result_path == path:
            continue
        with result_path.open(encoding="utf-8") as source:
            result = json.load(source)
        if result.get("gateId"):
            result["commit"] = evidence_revision
            with result_path.open("w", encoding="utf-8") as output:
                json.dump(result, output, indent=2)
                output.write("\n")
metadata["evidenceRevision"] = evidence_revision
metadata["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
metadata["outcome"] = "passed" if status == "0" else "failed"
with path.open("w", encoding="utf-8") as output:
    json.dump(metadata, output, indent=2)
    output.write("\n")
PY
tar -czf "$RESULT" -C "$EVIDENCE" .
exit "$OVERALL"
