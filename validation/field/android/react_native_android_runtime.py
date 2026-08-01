"""Pinned runtime, containment, process ownership, and cleanup evidence."""

from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path
from typing import TYPE_CHECKING

from nextplayer_permission_loop import sha256

if TYPE_CHECKING:
    from nextplayer_permission_loop import Device

APPIUM_VERSION = "3.5.2"
UIAUTOMATOR2_VERSION = "8.0.0"
AVD_SYSTEM_IMAGE = "system-images;android-36;google_apis;x86_64"
AVD_SYSTEM_IMAGE_SHA256 = (
    "eb4bd8cc387915563a0a051c51ac58012e183e1bd21bb0fe2e82f1b255de45a1"
)
EMULATOR_VERSION = "36.2.12"
EMULATOR_SHA256 = (
    "e4b47bf8b25304cf94a5a7c4e30a7224e0c19d196eb098c9f31f02b66f523d39"
)
# The worker builds this image from validation/release/android-x86/Dockerfile and
# tags it reproit-android-x86-<first 20 of the Dockerfile sha256>, so the NAME
# pins the recipe exactly. The image is built per host and never pushed, so its
# content digest is not reproducible across hosts and cannot be pinned here; the
# worker passes the digest it actually built and it is retained as evidence.
WORKER_IMAGE = "reproit-android-x86-07cae9bc60e8a40263ee"
WORKER_IMAGE_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
OWNED_PROCESSES: list[dict] = []


def process_identity(process_id: int) -> dict | None:
    stat_path = Path(f"/proc/{process_id}/stat")
    command_path = Path(f"/proc/{process_id}/cmdline")
    try:
        stat = stat_path.read_text(encoding="utf-8")
        command = command_path.read_bytes().replace(b"\0", b" ").decode().strip()
    except FileNotFoundError:
        return None
    command_end = stat.rfind(")")
    if command_end < 0:
        raise RuntimeError(f"could not parse process stat for PID {process_id}")
    fields_after_command = stat[command_end + 2 :].split()
    if len(fields_after_command) < 20:
        raise RuntimeError(f"process stat was truncated for PID {process_id}")
    return {
        "pid": process_id,
        "startClockTicks": fields_after_command[19],
        "command": command,
    }


def record_owned_process(kind: str, label: str, process_id: int) -> None:
    identity = process_identity(process_id)
    if identity is None:
        raise RuntimeError(f"owned {kind} process exited before identity capture")
    OWNED_PROCESSES.append({"kind": kind, "label": label, **identity})


def has_default_route(route_table: str) -> bool:
    rows = route_table.splitlines()
    for row in rows[1:]:
        fields = row.split()
        if len(fields) < 4:
            raise RuntimeError(f"malformed kernel route row: {row!r}")
        destination = fields[1]
        flags = int(fields[3], 16)
        if destination == "00000000" and flags & 0x2:
            return True
    return False


def _container_route_table() -> tuple[str, bool]:
    route_table = Path("/proc/net/route").read_text(encoding="utf-8").strip()
    return route_table, has_default_route(route_table)


def configure_android_environment(sdk: Path) -> dict:
    sdk_root = sdk.resolve()
    if not sdk_root.is_dir():
        raise RuntimeError(f"Android SDK root does not exist: {sdk_root}")
    sdk_root_text = str(sdk_root)
    os.environ["ANDROID_HOME"] = sdk_root_text
    os.environ["ANDROID_SDK_ROOT"] = sdk_root_text
    return {
        "androidHome": sdk_root_text,
        "androidSdkRoot": sdk_root_text,
    }


def enforce_offline(device: Device) -> dict:
    network_policy = os.environ.get("REPROIT_CONTAINER_NETWORK")
    interfaces = sorted(os.listdir("/sys/class/net"))
    container_routes, has_default_route = _container_route_table()
    if network_policy != "none":
        raise RuntimeError("campaign requires REPROIT_CONTAINER_NETWORK=none")
    if interfaces != ["lo"] or has_default_route:
        raise RuntimeError(
            "campaign container has a non-loopback network path: "
            f"{interfaces!r}, routes={container_routes!r}"
        )
    device.adb_run("shell", "cmd", "connectivity", "airplane-mode", "enable")
    device.adb_run("shell", "svc", "wifi", "disable")
    device.adb_run("shell", "svc", "data", "disable")
    device.adb_run(
        "shell",
        "settings",
        "put",
        "global",
        "mobile_data",
        "0",
    )
    airplane = device.adb_run(
        "shell",
        "cmd",
        "connectivity",
        "airplane-mode",
        capture=True,
    )
    wifi = device.adb_run(
        "shell",
        "settings",
        "get",
        "global",
        "wifi_on",
        capture=True,
    )
    mobile_data = device.adb_run(
        "shell",
        "settings",
        "get",
        "global",
        "mobile_data",
        capture=True,
    )
    if airplane != "enabled" or wifi != "0" or mobile_data not in {"0", "null"}:
        raise RuntimeError(
            "device offline controls did not hold: "
            f"airplane={airplane!r}, wifi={wifi!r}, data={mobile_data!r}"
        )
    return {
        "containerNetworkMode": network_policy,
        "containerInterfaces": interfaces,
        "containerRoutes": container_routes,
        "deviceAirplaneMode": airplane,
        "deviceWifiOn": wifi,
        "deviceMobileData": mobile_data,
    }


