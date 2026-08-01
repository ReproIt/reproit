#!/usr/bin/env python3
"""Exercise gopeed's proxy credential persistence across an Android restart.

The application is built in profile mode, so the observable is read from the
platform accessibility hierarchy rather than from the Dart VM service, exactly
as the LocalSend campaign reads it. gopeed's settings page renders every
configuration item as one merged semantics node, so the proxy card is reached
by a point inside that node, and the credential fields are addressed as the
editable elements UiAutomator2 exposes once the card is expanded.

The trigger is offline: type a proxy address and credentials, restart the
application, and read the fields back. The container runs with no network and
no download task is ever started.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from urllib import request

from localsend_receive_link import attach_vm_service
from nextplayer_permission_loop import (
    AppiumServer,
    AppiumSession,
    Device,
    run_with_reset,
    sha256,
)

AFFECTED_REVISION = "f7189668fd014696c9716bf7687ecc48fb91cd3b"
FIXED_REVISION = "5bb85413854f2f4202bdc8e6026a3a856358b4d4"
AFFECTED_APK_SHA256 = (
    "6e82a5d40ff98c14e83f0f208c8cad6d99b7ac7a5ca848ab35d031787d4c31d5"
)
FIXED_APK_SHA256 = (
    "f8ad8fe11aacc2c42d27af65dc8ff773290b6ab13b50f3b0cc8c77241f4d2fdf"
)
PACKAGE = "com.gopeed.gopeed"
APP_ACTIVITY = "com.gopeed.gopeed.MainActivity"
IDENTITY = "flutter-settings:proxy-credentials-dropped-on-restart"
ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"
PROXY_HOST = "127.0.0.1"
PROXY_PORT = "1080"
PROXY_USER = "reproituser"
PROXY_PASSWORD = "reproitpass"
BOUNDS = re.compile(r"\[(\d+),(\d+)]\[(\d+),(\d+)]")
# Measured on this application: the proxy card is one merged node and the mode
# dropdown sits at this fraction of it.
DROPDOWN_POINT = (0.41, 0.56)


def find_elements(session: AppiumSession, using: str, value: str) -> list:
    """The four proxy fields are addressed together, which the shared session
    helper has no call for, so the plural endpoint is issued here."""
    operation = request.Request(
        f"{session.appium_url}/session/{session.session_id}/elements",
        data=json.dumps({"using": using, "value": value}).encode(),
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    with request.urlopen(operation, timeout=120) as response:
        result = json.loads(response.read().decode())
    elements = result.get("value")
    if not isinstance(elements, list):
        raise RuntimeError("Appium returned no element list")
    return [entry[ELEMENT_KEY] for entry in elements]


def node_for(source: str, needle: str) -> dict | None:
    for node in ET.fromstring(source).iter():
        label = node.attrib.get("content-desc", "") or node.attrib.get("text", "")
        if needle in label:
            return dict(node.attrib)
    return None


def has_label(source: str, needle: str) -> bool:
    """Match on the decoded attribute, never on the raw document: uiautomator
    escapes the newline inside a merged label, so a substring test against the
    XML text silently never matches."""
    return node_for(source, needle) is not None


def field_values(source: str) -> list:
    return [
        node.attrib.get("text", "")
        for node in ET.fromstring(source).iter()
        if node.attrib.get("class") == "android.widget.EditText"
    ]


def wait_source(
    session: AppiumSession,
    device: Device,
    predicate,
    label: str,
    seconds: int = 60,
) -> str:
    source = ""
    for _ in range(seconds):
        source = session.source()
        if predicate(source):
            return source
        time.sleep(1)
    (device.evidence / f"{label}-wait-failure.xml").write_text(
        source,
        encoding="utf-8",
    )
    raise RuntimeError(f"{label} UI condition did not become true")


def tap_label(session: AppiumSession, source: str, needle: str, label: str) -> None:
    node = node_for(source, needle)
    if node is None or BOUNDS.fullmatch(node.get("bounds", "")) is None:
        raise RuntimeError(f"{label}: no node carrying {needle!r}")
    session.tap_bounds(node["bounds"])
    time.sleep(3)


def open_proxy_card(session: AppiumSession, device: Device, label: str) -> str:
    """Reach the settings page's advanced tab and expand the proxy card."""
    source = wait_source(
        session,
        device,
        lambda value: has_label(value, "Settings\nTab 3 of 3"),
        f"{label}-home",
    )
    tap_label(session, source, "Settings\nTab 3 of 3", label)
    source = wait_source(
        session,
        device,
        lambda value: has_label(value, "Advanced\nTab 2 of 2"),
        f"{label}-settings",
    )
    tap_label(session, source, "Advanced\nTab 2 of 2", label)
    return wait_source(
        session,
        device,
        lambda value: has_label(value, "Proxy"),
        f"{label}-advanced",
    )


