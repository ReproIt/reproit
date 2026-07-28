#!/usr/bin/env python3
"""Validate pinned native prerequisites before a gate owns external state."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "validation/native/toolchains.json"
COMMAND_TIMEOUT_SECONDS = 20


def prerequisite_path(command: str) -> Path | None:
    located = shutil.which(command)
    if located:
        return Path(located)
    if command != "adb":
        return None
    roots = [
        os.environ.get("ANDROID_SDK_ROOT"),
        os.environ.get("ANDROID_HOME"),
        str(Path.home() / "Library/Android/sdk"),
        "/usr/local/lib/android/sdk",
        "/opt/android-sdk",
    ]
    for root in roots:
        if root and (candidate := Path(root) / "platform-tools/adb").is_file():
            return candidate
    return None


def output(command: list[str]) -> str:
    result = subprocess.run(
        command,
        check=True,
        capture_output=True,
        text=True,
        timeout=COMMAND_TIMEOUT_SECONDS,
    )
    return f"{result.stdout}\n{result.stderr}".strip()


def require_version(label: str, actual: str, expected: str) -> None:
    if expected not in actual:
        raise ValueError(f"{label} is not pinned to {expected}: {actual.splitlines()[0]}")


def require_appium_driver(profile: str, pins: dict[str, object]) -> None:
    driver_name = "uiautomator2" if profile == "android" else "xcuitest"
    raw = output(["appium", "driver", "list", "--installed", "--json"])
    start = raw.find("{")
    if start < 0:
        raise ValueError("Appium did not return its installed driver inventory as JSON")
    drivers, _ = json.JSONDecoder().raw_decode(raw[start:])
    installed = drivers.get(driver_name)
    expected = str(pins["appiumDrivers"][driver_name])
    actual = installed.get("version") if isinstance(installed, dict) else None
    if actual != expected:
        raise ValueError(
            f"Appium {driver_name} driver is not pinned to {expected}: "
            f"{actual or 'not installed'}"
        )


def validate_versions(profile: str, pins: dict[str, object]) -> None:
    if profile not in {"linux-containers", "windows-bridge"}:
        require_version("rustc", output(["rustc", "--version"]), str(pins["rust"]))
    if profile in {"linux-hosted", "android", "macos-appium"}:
        node = output(["node", "--version"])
        if not re.match(rf"^v{pins['nodeMajor']}\.", node):
            raise ValueError(f"node major is not pinned to {pins['nodeMajor']}: {node}")
    if profile in {"android", "macos-flutter"}:
        require_version("Flutter", output(["flutter", "--version"]), str(pins["flutter"]))
    if profile in {"android", "macos-appium"}:
        require_version("Appium", output(["appium", "--version"]), str(pins["appium"]))
        require_appium_driver(profile, pins)
    if profile.startswith("macos"):
        xcode = output(["xcodebuild", "-version"])
        xcode_version = pins["xcodeByProfile"][profile]
        require_version("Xcode", xcode, f"Xcode {xcode_version}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("profile")
    parser.add_argument(
        "--require-ax-permission",
        action="store_true",
        help="require the runner registration to attest Accessibility permission",
    )
    args = parser.parse_args()
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if manifest.get("schema") != 1 or args.profile not in manifest.get("profiles", {}):
        raise ValueError(f"unknown native prerequisite profile: {args.profile}")
    missing = [
        command
        for command in manifest["profiles"][args.profile]
        if prerequisite_path(command) is None
    ]
    if missing:
        raise ValueError(f"{args.profile} is missing prerequisites: {', '.join(missing)}")
    if args.require_ax_permission and os.environ.get("REPROIT_AX_PERMISSION_CONFIRMED") != "1":
        raise ValueError(
            "set REPROIT_AX_PERMISSION_CONFIRMED=1 only on a runner whose service "
            "has macOS Accessibility permission"
        )
    validate_versions(args.profile, manifest["pins"])
    print(f"native preflight passed: {args.profile}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
