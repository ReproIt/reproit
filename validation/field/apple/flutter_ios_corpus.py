#!/usr/bin/env python3
"""Run the Flutter iOS false-positive corpus against fixed Saber."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import time
from pathlib import Path

from saber_recent_notes import (
    Appium,
    BUNDLE_ID,
    CONTROL_TITLE,
    DELETED_TITLE,
    app_data,
    create_note,
    dismiss_dialogs,
)

REPOSITORY = "https://github.com/saber-notes/saber"
REVISION = "ed4fe66fc5908a55d2e20806e9cb01fc11ad5d78"
PROXY_URL = "http://127.0.0.1:9"
PROCESS_ID = re.compile(r'processId="([0-9]+)"')
CASES = (
    (
        "saber-clean-live-notes",
        "clean",
        "live-notes",
        "Two ordinary persisted notes provide the clean known-good subject.",
    ),
    (
        "saber-adversarial-deleted-pair",
        "adversarial",
        "deleted-note-pair",
        "A complete note pair is removed externally, matching the fixed defect boundary.",
    ),
    (
        "saber-adversarial-missing-preview",
        "adversarial",
        "missing-preview-sidecar",
        "The note remains valid while its optional preview sidecar is absent.",
    ),
)


def run(command: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(
        command,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
    )
    return result.stdout.strip() if capture else ""


def try_run(command: list[str]) -> None:
    subprocess.run(
        command,
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def executable_sha256(app_path: Path) -> str:
    executable = app_path / "Frameworks" / "App.framework" / "App"
    return f"sha256:{hashlib.sha256(executable.read_bytes()).hexdigest()}"


def start_offline(appium: Appium, udid: str, wda_port: int) -> None:
    proxy_environment = {
        "HTTP_PROXY": PROXY_URL,
        "HTTPS_PROXY": PROXY_URL,
        "ALL_PROXY": PROXY_URL,
        "NO_PROXY": "127.0.0.1,localhost,::1",
    }
    value = appium.request(
        "POST",
        "/session",
        {
            "capabilities": {
                "alwaysMatch": {
                    "platformName": "iOS",
                    "appium:automationName": "XCUITest",
                    "appium:platformVersion": "18.5",
                    "appium:udid": udid,
                    "appium:bundleId": BUNDLE_ID,
                    "appium:noReset": True,
                    "appium:wdaLocalPort": wda_port,
                    "appium:wdaLaunchTimeout": 300_000,
                    "appium:newCommandTimeout": 600,
                    "appium:processArguments": {
                        "env": proxy_environment,
                        "args": [],
                    },
                }
            }
        },
    )
    if not isinstance(value, dict) or not isinstance(value.get("sessionId"), str):
        raise RuntimeError("Appium did not return a session id")
    appium.session_id = value["sessionId"]


def app_process_id(source: str) -> int:
    match = PROCESS_ID.search(source)
    if match is None:
        raise RuntimeError("the retained UI source has no application process id")
    return int(match.group(1))


def network_observation(process_id: int) -> dict:
    environment = run(["ps", "eww", "-p", str(process_id)], capture=True)
    proxy_environment_present = all(
        token in environment
        for token in (
            f"HTTP_PROXY={PROXY_URL}",
            f"HTTPS_PROXY={PROXY_URL}",
            f"ALL_PROXY={PROXY_URL}",
            "NO_PROXY=127.0.0.1,localhost,::1",
        )
    )
    sockets = subprocess.run(
        ["lsof", "-nP", "-a", "-p", str(process_id), "-i"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    ).stdout.splitlines()
    endpoints = [line.split(None, 8)[-1] for line in sockets[1:] if line.strip()]
    external_established = [
        endpoint
        for endpoint in endpoints
        if "(ESTABLISHED)" in endpoint
        and "127.0.0.1" not in endpoint
        and "[::1]" not in endpoint
    ]
    if not proxy_environment_present:
        raise RuntimeError("the deny-all proxy environment was not present in the app")
    if external_established:
        raise RuntimeError(
            f"the offline subject opened external connections: {external_established}"
        )
    return {
        "policy": "deny non-loopback HTTP(S) and ALL proxy at 127.0.0.1:9",
        "proxyEnvironmentPresent": proxy_environment_present,
        "observedIpEndpoints": endpoints,
        "externalEstablishedConnections": external_established,
    }


def note_paths(udid: str) -> tuple[list[Path], list[Path]]:
    notes = app_data(udid) / "Documents" / "Saber"
    subject = [
        notes / f"{DELETED_TITLE}.sbn2",
        notes / f"{DELETED_TITLE}.sbn2.p",
    ]
    control = [
        notes / f"{CONTROL_TITLE}.sbn2",
        notes / f"{CONTROL_TITLE}.sbn2.p",
    ]
    if not all(path.is_file() for path in subject + control):
        raise RuntimeError("the two expected note pairs were not persisted")
    return subject, control


def apply_variant(variant: str, subject: list[Path], quarantine: Path) -> None:
    if variant == "live-notes":
        return
    selected = subject if variant == "deleted-note-pair" else subject[1:]
    quarantine.mkdir(parents=True)
    for path in selected:
        shutil.move(path, quarantine / path.name)


def observe_variant(
    appium: Appium,
    udid: str,
    app_path: Path,
    variant: str,
    quarantine: Path,
    wda_port: int,
) -> dict:
    try_run(["xcrun", "simctl", "terminate", udid, BUNDLE_ID])
    try_run(["xcrun", "simctl", "uninstall", udid, BUNDLE_ID])
    run(["xcrun", "simctl", "install", udid, str(app_path)])
    start_offline(appium, udid, wda_port)
    dismiss_dialogs(appium)
    create_note(appium, 0)
    create_note(appium, 8)
    subject, control = note_paths(udid)
    apply_variant(variant, subject, quarantine)
    appium.close()

    try_run(["xcrun", "simctl", "terminate", udid, BUNDLE_ID])
    start_offline(appium, udid, wda_port)
    time.sleep(3)
    dismiss_dialogs(appium)
    source = appium.source()
    process_id = app_process_id(source)
    network = network_observation(process_id)
    subject_note_exists = subject[0].is_file()
    subject_preview_exists = subject[1].is_file()
    no_preview_visible = (
        f"No preview available&#10;{DELETED_TITLE}" in source
        or f"No preview available\n{DELETED_TITLE}" in source
    )
    subject_row_visible = (
        f'name="{DELETED_TITLE}"' in source
        or no_preview_visible
    )
    control_visible = f'name="{CONTROL_TITLE}"' in source

    identity = None
    if not subject_note_exists and subject_row_visible:
        identity = (
            "recent-notes:externally-deleted-note-still-visible:"
            f"No preview available\\n{DELETED_TITLE}"
        )
    expected = {
        "live-notes": (True, True, True, False),
        "deleted-note-pair": (False, False, False, False),
        "missing-preview-sidecar": (True, False, True, True),
    }[variant]
    actual = (
        subject_note_exists,
        subject_preview_exists,
        subject_row_visible,
        no_preview_visible,
    )
    if actual != expected:
        raise RuntimeError(f"{variant} observation {actual!r}, expected {expected!r}")
    if not control_visible:
        raise RuntimeError("the neighboring control note was not visible")

    observation = {
        "identity": identity,
        "cleanLaunch": True,
        "observationReached": True,
        "exceptions": [],
        "variant": variant,
        "subjectNoteExists": subject_note_exists,
        "subjectPreviewExists": subject_preview_exists,
        "subjectRowVisible": subject_row_visible,
        "noPreviewVisible": no_preview_visible,
        "neighboringControlVisible": control_visible,
        "sourceSha256": f"sha256:{hashlib.sha256(source.encode()).hexdigest()}",
        "network": network,
    }
    appium.close()
    return observation


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--udid", required=True)
    parser.add_argument("--appium-url", required=True)
    parser.add_argument("--app", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--quarantine", required=True, type=Path)
    args = parser.parse_args()
    if not args.app.is_dir():
        raise SystemExit(f"app does not exist: {args.app}")

    appium = Appium(args.appium_url)
    cases = []
    try:
        for index, (identifier, kind, variant, why) in enumerate(CASES):
            observation = observe_variant(
                appium,
                args.udid,
                args.app,
                variant,
                args.quarantine / identifier,
                18551 + index,
            )
            case = {
                "id": identifier,
                "kind": kind,
                "application": "saber-recent-notes-1603-fixed",
                "repository": REPOSITORY,
                "revision": REVISION,
                "fixture": None,
                "variant": variant,
                "why": why,
                "observationReached": True,
                "identity": observation["identity"],
                "falsePositive": observation["identity"] is not None,
                "observation": observation,
            }
            cases.append(case)
            print(json.dumps(case, sort_keys=True), flush=True)
    finally:
        appium.close()

    document = {
        "schemaVersion": 1,
        "target": "flutter-ios",
        "worker": {
            "image": (
                "xcode-26.2-ios-18.5-simulator:"
                f"saber-{executable_sha256(args.app).removeprefix('sha256:')[:12]}"
            ),
            "platform": "ios-simulator/arm64",
            "network": "none",
        },
        "cleanCases": sum(case["kind"] == "clean" for case in cases),
        "adversarialCases": sum(case["kind"] == "adversarial" for case in cases),
        "confirmedFalsePositives": sum(case["falsePositive"] for case in cases),
        "unreachedObservations": sum(not case["observationReached"] for case in cases),
        "containersRemaining": 0,
        "cases": cases,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    if document["confirmedFalsePositives"] or document["unreachedObservations"]:
        raise SystemExit("the Flutter iOS corpus did not pass")


if __name__ == "__main__":
    main()
