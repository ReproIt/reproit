#!/usr/bin/env bash
set -euo pipefail

ROOT="${REPROIT_SOURCE_ROOT:-/repo}"
EVIDENCE="${REPROIT_GATE_OUTPUT_DIR:-/evidence}"
GATE_CSV="${REPROIT_ANDROID_GATES:-compose-android,react-native-android,flutter-android}"
SDK_ROOT="${ANDROID_SDK_ROOT:-/android-sdk}"
AVD_HOME="${ANDROID_AVD_HOME:-/android-avd}"
AVD_NAME="ReproitValidation_API36_x86_64"
EMULATOR="$SDK_ROOT/emulator-36.2.12/emulator/emulator"
ADB="$SDK_ROOT/platform-tools/adb"
EMULATOR_PORT=5554
UDID="emulator-$EMULATOR_PORT"
EMULATOR_PID=""
XVFB_PID=""
HOST_UID="${REPROIT_HOST_UID:-}"
HOST_GID="${REPROIT_HOST_GID:-}"

stop_owned_process() {
  local process_id="$1"
  kill "$process_id" >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    if ! kill -0 "$process_id" 2>/dev/null; then
      wait "$process_id" >/dev/null 2>&1 || true
      return
    fi
    sleep 1
  done
  kill -KILL "$process_id" >/dev/null 2>&1 || true
  wait "$process_id" >/dev/null 2>&1 || true
}

