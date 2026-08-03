#!/usr/bin/env python3
"""Run the bounded macOS Accessibility field campaign against prebuilt apps."""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PROBE_SOURCE = ROOT / "validation/field/apple/macos_ax_probe.swift"
PLIST_BUDDY = "/usr/libexec/PlistBuddy"
MAX_READY_ATTEMPTS = 60
READY_INTERVAL_SECONDS = 0.25


@dataclass(frozen=True)
class AppBuild:
    application: str
    revision_kind: str
    source: Path
    executable_name: str


def run_command(
    arguments: list[str],
    *,
    check: bool = True,
    timeout_seconds: float = 30,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        arguments,
        check=check,
        capture_output=True,
        text=True,
        timeout=timeout_seconds,
    )


def prepare_app(build: AppBuild, run_root: Path, bundle_id: str) -> tuple[Path, Path]:
    if not build.source.is_dir():
        raise RuntimeError(f"missing prebuilt app: {build.source}")
    app = run_root / f"{build.application}.app"
    run_command(["/usr/bin/ditto", str(build.source), str(app)], timeout_seconds=120)
    info_plist = app / "Contents/Info.plist"
    run_command([PLIST_BUDDY, "-c", f"Set :CFBundleIdentifier {bundle_id}", str(info_plist)])
    run_command(["/usr/bin/codesign", "--force", "--sign", "-", str(app)])
    run_command(["/usr/bin/codesign", "--verify", "--strict", str(app)])
    return app, app / "Contents/MacOS" / build.executable_name


def compile_probe(output_root: Path) -> Path:
    probe = output_root / "macos_ax_probe"
    run_command(
        ["/usr/bin/xcrun", "swiftc", str(PROBE_SOURCE), "-o", str(probe)],
        timeout_seconds=120,
    )
    return probe


def probe_value(probe: Path, bundle_id: str, element: str, attribute: str) -> dict:
    result = run_command(
        [str(probe), bundle_id, element, attribute],
        timeout_seconds=10,
    )
    return json.loads(result.stdout)


def wait_for_probe(
    probe: Path,
    bundle_id: str,
    element: str,
    attribute: str,
) -> dict:
    last_error = "probe did not run"
    for _ in range(MAX_READY_ATTEMPTS):
        try:
            return probe_value(probe, bundle_id, element, attribute)
        except (subprocess.SubprocessError, json.JSONDecodeError) as error:
            last_error = str(error)
            time.sleep(READY_INTERVAL_SECONDS)
    raise RuntimeError(f"app did not become AX-ready: {last_error}")


def launch_app(app: Path, state_root: Path, subject: Path | None = None) -> None:
    arguments = [
        "/usr/bin/open",
        "-n",
        "-F",
        "-a",
        str(app),
        "--env",
        f"CFFIXED_USER_HOME={state_root}",
        "--env",
        "HTTP_PROXY=http://127.0.0.1:9",
        "--env",
        "HTTPS_PROXY=http://127.0.0.1:9",
        "--env",
        "ALL_PROXY=http://127.0.0.1:9",
        "--env",
        "NO_PROXY=localhost,127.0.0.1",
    ]
    if subject is not None:
        arguments.append(str(subject))
    run_command(arguments)


def process_id(executable: Path) -> int:
    result = run_command(["/usr/bin/pgrep", "-f", "-x", str(executable)])
    identifiers = [int(line) for line in result.stdout.splitlines() if line.strip()]
    if len(identifiers) != 1:
        raise RuntimeError(f"expected one owned process for {executable}, got {identifiers}")
    return identifiers[0]


def network_observation(pid: int) -> dict:
    result = run_command(
        [
            "/usr/sbin/lsof",
            "-nP",
            "-a",
            "-p",
            str(pid),
            "-iTCP",
            "-sTCP:ESTABLISHED",
        ],
        check=False,
    )
    connections = [line for line in result.stdout.splitlines()[1:] if line.strip()]
    external = [line for line in connections if "127.0.0.1" not in line and "[::1]" not in line]
    if external:
        raise RuntimeError(f"application established external connections: {external}")
    return {
        "policy": "HTTP(S) and ALL proxy sent to closed loopback port 9",
        "externalEstablishedConnections": external,
        "observedConnections": connections,
    }


def stop_owned_process(executable: Path) -> None:
    result = run_command(
        ["/usr/bin/pgrep", "-f", "-x", str(executable)],
        check=False,
    )
    identifiers = [int(line) for line in result.stdout.splitlines() if line.strip()]
    for pid in identifiers:
        os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while identifiers and time.monotonic() < deadline:
        live = []
        for pid in identifiers:
            try:
                os.kill(pid, 0)
                live.append(pid)
            except ProcessLookupError:
                pass
        identifiers = live
        if identifiers:
            time.sleep(0.1)
    for pid in identifiers:
        os.kill(pid, signal.SIGKILL)


