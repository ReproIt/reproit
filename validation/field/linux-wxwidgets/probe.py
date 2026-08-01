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

# wxMaxima: the pane the defect forces open, plus two neighbours whose own
# default visibility must survive untouched on both revisions.
GREEK_PANE = "Greek Letters"
NEIGHBOR_SHOWN = "Main Toolbar"
NEIGHBOR_HIDDEN = "Statistics"

# poedit: the file viewer's failure headings. "No usage information" is listed
# so a fixture that lost its reference entirely cannot pass as the defect.
VIEWER_ERRORS = (
    "Source code not found",
    "File cannot be opened",
    "No usage information",
)
SOURCE_C = '#include <stdio.h>\nint main(void) { puts("hello"); return 0; }\n'
# A line the viewer can only be showing if it actually opened the fixture.
SOURCE_MARKER = "int main(void)"
PO_TEMPLATE = """msgid ""
msgstr ""
"MIME-Version: 1.0\\n"
"Content-Type: text/plain; charset=UTF-8\\n"
"Content-Transfer-Encoding: 8bit\\n"
"Language: cs\\n"

#: {source}
msgid "reference without a line number"
msgstr ""

#: {source}:2
msgid "reference with a line number"
msgstr ""
"""


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
) -> subprocess.Popen[str]:
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


