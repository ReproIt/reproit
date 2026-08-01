#!/usr/bin/env python3
"""Bounded offline AT-SPI probes for the Linux wxWidgets field campaign."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import time

from atspi_helpers import (
    do_action,
    find_application,
    node_record,
    wait_until,
    walk,
)

WAIT_SECONDS = 40

# The pane the defect forces open, plus two neighbours that must keep their
# own default visibility on both revisions.
GREEK_PANE = "Greek Letters"
NEIGHBOR_SHOWN = "Main Toolbar"
NEIGHBOR_HIDDEN = "Statistics"


def start_desktop() -> tuple[subprocess.Popen[str], subprocess.Popen[str]]:
    xvfb = subprocess.Popen(
        ["Xvfb", ":99", "-screen", "0", "1280x900x24", "-nolisten", "tcp"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    wait_until(lambda: pathlib.Path("/tmp/.X11-unix/X99").exists(), "Xvfb")
    openbox = subprocess.Popen(
        ["openbox"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    wait_until(
        lambda: subprocess.run(["wmctrl", "-m"], capture_output=True).returncode == 0,
        "Openbox",
    )
    return xvfb, openbox


def launch(binary: str, home: pathlib.Path) -> subprocess.Popen[str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local/share"),
            "XDG_STATE_HOME": str(home / ".local/state"),
            "GTK_MODULES": "gail:atk-bridge",
        }
    )
    return subprocess.Popen(
        [binary],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )


def stop(process: subprocess.Popen[str]) -> dict[str, object]:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)
    stdout, stderr = process.communicate(timeout=10)
    return {
        "exitCode": process.returncode,
        "stdoutTail": stdout[-2_048:],
        "stderrTail": stderr[-2_048:],
    }


def pane_item(application: object, pane: str) -> object:
    def candidate() -> object | None:
        for node in walk(application):
            record = node_record(node)
            if record["role"] == "check menu item" and record["name"] == pane:
                return node
        return None

    return wait_until(candidate, f"{pane!r} view check menu item")


def pane_record(application: object, pane: str) -> dict[str, object]:
    record = node_record(pane_item(application, pane))
    record["shown"] = "checked" in record["states"]
    return record


def probe_wxmaxima(binary: str, run_root: pathlib.Path, variant: str) -> dict[str, object]:
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    process = launch(binary, home)
    try:
        application = find_application(r"maxima")
        greek = pane_record(application, GREEK_PANE)
        shown_neighbor = pane_record(application, NEIGHBOR_SHOWN)
        hidden_neighbor = pane_record(application, NEIGHBOR_HIDDEN)
        # The defect forces the Greek pane open. Toggling it must still work,
        # which proves the read is a live query and not a stale snapshot.
        do_action(pane_item(application, GREEK_PANE))
        toggled = wait_until(
            lambda: pane_record(application, GREEK_PANE)["shown"] != greek["shown"],
            f"{GREEK_PANE!r} responding to its own view toggle",
        )
        after_toggle = pane_record(application, GREEK_PANE)
        del toggled
        if variant == "neighboring-panes":
            offending = shown_neighbor["shown"] is False or hidden_neighbor["shown"]
        else:
            offending = greek["shown"]
        return {
            "identity": (
                "aui-perspective:greek-pane-forced-open-on-launch"
                if offending
                else None
            ),
            "variant": variant,
            "observationReached": True,
            "cleanLaunch": True,
            "exceptions": [],
            "memoryMeasurement": "unavailable",
            "jsHeapMiB": None,
            "elapsedSeconds": round(time.monotonic() - started, 3),
            "atspiApplication": application.name,
            "greekPane": greek,
            "greekPaneAfterOwnToggle": after_toggle,
            "neighboringLegalBehavior": {
                "shownByDefault": shown_neighbor,
                "hiddenByDefault": hidden_neighbor,
                "greekPaneRespondsToToggle": after_toggle["shown"] != greek["shown"],
            },
        }
    finally:
        record = stop(process)
        if record["exitCode"] not in {0, -signal.SIGTERM}:
            print(json.dumps({"process": record}), file=sys.stderr)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", choices=("wxmaxima",), required=True)
    parser.add_argument("--revision", choices=("affected", "fixed"), required=True)
    parser.add_argument("--run", type=int, choices=range(1, 4), required=True)
    parser.add_argument(
        "--variant",
        choices=("default", "neighboring-panes"),
        default="default",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    run_root = pathlib.Path("/tmp/reproit-field") / (
        f"{args.application}-{args.revision}-{args.variant}-{args.run}"
    )
    if run_root.exists():
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True)
    xvfb, openbox = start_desktop()
    try:
        result = probe_wxmaxima(
            f"/opt/reproit/wxmaxima-{args.revision}",
            run_root,
            args.variant,
        )
    except Exception as error:
        result = {
            "identity": None,
            "variant": args.variant,
            "observationReached": False,
            "cleanLaunch": True,
            "exceptions": [f"{type(error).__name__}: {error}"],
            "memoryMeasurement": "unavailable",
            "jsHeapMiB": None,
        }
    finally:
        stop(openbox)
        stop(xvfb)
    result.update(
        {
            "application": args.application,
            "revision": args.revision,
            "run": args.run,
        }
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, sort_keys=True))
    raise SystemExit(0 if result["observationReached"] else 1)


if __name__ == "__main__":
    main()
