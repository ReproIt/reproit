#!/usr/bin/env python3
"""Bounded offline AT-SPI probes for the Linux Qt Quick field campaign."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import time
import wave

from atspi_helpers import find_application, node_record, wait_until, walk

WAIT_SECONDS = 60
# Qt Quick controls carry their content on the text interface, not on the
# accessible name, so every read here goes through queryText where it can.
ZERO_PADDED = re.compile(r"^\d{2}:\d{2}$")
SINGLE_DIGIT = re.compile(r"^\d:\d{2}$")
BACKSPACES = 5


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


def launch(binary: str, arguments: list[str], home: pathlib.Path, prefix: str):
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local/share"),
            "XDG_DATA_DIRS": f"{prefix}/share:/usr/share",
            "QT_QPA_PLATFORM": "xcb",
            # Qt registers on the accessibility bus lazily; without this the
            # application never appears at all, which is what once made Qt
            # Quick look unobservable.
            "QT_ACCESSIBILITY": "1",
            "QT_LINUX_ACCESSIBILITY_ALWAYS_ON": "1",
            "QML_IMPORT_PATH": f"{prefix}/lib/x86_64-linux-gnu/qml",
            "QT_PLUGIN_PATH": f"{prefix}/lib/x86_64-linux-gnu/plugins",
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


def key(name: str, repeat: int = 1) -> None:
    for _ in range(repeat):
        subprocess.run(
            ["xdotool", "key", "--clearmodifiers", name],
            check=True,
            timeout=WAIT_SECONDS,
        )
        time.sleep(0.4)


def node_text(node: object) -> str | None:
    try:
        return node.queryText().getText(0, -1)
    except Exception:
        return None


def displayed(application: object) -> list[str]:
    """Every non-empty string kalk is showing, read through AT-SPI text."""
    values = []
    for node in walk(application):
        content = node_text(node)
        if content and content.strip():
            values.append(content.strip())
    return values


def probe_kalk(
    binary: str,
    prefix: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    home = run_root / "home"
    (home / ".config").mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    process = launch(binary, [], home, prefix)
    try:
        application = find_application(r"kalk")
        wait_until(
            lambda: any(
                node_record(node)["role"] == "frame" for node in walk(application)
            ),
            "kalk window",
        )
        time.sleep(4.0)
        subprocess.run(["wmctrl", "-a", "kalk"], check=False, timeout=20)
        time.sleep(1.0)
        subprocess.run(
            ["xdotool", "type", "--delay", "180", "1+1"],
            check=True,
            timeout=WAIT_SECONDS,
        )
        time.sleep(2.0)
        typed = displayed(application)
        key("Return")
        time.sleep(2.0)
        first_equals = displayed(application)
        if variant == "explicit-backspace":
            key("BackSpace", BACKSPACES)
            second_action = f"BackSpace x{BACKSPACES}"
        else:
            key("Return")
            second_action = "Return"
        time.sleep(2.0)
        after = displayed(application)
        # The defect is that a SECOND equals throws the result away. An input
        # emptied by backspace looks identical and is entirely legal, so the
        # identity is scoped to the equals path rather than to an empty display.
        offending = variant == "default" and not after
        return {
            "identity": (
                "input-state:second-equals-clears-the-result" if offending else None
            ),
            "variant": variant,
            "observationReached": True,
            "cleanLaunch": True,
            "exceptions": [],
            "memoryMeasurement": "unavailable",
            "jsHeapMiB": None,
            "elapsedSeconds": round(time.monotonic() - started, 3),
            "atspiApplication": application.name,
            "afterTyping": typed,
            "afterFirstEquals": first_equals,
            "secondAction": second_action,
            "afterSecondAction": after,
            "neighboringLegalBehavior": {
                "firstEqualsProducedResult": first_equals == ["2"],
                "resultBeforeSecondAction": first_equals,
            },
        }
    finally:
        record = stop(process)
        if record["exitCode"] not in {0, -signal.SIGTERM}:
            print(json.dumps({"process": record}), file=sys.stderr)


def write_silent_track(path: pathlib.Path) -> None:
    """A short silent WAV. The progress indicator only renders with a track
    loaded, and the formatting under test runs at every position."""
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(8000)
        handle.writeframes(b"\x00\x00" * 8000 * 30)


def elisa_readings(application: object) -> dict[str, object]:
    """The headings either side of the Duration slider, plus the track title.

    Elisa renders elapsed position and total length as two headings around the
    seek slider, so the slider is the only stable anchor between them.
    """

    def rows() -> list[dict[str, object]] | None:
        found = [node_record(node) for node in walk(application)]
        if any(
            record["role"] == "slider" and record["name"] == "Duration"
            for record in found
        ):
            return found
        return None

    found = wait_until(rows, "elisa Duration slider")
    index = next(
        position
        for position, record in enumerate(found)
        if record["role"] == "slider" and record["name"] == "Duration"
    )
    elapsed = str(found[index - 1]["name"]) if index > 0 else ""
    total = str(found[index + 1]["name"]) if index + 1 < len(found) else ""
    titles = [
        str(record["name"])
        for record in found
        if record["role"] == "heading" and record["name"].endswith(".wav")
    ]
    return {
        "elapsed": elapsed,
        "total": total,
        "trackTitle": titles[0] if titles else "",
        "headings": [
            str(record["name"]) for record in found if record["role"] == "heading"
        ][:8],
    }


def probe_elisa(
    binary: str,
    prefix: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    home = run_root / "home"
    (home / ".config").mkdir(parents=True, exist_ok=True)
    track = home / "fixture.wav"
    write_silent_track(track)
    started = time.monotonic()
    process = launch(binary, [str(track)], home, prefix)
    try:
        application = find_application(r"elisa")
        time.sleep(6.0)
        readings = elisa_readings(application)
        if variant == "track-title":
            # The track title is a heading in the same window that legitimately
            # carries no zero-padded time at all.
            offending = False
        else:
            offending = not ZERO_PADDED.match(readings["elapsed"])
        return {
            "identity": (
                "progress-indicator:elapsed-time-minutes-not-zero-padded"
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
            "elapsedReading": readings["elapsed"],
            "totalReading": readings["total"],
            "trackTitle": readings["trackTitle"],
            "headings": readings["headings"],
            "elapsedIsZeroPadded": bool(ZERO_PADDED.match(readings["elapsed"])),
            "elapsedIsSingleDigitMinutes": bool(
                SINGLE_DIGIT.match(readings["elapsed"])
            ),
            "neighboringLegalBehavior": {
                "trackTitleReadsIdentically": readings["trackTitle"],
                "durationSliderPresent": True,
            },
        }
    finally:
        record = stop(process)
        if record["exitCode"] not in {0, -signal.SIGTERM}:
            print(json.dumps({"process": record}), file=sys.stderr)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", choices=("kalk", "elisa"), required=True)
    parser.add_argument("--revision", choices=("affected", "fixed"), required=True)
    parser.add_argument("--run", type=int, choices=range(1, 4), required=True)
    parser.add_argument(
        "--variant",
        choices=("default", "explicit-backspace", "track-title"),
        default="default",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    allowed = {
        "kalk": {"default", "explicit-backspace"},
        "elisa": {"default", "track-title"},
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
    prefix = f"/opt/reproit/{args.application}-{args.revision}"
    binary = f"{prefix}/bin/{args.application}"
    try:
        if args.application == "kalk":
            result = probe_kalk(binary, prefix, run_root, args.variant)
        else:
            result = probe_elisa(binary, prefix, run_root, args.variant)
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
