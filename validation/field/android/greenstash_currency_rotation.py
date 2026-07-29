#!/usr/bin/env python3
"""Exercise GreenStash currency-picker restoration across Android rotation."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from nextplayer_permission_loop import BOUNDS, Device, sha256

AFFECTED_REVISION = "eeb3bc077f796ce45cae66c45a00c82def9ee599"
FIXED_REVISION = "78a1cf4aaa5a673e50bc54f9bbed66d4e6514200"
AFFECTED_APK_SHA256 = (
    "8cd8d4cf7cdb13c0d11e3c3526a393ee983503e15361befdebb5fee1025addd0"
)
FIXED_APK_SHA256 = (
    "3c06ce1ada421dae3bb1715c3f6c0d49f144fb4e9e340e0075f79fa1a0b6d56e"
)
PACKAGE = "com.starry.greenstash"
ACTIVITY = f"{PACKAGE}/.MainActivity"
IDENTITY = "compose-state:currency-picker-dismissed-on-rotation"
DEFAULT_CURRENCY = "US Dollar ($)"
SELECTED_CURRENCY = "Japanese Yen (¥)"


def wait_source(device: Device, predicate, label: str, seconds: int = 30) -> str:
    source = ""
    for _ in range(seconds):
        source = device.source()
        if predicate(source):
            return source
        time.sleep(1)
    diagnostic = device.evidence / f"{label}-wait-failure.xml"
    diagnostic.write_text(source, encoding="utf-8")
    raise RuntimeError(f"{label} UI condition did not become true")


def tap_node(device: Device, source: str, predicate) -> None:
    root = ET.fromstring(source)
    parents = {child: parent for parent in root.iter() for child in parent}
    selected = next((node for node in root.iter("node") if predicate(node.attrib)), None)
    if selected is None:
        raise RuntimeError("requested UI node was absent")
    while selected.attrib.get("clickable") != "true" and selected in parents:
        selected = parents[selected]
    match = BOUNDS.fullmatch(selected.attrib.get("bounds", ""))
    if match is None:
        raise RuntimeError("requested UI node had no usable bounds")
    left, top, right, bottom = map(int, match.groups())
    device.adb_run(
        "shell",
        "input",
        "tap",
        str((left + right) // 2),
        str((top + bottom) // 2),
    )


def save_observation(device: Device, label: str, source: str) -> dict:
    source_path = device.evidence / f"{label}-source.xml"
    source_path.write_text(source, encoding="utf-8")
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
        "sourceSha256": f"sha256:{hashlib.sha256(source.encode()).hexdigest()}",
        "screenshotSha256": f"sha256:{sha256(screenshot)}",
    }


def observe(
    device: Device,
    apk: Path,
    label: str,
    expected_identity: str | None,
    neighboring: bool,
) -> dict:
    started = time.monotonic()
    device.adb_run("uninstall", PACKAGE, capture=True, check=False)
    device.adb_run("install", str(apk), timeout=180)
    device.adb_run("logcat", "-c")
    device.adb_run("shell", "am", "start", "-W", "-n", ACTIVITY)
    source = wait_source(
        device,
        lambda value: DEFAULT_CURRENCY in value,
        f"{label}-welcome",
    )
    if not neighboring:
        tap_node(device, source, lambda attributes: attributes.get("text") == DEFAULT_CURRENCY)
        source = wait_source(
            device,
            lambda value: "Search currency" in value,
            f"{label}-picker",
        )
        tap_node(
            device,
            source,
            lambda attributes: attributes.get("class") == "android.widget.EditText",
        )
        time.sleep(1)
        device.adb_run("shell", "input", "text", "Yen")
        source = wait_source(
            device,
            lambda value: SELECTED_CURRENCY in value and 'text="Yen"' in value,
            f"{label}-search",
        )
        tap_node(
            device,
            source,
            lambda attributes: attributes.get("text") == SELECTED_CURRENCY,
        )
        wait_source(
            device,
            lambda value: (
                SELECTED_CURRENCY in value and 'checked="true"' in value
            ),
            f"{label}-selected",
        )

    device.adb_run("shell", "wm", "user-rotation", "lock", "1")
    time.sleep(3)
    source = device.source()
    foreground = f'package="{PACKAGE}"' in source
    picker_visible = 'class="android.widget.EditText"' in source
    search_retained = 'text="Yen"' in source
    selected_retained = SELECTED_CURRENCY in source and 'checked="true"' in source
    default_visible = DEFAULT_CURRENCY in source

    if neighboring:
        observation_reached = foreground and default_visible and not picker_visible
        identity = None
    elif expected_identity is None:
        observation_reached = (
            foreground and picker_visible and search_retained and selected_retained
        )
        identity = None
    else:
        observation_reached = foreground and default_visible and not picker_visible
        identity = IDENTITY if observation_reached else None
    retained = save_observation(device, label, source)
    if not observation_reached:
        raise RuntimeError(f"{label} did not reach the rotation observation")
    if identity != expected_identity:
        raise RuntimeError(
            f"{label} identity was {identity!r}, expected {expected_identity!r}"
        )
    return {
        "run": int(label.rsplit("-", 1)[-1]) if label[-1].isdigit() else None,
        "status": "reproduced" if identity else "not_reproduced",
        "identity": identity,
        "cleanLaunch": True,
        "observationReached": True,
        "exceptions": [],
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "foreground": foreground,
        "pickerVisible": picker_visible,
        "searchTextRetained": search_retained,
        "selectedCurrencyRetained": selected_retained,
        "defaultCurrencyVisible": default_visible,
        **retained,
    }


def validate_apk(path: Path, expected: str) -> None:
    if not path.is_file() or sha256(path) != expected:
        raise SystemExit(f"APK identity mismatch: {path}")


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
        for variant, apk in (("affected", args.affected_apk), ("fixed", args.fixed_apk)):
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
        "application": "greenstash",
        "repository": "https://github.com/Pool-Of-Tears/GreenStash",
        "issue": "https://github.com/Pool-Of-Tears/GreenStash/issues/213",
        "affectedRevision": AFFECTED_REVISION,
        "fixedRevision": FIXED_REVISION,
        "affectedApkSha256": f"sha256:{AFFECTED_APK_SHA256}",
        "fixedApkSha256": f"sha256:{FIXED_APK_SHA256}",
        "identity": IDENTITY,
        "memoryMeasurement": "unavailable",
        "affected": affected,
        "fixed": fixed,
        "neighboringLegalBehavior": (
            "rotating the untouched welcome screen preserves the default currency "
            "and does not open the picker on both revisions"
        ),
        "neighboring": neighbors,
        "minimizedAction": (
            "open currency picker, search Yen, select Japanese Yen, rotate to landscape"
        ),
        "runtime": {
            "platform": "android-emulator/x86_64",
            "apiLevel": 36,
            "avd": device.name,
            "network": "none",
            "reset": "recreate AVD directory and boot with -wipe-data -no-snapshot",
        },
    }
    output = args.evidence / "greenstash-currency-picker-rotation-225.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
