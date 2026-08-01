#!/usr/bin/env python3
"""Clean and adversarial corpus subject for the swiftui-ios target.

The field benchmark proves the oracle finds its defect. This fixture drives the
same oracle against known-good subjects and must report nothing. Every subject
runs behind the deny-all proxy the flutter-ios corpus uses, and the retained
observation names the sockets the application process held, so "offline" is a
measurement rather than a claim.

The subjects are dimeApp only. The other benchmark application, kiwix-apple,
fetches its online catalogue through URLSession on every launch, and URLSession
does not honour the proxy environment: an attempt to enrol it produced a
measured, retained external connection to download.kiwix.org, so it is not a
corpus subject rather than being recorded as one that happened to be offline.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import time
from pathlib import Path

from swiftui_ios_driver import (
    DIME_BUNDLE_ID,
    DIME_MODES,
    Session,
    dime_observe,
    dime_setup,
    reset_application,
    start_server,
)

PROXY_URL = "http://127.0.0.1:9"
PROXY_ENVIRONMENT = {
    "HTTP_PROXY": PROXY_URL,
    "HTTPS_PROXY": PROXY_URL,
    "ALL_PROXY": PROXY_URL,
    "NO_PROXY": "127.0.0.1,localhost,::1",
}
PROCESS_ID = re.compile(r'<XCUIElementTypeApplication[^>]*\bprocessId="(\d+)"')


def application_process_id(source: str) -> int:
    match = PROCESS_ID.search(source)
    if match is None:
        raise RuntimeError("the retained UI source has no application process id")
    return int(match.group(1))


def network_observation(process_id: int) -> dict:
    environment = subprocess.run(
        ["ps", "eww", "-p", str(process_id)],
        capture_output=True,
        text=True,
        timeout=30,
    ).stdout
    present = all(f"{name}={value}" in environment for name, value in PROXY_ENVIRONMENT.items())
    if not present:
        raise RuntimeError("the deny-all proxy environment was not present in the application")
    sockets = subprocess.run(
        ["lsof", "-nP", "-a", "-p", str(process_id), "-i"],
        capture_output=True,
        text=True,
        timeout=60,
    ).stdout.splitlines()
    endpoints = [line.split(None, 8)[-1] for line in sockets[1:] if line.strip()]
    external = [
        endpoint
        for endpoint in endpoints
        if "(ESTABLISHED)" in endpoint
        and "127.0.0.1" not in endpoint
        and "[::1]" not in endpoint
    ]
    if external:
        raise RuntimeError(f"the corpus subject held external connections: {external}")
    return {
        "policy": "deny non-loopback HTTP(S) and ALL proxy at 127.0.0.1:9",
        "proxyEnvironmentPresent": present,
        "observedIpEndpoints": endpoints,
        "externalEstablishedConnections": external,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", choices=("clean", "adversarial"), required=True)
    parser.add_argument("--mode", choices=DIME_MODES, required=True)
    parser.add_argument("--app-bundle", type=Path, required=True)
    parser.add_argument("--udid", required=True)
    parser.add_argument("--case", required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--appium-port", type=int, default=4733)
    parser.add_argument("--wda-port", type=int, default=8230)
    arguments = parser.parse_args()

    bundle_id = DIME_BUNDLE_ID
    arguments.evidence.mkdir(parents=True, exist_ok=True)
    # A clean subject is the fixed build. An adversarial subject is the affected
    # build driven through an adjacent, legal action on the same control.
    if arguments.kind == "adversarial" and arguments.mode == "exact":
        raise SystemExit("an adversarial subject must not use the defect trigger")

    reset_application(arguments.udid, bundle_id, arguments.app_bundle)
    server = start_server(
        arguments.evidence / f"{arguments.case}-appium.log", arguments.appium_port
    )
    session = None
    record = {"case": arguments.case, "kind": arguments.kind, "bundleId": bundle_id}
    try:
        session = Session(
            f"http://127.0.0.1:{arguments.appium_port}",
            arguments.udid,
            bundle_id,
            **{
                "appium:wdaLocalPort": arguments.wda_port,
                "appium:processArguments": {"env": PROXY_ENVIRONMENT, "args": []},
            },
        )
        time.sleep(4)
        dime_setup(session)
        observation = dime_observe(session, mode=arguments.mode)
        source = session.source()
        (arguments.evidence / f"{arguments.case}-observation.xml").write_text(
            source, encoding="utf-8"
        )
        session.screenshot(arguments.evidence / f"{arguments.case}-observation.png")
        observation["network"] = network_observation(application_process_id(source))
        record["observation"] = observation
        record["falsePositive"] = observation["identity"] is not None
    except Exception as failure:  # noqa: BLE001 - a failed subject is still evidence
        record["error"] = repr(failure)[:500]
    finally:
        if session is not None:
            try:
                session.close()
            except Exception:  # noqa: BLE001
                pass
        server.terminate()

    output = arguments.evidence / f"{arguments.case}-corpus.json"
    output.write_text(json.dumps(record, indent=1) + "\n", encoding="utf-8")
    print(json.dumps(record, indent=1))
    failed = "error" in record or record.get("falsePositive")
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    main()
