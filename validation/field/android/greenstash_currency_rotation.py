#!/usr/bin/env python3
"""Exercise GreenStash currency-picker restoration across Android rotation."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from nextplayer_permission_loop import (
    AppiumServer,
    AppiumSession,
    BOUNDS,
    Device,
    run_with_reset,
    sha256,
)

AFFECTED_REVISION = "eeb3bc077f796ce45cae66c45a00c82def9ee599"
FIXED_REVISION = "78a1cf4aaa5a673e50bc54f9bbed66d4e6514200"
AFFECTED_APK_SHA256 = (
    "8cd8d4cf7cdb13c0d11e3c3526a393ee983503e15361befdebb5fee1025addd0"
)
FIXED_APK_SHA256 = (
    "3c06ce1ada421dae3bb1715c3f6c0d49f144fb4e9e340e0075f79fa1a0b6d56e"
)
PACKAGE = "com.starry.greenstash"
APP_ACTIVITY = ".MainActivity"
IDENTITY = "compose-state:currency-picker-dismissed-on-rotation"
DEFAULT_CURRENCY = "US Dollar ($)"
SELECTED_CURRENCY = "Japanese Yen (¥)"


def wait_source(
    session: AppiumSession,
    device: Device,
    predicate,
    label: str,
    seconds: int = 30,
) -> str:
    source = ""
    for _ in range(seconds):
        source = session.source()
        if predicate(source):
            return source
        time.sleep(1)
    diagnostic = device.evidence / f"{label}-wait-failure.xml"
    diagnostic.write_text(source, encoding="utf-8")
    raise RuntimeError(f"{label} UI condition did not become true")


def tap_node(session: AppiumSession, source: str, predicate) -> None:
    root = ET.fromstring(source)
    parents = {child: parent for parent in root.iter() for child in parent}
    selected = next((node for node in root.iter() if predicate(node.attrib)), None)
    if selected is None:
        raise RuntimeError("requested UI node was absent")
    while selected.attrib.get("clickable") != "true" and selected in parents:
        selected = parents[selected]
    session.tap_bounds(selected.attrib.get("bounds", ""))


def save_observation(
    session: AppiumSession,
    device: Device,
    label: str,
    source: str,
) -> dict:
    source_path = device.evidence / f"{label}-source.xml"
    source_path.write_text(source, encoding="utf-8")
    screenshot = device.evidence / f"{label}-screen.png"
    session.screenshot(screenshot)
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
    server = AppiumServer(device.evidence, label)
    session = None
    try:
        appium_url = server.start()
        session = AppiumSession(appium_url, device.udid, PACKAGE, APP_ACTIVITY)
        appium = session.evidence()
        source = wait_source(
            session,
            device,
            lambda value: DEFAULT_CURRENCY in value,
            f"{label}-welcome",
        )
        if not neighboring:
            tap_node(
                session,
                source,
                lambda attributes: attributes.get("text") == DEFAULT_CURRENCY,
            )
            source = wait_source(
                session,
                device,
                lambda value: "Search currency" in value,
                f"{label}-picker",
            )
            edit_text = session.find_element(
                "class name",
                "android.widget.EditText",
            )
            session.click(edit_text)
            session.send_keys(edit_text, "Yen")
            source = wait_source(
                session,
                device,
                lambda value: (
                    SELECTED_CURRENCY in value and 'text="Yen"' in value
                ),
                f"{label}-search",
            )
            session.hide_keyboard()
            source = wait_source(
                session,
                device,
                lambda value: (
                    SELECTED_CURRENCY in value and 'text="Yen"' in value
                ),
                f"{label}-keyboard-hidden",
            )
            for attempt in range(3):
                tap_node(
                    session,
                    source,
                    lambda attributes: (
                        attributes.get("text") == SELECTED_CURRENCY
                    ),
                )
                try:
                    wait_source(
                        session,
                        device,
                        lambda value: (
                            SELECTED_CURRENCY in value
                            and 'checked="true"' in value
                        ),
                        f"{label}-selected",
                        seconds=10,
                    )
                    break
                except RuntimeError:
                    if attempt == 2:
                        raise
                    source = session.source()

        session.set_orientation("LANDSCAPE")
        if neighboring or expected_identity is not None:
            rotated = lambda value: (
                DEFAULT_CURRENCY in value
                and 'class="android.widget.EditText"' not in value
            )
        else:
            rotated = lambda value: (
                'class="android.widget.EditText"' in value
                and 'text="Yen"' in value
                and SELECTED_CURRENCY in value
                and 'checked="true"' in value
            )
        source = wait_source(
            session,
            device,
            rotated,
            f"{label}-rotation",
        )
        retained = save_observation(session, device, label, source)
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()
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
        "appium": appium,
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
        for variant, apk in (("affected", args.affected_apk), ("fixed", args.fixed_apk)):
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
            "automation": "Appium UiAutomator2",
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