def base_run(run_number: int, elapsed_seconds: float, observation: dict, network: dict) -> dict:
    return {
        "run": run_number,
        "cleanLaunch": True,
        "exceptions": [],
        "jsHeapMiB": None,
        "elapsedSeconds": round(elapsed_seconds, 3),
        "observationReached": True,
        "observation": observation,
        "network": network,
    }


def platypus_run(
    build: AppBuild,
    probe: Path,
    output_root: Path,
    run_number: int,
) -> dict:
    run_root = output_root / f"platypus-{build.revision_kind}-{run_number}"
    run_root.mkdir()
    bundle_id = f"com.reproit.field.platypus.{build.revision_kind}.r{run_number}"
    app, executable = prepare_app(build, run_root, bundle_id)
    started = time.monotonic()
    try:
        launch_app(app, run_root)
        notification = wait_for_probe(probe, bundle_id, "Send notifications", "AXHelp")
        neighbor = probe_value(probe, bundle_id, "Run in background", "AXHelp")
        pid = process_id(executable)
        network = network_observation(pid)
    finally:
        stop_owned_process(executable)
    observation = {
        "sendNotificationsHelp": notification["value"],
        "runInBackgroundHelp": neighbor["value"],
    }
    return base_run(run_number, time.monotonic() - started, observation, network)


def coteditor_run(
    build: AppBuild,
    probe: Path,
    output_root: Path,
    run_number: int,
    *,
    focus_document: bool,
) -> dict:
    suffix = "focused" if focus_document else "folder"
    run_root = output_root / f"coteditor-{build.revision_kind}-{suffix}-{run_number}"
    run_root.mkdir()
    bundle_id = f"com.reproit.field.coteditor.{build.revision_kind}.{suffix}.r{run_number}"
    app, executable = prepare_app(build, run_root, bundle_id)
    workspace = run_root / "workspace"
    workspace.mkdir()
    document = workspace / "document.txt"
    document.write_text("field benchmark text\n", encoding="utf-8")
    started = time.monotonic()
    try:
        launch_app(app, run_root, workspace)
        wait_for_probe(probe, bundle_id, "role:AXMenuButton#0", "AXEnabled")
        if focus_document:
            run_command(["/usr/bin/open", "-a", str(app), str(document)])
            wait_for_probe(probe, bundle_id, "role:AXTextArea#0", "AXValue")
        probe_value(probe, bundle_id, "role:AXMenuButton#0", "AXPress")
        new_file = wait_for_probe(probe, bundle_id, "New File", "AXEnabled")
        pid = process_id(executable)
        network = network_observation(pid)
    finally:
        stop_owned_process(executable)
    observation = {
        "documentFocused": focus_document,
        "newFileEnabled": new_file["value"] == "1",
    }
    return base_run(run_number, time.monotonic() - started, observation, network)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--platypus-affected-app", type=Path, required=True)
    parser.add_argument("--platypus-fixed-app", type=Path, required=True)
    parser.add_argument("--coteditor-affected-app", type=Path, required=True)
    parser.add_argument("--coteditor-fixed-app", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    probe = compile_probe(args.output)
    builds = {
        "platypus_affected": AppBuild(
            "Platypus", "affected", args.platypus_affected_app, "Platypus"
        ),
        "platypus_fixed": AppBuild(
            "Platypus", "fixed", args.platypus_fixed_app, "Platypus"
        ),
        "coteditor_affected": AppBuild(
            "CotEditor", "affected", args.coteditor_affected_app, "CotEditor"
        ),
        "coteditor_fixed": AppBuild(
            "CotEditor", "fixed", args.coteditor_fixed_app, "CotEditor"
        ),
    }
    results = {
        "platform": "macOS/arm64",
        "xcode": run_command(["/usr/bin/xcodebuild", "-version"]).stdout.strip(),
        "platypus": {
            "affected": [
                platypus_run(builds["platypus_affected"], probe, args.output, run)
                for run in range(1, 4)
            ],
            "fixed": [
                platypus_run(builds["platypus_fixed"], probe, args.output, run)
                for run in range(1, 4)
            ],
        },
        "coteditor": {
            "affected": [
                coteditor_run(builds["coteditor_affected"], probe, args.output, run,
                              focus_document=True)
                for run in range(1, 4)
            ],
            "fixed": [
                coteditor_run(builds["coteditor_fixed"], probe, args.output, run,
                              focus_document=True)
                for run in range(1, 4)
            ],
            "neighboringLegalBehavior": [
                coteditor_run(builds["coteditor_affected"], probe, args.output, 1,
                              focus_document=False),
                coteditor_run(builds["coteditor_fixed"], probe, args.output, 1,
                              focus_document=False),
            ],
        },
    }
    print(json.dumps(results, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
