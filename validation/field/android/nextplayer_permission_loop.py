#!/usr/bin/env python3
"""Exercise NextPlayer's clean-install permission transition on Android."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import signal
import shutil
import socket
import subprocess
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Callable, TextIO
from urllib import error, request

AFFECTED_REVISION = "b00807bc0ba28b41365c5f4e41e0af2062e7715e"
FIXED_REVISION = "b2875cc4d4e866912c04c26aff8b6fbff9e0de57"
AFFECTED_APK_SHA256 = (
    "96c3c4cd2f01b45f845d68a82ef221785a66f83c742480488514c3b69ed3370e"
)
FIXED_APK_SHA256 = (
    "db6c669e2e7f759034c571ebb9392accdfabf3ca1b85754fb033126ebb50789c"
)
PACKAGE = "dev.anilbeesetti.nextplayer.release"
APP_ACTIVITY = "dev.anilbeesetti.nextplayer.MainActivity"
PERMISSION = "android.permission.READ_MEDIA_VIDEO"
IDENTITY = "compose-permission:clean-install-prompt-unreachable-loading"
BOUNDS = re.compile(r"\[(\d+),(\d+)]\[(\d+),(\d+)]")
ALLOW_BUTTON_IDS = {
    "com.android.permissioncontroller:id/permission_allow_button",
    "com.android.permissioncontroller:id/permission_allow_all_button",
    "com.android.permissioncontroller:id/permission_allow_foreground_only_button",
}
ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"


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


class AppiumSession:
    def __init__(
        self,
        appium_url: str,
        udid: str,
        package: str,
        activity: str,
    ) -> None:
        self.appium_url = appium_url.rstrip("/")
        self.requested_capabilities = {
            "platformName": "Android",
            "appium:automationName": "UiAutomator2",
            "appium:udid": udid,
            "appium:appPackage": package,
            "appium:appActivity": activity,
            "appium:noReset": True,
            "appium:forceAppLaunch": True,
            "appium:newCommandTimeout": 600,
            "appium:adbExecTimeout": 120000,
            "appium:disableWindowAnimation": True,
        }
        response = self._request(
            "POST",
            "/session",
            {"capabilities": {"alwaysMatch": self.requested_capabilities}},
        )
        value = response.get("value", {})
        self.session_id = value.get("sessionId") or response.get("sessionId")
        if not isinstance(self.session_id, str) or not self.session_id:
            raise RuntimeError(f"Appium returned no session id: {response!r}")
        self.returned_capabilities = value.get("capabilities", {})

    def _request(
        self,
        method: str,
        path: str,
        payload: dict | None = None,
        timeout: int = 120,
    ) -> dict:
        body = None if payload is None else json.dumps(payload).encode()
        operation = request.Request(
            f"{self.appium_url}{path}",
            data=body,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        try:
            with request.urlopen(operation, timeout=timeout) as response:
                result = json.loads(response.read().decode())
        except error.HTTPError as failure:
            detail = failure.read().decode(errors="replace")
            raise RuntimeError(
                f"Appium {method} {path} failed with {failure.code}: {detail}"
            ) from failure
        if not isinstance(result, dict):
            raise RuntimeError(f"Appium {method} {path} returned a non-object")
        value = result.get("value")
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(f"Appium {method} {path} failed: {value!r}")
        return result

    def close(self) -> None:
        if not getattr(self, "session_id", None):
            return
        try:
            self._request("DELETE", f"/session/{self.session_id}")
        finally:
            self.session_id = ""

    def source(self) -> str:
        result = self._request("GET", f"/session/{self.session_id}/source")
        source = result.get("value")
        if not isinstance(source, str):
            raise RuntimeError("Appium source response was not text")
        return source

    def screenshot(self, output: Path) -> None:
        result = self._request("GET", f"/session/{self.session_id}/screenshot")
        encoded = result.get("value")
        if not isinstance(encoded, str):
            raise RuntimeError("Appium screenshot response was not base64 text")
        output.write_bytes(base64.b64decode(encoded, validate=True))

    def tap_bounds(self, bounds: str) -> None:
        match = BOUNDS.fullmatch(bounds)
        if match is None:
            raise RuntimeError("requested UI node had no usable bounds")
        left, top, right, bottom = map(int, match.groups())
        actions = {
            "actions": [
                {
                    "type": "pointer",
                    "id": "finger",
                    "parameters": {"pointerType": "touch"},
                    "actions": [
                        {
                            "type": "pointerMove",
                            "duration": 0,
                            "origin": "viewport",
                            "x": (left + right) // 2,
                            "y": (top + bottom) // 2,
                        },
                        {"type": "pointerDown", "button": 0},
                        {"type": "pause", "duration": 100},
                        {"type": "pointerUp", "button": 0},
                    ],
                }
            ]
        }
        self._request(
            "POST",
            f"/session/{self.session_id}/actions",
            actions,
        )
        self._request("DELETE", f"/session/{self.session_id}/actions")

    def find_element(self, using: str, value: str) -> str:
        result = self._request(
            "POST",
            f"/session/{self.session_id}/element",
            {"using": using, "value": value},
        )
        element = result.get("value", {}).get(ELEMENT_KEY)
        if not isinstance(element, str) or not element:
            raise RuntimeError("Appium returned no element id")
        return element

    def click(self, element: str) -> None:
        self._request(
            "POST",
            f"/session/{self.session_id}/element/{element}/click",
            {},
        )

    def send_keys(self, element: str, text: str) -> None:
        self._request(
            "POST",
            f"/session/{self.session_id}/element/{element}/value",
            {"text": text, "value": list(text)},
        )

    def hide_keyboard(self) -> None:
        self._request(
            "POST",
            f"/session/{self.session_id}/appium/device/hide_keyboard",
            {},
        )

    def set_orientation(self, orientation: str) -> None:
        self._request(
            "POST",
            f"/session/{self.session_id}/orientation",
            {"orientation": orientation.upper()},
        )

    def evidence(self) -> dict:
        return {
            "server": self.appium_url,
            "sessionId": self.session_id,
            "requestedCapabilities": self.requested_capabilities,
            "returnedCapabilities": self.returned_capabilities,
        }


class AppiumServer:
    def __init__(self, evidence: Path, label: str) -> None:
        self.log = (evidence / f"{label}-appium.log").open("w", encoding="utf-8")
        self.process: subprocess.Popen[str] | None = None
        self.url = ""

    def start(self) -> str:
        port = None
        for candidate_port in range(4723, 4755):
            with socket.socket() as candidate:
                try:
                    candidate.bind(("127.0.0.1", candidate_port))
                except OSError:
                    continue
                port = candidate_port
                break
        if port is None:
            raise RuntimeError("no free Appium port in bounded range")
        self.url = f"http://127.0.0.1:{port}"
        self.process = subprocess.Popen(
            [
                "appium",
                "--address",
                "127.0.0.1",
                "--port",
                str(port),
                "--log-level",
                "debug",
                "--relaxed-security",
            ],
            text=True,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        for _ in range(120):
            if self.process.poll() is not None:
                raise RuntimeError("Appium exited before readiness")
            try:
                with request.urlopen(f"{self.url}/status", timeout=2) as response:
                    if response.status == 200:
                        return self.url
            except (OSError, error.URLError):
                time.sleep(0.25)
        raise RuntimeError("Appium did not become ready within 30 seconds")

    def stop(self) -> None:
        if self.process is not None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=10)
            self.process = None
        self.log.close()


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

    def grant_from_dialog(self, session: AppiumSession, label: str) -> None:
        deadline = time.monotonic() + 30
        source = ""
        while time.monotonic() < deadline:
            source = session.source()
            root = ET.fromstring(source)
            for node in root.iter():
                resource_id = node.attrib.get("resource-id")
                text = node.attrib.get("text", "")
                if resource_id not in ALLOW_BUTTON_IDS and text not in {
                    "Allow",
                    "Allow all",
                    "While using the app",
                }:
                    continue
                bounds = node.attrib.get("bounds", "")
                if BOUNDS.fullmatch(bounds) is None:
                    continue
                session.tap_bounds(bounds)
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


def run_with_reset(
    device: Device,
    label: str,
    observation: Callable[[], dict],
) -> dict:
    retry_reasons = []
    for attempt in range(1, 4):
        reset = device.reset_and_start(label)
        try:
            record = observation()
        except RuntimeError as failure:
            if "device offline" not in str(failure) or attempt == 3:
                raise
            retry_reasons.append(str(failure))
            continue
        record["device"] = reset
        record["infrastructureAttempts"] = attempt
        record["infrastructureRetryReasons"] = retry_reasons
        return record
    raise AssertionError("bounded infrastructure retry loop exhausted")


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
    server = AppiumServer(device.evidence, label)
    session = None
    try:
        appium_url = server.start()
        session = AppiumSession(appium_url, device.udid, PACKAGE, APP_ACTIVITY)
        appium = session.evidence()
        if not pregrant and expected_identity is None:
            device.grant_from_dialog(session, label)

        source = ""
        loading = False
        empty = False
        for _ in range(30):
            source = session.source()
            loading = 'class="android.widget.ProgressBar"' in source
            empty = "No videos found" in source
            expected_view_reached = (
                loading if expected_identity is not None else empty
            )
            if expected_view_reached:
                break
            time.sleep(1)
        screenshot = device.evidence / f"{label}-screen.png"
        session.screenshot(screenshot)
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()
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
        "appium": appium,
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
    parser.add_argument("--cli-commit", required=True)
    parser.add_argument("--runs", type=int, choices=range(1, 4), default=3)
    args = parser.parse_args()
    if re.fullmatch(r"[0-9a-f]{40}", args.cli_commit) is None:
        parser.error("--cli-commit must be a full lowercase Git commit")
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
                record = run_with_reset(
                    device,
                    label,
                    lambda: observe(
                        device,
                        apk,
                        label,
                        expected,
                        False,
                    ),
                )
                records.append(record)
        for variant, apk in (
            ("affected", args.affected_apk),
            ("fixed", args.fixed_apk),
        ):
            label = f"neighbor-{variant}"
            record = run_with_reset(
                device,
                label,
                lambda: observe(
                    device,
                    apk,
                    label,
                    None,
                    True,
                ),
            )
            neighbors[variant] = record
    finally:
        device.stop()

    result = {
        "schemaVersion": 1,
        "target": "compose-android",
        "cliCommit": args.cli_commit,
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
            "automation": "Appium UiAutomator2",
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