def select_custom_proxy(session: AppiumSession, device: Device, label: str) -> str:
    source = open_proxy_card(session, device, label)
    node = node_for(source, "Proxy")
    matched = BOUNDS.fullmatch(node["bounds"]) if node else None
    if matched is None:
        raise RuntimeError(f"{label}: the proxy card exposed no usable bounds")
    left, top, right, bottom = map(int, matched.groups())
    x = left + int(DROPDOWN_POINT[0] * (right - left))
    y = top + int(DROPDOWN_POINT[1] * (bottom - top))
    for _ in range(3):
        session.tap_bounds(f"[{x},{y}][{x},{y}]")
        time.sleep(3)
        source = session.source()
        if "Custom Proxy" in source:
            break
    else:
        raise RuntimeError(f"{label}: the proxy mode dropdown did not open")
    tap_label(session, source, "Custom Proxy", label)
    return wait_source(
        session,
        device,
        lambda value: len(field_values(value)) >= 4,
        f"{label}-custom",
    )


def fill_fields(session: AppiumSession, values: list) -> None:
    for index, value in enumerate(values):
        if value is None:
            continue
        elements = find_elements(session, "class name", "android.widget.EditText")
        session.click(elements[index])
        time.sleep(1)
        session.send_keys(elements[index], value)
        time.sleep(1)


def restart_application(session: AppiumSession, device: Device, label: str) -> None:
    device.adb_run("shell", "am", "force-stop", PACKAGE)
    time.sleep(3)
    device.adb_run(
        "shell",
        "monkey",
        "-p",
        PACKAGE,
        "-c",
        "android.intent.category.LAUNCHER",
        "1",
        check=False,
        timeout=120,
    )
    wait_source(
        session,
        device,
        lambda value: has_label(value, "Settings\nTab 3 of 3"),
        f"{label}-relaunch",
    )


def save_observation(
    session: AppiumSession,
    device: Device,
    label: str,
    source: str,
) -> dict:
    (device.evidence / f"{label}-source.xml").write_text(source, encoding="utf-8")
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
    device.adb_run("install", "-r", "-t", str(apk), timeout=600)
    device.adb_run(
        "shell",
        "pm",
        "grant",
        PACKAGE,
        "android.permission.POST_NOTIFICATIONS",
        check=False,
    )
    device.adb_run("logcat", "-c")
    server = AppiumServer(device.evidence, label)
    session = None
    try:
        appium_url = server.start()
        session = AppiumSession(appium_url, device.udid, PACKAGE, APP_ACTIVITY)
        appium = session.evidence()
        vm_service = attach_vm_service(device, label)
        select_custom_proxy(session, device, label)
        typed = (
            [PROXY_HOST, PROXY_PORT, None, None]
            if neighboring
            else [PROXY_HOST, PROXY_PORT, PROXY_USER, PROXY_PASSWORD]
        )
        fill_fields(session, typed)
        # The application debounces its configuration save, so give the write
        # time to land before the process is stopped.
        time.sleep(8)
        restart_application(session, device, label)
        source = open_proxy_card(session, device, label)
        tap_label(session, source, "Proxy", label)
        source = wait_source(
            session,
            device,
            lambda value: len(field_values(value)) >= 4,
            f"{label}-restored",
        )
        values = field_values(source)
        retained = save_observation(session, device, label, source)
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()
    address_retained = values[0] == PROXY_HOST and values[1] == PROXY_PORT
    user_retained = values[2] == PROXY_USER
    password_retained = bool(values[3])
    observation_reached = len(values) >= 4 and address_retained
    identity = None
    if not neighboring and observation_reached and not user_retained:
        identity = IDENTITY
    if not observation_reached:
        raise RuntimeError(f"{label} did not reach the restored-settings observation")
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
        "typedCredentials": not neighboring,
        "restoredFields": values,
        "addressRetained": address_retained,
        "usernameRetained": user_retained,
        "passwordRetained": password_retained,
        "vmService": vm_service,
        **retained,
        "appium": appium,
    }


