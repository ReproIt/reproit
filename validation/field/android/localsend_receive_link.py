#!/usr/bin/env python3
"""Exercise LocalSend's receive-page link classification on Android.

The application is built in profile mode, where the Dart diagnostics behind
debugDumpApp and debugDumpRenderTree are compiled out, so those RPCs answer
with nothing. The channel that does carry the observable is the platform
accessibility hierarchy: driving the device through UiAutomator2 attaches a
UiAutomation, which is an assistive technology as far as the platform is
concerned, so Flutter starts producing semantics and the receive-page subtitle
becomes readable. The Dart VM service is still connected, because it is the
declared runtime bound and it proves the profile isolate is live, but it is not
the read path.

The trigger is offline: a prepare-upload request on loopback carrying exactly
one text file. LocalSend does not answer that request until the receive page is
accepted or declined, so the request is deliberately left pending on its own
thread. The pending request is the state under test.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import ssl
import threading
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

from flutter_android_vm_service import VmService
from nextplayer_permission_loop import (
    AppiumServer,
    AppiumSession,
    Device,
    run_with_reset,
    sha256,
)

AFFECTED_REVISION = "3ec2d77875fc31dab21548ae4966ca693e8b2733"
FIXED_REVISION = "9e4a5985b5fd1377f7c4c1fa9127a00b8fc9abff"
AFFECTED_APK_SHA256 = (
    "e1277b50f7ed45f0bf572fb7d5b9f6be0333238a16399b6d8b2ea5b7624c7448"
)
FIXED_APK_SHA256 = (
    "2780b0293e7d3a07f8de6e4ea16967ac9ebbbd044c9c2d90d56558d64c562675"
)
PACKAGE = "org.localsend.localsend_app"
APP_ACTIVITY = "org.localsend.localsend_app.MainActivity"
IDENTITY = "flutter-receive:trailing-text-message-classified-as-link"
LINK_SUBTITLE = "sent you a link:"
MESSAGE_SUBTITLE = "sent you a message:"
DEFECT_MESSAGE = "https://example.com some extra text"
CONTROL_MESSAGE = "https://example.com"
LOCALSEND_PORT = 53317
SERVICE_PATTERN = re.compile(
    r"Dart VM service is listening on (http://127\.0\.0\.1:\d+/[^/]*/)"
)


def relaxed_context() -> ssl.SSLContext:
    """LocalSend serves its API with a certificate the application generates
    for itself, so the peer here is the device under test rather than a trust
    decision. Verification is off and legacy parameters are accepted."""
    context = ssl.create_default_context()
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    try:
        context.set_ciphers("DEFAULT:@SECLEVEL=0")
    except ssl.SSLError:
        pass
    return context


def prepare_upload_body(message: str) -> bytes:
    return json.dumps(
        {
            "info": {
                "alias": "ReproitField",
                "version": "2.0",
                "deviceModel": "reproit",
                "deviceType": "desktop",
                "fingerprint": str(uuid.uuid4()),
                "port": 53318,
                "protocol": "http",
                "download": False,
            },
            "files": {
                "message-1": {
                    "id": "message-1",
                    "fileName": "message.txt",
                    "size": len(message.encode()),
                    "fileType": "text",
                    "preview": message,
                }
            },
        }
    ).encode()


class PendingUpload:
    """A prepare-upload request that is expected not to answer.

    The application only answers once the receive page is resolved, so the
    request stays open for the whole observation. Connection refusals are
    retried within a bound, because the Dart HTTP server binds a moment after
    the isolate starts."""

    def __init__(self, port: int, message: str) -> None:
        self.port = port
        self.message = message
        self.outcome: dict = {}
        self.thread = threading.Thread(target=self._deliver, daemon=True)

    def start(self) -> None:
        self.thread.start()

    def _deliver(self) -> None:
        body = prepare_upload_body(self.message)
        context = relaxed_context()
        errors = []
        for _ in range(30):
            for scheme in ("https", "http"):
                operation = urllib.request.Request(
                    f"{scheme}://127.0.0.1:{self.port}"
                    "/api/localsend/v2/prepare-upload",
                    data=body,
                    headers={"Content-Type": "application/json"},
                )
                try:
                    with urllib.request.urlopen(
                        operation,
                        timeout=600,
                        context=context if scheme == "https" else None,
                    ) as response:
                        self.outcome = {
                            "scheme": scheme,
                            "status": response.status,
                        }
                        return
                except urllib.error.HTTPError as failure:
                    self.outcome = {"scheme": scheme, "status": failure.code}
                    return
                except OSError as failure:
                    errors.append(f"{scheme}: {type(failure).__name__}: {failure}")
            time.sleep(2)
        self.outcome = {"error": "; ".join(errors[-2:])}

    def pending(self) -> bool:
        return self.thread.is_alive()


def wait_source(
    session: AppiumSession,
    device: Device,
    predicate,
    label: str,
    seconds: int = 90,
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


def attach_vm_service(device: Device, label: str) -> dict:
    """Confirm the declared runtime bound is live. The profile build announces
    its service on stdout, which lands in logcat, so the port is discovered
    rather than assumed."""
    uri = ""
    for _ in range(180):
        logs = device.adb_run("logcat", "-d", capture=True, check=False, timeout=60)
        found = SERVICE_PATTERN.search(logs)
        if found:
            uri = found.group(1)
            break
        time.sleep(1)
    if not uri:
        raise RuntimeError(f"{label}: the profile build announced no VM service")
    port = int(uri.split(":")[2].split("/")[0])
    device.adb_run("forward", f"tcp:{port}", f"tcp:{port}")
    service = VmService(uri)
    try:
        version = service.call("getVersion")
        isolates = []
        for _ in range(90):
            isolates = service.call("getVM").get("isolates", [])
            if isolates:
                break
            time.sleep(1)
        if not isolates:
            raise RuntimeError(f"{label}: the profile build exposed no isolate")
        return {
            "uri": uri,
            "protocolVersion": f"{version.get('major')}.{version.get('minor')}",
            "isolate": isolates[0]["id"],
            "isolateNames": [str(entry.get("name")) for entry in isolates][:5],
            "role": "liveness only; the observable is read from the hierarchy",
        }
    finally:
        service.close()


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
    message: str,
    expected_identity: str | None,
    neighboring: bool,
) -> dict:
    started = time.monotonic()
    device.adb_run("uninstall", PACKAGE, capture=True, check=False)
    device.adb_run("install", "-r", "-t", str(apk), timeout=600)
    device.adb_run("logcat", "-c")
    server = AppiumServer(device.evidence, label)
    session = None
    upload = PendingUpload(LOCALSEND_PORT, message)
    try:
        appium_url = server.start()
        session = AppiumSession(appium_url, device.udid, PACKAGE, APP_ACTIVITY)
        appium = session.evidence()
        wait_source(
            session,
            device,
            lambda value: f'package="{PACKAGE}"' in value,
            f"{label}-launch",
        )
        vm_service = attach_vm_service(device, label)
        device.adb_run("forward", f"tcp:{LOCALSEND_PORT}", f"tcp:{LOCALSEND_PORT}")
        upload.start()
        source = wait_source(
            session,
            device,
            lambda value: LINK_SUBTITLE in value or MESSAGE_SUBTITLE in value,
            f"{label}-receive",
        )
        pending = upload.pending()
        retained = save_observation(session, device, label, source)
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()
            device.adb_run("forward", "--remove-all", check=False, timeout=60)
    foreground = f'package="{PACKAGE}"' in source
    link_subtitle = LINK_SUBTITLE in source
    message_subtitle = MESSAGE_SUBTITLE in source
    open_link_button = 'content-desc="Open"' in source
    observation_reached = foreground and (link_subtitle != message_subtitle)
    identity = IDENTITY if (not neighboring and link_subtitle) else None
    if not observation_reached:
        raise RuntimeError(f"{label} did not reach the receive-page observation")
    if not pending:
        raise RuntimeError(f"{label} prepare-upload answered before the observation")
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
        "message": message,
        "foreground": foreground,
        "linkSubtitleVisible": link_subtitle,
        "messageSubtitleVisible": message_subtitle,
        "openLinkButtonVisible": open_link_button,
        "prepareUploadPending": pending,
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
        "application": "localsend",
        "repository": "https://github.com/localsend/localsend",
        "issue": "https://github.com/localsend/localsend/issues/2904",
        "pullRequest": "https://github.com/localsend/localsend/pull/2975",
        "affectedRevision": AFFECTED_REVISION,
        "fixedRevision": FIXED_REVISION,
        "affectedApkSha256": f"sha256:{AFFECTED_APK_SHA256}",
        "fixedApkSha256": f"sha256:{FIXED_APK_SHA256}",
        "identity": IDENTITY,
        "memoryMeasurement": "unavailable",
        "affected": affected,
        "fixed": fixed,
        "neighboringLegalBehavior": (
            "a bare absolute URL with no trailing text is classified as a link "
            "on both revisions, so the link path itself is untouched and only "
            "the message that merely starts with a URL moves"
        ),
        "neighboring": neighbors,
        "minimizedAction": (
            "one prepare-upload request on loopback carrying exactly one text "
            "file whose preview is the message, left pending until the receive "
            "page subtitle is read"
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
                            device,
                            apk,
                            label,
                            DEFECT_MESSAGE,
                            expected,
                            False,
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
                    device,
                    apk,
                    label,
                    CONTROL_MESSAGE,
                    None,
                    True,
                ),
            )
    finally:
        device.stop()

    result = campaign_result(args.cli_commit, device, affected, fixed, neighbors)
    output = args.evidence / "localsend-receive-link-2904.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