def validate_runtime(device: Device) -> dict:
    worker_image = os.environ.get("REPROIT_WORKER_IMAGE", "")
    image_name, _, image_id = worker_image.partition("@")
    if image_name != WORKER_IMAGE or WORKER_IMAGE_ID.match(image_id) is None:
        raise RuntimeError(
            f"worker image provenance was {worker_image!r}, expected "
            f"{WORKER_IMAGE}@sha256:<64 hex>"
        )
    android_environment = configure_android_environment(device.sdk)
    appium_version = subprocess.run(
        ["appium", "--version"],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    ).stdout.strip()
    drivers_output = subprocess.run(
        ["appium", "driver", "list", "--installed", "--json"],
        check=True,
        capture_output=True,
        text=True,
        timeout=60,
    ).stdout
    drivers = json.loads(drivers_output)
    uiautomator2 = drivers.get("uiautomator2", {})
    if appium_version != APPIUM_VERSION:
        raise RuntimeError(f"unexpected Appium version: {appium_version!r}")
    if (
        uiautomator2.get("version") != UIAUTOMATOR2_VERSION
        or uiautomator2.get("installed") is not True
    ):
        raise RuntimeError(f"unexpected UiAutomator2 driver: {uiautomator2!r}")

    system_image = (
        device.sdk
        / "system-images"
        / "android-36"
        / "google_apis"
        / "x86_64"
        / "system.img"
    )
    image_sha256 = sha256(system_image)
    emulator_sha256 = sha256(device.emulator)
    if image_sha256 != AVD_SYSTEM_IMAGE_SHA256:
        raise RuntimeError(f"unexpected Android system image hash: {image_sha256}")
    if emulator_sha256 != EMULATOR_SHA256:
        raise RuntimeError(f"unexpected emulator executable hash: {emulator_sha256}")
    return {
        "workerImage": worker_image,
        **android_environment,
        "appiumVersion": appium_version,
        "uiautomator2Version": uiautomator2["version"],
        "systemImageFile": str(system_image),
        "systemImageFileSha256": f"sha256:{image_sha256}",
        "emulatorExecutable": str(device.emulator),
        "emulatorExecutableSha256": f"sha256:{emulator_sha256}",
    }


def cleanup_audit(device: Device) -> dict:
    adb = subprocess.run(
        [str(device.adb), "devices"],
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    processes = subprocess.run(
        [
            "pgrep",
            "-af",
            (
                "[e]mulator.*ReproitField_API36_x86_64|"
                "[a]ppium.*--address 127.0.0.1"
            ),
        ],
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    exact_processes = []
    for owned in OWNED_PROCESSES:
        current = process_identity(owned["pid"])
        original_gone = (
            current is None
            or current["startClockTicks"] != owned["startClockTicks"]
        )
        exact_processes.append(
            {
                **owned,
                "originalProcessGone": original_gone,
                "currentProcessAtPid": current,
            }
        )
    audit = {
        "avdDirectoryExists": device.avd_home.exists(),
        "adbDevices": adb.stdout.strip().splitlines(),
        "exactOwnedProcesses": exact_processes,
        "ownedProcesses": processes.stdout.strip().splitlines(),
        "containerInterfaces": sorted(os.listdir("/sys/class/net")),
    }
    remaining_devices = [
        line for line in audit["adbDevices"] if line.startswith("emulator-")
    ]
    audit["passed"] = not any(
        (
            audit["avdDirectoryExists"],
            remaining_devices,
            [item for item in exact_processes if not item["originalProcessGone"]],
            audit["ownedProcesses"],
        )
    )
    return audit
