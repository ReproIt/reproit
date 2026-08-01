#!/usr/bin/env python3
"""Bounded offline AT-SPI probes for the Linux GTK field campaign."""

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

from atspi_helpers import find_application, node_record, wait_until, walk

WAIT_SECONDS = 40
ENTRY_ROLES = {"entry", "text", "password text"}
BUTTON_ROLES = {"button", "push button"}

# gnome-text-editor: the two search-bar entries the fix labels.
SEARCH_LABEL = "Search"
REPLACE_LABEL = "Replace"

# gnome-clocks: the dialog whose initial focus the fix moves to its entry.
CLOCK_DIALOG = "Add a New World Clock"
ADD_CLOCK_BUTTON = "Add World Clock"
TAB_STEPS = 6


def start_desktop() -> tuple[subprocess.Popen[str], subprocess.Popen[str]]:
    xvfb = subprocess.Popen(
        ["Xvfb", ":99", "-screen", "0", "1400x900x24", "-nolisten", "tcp"],
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


def launch(
    binary: str,
    arguments: list[str],
    home: pathlib.Path,
    prefix: str,
) -> subprocess.Popen[str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local/share"),
            "XDG_STATE_HOME": str(home / ".local/state"),
            "GSETTINGS_SCHEMA_DIR": f"{prefix}/share/glib-2.0/schemas",
            "XDG_DATA_DIRS": f"{prefix}/share:/usr/share",
            "LC_ALL": "C.UTF-8",
        }
    )
    return subprocess.Popen(
        [binary, *arguments],
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


def press(key: str) -> None:
    subprocess.run(
        ["xdotool", "key", "--clearmodifiers", key],
        check=True,
        timeout=WAIT_SECONDS,
    )


def records(application: object) -> list[dict[str, object]]:
    return [node_record(node) for node in walk(application)]


def activate(node: object, record: dict[str, object]) -> str:
    """Press an accessible, preferring its own action over a pointer click."""
    try:
        action = node.queryAction()
        if action.nActions > 0:
            action.doAction(0)
            return "atspi-action"
    except Exception:
        pass
    extents = node.queryComponent().getExtents(0)
    if extents.width <= 0 or extents.height <= 0:
        raise RuntimeError(f"cannot click zero-sized node {record['name']!r}")
    subprocess.run(
        [
            "xdotool",
            "mousemove",
            "--sync",
            str(extents.x + extents.width // 2),
            str(extents.y + extents.height // 2),
        ],
        check=True,
        timeout=WAIT_SECONDS,
    )
    subprocess.run(["xdotool", "click", "1"], check=True, timeout=WAIT_SECONDS)
    return "xtest-click"


def probe_text_editor(
    binary: str,
    prefix: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    document = home / "fixture.c"
    document.write_text("int main(void) { return 0; }\n", encoding="utf-8")
    started = time.monotonic()
    process = launch(binary, [str(document)], home, prefix)
    try:
        application = find_application(r"text.?editor")
        wait_until(
            lambda: any(
                record["role"] == "frame" and "Text Editor" in str(record["name"])
                for record in records(application)
            ),
            "Text Editor window",
        )
        subprocess.run(["wmctrl", "-a", "Text Editor"], check=False, timeout=20)
        # Ctrl+H opens the search bar in replace mode, so both entries the fix
        # labels are realised. No popover is involved: GTK 4 popover menu items
        # are not exposed over AT-SPI at all, which is why the Document Type
        # candidate for this application is unreachable and this one is not.
        press("ctrl+h")
        tree = wait_until(
            lambda: (
                records(application)
                if any(
                    record["role"] in BUTTON_ROLES
                    and record["name"] == "Close Search"
                    for record in records(application)
                )
                else None
            ),
            "search bar to open",
        )
        entries = [record for record in tree if record["role"] in ENTRY_ROLES]
        buttons = sorted(
            {
                str(record["name"])
                for record in tree
                if record["role"] in BUTTON_ROLES and str(record["name"]).strip()
            }
        )
        search_labeled = any(record["name"] == SEARCH_LABEL for record in entries)
        replace_labeled = any(record["name"] == REPLACE_LABEL for record in entries)
        unnamed = [record for record in entries if not str(record["name"]).strip()]
        offending = not (search_labeled and replace_labeled)
        return {
            "identity": (
                "accessibility:search-and-replace-entries-unlabeled"
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
            "entries": entries,
            "searchEntryLabeled": search_labeled,
            "replaceEntryLabeled": replace_labeled,
            "unnamedEntryCount": len(unnamed),
            "neighboringLegalBehavior": {
                "namedButtonsInSameTree": buttons,
                "documentBodyIsLegitimatelyUnnamed": bool(unnamed),
            },
        }
    finally:
        record = stop(process)
        if record["exitCode"] not in {0, -signal.SIGTERM}:
            print(json.dumps({"process": record}), file=sys.stderr)


def focused(application: object) -> list[dict[str, object]]:
    return [
        record for record in records(application) if "focused" in record["states"]
    ]


def probe_clocks(
    binary: str,
    prefix: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    process = launch(binary, [], home, prefix)
    try:
        application = find_application(r"clocks")
        button = wait_until(
            lambda: next(
                (
                    node
                    for node in walk(application)
                    if node_record(node)["role"] in BUTTON_ROLES
                    and str(node_record(node)["name"])
                    .replace("_", "")
                    .startswith(ADD_CLOCK_BUTTON)
                ),
                None,
            ),
            "Add World Clock button",
        )
        main_window_focus = focused(application)
        action = activate(button, node_record(button))
        wait_until(
            lambda: any(
                record["name"] == CLOCK_DIALOG for record in records(application)
            ),
            "world clock dialog",
        )
        time.sleep(1.5)
        initial = focused(application)
        # Neighbouring legal behaviour: the entry must be reachable through the
        # dialog's own focus chain on both revisions, so only the initial
        # assignment differs rather than the entry being unfocusable.
        chain = []
        entry_reached = False
        for _ in range(TAB_STEPS):
            current = focused(application)
            chain.append([(r["role"], r["name"]) for r in current])
            if any(record["role"] in ENTRY_ROLES for record in current):
                entry_reached = True
                break
            press("Tab")
            time.sleep(0.8)
        offending = bool(initial) and not any(
            record["role"] in ENTRY_ROLES for record in initial
        )
        return {
            "identity": (
                "dialog-focus:initial-focus-on-cancel-instead-of-entry"
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
            "addClockAction": action,
            "mainWindowFocusBeforeDialog": main_window_focus,
            "dialogInitialFocus": initial,
            "focusChain": chain,
            "neighboringLegalBehavior": {
                "entryReachableByTab": entry_reached,
                "mainWindowFocusIsAButton": any(
                    record["role"] in BUTTON_ROLES for record in main_window_focus
                ),
            },
        }
    finally:
        record = stop(process)
        if record["exitCode"] not in {0, -signal.SIGTERM}:
            print(json.dumps({"process": record}), file=sys.stderr)


PREFIXES = {
    "gnome-text-editor": "text-editor",
    "gnome-clocks": "clocks",
}
BINARIES = {
    "gnome-text-editor": "bin/gnome-text-editor",
    "gnome-clocks": "bin/gnome-clocks",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--application",
        choices=("gnome-text-editor", "gnome-clocks"),
        required=True,
    )
    parser.add_argument("--revision", choices=("affected", "fixed"), required=True)
    parser.add_argument("--run", type=int, choices=range(1, 4), required=True)
    parser.add_argument(
        "--variant",
        choices=("default", "document-body", "main-window-focus"),
        default="default",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    allowed = {
        "gnome-text-editor": {"default", "document-body"},
        "gnome-clocks": {"default", "main-window-focus"},
    }
    if arguments.variant not in allowed[arguments.application]:
        parser.error(f"{arguments.variant!r} is not a {arguments.application} variant")
    return arguments


def main() -> None:
    args = parse_args()
    os.environ["DISPLAY"] = ":99"
    run_root = pathlib.Path("/tmp/reproit-field") / (
        f"{args.application}-{args.revision}-{args.variant}-{args.run}"
    )
    if run_root.exists():
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True)
    xvfb, openbox = start_desktop()
    prefix = f"/opt/reproit/{PREFIXES[args.application]}-{args.revision}"
    binary = f"{prefix}/{BINARIES[args.application]}"
    try:
        if args.application == "gnome-text-editor":
            result = probe_text_editor(binary, prefix, run_root, args.variant)
        else:
            result = probe_clocks(binary, prefix, run_root, args.variant)
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
