#!/usr/bin/env bash
# Drive one Flutter Android field campaign on the native x86_64 worker.
#
# Every Android system image on the development machine is arm64-v8a, so the
# flutter-android bound of android-emulator/x86_64 is met on the zgx gateway's
# strix host. This script runs there: it launches the pinned lane worker image
# with Docker network mode none, mounts the already-built profile APKs, and
# runs the owned campaign driver inside.
#
# The lane SDK is mounted read-only with SELinux labelling disabled rather than
# with :Z. Two containers cannot share it through :Z, because each relabels it
# exclusively and steals it from the other; that surfaces as a bogus "No
# Android SDK found" in whichever container did not relabel it last.
set -euo pipefail

BASE="${1:?campaign base directory}"
DRIVER="${2:?campaign driver file name}"
COMMIT="${3:?reproit-cli commit}"
RUNS="${4:-3}"

SDK_ROOT="${REPROIT_ANDROID_SDK:-$HOME/reproit-validation/android-sdk}"
SCRIPTS="$BASE/scripts"
EVIDENCE="$BASE/campaign-evidence"
AVD="$BASE/campaign-avd"
PAYLOAD="$BASE/out"
RUN_CONTAINER="reproit-flutter-field-$$"

[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "invalid commit: $COMMIT" >&2; exit 2; }
[[ "$RUNS" =~ ^[1-3]$ ]] || { echo "--runs must be 1, 2, or 3" >&2; exit 2; }
[[ -d "$SCRIPTS" ]] || { echo "no campaign scripts at $SCRIPTS" >&2; exit 2; }
[[ -f "$PAYLOAD/affected.apk" && -f "$PAYLOAD/fixed.apk" ]] || {
  echo "both profile APKs must exist under $PAYLOAD" >&2
  exit 2
}
if [[ "$(sha256sum "$PAYLOAD/affected.apk" | awk '{print $1}')" == \
      "$(sha256sum "$PAYLOAD/fixed.apk" | awk '{print $1}')" ]]; then
  echo "the two application archives are identical, so no pair is under test" >&2
  exit 2
fi
[[ "$(uname -m)" == x86_64 ]] || { echo "worker is not native x86_64" >&2; exit 2; }
[[ -c /dev/kvm ]] || { echo "worker does not expose KVM" >&2; exit 2; }

DOCKERFILE="$BASE/lane-Dockerfile"
[[ -f "$DOCKERFILE" ]] || { echo "no lane Dockerfile at $DOCKERFILE" >&2; exit 2; }
DIGEST="$(sha256sum "$DOCKERFILE" | awk '{print $1}')"
IMAGE="reproit-android-x86-${DIGEST:0:20}"
docker image inspect "$IMAGE" --format '{{.Id}}' >/dev/null || {
  echo "the pinned lane image $IMAGE is not present on this worker" >&2
  exit 2
}
IMAGE_ID="$(docker image inspect "$IMAGE" --format '{{.Id}}')"

cleanup() {
  docker rm -f "$RUN_CONTAINER" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

mkdir -p "$EVIDENCE" "$AVD"
DEVICE_ARGS=(--device /dev/kvm)
if [[ -c /dev/dri/renderD128 ]]; then
  DEVICE_ARGS+=(--device /dev/dri/renderD128)
fi

OVERALL=0
docker run --rm \
  --name "$RUN_CONTAINER" \
  --network none \
  --user 0:0 \
  "${DEVICE_ARGS[@]}" \
  --env ANDROID_AVD_HOME=/android-avd \
  --env ANDROID_HOME=/android-sdk \
  --env ANDROID_SDK_ROOT=/android-sdk \
  --env PYTHONPATH=/campaign/scripts \
  --env REPROIT_CONTAINER_NETWORK=none \
  --env REPROIT_FIELD_AFFECTED_APK=/payload/affected.apk \
  --env REPROIT_FIELD_AVD_HOME=/android-avd/run \
  --env REPROIT_FIELD_CLI_COMMIT="$COMMIT" \
  --env REPROIT_FIELD_DRIVER="$DRIVER" \
  --env REPROIT_FIELD_EVIDENCE=/evidence \
  --env REPROIT_FIELD_FIXED_APK=/payload/fixed.apk \
  --env REPROIT_FIELD_RUNS="$RUNS" \
  --env REPROIT_OFFLINE=1 \
  --env REPROIT_WORKER_IMAGE="$IMAGE@$IMAGE_ID" \
  --volume "$SCRIPTS:/campaign/scripts:ro,Z" \
  --volume "$EVIDENCE:/evidence:Z" \
  --volume "$PAYLOAD:/payload:ro,Z" \
  --volume "$AVD:/android-avd:Z" \
  --security-opt label=disable \
  --volume "$SDK_ROOT:/android-sdk:ro" \
  "$IMAGE" \
  bash /campaign/scripts/run_android_field_driver.sh \
  2>&1 | tee "$EVIDENCE/field-worker.log" || OVERALL=1

docker run --rm \
  --network none \
  --user 0:0 \
  --volume "$EVIDENCE:/owned:Z" \
  "$IMAGE" \
  chown -R "$(id -u):$(id -g)" /owned || OVERALL=1
exit "$OVERALL"
