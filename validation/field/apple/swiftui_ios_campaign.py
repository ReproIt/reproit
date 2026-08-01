#!/usr/bin/env python3
"""Run one swiftui-ios field reproduction and retain its evidence.

One invocation is one run against one exact build on one disposable simulator.
The caller supplies the revision under test and whether this run is the
neighboring-legal-behavior control; the driver never decides that for itself,
so an affected run and a fixed run differ only in the application bundle.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from pathlib import Path

from swiftui_ios_driver import (
    DIME_BUNDLE_ID,
    DIME_IDENTITY,
    KIWIX_BUNDLE_ID,
    KIWIX_IDENTITY,
    MAX_SETUP_SECONDS,
    Session,
    dime_observe,
    dime_setup,
    kiwix_observe,
    reset_application,
    start_server,
)

APPLICATIONS = {
    "dime": {
        "bundleId": DIME_BUNDLE_ID,
        "identity": DIME_IDENTITY,
        "binary": "dime",
    },
    "kiwix": {
        "bundleId": KIWIX_BUNDLE_ID,
        "identity": KIWIX_IDENTITY,
        "binary": "Kiwix",
    },
}


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", choices=sorted(APPLICATIONS), required=True)
    parser.add_argument("--revision", choices=("affected", "fixed"), required=True)
    parser.add_argument("--app-bundle", type=Path, required=True, help="the .app bundle")
    parser.add_argument("--udid", required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--appium-port", type=int, default=4733)
    parser.add_argument("--wda-port", type=int, default=8230)
    parser.add_argument("--legal-neighbor", action="store_true")
    arguments = parser.parse_args()

    profile = APPLICATIONS[arguments.application]
    arguments.evidence.mkdir(parents=True, exist_ok=True)
    record = {
        "tag": arguments.tag,
        "application": arguments.application,
        "revision": arguments.revision,
        "udid": arguments.udid,
        "legalNeighbor": arguments.legal_neighbor,
        "binarySha256": digest(arguments.app_bundle / profile["binary"]),
    }

    reset_application(arguments.udid, profile["bundleId"], arguments.app_bundle)
    server = start_server(
        arguments.evidence / f"{arguments.tag}-appium.log", arguments.appium_port
    )
    session = None
    started_at = time.time()
    try:
        session = Session(
            f"http://127.0.0.1:{arguments.appium_port}",
            arguments.udid,
            profile["bundleId"],
            **{
                "appium:wdaLocalPort": arguments.wda_port,
            },
        )
        record["sessionId"] = session.session_id
        time.sleep(8 if arguments.application == "kiwix" else 4)
        if arguments.application == "dime":
            dime_setup(session)
        setup_seconds = round(time.time() - started_at, 3)
        if setup_seconds > MAX_SETUP_SECONDS:
            raise RuntimeError(f"setup exceeded its {MAX_SETUP_SECONDS}-second bound")
        record["setupSeconds"] = setup_seconds

        replay_at = time.time()
        if arguments.application == "dime":
            observation = dime_observe(
                session, mode="nonzero" if arguments.legal_neighbor else "exact"
            )
        else:
            observation = kiwix_observe(session, legal_neighbor=arguments.legal_neighbor)
        record["replaySeconds"] = round(time.time() - replay_at, 3)
        record["observation"] = observation
        record["cleanLaunch"] = observation["cleanLaunch"]
        record["observationReached"] = observation["observationReached"]
        record["identity"] = observation["identity"]
        record["status"] = "reproduced" if observation["identity"] else "not_reproduced"
        record["exceptions"] = []
        (arguments.evidence / f"{arguments.tag}-observation.xml").write_text(
            session.source(), encoding="utf-8"
        )
        session.screenshot(arguments.evidence / f"{arguments.tag}-observation.png")
    except Exception as failure:  # noqa: BLE001 - a failed run is still evidence
        record["status"] = "error"
        record["observationReached"] = False
        record["identity"] = None
        record["exceptions"] = [repr(failure)[:500]]
    finally:
        if session is not None:
            try:
                session.close()
            except Exception:  # noqa: BLE001
                pass
        server.terminate()

    if record["status"] != "error":
        expected = profile["identity"]
        if arguments.revision == "affected" and not arguments.legal_neighbor:
            if record["identity"] != expected:
                record["contractViolation"] = "affected run did not report the identity"
        elif record["identity"] is not None:
            record["contractViolation"] = "a control run reported the defect identity"

    output = arguments.evidence / f"{arguments.tag}-run.json"
    output.write_text(json.dumps(record, indent=1) + "\n", encoding="utf-8")
    print(json.dumps(record, indent=1))
    raise SystemExit(0 if record["status"] != "error" else 1)


if __name__ == "__main__":
    main()
