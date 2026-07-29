#!/usr/bin/env python3
"""Reproduce LocalSend issue 2904 through its real iOS network server and UI."""

from __future__ import annotations

import argparse
import hashlib
import json
import ssl
import subprocess
import threading
import time
import urllib.request
from pathlib import Path

from saber_recent_notes import Appium

BUNDLE_ID = "org.localsend.localsendApp"
PORT = 53317
TRIGGER = "https://example.com followed by text"
CONTROL = "https://example.com"
FAILURE_IDENTITY = "receive-message:compound-uri-misclassified-as-link:open-button-visible"


def run(command: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(
        command,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    )
    return result.stdout.strip() if capture else ""


def app_sha256(app_path: Path) -> str:
    executable = app_path / "Frameworks" / "App.framework" / "App"
    return f"sha256:{hashlib.sha256(executable.read_bytes()).hexdigest()}"


def request_payload(message: str) -> bytes:
    return json.dumps(
        {
            "info": {
                "alias": "Campaign Sender",
                "version": "2.1",
                "deviceModel": "macOS host",
                "deviceType": "desktop",
                "fingerprint": "campaign-sender-2904",
                "port": 53318,
                "protocol": "https",
                "download": False,
            },
            "files": {
                "message": {
                    "id": "message",
                    "fileName": "message.txt",
                    "size": len(message.encode()),
                    "fileType": "text/plain",
                    "preview": message,
                }
            },
        }
    ).encode()


class PendingRequest:
    def __init__(self, message: str) -> None:
        self.message = message
        self.status: int | None = None
        self.error: str | None = None
        self.thread = threading.Thread(target=self._send, daemon=True)

    def _send(self) -> None:
        request = urllib.request.Request(
            f"https://127.0.0.1:{PORT}/api/localsend/v2/prepare-upload",
            data=request_payload(self.message),
            method="POST",
            headers={"Content-Type": "application/json"},
        )
        context = ssl.create_default_context()
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        try:
            with urllib.request.urlopen(
                request,
                context=context,
                timeout=60,
            ) as response:
                response.read()
                self.status = response.status
        except Exception as error:
            self.error = f"{type(error).__name__}: {error}"

    def start(self) -> None:
        self.thread.start()

    def finish(self) -> None:
        self.thread.join(timeout=20)
        if self.thread.is_alive():
            raise RuntimeError("LocalSend request did not finish")
        if self.error:
            raise RuntimeError(self.error)
        if self.status != 204:
            raise RuntimeError(f"LocalSend returned status {self.status}, expected 204")


def wait_for_server() -> None:
    context = ssl.create_default_context()
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    for _ in range(30):
        try:
            with urllib.request.urlopen(
                f"https://127.0.0.1:{PORT}/api/localsend/v2/info",
                context=context,
                timeout=2,
            ) as response:
                if response.status == 200:
                    return
        except OSError:
            time.sleep(1)
    raise RuntimeError("LocalSend HTTPS server did not become ready")


def wait_for_message(appium: Appium, message: str) -> str:
    for _ in range(20):
        source = appium.source()
        if f'value="{message}"' in source:
            return source
        time.sleep(1)
    raise RuntimeError(f"LocalSend did not render the message: {message}")


def wait_for_home(appium: Appium) -> None:
    for _ in range(20):
        source = appium.source()
        if 'name="Quick Save"' in source and 'Tab 1 of 3' in source:
            return
        time.sleep(1)
    raise RuntimeError("LocalSend did not return to its receive home screen")


def deliver_and_observe(appium: Appium, message: str) -> tuple[str, bool]:
    pending = PendingRequest(message)
    pending.start()
    source = wait_for_message(appium, message)
    is_link = 'name="Open"' in source and 'sent you a link:' in source
    appium.click("accessibility id", "Close")
    pending.finish()
    wait_for_home(appium)
    return source, is_link


def execute_run(
    appium: Appium,
    udid: str,
    app_path: Path,
    expected: str,
    run_number: int,
) -> dict:
    started = time.monotonic()
    exceptions: list[str] = []
    trigger_source = ""
    control_source = ""
    trigger_is_link = False
    control_is_link = False
    try:
        try:
            run(["xcrun", "simctl", "terminate", udid, BUNDLE_ID])
        except subprocess.CalledProcessError:
            pass
        try:
            run(["xcrun", "simctl", "uninstall", udid, BUNDLE_ID])
        except subprocess.CalledProcessError:
            pass
        run(["xcrun", "simctl", "install", udid, str(app_path)])
        appium.start(udid, 18451, BUNDLE_ID)
        wait_for_server()
        trigger_source, trigger_is_link = deliver_and_observe(appium, TRIGGER)
        control_source, control_is_link = deliver_and_observe(appium, CONTROL)
    except Exception as error:
        exceptions.append(f"{type(error).__name__}: {error}")
    finally:
        appium.close()

    expected_failure = expected == "affected"
    if trigger_is_link != expected_failure:
        exceptions.append(
            f"compound URI isLink={trigger_is_link}, expected={expected_failure}"
        )
    if not control_is_link:
        exceptions.append("neighboring exact URL was not classified as a link")
    return {
        "run": run_number,
        "cleanLaunch": True,
        "observationReached": bool(trigger_source),
        "identity": FAILURE_IDENTITY if trigger_is_link else None,
        "neighboringControlReached": control_is_link,
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "exceptions": exceptions,
        "triggerSourceSha256": (
            f"sha256:{hashlib.sha256(trigger_source.encode()).hexdigest()}"
            if trigger_source
            else None
        ),
        "controlSourceSha256": (
            f"sha256:{hashlib.sha256(control_source.encode()).hexdigest()}"
            if control_source
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
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.runs <= 3:
        raise SystemExit("--runs must be from 1 to 3")
    if not args.app.is_dir():
        raise SystemExit(f"app does not exist: {args.app}")

    document = {
        "schemaVersion": 1,
        "target": "flutter-ios",
        "application": "localsend",
        "issue": "https://github.com/localsend/localsend/issues/2904",
        "expected": args.expected,
        "simulatorUdid": args.udid,
        "runtime": "iOS 18.5",
        "bundleId": BUNDLE_ID,
        "appExecutableSha256": app_sha256(args.app),
        "reset": "application uninstalled and reinstalled before every run",
        "minimizedAction": (
            "POST one text/plain message whose preview is a URI followed by "
            "whitespace and text to the application's real LocalSend v2 HTTPS server."
        ),
        "expectedIdentity": FAILURE_IDENTITY,
        "neighboringLegalBehavior": (
            "A message containing exactly one absolute URL remains classified "
            "as a link and exposes the Open button."
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