def probe_wxmaxima(
    binary: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    process = launch(binary, [], home)
    try:
        application = find_application(r"maxima")
        greek = pane_record(application, GREEK_PANE)
        shown_neighbor = pane_record(application, NEIGHBOR_SHOWN)
        hidden_neighbor = pane_record(application, NEIGHBOR_HIDDEN)
        # Toggling the pane through its own menu proves the read is a live
        # query rather than a stale snapshot of the accessibility tree.
        do_action(pane_item(application, GREEK_PANE))
        wait_until(
            lambda: pane_record(application, GREEK_PANE)["shown"] != greek["shown"],
            f"{GREEK_PANE!r} responding to its own view toggle",
        )
        after_toggle = pane_record(application, GREEK_PANE)
        if variant == "neighboring-panes":
            offending = not shown_neighbor["shown"] or hidden_neighbor["shown"]
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


def write_poedit_fixture(work: pathlib.Path, source_present: bool) -> pathlib.Path:
    work.mkdir(parents=True, exist_ok=True)
    source = "hello.c" if source_present else "absent.c"
    if source_present:
        (work / source).write_text(SOURCE_C, encoding="utf-8")
    catalog = work / "fixture.po"
    catalog.write_text(PO_TEMPLATE.format(source=source), encoding="utf-8")
    return catalog


VIEWER_FRAME = "Code Occurrences"


def viewer_labels(frame: object) -> list[str]:
    labels = []
    for node in walk(frame):
        record = node_record(node)
        if record["role"] not in {"label", "static", "heading"}:
            continue
        if str(record["name"]).strip():
            labels.append(str(record["name"]))
    return labels


def viewer_source_text(frame: object) -> str:
    """Any text the viewer rendered. The source lands inside a wxWebView, so
    this reads every text-bearing node rather than one expected control."""
    parts = []
    for node in walk(frame):
        try:
            content = node.queryText().getText(0, -1)
        except Exception:
            continue
        if content and content.strip():
            parts.append(content)
    return "\n".join(parts)


def viewer_settled(application: object) -> tuple[list[str], str] | None:
    """The viewer has resolved once it shows either an error or the source.

    The frame node is re-resolved on every poll: the web view that renders the
    source replaces the frame's accessible while it loads, so a reference taken
    once goes stale and would report an empty subtree forever. Waiting on the
    outcome rather than on a timer also keeps the read honest under a loaded
    worker, where a slow load must not pass as a missing file.
    """
    frame = next(
        (
            node
            for node in walk(application)
            if node_record(node)["role"] == "frame"
            and node_record(node)["name"] == VIEWER_FRAME
        ),
        None,
    )
    if frame is None:
        return None
    labels = viewer_labels(frame)
    errors = [error for error in VIEWER_ERRORS if any(error in l for l in labels)]
    source = viewer_source_text(frame)
    if errors or SOURCE_MARKER in source:
        return labels, source
    return None


def poedit_reference(
    binary: str,
    catalog: pathlib.Path,
    home: pathlib.Path,
    row: int,
) -> dict[str, object]:
    """Open one catalog entry's code occurrence and read the viewer back.

    Each row gets its own launch: opening the viewer moves keyboard focus off
    the translation list, so driving both rows in one process would depend on
    refocusing rather than on a clean, repeatable starting state.
    """
    process = launch(binary, [str(catalog)], home)
    try:
        application = find_application(r"poedit")
        wait_until(
            lambda: any(
                node_record(node)["role"] == "table" for node in walk(application)
            ),
            "poedit translation list",
        )
        time.sleep(2.0)
        key("Home")
        key("Down", row)
        item = wait_until(
            lambda: next(
                (
                    node
                    for node in walk(application)
                    if node_record(node)["role"] == "menu item"
                    and str(node_record(node)["name"]).replace("_", "")
                    == "Show Code Occurrences"
                ),
                None,
            ),
            "Show Code Occurrences menu item",
        )
        do_action(item)
        labels, source = wait_until(
            lambda: viewer_settled(application),
            "code occurrence viewer to resolve",
        )
        errors = [error for error in VIEWER_ERRORS if any(error in l for l in labels)]
        sidebar = viewer_labels(application)
        return {
            "row": row,
            "labels": labels[:60],
            "errors": errors,
            "sourceShown": SOURCE_MARKER in source,
            "sourceHead": source[:200],
            "occurrenceCounted": any("code occurrence" in l for l in sidebar),
        }
    finally:
        stop(process)


def probe_poedit(
    binary: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    catalog = write_poedit_fixture(
        run_root / "catalog",
        source_present=variant != "missing-source",
    )
    started = time.monotonic()
    lineless = poedit_reference(binary, catalog, home, 0)
    numbered = poedit_reference(binary, catalog, home, 1)
    # The defect is that dropping the line number loses the file, so the
    # identity needs both halves: the line-less reference must fail while the
    # line-numbered reference to the same file still resolves. A genuinely
    # absent source file fails on both and is therefore not attributable.
    numbered_resolved = not numbered["errors"] and numbered["sourceShown"]
    offending = bool(lineless["errors"]) and numbered_resolved
    return {
        "identity": (
            "source-reference:line-less-reference-not-resolved" if offending else None
        ),
        "variant": variant,
        "observationReached": True,
        "cleanLaunch": True,
        "exceptions": [],
        "memoryMeasurement": "unavailable",
        "jsHeapMiB": None,
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "catalog": str(catalog),
        "sourceFilePresent": variant != "missing-source",
        "linelessReference": lineless,
        "lineNumberedReference": numbered,
        "neighboringLegalBehavior": {
            "lineNumberedReferenceResolved": numbered_resolved,
            "bothReferencesCounted": (
                lineless["occurrenceCounted"] and numbered["occurrenceCounted"]
            ),
        },
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", choices=("wxmaxima", "poedit"), required=True)
    parser.add_argument("--revision", choices=("affected", "fixed"), required=True)
    parser.add_argument("--run", type=int, choices=range(1, 4), required=True)
    parser.add_argument(
        "--variant",
        choices=("default", "neighboring-panes", "missing-source"),
        default="default",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    allowed = {
        "wxmaxima": {"default", "neighboring-panes"},
        "poedit": {"default", "missing-source"},
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
    try:
        binary = f"/opt/reproit/{args.application}-{args.revision}"
        if args.application == "wxmaxima":
            result = probe_wxmaxima(binary, run_root, args.variant)
        else:
            result = probe_poedit(binary, run_root, args.variant)
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
