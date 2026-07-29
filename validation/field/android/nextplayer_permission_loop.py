#!/usr/bin/env python3
"""Exercise NextPlayer's clean-install permission transition on Android."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import TextIO

AFFECTED_REVISION = "b00807bc0ba28b41365c5f4e41e0af2062e7715e"
FIXED_REVISION = "b2875cc4d4e866912c04c26aff8b6fbff9e0de57"
AFFECTED_APK_SHA256 = (
    "96c3c4cd2f01b45f845d68a82ef221785a66f83c742480488514c3b69ed3370e"
)
FIXED_APK_SHA256 = (
    "db6c669e2e7f759034c571ebb9392accdfabf3ca1b85754fb033126ebb50789c"
)
PACKAGE = "dev.anilbeesetti.nextplayer.release"
ACTIVITY = f"{PACKAGE}/dev.anilbeesetti.nextplayer.MainActivity"
PERMISSION = "android.permission.READ_MEDIA_VIDEO"
IDENTITY = "compose-permission:clean-install-prompt-unreachable-loading"
BOUNDS = re.compile(r"\[(\d+),(\d+)]\[(\d+),(\d+)]")
ALLOW_BUTTON_IDS = {
    "com.android.permissioncontroller:id/permission_allow_button",
    "com.android.permissioncontroller:id/permission_allow_all_button",
    "com.android.permissioncontroller:id/permission_allow_foreground_only_button",
}


def run(
    command: list[str],
    *,
    capture: bool = False,
    check: bool = True,
    timeout: int = 120,
    environment: dict[str, str] | None = None,
) -> str:
    result = subprocess.run(
        command,
        check=check,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
        timeout=timeout,
        env=environment,
    )
    return result.stdout.strip() if capture else ""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class Device:
    def __init__(self, sdk: Path, avd_home: Path, evidence: Path) -> None:
        self.sdk = sdk
        self.avd_home = avd_home
        self.evidence = evidence
        self.adb = sdk / "platform-tools" / "adb"
        self.emulator = sdk / "emulator-36.2.12" / "emulator" / "emulator"
        self.avdmanager = sdk / "cmdline-tools" / "latest" / "bin" / "avdmanager"
        self.udid = "emulator-5554"
        self.name = "ReproitField_API36_x86_64"
        self.process: subprocess.Popen[str] | None = None
        self.emulator_log: TextIO | None = None

    def adb_run(
        self,
        *arguments: str,
        capture: bool = False,
        check: bool = True,
        timeout: int = 120,
    ) -> str:
        return run(
            [str(self.adb), "-s", self.udid, *arguments],
            capture=capture,
            check=check,
            timeout=timeout,
        )

    def reset_and_start(self, label: str) -> dict:
        self.stop()
        shutil.rmtree(self.avd_home, ignore_errors=True)
        self.avd_home.mkdir(parents=True)
        environment = os.environ.copy()
        environment["ANDROID_AVD_HOME"] = str(self.avd_home)
        created = subprocess.run(
            [
                str(self.avdmanager),
                "create",
                "avd",
                "--force",
                "--name",
                self.name,
                "--package",
                "system-images;android-36;google_apis;x86_64",
                "--device",
                "pixel_6",
            ],
            check=True,
            input="no\n",
            text=True,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=120,
        )
        (self.evidence / f"{label}-avd-create.log").write_text(
            created.stdout,
            encoding="utf-8",
        )
        self.emulator_log = (self.evidence / f"{label}-emulator.log").open(
            "w",
            encoding="utf-8",
        )
        self.process = subprocess.Popen(
            [
                str(self.emulator),
                f"@{self.name}",
                "-port",
                "5554",
                "-wipe-data",
                "-no-snapshot",
                "-no-window",
                "-no-audio",
                "-no-boot-anim",
                "-no-metrics",
                "-gpu",
                "host",
                "-feature",
                "-Vulkan",
            ],
            text=True,
            stdout=self.emulator_log,
            stderr=subprocess.STDOUT,
            env=environment,
        )
        started = time.monotonic()
        for _ in range(600):
            if self.process.poll() is not None:
                raise RuntimeError(f"emulator exited during {label} boot")
            booted = self.adb_run(
                "shell",
                "getprop",
                "sys.boot_completed",
                capture=True,
                check=False,
                timeout=10,
            )
            if booted.strip() == "1":
                break
            time.sleep(1)
        else:
            raise RuntimeError(f"emulator did not boot during {label}")
        abi = self.adb_run("shell", "getprop", "ro.product.cpu.abi", capture=True)
        api = self.adb_run("shell", "getprop", "ro.build.version.sdk", capture=True)
        avd = self.adb_run("emu", "avd", "name", capture=True).splitlines()[0]
        if (abi, api, avd) != ("x86_64", "36", self.name):
            raise RuntimeError(f"unexpected device identity: {(abi, api, avd)!r}")
        for scale in (
            "window_animation_scale",
            "transition_animation_scale",
            "animator_duration_scale",
        ):
            self.adb_run("shell", "settings", "put", "global", scale, "0")
        return {
            "avd": avd,
            "apiLevel": api,
            "abi": abi,
            "bootCompleted": True,
            "bootSeconds": round(time.monotonic() - started, 3),
            "reset": {
                "avdDirectoryRecreated": True,
                "wipeData": True,
                "snapshotsDisabled": True,
            },
        }

    def stop(self) -> None:
        if self.process is not None:
            try:
                self.adb_run("emu", "kill", check=False, timeout=20)
            except (OSError, subprocess.SubprocessError):
                pass
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
            self.process = None
        if self.emulator_log is not None:
            self.emulator_log.close()
            self.emulator_log = None
        try:
            self.adb_run("kill-server", check=False, timeout=20)
        except (OSError, subprocess.SubprocessError):
            pass
        shutil.rmtree(self.avd_home, ignore_errors=True)

    def source(self) -> str:
        self.adb_run(
            "shell",
            "uiautomator",
            "dump",
            "/sdcard/reproit-window.xml",
            capture=True,
            timeout=30,
        )
        return self.adb_run(
            "exec-out",
            "cat",
            "/sdcard/reproit-window.xml",
            capture=True,
            timeout=30,
        )

    def grant_from_dialog(self, label: str) -> None:
        deadline = time.monotonic() + 30
        source = ""
        while time.monotonic() < deadline:
            source = self.source()
            root = ET.fromstring(source)
            for node in root.iter("node"):
                resource_id = node.attrib.get("resource-id")
                text = node.attrib.get("text", "")
                if resource_id not in ALLOW_BUTTON_IDS and text not in {
                    "Allow",
                    "Allow all",
                    "While using the app",
                }:
                    continue
                match = BOUNDS.fullmatch(node.attrib.get("bounds", ""))
                if match is None:
                    continue
                left, top, right, bottom = map(int, match.groups())
                self.adb_run(
                    "shell",
                    "input",
                    "tap",
                    str((left + right) // 2),
                    str((top + bottom) // 2),
                )
                return
            time.sleep(1)
        (self.evidence / f"{label}-permission-dialog.xml").write_text(
            source,
            encoding="utf-8",
        )
        raise RuntimeError("Android permission dialog did not expose an allow button")

    def permission_granted(self) -> bool:
        value = self.adb_run(
            "shell",
            "dumpsys",
            "package",
            PACKAGE,
            capture=True,
            timeout=60,
        )
        return f"{PERMISSION}: granted=true" in value


def observe(
    device: Device,
    apk: Path,
    label: str,
    expected_identity: str | None,
    pregrant: bool,
) -> dict:
    started = time.monotonic()
    device.adb_run("uninstall", PACKAGE, capture=True, check=False)
    device.adb_run("install", str(apk), timeout=180)
    if pregrant:
        device.adb_run("shell", "pm", "grant", PACKAGE, PERMISSION)
    device.adb_run("logcat", "-c")
    device.adb_run("shell", "am", "start", "-W", "-n", ACTIVITY)
    if not pregrant and expected_identity is None:
        device.grant_from_dialog(label)

    source = ""
    loading = False
    empty = False
    for _ in range(30):
        source = device.source()
        loading = 'class="android.widget.ProgressBar"' in source
        empty = "No videos found" in source
        expected_view_reached = (
            loading if expected_identity is not None else empty
        )
        if expected_view_reached:
            break
        time.sleep(1)
    window_state = device.adb_run(
        "shell",
        "dumpsys",
        "window",
        "windows",
        capture=True,
    )
    foreground = f'package="{PACKAGE}"' in source
    permission_granted = device.permission_granted()
    if expected_identity is None:
        observation_reached = foreground and empty
        identity = None
    else:
        observation_reached = foreground and loading and not empty
        identity = IDENTITY if observation_reached else None
    source_path = device.evidence / f"{label}-source.xml"
    source_path.write_text(source, encoding="utf-8")
    if not observation_reached:
        raise RuntimeError(f"{label} did not reach the media observation")
    if identity != expected_identity:
        raise RuntimeError(
            f"{label} identity was {identity!r}, expected {expected_identity!r}"
        )

    screenshot = device.evidence / f"{label}-screen.png"
    with screenshot.open("wb") as output:
        subprocess.run(
            [str(device.adb), "-s", device.udid, "exec-out", "screencap", "-p"],
            check=True,
            stdout=output,
            timeout=30,
        )
    logcat = device.adb_run(
        "logcat",
        "-d",
        "-t",
        "2000",
        capture=True,
        check=False,
        timeout=60,
    )
    (device.evidence / f"{label}-logcat.log").write_text(logcat, encoding="utf-8")
    return {
        "run": int(label.rsplit("-", 1)[-1]) if label[-1].isdigit() else None,
        "status": "reproduced" if identity else "not_reproduced",
        "identity": identity,
        "cleanLaunch": True,
        "observationReached": observation_reached,
        "exceptions": [],
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "permissionGranted": permission_granted,
        "foreground": foreground,
        "windowStateNamesPackage": PACKAGE in window_state,
        "loadingIndicatorVisible": loading,
        "emptyMediaViewVisible": empty,
        "sourceSha256": f"sha256:{hashlib.sha256(source.encode()).hexdigest()}",
        "screenshotSha256": f"sha256:{sha256(screenshot)}",
    }


def validate_apk(path: Path, expected_sha256: str) -> None:
    if not path.is_file():
        raise SystemExit(f"APK does not exist: {path}")
    actual = sha256(path)
    if actual != expected_sha256:
        raise SystemExit(f"APK digest mismatch for {path}: {actual}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk", required=True, type=Path)
    parser.add_argument("--avd-home", required=True, type=Path)
    parser.add_argument("--affected-apk", required=True, type=Path)
    parser.add_argument("--fixed-apk", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--runs", type=int, choices=range(1, 4), default=3)
    args = parser.parse_args()
    validate_apk(args.affected_apk, AFFECTED_APK_SHA256)
    validate_apk(args.fixed_apk, FIXED_APK_SHA256)
    args.evidence.mkdir(parents=True, exist_ok=True)

    device = Device(args.sdk, args.avd_home, args.evidence)
    affected = []
    fixed = []
    neighbors = {}
    try:
        for variant, apk, expected, records in (
            ("affected", args.affected_apk, IDENTITY, affected),
            ("fixed", args.fixed_apk, None, fixed),
        ):
            for index in range(1, args.runs + 1):
                label = f"{variant}-{index}"
                reset = device.reset_and_start(label)
                record = observe(device, apk, label, expected, False)
                record["device"] = reset
                records.append(record)
        for variant, apk in (
            ("affected", args.affected_apk),
            ("fixed", args.fixed_apk),
        ):
            label = f"neighbor-{variant}"
            reset = device.reset_and_start(label)
            record = observe(device, apk, label, None, True)
            record["device"] = reset
            neighbors[variant] = record
    finally:
        device.stop()

    result = {
        "schemaVersion": 1,
        "target": "compose-android",
        "application": "nextplayer",
        "repository": "https://github.com/anilbeesetti/nextplayer",
        "issue": "https://github.com/anilbeesetti/nextplayer/issues/1820",
        "affectedRevision": AFFECTED_REVISION,
        "fixedRevision": FIXED_REVISION,
        "affectedApkSha256": f"sha256:{AFFECTED_APK_SHA256}",
        "fixedApkSha256": f"sha256:{FIXED_APK_SHA256}",
        "identity": IDENTITY,
        "memoryMeasurement": "unavailable",
        "affected": affected,
        "fixed": fixed,
        "neighboringLegalBehavior": (
            "permission already granted before launch reaches the empty media view "
            "on both revisions"
        ),
        "neighboring": neighbors,
        "minimizedAction": (
            "clean install and launch; observe whether the media permission prompt "
            "is reachable or hidden behind the loading state"
        ),
        "runtime": {
            "platform": "android-emulator/x86_64",
            "apiLevel": 36,
            "avd": device.name,
            "network": "none",
            "reset": "recreate AVD directory and boot with -wipe-data -no-snapshot",
        },
    }
    output = args.evidence / "nextplayer-permission-loop-1825.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
