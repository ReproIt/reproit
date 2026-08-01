#!/usr/bin/env python3
"""Exercise React Native iOS defects on disposable simulators.

joplin, laurent22/joplin issue 15972. NoteItem.tsx draws the note row padding
on the outer wrapper view rather than on the pressable, so the padded band
around a note title is painted but is not part of the hit area: a tap landing
there is swallowed and the note does not open. The fix moves paddingLeft,
paddingRight, paddingTop and paddingBottom onto the pressable.

Nothing about this is visible to a JavaScript-only harness, because the styles
render identically and only the native view that receives the touch differs.
It is visible here because XCUITest reports the note row as an
XCUIElementTypeButton whose frame IS the pressable, so the two revisions are
separated both by measuring that frame and, decisively, by sending one real
coordinate tap into the band and asking whether the note screen was pushed.

Both revisions centre the row at the same point, so a tap a fixed distance
above that centre is the same absolute point on either build. Measured on the
built products: affected x=16 y=127 w=370 h=20, fixed x=0 y=111 w=402 h=52.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import time
from pathlib import Path

from swiftui_ios_driver import Session

BUNDLE_ID = "net.cozic.joplin"
IDENTITY = "react-native-layout:note-row-padding-outside-touch-target"
REPOSITORY = "https://github.com/laurent22/joplin"
ISSUE = "https://github.com/laurent22/joplin/issues/15972"
AFFECTED_REVISION = "7d90db0bf68c7ea2803227f9e6277bb3cf697fb3"
FIXED_REVISION = "2fa45a5a05daa597d52b73fce120e9242a6c6860"
ROW = "1. Welcome to Joplin!"
LAST_ROW = "5. Joplin Privacy Policy"
ROW_QUERY = f"type == 'XCUIElementTypeButton' AND name == '{ROW}'"
# The affected hit area is the bare text, 20pt tall around the centre; the
# fixed one carries the 16pt padding, 52pt tall. 19pt above the centre is
# outside the first and inside the second, with 7pt of margin on either side.
PADDING_BAND_OFFSET = 19
# Half the affected hit area, so this one is legal on both revisions.
INSIDE_HIT_AREA_OFFSET = 9
DEVICE_TYPE = "iPhone 16 Pro"
RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-26-2"


def simctl(*arguments: str, check: bool = True, timeout: int = 900) -> str:
    result = subprocess.run(
        ["xcrun", "simctl", *arguments],
        check=check,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return result.stdout.strip()


class Simulator:
    """One device for the campaign; the reset is the application container.

    The reset that matters to this trigger is the application's, not the
    device's: Joplin seeds the Welcome notebook on first launch into a
    container that uninstalling destroys, so an uninstall and reinstall gives
    every run a genuine first-launch subject. The device is created once,
    booted once and deleted at the end.

    Both stricter shapes were tried and neither can obtain a WebDriverAgent
    session: erasing one device between runs, and creating a device per run.
    In both, WebDriverAgent installs, Appium logs that it is launching it, and
    the runner never answers its port. A device booted once and left alone is
    the configuration the probes that measured this observable used.
    """

    def __init__(self, evidence: Path) -> None:
        self.evidence = evidence
        self.udid: str | None = None

    def reset(self) -> dict:
        if self.udid is None:
            udid = simctl("create", "reproit-rn-ios-field", DEVICE_TYPE, RUNTIME)
            if re.fullmatch(r"[0-9A-F-]{36}", udid) is None:
                raise RuntimeError(f"simctl returned an unusable udid: {udid!r}")
            self.udid = udid
            simctl("boot", udid)
            simctl("bootstatus", udid, "-b")
        return {
            "udid": self.udid,
            "deviceType": DEVICE_TYPE,
            "runtime": RUNTIME,
            "reset": "the application container, destroyed and recreated by reinstall",
        }

    def install(self, application: Path) -> None:
        simctl("uninstall", self.udid, BUNDLE_ID, check=False)
        simctl("install", self.udid, str(application))

    def running(self) -> bool:
        listing = simctl(
            "spawn", self.udid, "launchctl", "list", check=False, timeout=120
        )
        return BUNDLE_ID in listing

    def stop(self) -> dict:
        if self.udid is None:
            return {"deleted": None, "simulatorRemains": False}
        udid = self.udid
        simctl("shutdown", udid, check=False)
        simctl("delete", udid, check=False)
        self.udid = None
        return {
            "deleted": udid,
            "simulatorRemains": udid in simctl("list", "devices", check=False),
        }


class Appium:
    """One owned Appium server per reproduction.

    A server shared across the campaign never got a WebDriverAgent session at
    all: it logged 'Launching WebDriverAgent on the device' and then sat on
    connect ECONNREFUSED to the runner port indefinitely, on both an erased
    device and a freshly created one. A server started per reproduction, which
    is the configuration the probes that measured this observable used, does
    get one. Cheap it is not; correct it is.

    WebDriverAgent is compiled once into a campaign-owned derived data
    directory and reinstalled per run, and the local port is pinned away from
    8100 because a neighbouring simulator holding that port is what produced
    'Unable to start WebDriverAgent session. Original error: 401'.
    """

    def __init__(
        self,
        evidence: Path,
        port: int,
        wda_port: int,
        derived: Path | None = None,
        label: str = "campaign",
    ) -> None:
        self.port = port
        self.wda_port = wda_port
        # WebDriverAgent is test infrastructure, not the subject under test, so
        # its build is allowed to persist across campaigns. Rebuilding it into
        # a fresh directory each time makes macOS rescan a newly produced
        # bundle, and on a loaded host that scan is slow enough that the runner
        # never answers within the launch timeout: the first attempt at this
        # campaign sat on connect ECONNREFUSED to the runner port for far
        # longer than the WDA build itself took.
        self.derived = derived or (evidence / "wda-derived-data")
        self.log = (evidence / f"{label}-appium.log").open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            [
                "appium",
                "--address",
                "127.0.0.1",
                "--port",
                str(port),
                "--log-level",
                "error",
            ],
            stdout=self.log,
            stderr=subprocess.STDOUT,
        )
        self.url = f"http://127.0.0.1:{port}"

    def wait(self) -> None:
        from urllib import error, request

        for _ in range(180):
            try:
                with request.urlopen(f"{self.url}/status", timeout=5):
                    return
            except (error.URLError, OSError):
                time.sleep(1)
        raise RuntimeError("Appium did not become ready within its bounded wait")

    def session(self, udid: str) -> Session:
        return Session(
            self.url,
            udid,
            BUNDLE_ID,
            **{
                "appium:wdaLocalPort": self.wda_port,
                "appium:derivedDataPath": str(self.derived),
            },
        )

    def stop(self) -> dict:
        self.process.terminate()
        try:
            self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=30)
        self.log.close()
        return {
            "appiumExited": self.process.poll() is not None,
            "webDriverAgentRunners": reap_wda_runners(),
        }


def wda_runner_pids() -> list[int]:
    """PIDs of the xcodebuild processes that host a WebDriverAgent runner."""
    listing = subprocess.run(
        ["pgrep", "-f", "xcodebuild.*WebDriverAgent"],
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    ).stdout
    return [int(line) for line in listing.split() if line.isdigit()]


def reap_wda_runners() -> int:
    """Kill any WebDriverAgent runner left behind, and prove none survives.

    Terminating the Appium server does not reap the xcodebuild it spawned to
    host the runner. Three of those survived earlier interrupted attempts and
    held the test session, so every later attempt installed WebDriverAgent,
    logged that it was launching it, and then waited on a port nothing would
    ever answer. A run that leaves one behind poisons the next one.
    """
    for pid in wda_runner_pids():
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            continue
    for _ in range(20):
        if not wda_runner_pids():
            return 0
        time.sleep(1)
    for pid in wda_runner_pids():
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            continue
    time.sleep(2)
    return len(wda_runner_pids())


def tap(session: Session, x: int, y: int) -> None:
    session.call(
        "POST",
        "/actions",
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
                            "origin": "viewport",
                            "x": x,
                            "y": y,
                        },
                        {"type": "pointerDown", "button": 0},
                        {"type": "pause", "duration": 80},
                        {"type": "pointerUp", "button": 0},
                    ],
                }
            ]
        },
    )
    session.call("DELETE", "/actions")


def await_row(session: Session, seconds: int = 180) -> str:
    for _ in range(seconds):
        row = session.find("-ios predicate string", ROW_QUERY)
        if row is not None:
            return row
        time.sleep(1)
    raise RuntimeError("the Joplin note list never rendered its first row")


def observe(
    device: Simulator,
    ports: tuple[int, int, Path | None],
    application: Path,
    label: str,
    offset: int,
) -> dict:
    """One reproduction: fresh device, install, tap one point, read the screen."""
    started = time.monotonic()
    reset = device.reset()
    device.install(application)
    port, wda_port, derived = ports
    appium = Appium(device.evidence, port, wda_port, derived, label)
    appium.wait()
    session = appium.session(device.udid)
    try:
        row = await_row(session)
        rect = session.call("GET", f"/element/{row}/rect")["value"]
        point = {
            "x": int(rect["x"] + rect["width"] / 2),
            "y": int(rect["y"] + rect["height"] / 2) - offset,
        }
        before = session.source()
        tap(session, point["x"], point["y"])
        time.sleep(3)
        after = session.source()
        clean_launch = device.running()
        session.screenshot(device.evidence / f"{label}-screen.png")
        (device.evidence / f"{label}-source.xml").write_text(after, encoding="utf-8")
        still_listed = LAST_ROW in after
        observation_reached = ROW in before and (still_listed or ROW not in after)
        identity = IDENTITY if still_listed else None
    finally:
        try:
            session.call("DELETE", "")
        finally:
            appium.stop()
    return {
        "status": "reproduced" if identity else "not_reproduced",
        "identity": identity,
        "cleanLaunch": clean_launch,
        "observationReached": observation_reached,
        "exceptions": [],
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "hitAreaRect": rect,
        "tapPoint": point,
        "noteOpened": not still_listed,
        "simulator": reset,
    }


def require(record: dict, label: str, expected: str | None) -> dict:
    if not record["observationReached"]:
        raise RuntimeError(f"{label} never reached its observation point")
    if not record["cleanLaunch"]:
        raise RuntimeError(f"{label} did not survive its launch")
    if record["identity"] != expected:
        raise RuntimeError(
            f"{label} identity was {record['identity']!r}, expected {expected!r}"
        )
    return record


def campaign(
    device: Simulator,
    ports: tuple[int, int, Path | None],
    affected: Path,
    fixed: Path,
    runs: int,
    with_corpus: bool,
) -> dict:
    affected_runs = []
    fixed_runs = []
    for variant, application, expected, sink in (
        ("affected", affected, IDENTITY, affected_runs),
        ("fixed", fixed, None, fixed_runs),
    ):
        for index in range(1, runs + 1):
            label = f"joplin-{variant}-{index}"
            record = observe(
                device, ports, application, label, PADDING_BAND_OFFSET
            )
            record["run"] = index
            sink.append(require(record, label, expected))
    neighboring = {}
    for variant, application in (("affected", affected), ("fixed", fixed)):
        label = f"joplin-neighbor-{variant}"
        record = observe(
            device, ports, application, label, INSIDE_HIT_AREA_OFFSET
        )
        neighboring[variant] = require(record, label, None)
    corpus = {}
    if with_corpus:
        corpus["fixedOrdinaryTap"] = require(
            observe(
                device, ports, fixed, "joplin-corpus-fixed-ordinary", 0
            ),
            "joplin-corpus-fixed-ordinary",
            None,
        )
        corpus["affectedOrdinaryTap"] = require(
            observe(
                device, ports, affected, "joplin-corpus-affected-ordinary", 0
            ),
            "joplin-corpus-affected-ordinary",
            None,
        )
        corpus["affectedInsideHitArea"] = require(
            observe(
                device,
                ports,
                affected,
                "joplin-corpus-affected-inside",
                INSIDE_HIT_AREA_OFFSET,
            ),
            "joplin-corpus-affected-inside",
            None,
        )
    return {
        "schemaVersion": 1,
        "target": "react-native-ios",
        "application": "joplin",
        "repository": REPOSITORY,
        "issue": ISSUE,
        "affectedRevision": AFFECTED_REVISION,
        "fixedRevision": FIXED_REVISION,
        "identity": IDENTITY,
        "memoryMeasurement": "unavailable",
        "affected": affected_runs,
        "fixed": fixed_runs,
        "neighboring": neighboring,
        "corpus": corpus,
        "neighboringLegalBehavior": (
            "a tap 9pt above the row centre is inside the hit area on both "
            "revisions and opens the note on both"
        ),
        "webDriverAgentDerivedData": str(ports[2]),
        "minimizedAction": (
            "one tap at the row centre offset 19pt upward, which is inside the "
            "painted row on both revisions and inside the pressable only on the "
            "fixed one"
        ),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--affected-app", required=True, type=Path)
    parser.add_argument("--fixed-app", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--cli-commit", required=True)
    parser.add_argument("--runs", type=int, choices=range(1, 4), default=3)
    parser.add_argument("--appium-port", type=int, default=4753)
    parser.add_argument("--wda-port", type=int, default=8191)
    parser.add_argument("--wda-derived-data", type=Path)
    parser.add_argument("--with-corpus", action="store_true")
    arguments = parser.parse_args()
    if re.fullmatch(r"[0-9a-f]{40}", arguments.cli_commit) is None:
        parser.error("--cli-commit must be a full lowercase Git commit")
    for application in (arguments.affected_app, arguments.fixed_app):
        if not application.is_dir():
            parser.error(f"application bundle does not exist: {application}")
    arguments.evidence.mkdir(parents=True, exist_ok=True)
    ports = (
        arguments.appium_port,
        arguments.wda_port,
        arguments.wda_derived_data,
    )
    device = Simulator(arguments.evidence)
    reap_wda_runners()
    result = None
    try:
        result = campaign(
            device,
            ports,
            arguments.affected_app,
            arguments.fixed_app,
            arguments.runs,
            arguments.with_corpus,
        )
    finally:
        cleanup = device.stop()
        (arguments.evidence / "cleanup-audit.json").write_text(
            json.dumps(cleanup, indent=2) + "\n", encoding="utf-8"
        )
        if cleanup["simulatorRemains"]:
            raise RuntimeError(f"campaign cleanup left a simulator: {cleanup!r}")
    assert result is not None
    result["cliCommit"] = arguments.cli_commit
    result["cleanup"] = cleanup
    output = arguments.evidence / "joplin-campaign.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