def validate_apk(path: Path, expected: str) -> None:
    if not path.is_file() or sha256(path) != expected:
        raise SystemExit(f"APK identity mismatch: {path}")


def campaign_result(
    cli_commit: str,
    device: Device,
    affected: list,
    fixed: list,
    neighbors: dict,
) -> dict:
    return {
        "schemaVersion": 1,
        "target": "flutter-android",
        "cliCommit": cli_commit,
        "application": "gopeed",
        "repository": "https://github.com/GopeedLab/gopeed",
        "issue": "https://github.com/GopeedLab/gopeed/issues/1180",
        "pullRequest": "https://github.com/GopeedLab/gopeed/pull/1183",
        "affectedRevision": AFFECTED_REVISION,
        "fixedRevision": FIXED_REVISION,
        "affectedApkSha256": f"sha256:{AFFECTED_APK_SHA256}",
        "fixedApkSha256": f"sha256:{FIXED_APK_SHA256}",
        "identity": IDENTITY,
        "memoryMeasurement": "unavailable",
        "affected": affected,
        "fixed": fixed,
        "neighboringLegalBehavior": (
            "the proxy server and port typed into the same card survive the "
            "same restart on both revisions, so the restart itself is not what "
            "loses state and only the credential fields move"
        ),
        "neighboring": neighbors,
        "minimizedAction": (
            "select the custom proxy mode, type a server, port, username and "
            "password, restart the application, and read the fields back"
        ),
        "observationChannel": (
            "platform accessibility hierarchy through UiAutomator2; the "
            "profile-mode VM service carries no widget, render or semantics "
            "tree and refuses evaluate"
        ),
        "runtime": {
            "platform": "android-emulator/x86_64",
            "automation": "Appium UiAutomator2",
            "apiLevel": 36,
            "avd": device.name,
            "buildMode": "profile",
            "network": "none",
            "reset": "recreate AVD directory and boot with -wipe-data -no-snapshot",
        },
    }


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
    affected: list = []
    fixed: list = []
    neighbors: dict = {}
    try:
        for variant, apk, expected, records in (
            ("affected", args.affected_apk, IDENTITY, affected),
            ("fixed", args.fixed_apk, None, fixed),
        ):
            for index in range(1, args.runs + 1):
                label = f"{variant}-{index}"
                records.append(
                    run_with_reset(
                        device,
                        label,
                        lambda apk=apk, label=label, expected=expected: observe(
                            device, apk, label, expected, False
                        ),
                    )
                )
        for variant, apk in (
            ("affected", args.affected_apk),
            ("fixed", args.fixed_apk),
        ):
            label = f"neighbor-{variant}"
            neighbors[variant] = run_with_reset(
                device,
                label,
                lambda apk=apk, label=label: observe(
                    device, apk, label, None, True
                ),
            )
    finally:
        device.stop()

    result = campaign_result(args.cli_commit, device, affected, fixed, neighbors)
    output = args.evidence / "gopeed-proxy-credentials-1180.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