cleanup() {
  if [[ -n "$EMULATOR_PID" ]]; then
    "$ADB" -s "$UDID" logcat -d -t 4000 \
      >"$EVIDENCE/device-logcat.log" 2>&1 || true
    "$ADB" -s "$UDID" shell dumpsys activity activities \
      >"$EVIDENCE/device-activities.log" 2>&1 || true
    "$ADB" -s "$UDID" emu kill >/dev/null 2>&1 || true
    stop_owned_process "$EMULATOR_PID"
  fi
  if [[ -n "$XVFB_PID" ]]; then
    stop_owned_process "$XVFB_PID"
  fi
  "$ADB" kill-server >/dev/null 2>&1 || true
  if [[ "$HOST_UID" =~ ^[0-9]+$ && "$HOST_GID" =~ ^[0-9]+$ ]]; then
    chown -R "$HOST_UID:$HOST_GID" "$EVIDENCE" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

mkdir -p "$EVIDENCE" "$AVD_HOME"
git config --global --add safe.directory "$ROOT"
python3 "$ROOT/validation/native/preflight.py" android

test "$(uname -m)" = "x86_64"
test -c /dev/kvm
test -x "$EMULATOR"
test -x "$ADB"
test "$(sed -n 's/^Pkg.Revision=//p' \
  "$SDK_ROOT/emulator-36.2.12/emulator/source.properties")" = "36.2.12"
printf '%s  %s\n' \
  "e4b47bf8b25304cf94a5a7c4e30a7224e0c19d196eb098c9f31f02b66f523d39" \
  "$EMULATOR" | sha256sum --check --strict
printf '%s  %s\n' \
  "55e7ce272dd27413855c81b4629c107a1553d07a2f77fa55a6049fdca22f4221" \
  "/android-downloads/emulator-linux_x64-14214601.zip" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "eb4bd8cc387915563a0a051c51ac58012e183e1bd21bb0fe2e82f1b255de45a1" \
  "$SDK_ROOT/system-images/android-36/google_apis/x86_64/system.img" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b" \
  "$SDK_ROOT/platforms/android-35/android.jar" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "6cea1df3efb77103ac3e2beb9bf4718964b0e0869ab16d39d29d5cbae1c147ad" \
  "$SDK_ROOT/platforms/android-34/android.jar" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "760aa057e89e3940c3e944e6bec61eda864ade42fd9c0a3113735e329657705f" \
  "$SDK_ROOT/build-tools/34.0.0/aapt2" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "d1096e11aba9c974644369ee3c50d239acac3f3428ffa928e5b9c14dfb7a57de" \
  "$SDK_ROOT/build-tools/35.0.0/aapt2" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "4dab4ba20f79dc510ce760110d897d07a89d7389c2af162c416d14133a7102c7" \
  "$SDK_ROOT/ndk/28.2.13676358/ndk-build" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "6156dd4e5e466333197a00b8d20bca72c292186643b749dcaaa3aa9164afb1de" \
  "$SDK_ROOT/ndk/28.2.13676358/toolchains/llvm/prebuilt/linux-x86_64/bin/clang" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "c8c39aee0443330e9b1866e1d85cc2405a4eec5dfbb468c5017c3eaecb4964f5" \
  "$SDK_ROOT/cmake/3.22.1/bin/cmake" \
  | sha256sum --check --strict
printf '%s  %s\n' \
  "6fa84be1efc3ab25d1cf397d0bb35891e5f99316a35d89cd8c04be5898730174" \
  "$SDK_ROOT/cmake/3.22.1/bin/ninja" \
  | sha256sum --check --strict
test "$(sed -n 's/^Pkg.Revision=//p' \
  "$SDK_ROOT/platforms/android-35/source.properties")" = "2"
test "$(sed -n 's/^Pkg.Revision=//p' \
  "$SDK_ROOT/platforms/android-34/source.properties")" = "3"
test "$(sed -n 's/^Pkg.Revision=//p' \
  "$SDK_ROOT/build-tools/34.0.0/source.properties" | head -1)" = "34.0.0"
test "$(sed -n 's/^Pkg.Revision=//p' \
  "$SDK_ROOT/build-tools/35.0.0/source.properties" | head -1)" = "35.0.0"
test "$(sed -n 's/^Pkg.Revision = //p' \
  "$SDK_ROOT/ndk/28.2.13676358/source.properties" | head -1)" = "28.2.13676358"
test "$(sed -n 's/^Pkg.Revision = //p' \
  "$SDK_ROOT/cmake/3.22.1/source.properties" | head -1)" = "3.22.1"
test "$(sed -n 's/^AndroidVersion.ApiLevel=//p' \
  "$SDK_ROOT/system-images/android-36/google_apis/x86_64/source.properties")" = "36"
test "$(sed -n 's/^SystemImage.Abi=//p' \
  "$SDK_ROOT/system-images/android-36/google_apis/x86_64/source.properties")" = "x86_64"

rm -rf "$AVD_HOME/$AVD_NAME.avd" "$AVD_HOME/$AVD_NAME.ini"
printf 'no\n' | "$SDK_ROOT/cmdline-tools/latest/bin/avdmanager" create avd \
  --force \
  --name "$AVD_NAME" \
  --package "system-images;android-36;google_apis;x86_64" \
  --device "pixel_6"

Xvfb :99 -screen 0 1280x800x24 >"$EVIDENCE/xvfb.log" 2>&1 &
XVFB_PID=$!
export DISPLAY=:99
display_ready=0
for _ in $(seq 1 30); do
  if glxinfo -B >"$EVIDENCE/glxinfo.log" 2>&1; then
    display_ready=1
    break
  fi
  sleep 1
done
if [[ "$display_ready" != 1 ]]; then
  echo "Xvfb did not expose a GL renderer within its 30-second bound" >&2
  exit 1
fi

"$EMULATOR" "@$AVD_NAME" \
  -port "$EMULATOR_PORT" \
  -wipe-data \
  -no-snapshot \
  -no-window \
  -no-audio \
  -no-boot-anim \
  -no-metrics \
  -gpu host \
  -feature -Vulkan >"$EVIDENCE/emulator.log" 2>&1 &
EMULATOR_PID=$!

booted=0
for _ in $(seq 1 600); do
  if ! kill -0 "$EMULATOR_PID" 2>/dev/null; then
    echo "Android emulator exited before boot completion" >&2
    tail -n 200 "$EVIDENCE/emulator.log" >&2
    exit 1
  fi
  if [[ "$("$ADB" -s "$UDID" shell getprop sys.boot_completed \
      2>/dev/null | tr -d '\r')" == 1 ]]; then
    booted=1
    break
  fi
  sleep 1
done
if [[ "$booted" != 1 ]]; then
  echo "Android emulator did not boot within its 600-second bound" >&2
  exit 1
fi

test "$("$ADB" -s "$UDID" shell getprop ro.product.cpu.abi | tr -d '\r')" = "x86_64"
test "$("$ADB" -s "$UDID" shell getprop ro.build.version.sdk | tr -d '\r')" = "36"
test "$("$ADB" -s "$UDID" emu avd name | head -1 | tr -d '\r')" = "$AVD_NAME"
"$ADB" -s "$UDID" shell settings put global window_animation_scale 0
"$ADB" -s "$UDID" shell settings put global transition_animation_scale 0
"$ADB" -s "$UDID" shell settings put global animator_duration_scale 0

python3 - "$EVIDENCE/device.json" "$UDID" "$AVD_NAME" <<'PY'
import datetime
import json
import os
import subprocess
import sys

path, udid, avd_name = sys.argv[1:]
adb = os.environ["ANDROID_SDK_ROOT"] + "/platform-tools/adb"

def prop(name: str) -> str:
    return subprocess.check_output(
        [adb, "-s", udid, "shell", "getprop", name],
        text=True,
        timeout=20,
    ).strip()

record = {
    "schema": 1,
    "recordedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "udid": udid,
    "avd": avd_name,
    "apiLevel": prop("ro.build.version.sdk"),
    "abi": prop("ro.product.cpu.abi"),
    "bootCompleted": prop("sys.boot_completed"),
    "emulatorVersion": "36.2.12",
    "emulatorBuildId": "14214601",
    "emulatorArchiveSha256":
        "55e7ce272dd27413855c81b4629c107a1553d07a2f77fa55a6049fdca22f4221",
    "systemImageSha256":
        "eb4bd8cc387915563a0a051c51ac58012e183e1bd21bb0fe2e82f1b255de45a1",
    "compileSdk35AndroidJarSha256":
        "4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b",
    "compileSdk34AndroidJarSha256":
        "6cea1df3efb77103ac3e2beb9bf4718964b0e0869ab16d39d29d5cbae1c147ad",
    "buildTools34Aapt2Sha256":
        "760aa057e89e3940c3e944e6bec61eda864ade42fd9c0a3113735e329657705f",
    "buildTools35Aapt2Sha256":
        "d1096e11aba9c974644369ee3c50d239acac3f3428ffa928e5b9c14dfb7a57de",
    "ndkRevision": "28.2.13676358",
    "ndkBuildSha256":
        "4dab4ba20f79dc510ce760110d897d07a89d7389c2af162c416d14133a7102c7",
    "ndkClangSha256":
        "6156dd4e5e466333197a00b8d20bca72c292186643b749dcaaa3aa9164afb1de",
    "cmakeRevision": "3.22.1",
    "cmakeSha256":
        "c8c39aee0443330e9b1866e1d85cc2405a4eec5dfbb468c5017c3eaecb4964f5",
    "cmakeNinjaSha256":
        "6fa84be1efc3ab25d1cf397d0bb35891e5f99316a35d89cd8c04be5898730174",
    "network": "Docker network mode none",
    "reset": "new AVD directory plus emulator -wipe-data and -no-snapshot",
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(record, output, indent=2)
    output.write("\n")
PY

export ANDROID_HOME="$SDK_ROOT"
export ANDROID_SDK_ROOT="$SDK_ROOT"
export ANDROID_SERIAL="$UDID"
export REPROIT_ANDROID_UDID="$UDID"
export REPROIT_GATE_OUTPUT_DIR="$EVIDENCE"
export npm_config_offline=true

overall=0
IFS=, read -r -a GATES <<<"$GATE_CSV"
for gate in "${GATES[@]}"; do
  case "$gate" in
    compose-android|react-native-android|flutter-android) ;;
    *)
      echo "gate is not assigned to the Android x86_64 lane: $gate" >&2
      exit 2
      ;;
  esac
  (
    cd "$ROOT"
    python3 validation/backends/gate.py "$gate" --architecture x86_64
  ) || overall=1
done
exit "$overall"
