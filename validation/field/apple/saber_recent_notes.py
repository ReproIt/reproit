#!/usr/bin/env python3
"""Reproduce Saber issue 1603 through a real iOS simulator application."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"
BUNDLE_ID = "com.adilhanney.saber"
DELETED_TITLE = "26-07-29 Untitled"
CONTROL_TITLE = "26-07-29 Untitled (2)"
FAILURE_IDENTITY = (
    "recent-notes:externally-deleted-note-still-visible:"
    "No preview available\\n26-07-29 Untitled"
)


def run(command: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(
        command,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    )
    return result.stdout.strip() if capture else ""


class Appium:
    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.session_id = ""

    def request(self, method: str, path: str, body: dict | None = None) -> object:
        payload = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=payload,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=320) as response:
            document = json.load(response)
        if "value" not in document:
            raise RuntimeError(f"invalid Appium response for {path}")
        value = document["value"]
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(f"Appium {path}: {value}")
        return value

    def start(self, udid: str, wda_port: int, bundle_id: str = BUNDLE_ID) -> None:
        value = self.request(
            "POST",
            "/session",
            {
                "capabilities": {
                    "alwaysMatch": {
                        "platformName": "iOS",
                        "appium:automationName": "XCUITest",
                        "appium:platformVersion": "18.5",
                        "appium:udid": udid,
                        "appium:bundleId": bundle_id,
                        "appium:noReset": True,
                        "appium:wdaLocalPort": wda_port,
                        "appium:wdaLaunchTimeout": 300_000,
                        "appium:newCommandTimeout": 600,
                    }
                }
            },
        )
        if not isinstance(value, dict) or not isinstance(value.get("sessionId"), str):
            raise RuntimeError("Appium did not return a session id")
        self.session_id = value["sessionId"]

    def close(self) -> None:
        if not self.session_id:
            return
        try:
            self.request("DELETE", f"/session/{self.session_id}")
        except (OSError, RuntimeError, urllib.error.URLError):
            pass
        self.session_id = ""

    def element(self, using: str, value: str) -> str:
        result = self.request(
            "POST",
            f"/session/{self.session_id}/element",
            {"using": using, "value": value},
        )
        if not isinstance(result, dict) or not isinstance(result.get(ELEMENT_KEY), str):
            raise RuntimeError(f"element not found: {using}={value}")
        return result[ELEMENT_KEY]

    def click(self, using: str, value: str) -> None:
        last_error: Exception | None = None
        for _ in range(10):
            try:
                element = self.element(using, value)
                self.request(
                    "POST",
                    f"/session/{self.session_id}/element/{element}/click",
                    {},
                )
                return
            except urllib.error.HTTPError as error:
                last_error = error
                if error.code != 404:
                    raise
                time.sleep(1)
        raise RuntimeError(f"element did not become clickable: {using}={value}") from last_error

    def source(self) -> str:
        value = self.request("GET", f"/session/{self.session_id}/source")
        if not isinstance(value, str):
            raise RuntimeError("Appium source was not text")
        return value

    def draw_stroke(self, offset: int) -> None:
        self.request(
            "POST",
            f"/session/{self.session_id}/actions",
            {
                "actions": [
                    {
                        "type": "pointer",
                        "id": "finger",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {
                                "type": "pointerMove",
                                "duration": 0,
                                "x": 140 + offset,
                                "y": 300 + offset,
                            },
                            {"type": "pointerDown", "button": 0},
                            {
                                "type": "pointerMove",
                                "duration": 300,
                                "x": 220 + offset,
                                "y": 360 + offset,
                            },
                            {"type": "pointerUp", "button": 0},
                        ],
                    }
                ]
            },
        )

    def tap(self, x: int, y: int) -> None:
        self.request(
            "POST",
            f"/session/{self.session_id}/actions",
            {
                "actions": [
                    {
                        "type": "pointer",
                        "id": "finger",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {"type": "pointerMove", "duration": 0, "x": x, "y": y},
                            {"type": "pointerDown", "button": 0},
                            {"type": "pause", "duration": 100},
                            {"type": "pointerUp", "button": 0},
                        ],
                    }
                ]
            },
        )


def dismiss_dialogs(appium: Appium) -> None:
    for _ in range(20):
        source = appium.source()
        if (
            'name="Update available"' in source
            and 'name="Dismiss"' in source
        ):
            appium.tap(130, 603)
            time.sleep(1)
            continue
        if (
            'name="Help improve Saber?"' in source
            and 'name="No"' in source
        ):
            appium.tap(300, 617)
            time.sleep(1)
            continue
        if 'name="New note"' in source or 'name="Recent notes"' in source:
            return
        time.sleep(1)
    raise RuntimeError("Saber did not finish its bounded launch dialogs")


def create_note(appium: Appium, offset: int) -> None:
    appium.click("accessibility id", "New note")
    time.sleep(1)
    appium.click(
        "xpath",
        '(//XCUIElementTypeButton[@visible="true"])[last()]',
    )
    time.sleep(1)
    appium.draw_stroke(offset)
    time.sleep(1)
    appium.click(
        "xpath",
        '//XCUIElementTypeButton[@x="4" and @y="66"]',
    )
    for _ in range(10):
        time.sleep(1)
        if 'name="New note"' in appium.source():
            return
        appium.click(
            "xpath",
            '//XCUIElementTypeButton[@x="4" and @y="66"]',
        )
    raise RuntimeError("Saber editor did not return to the home screen")


def app_data(udid: str) -> Path:
    path = run(
        ["xcrun", "simctl", "get_app_container", udid, BUNDLE_ID, "data"],
        capture=True,
    )
    return Path(path)


def relaunch(udid: str) -> None:
    run(["xcrun", "simctl", "terminate", udid, BUNDLE_ID])
    run(["xcrun", "simctl", "launch", udid, BUNDLE_ID])
    time.sleep(3)


def app_sha256(app_path: Path) -> str:
    executable = app_path / "Frameworks" / "App.framework" / "App"
    return f"sha256:{hashlib.sha256(executable.read_bytes()).hexdigest()}"


def execute_run(
    appium: Appium,
    udid: str,
    app_path: Path,
    expected: str,
    run_number: int,
    quarantine: Path,
) -> dict:
    started = time.monotonic()
    exceptions: list[str] = []
    source = ""
    try:
        run(["xcrun", "simctl", "terminate", udid, BUNDLE_ID])
    except subprocess.CalledProcessError:
        pass
    try:
        run(["xcrun", "simctl", "uninstall", udid, BUNDLE_ID])
    except subprocess.CalledProcessError:
        pass
    try:
        run(["xcrun", "simctl", "install", udid, str(app_path)])
        appium.start(udid, 18451)
        dismiss_dialogs(appium)
        create_note(appium, 0)
        create_note(appium, 8)

        notes = app_data(udid) / "Documents" / "Saber"
        deleted_files = [
            notes / f"{DELETED_TITLE}.sbn2",
            notes / f"{DELETED_TITLE}.sbn2.p",
        ]
        control_files = [
            notes / f"{CONTROL_TITLE}.sbn2",
            notes / f"{CONTROL_TITLE}.sbn2.p",
        ]
        if not all(path.is_file() for path in deleted_files + control_files):
            raise RuntimeError("the two expected note pairs were not persisted")
        run_quarantine = quarantine / f"{expected}-{run_number}"
        run_quarantine.mkdir(parents=True)
        for path in deleted_files:
            shutil.move(path, run_quarantine / path.name)

        relaunch(udid)
        dismiss_dialogs(appium)
        source = appium.source()
    except Exception as error:
        exceptions.append(f"{type(error).__name__}: {error}")
    finally:
        appium.close()

    deleted_row_visible = (
        f"No preview available&#10;{DELETED_TITLE}" in source
        or f"No preview available\n{DELETED_TITLE}" in source
    )
    control_visible = f'name="{CONTROL_TITLE}"' in source
    if not control_visible:
        exceptions.append("neighboring control note was not visible")
    expected_failure = expected == "affected"
    if deleted_row_visible != expected_failure:
        exceptions.append(
            f"deleted row visible={deleted_row_visible}, expected={expected_failure}"
        )
    elapsed_seconds = round(time.monotonic() - started, 3)
    return {
        "run": run_number,
        "cleanLaunch": True,
        "observationReached": bool(source),
        "identity": FAILURE_IDENTITY if deleted_row_visible else None,
        "neighboringControlReached": control_visible,
        "elapsedSeconds": elapsed_seconds,
        "jsHeapMiB": None,
        "exceptions": exceptions,
        "sourceSha256": (
            f"sha256:{hashlib.sha256(source.encode()).hexdigest()}"
            if source
            else None
        ),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--udid", required=True)
    parser.add_argument("--appium-url", required=True)
    parser.add_argument("--app", required=True, type=Path)
    parser.add_argument("--expected", choices=("affected", "fixed"), required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--quarantine", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.runs <= 3:
        raise SystemExit("--runs must be from 1 to 3")
    if not args.app.is_dir():
        raise SystemExit(f"app does not exist: {args.app}")

    document = {
        "schemaVersion": 1,
        "target": "flutter-ios",
        "application": "saber",
        "issue": "https://github.com/saber-notes/saber/issues/1603",
        "expected": args.expected,
        "simulatorUdid": args.udid,
        "runtime": "iOS 18.5",
        "bundleId": BUNDLE_ID,
        "appExecutableSha256": app_sha256(args.app),
        "reset": "application uninstalled and reinstalled before every run",
        "minimizedAction": (
            "Create two notes, move exactly one note pair outside the application "
            "container, terminate, and relaunch."
        ),
        "expectedIdentity": FAILURE_IDENTITY,
        "neighboringLegalBehavior": (
            "The second note remains on the Recent Notes list after the first "
            "note is moved out of the application container."
        ),
        "runs": [],
    }
    appium = Appium(args.appium_url)
    for run_number in range(1, args.runs + 1):
        result = execute_run(
            appium,
            args.udid,
            args.app,
            args.expected,
            run_number,
            args.quarantine,
        )
        document["runs"].append(result)
        print(json.dumps(result, sort_keys=True), flush=True)
        if result["exceptions"]:
            break

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    if len(document["runs"]) != args.runs:
        raise SystemExit("campaign did not complete its requested runs")
    if any(result["exceptions"] for result in document["runs"]):
        raise SystemExit("campaign observed an exception or contract failure")


if __name__ == "__main__":
    main()
