#!/usr/bin/env python3
"""Native-window channel for the tauri-linux worker.

A Tauri application is not only its webview. File choosers, message dialogs, and
menus are native GTK windows, and WebDriver cannot see them: the webview channel
reports that nothing happened. This module reads and drives those windows over
AT-SPI, so a campaign can say what a native window contained instead of
declaring the subject unusable.

Two properties matter and both are enforced here:

1. A single desktop read is not a measurement. An application registers with the
   accessibility bus asynchronously, so one traversal right after a click finds
   an empty desktop and makes a live toolkit look invisible. Every lookup polls
   to a deadline and reports how long it waited.

2. The channel never invents an interaction. Text goes in through
   EditableText.setTextContents and buttons are pressed through the accessible
   action, so a subject that exposes neither is reported as unreachable rather
   than driven by synthetic X11 keystrokes that no accessible tree can confirm.

usage:
  atspi_window.py windows [--timeout S]
  atspi_window.py dump --window NAME [--timeout S] [--max N]
  atspi_window.py open-path --window NAME --path P[,P...] [--timeout S]
"""

from __future__ import annotations

import argparse
import json
import sys
import time

import pyatspi

DEFAULT_TIMEOUT_SECONDS = 30.0
POLL_SECONDS = 0.5
MAX_NODES = 4000


def _desktop():
    return pyatspi.Registry.getDesktop(0)


def _applications():
    desktop = _desktop()
    for index in range(desktop.childCount):
        try:
            application = desktop.getChildAtIndex(index)
        except Exception:  # noqa: BLE001 - a peer can vanish mid-traversal
            continue
        if application is not None:
            yield application


def _windows():
    for application in _applications():
        name = application.name or ""
        for index in range(application.childCount):
            try:
                window = application.getChildAtIndex(index)
            except Exception:  # noqa: BLE001
                continue
            if window is None:
                continue
            yield name, window


def list_windows(timeout: float) -> list[dict]:
    """Poll until at least one window is on the bus, then report every window."""
    deadline = time.monotonic() + timeout
    while True:
        found = [
            {
                "application": application,
                "name": window.name or "",
                "role": window.getRoleName(),
                "children": window.childCount,
            }
            for application, window in _windows()
        ]
        if found or time.monotonic() >= deadline:
            return found
        time.sleep(POLL_SECONDS)


def find_window(name: str, timeout: float):
    """Poll for a window whose name or role name contains `name`."""
    needle = name.lower()
    deadline = time.monotonic() + timeout
    waited = 0.0
    while True:
        for application, window in _windows():
            haystack = f"{application} {window.name or ''} {window.getRoleName()}".lower()
            if needle in haystack:
                return window, waited
        if time.monotonic() >= deadline:
            return None, waited
        time.sleep(POLL_SECONDS)
        waited += POLL_SECONDS


def walk(node, depth: int = 0, budget: list[int] | None = None):
    if budget is None:
        budget = [MAX_NODES]
    if budget[0] <= 0:
        return
    budget[0] -= 1
    yield depth, node
    for index in range(node.childCount):
        try:
            child = node.getChildAtIndex(index)
        except Exception:  # noqa: BLE001
            continue
        if child is None:
            continue
        yield from walk(child, depth + 1, budget)


def dump(window, maximum: int) -> list[dict]:
    nodes = []
    for depth, node in walk(window):
        try:
            nodes.append({
                "depth": depth,
                "role": node.getRoleName(),
                "name": node.name or "",
                "actions": _action_names(node),
                "editable": _is_editable(node),
            })
        except Exception:  # noqa: BLE001
            continue
        if len(nodes) >= maximum:
            break
    return nodes


def _action_names(node) -> list[str]:
    try:
        action = node.queryAction()
    except NotImplementedError:
        return []
    return [action.getName(index) for index in range(action.nActions)]


def _is_editable(node) -> bool:
    try:
        node.queryEditableText()
    except NotImplementedError:
        return False
    return True


def _first(window, predicate):
    for _, node in walk(window):
        try:
            if predicate(node):
                return node
        except Exception:  # noqa: BLE001
            continue
    return None


def _do_named_action(node, wanted: str) -> bool:
    try:
        action = node.queryAction()
    except NotImplementedError:
        return False
    for index in range(action.nActions):
        if action.getName(index) == wanted:
            action.doAction(index)
            return True
    return False


def open_path(window, paths: list[str], timeout: float = 10.0) -> dict:
    """Type paths into a GTK file chooser's location entry and accept it.

    The chooser hides its location entry until it is asked for it. AT-SPI
    exposes that as the file-chooser widget's own `show_location` action, which
    is the accessible equivalent of Ctrl+L and is used here rather than
    synthetic keystrokes. The entry then appears asynchronously, so it is polled
    for, filled through EditableText, and activated through the entry's own
    `activate` action; the Open button is the fallback when the entry has none.
    """
    revealed = _first(window, lambda node: "show_location" in _action_names(node))
    if revealed is None or not _do_named_action(revealed, "show_location"):
        return {"typed": False, "reason": "the chooser exposes no show_location action"}

    # The chooser holds three editable text nodes: the location entry and two
    # typeahead entries, all role "text" with an activate action. Only the
    # location entry lives under the filler GTK names "Location Layer", and
    # typing into either of the others silently does nothing, which is exactly
    # the shape of a run that looks driven and is not. Match the parent, and
    # fall back to the focused showing entry rather than to document order.
    def _is_location_entry(node) -> bool:
        if node.getRoleName() not in {"text", "entry"} or not _is_editable(node):
            return False
        parent = node.parent
        return parent is not None and (parent.name or "") == "Location Layer"

    def _is_focused_entry(node) -> bool:
        if node.getRoleName() not in {"text", "entry"} or not _is_editable(node):
            return False
        states = node.getState()
        return (states.contains(pyatspi.STATE_FOCUSED)
                and states.contains(pyatspi.STATE_SHOWING))

    deadline = time.monotonic() + timeout
    entry = None
    while entry is None and time.monotonic() < deadline:
        entry = _first(window, _is_location_entry) or _first(window, _is_focused_entry)
        if entry is None:
            time.sleep(POLL_SECONDS)
    if entry is None:
        return {"typed": False, "reason": "the location entry never appeared"}

    text = " ".join(f'"{path}"' for path in paths) if len(paths) > 1 else paths[0]
    entry.queryEditableText().setTextContents(text)
    activated = _do_named_action(entry, "activate")
    accept = None
    if not activated:
        accept = _first(window, lambda node: node.getRoleName() == "push button"
                        and (node.name or "").strip().lower() in {"open", "_open", "select"})
        if accept is None:
            return {"typed": True, "accepted": False,
                    "reason": "the entry has no activate action and the dialog has no Open button"}
        accept.queryAction().doAction(0)
    return {
        "typed": True,
        "accepted": True,
        "entryRole": entry.getRoleName(),
        "acceptedBy": "entry-activate" if activated else f"button:{accept.name}",
        "text": text,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["windows", "dump", "open-path"])
    parser.add_argument("--window", default="")
    parser.add_argument("--path", default="")
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--max", type=int, default=200)
    arguments = parser.parse_args()

    if arguments.command == "windows":
        print(json.dumps(list_windows(arguments.timeout), indent=1))
        return 0

    window, waited = find_window(arguments.window, arguments.timeout)
    if window is None:
        print(json.dumps({"found": False, "waitedSeconds": waited,
                          "window": arguments.window}))
        return 1
    if arguments.command == "dump":
        print(json.dumps({"found": True, "waitedSeconds": waited,
                          "name": window.name or "", "role": window.getRoleName(),
                          "nodes": dump(window, arguments.max)}, indent=1))
        return 0
    paths = [item for item in arguments.path.split(",") if item]
    if not paths:
        print(json.dumps({"found": True, "error": "no --path given"}))
        return 2
    result = open_path(window, paths)
    print(json.dumps({"found": True, "waitedSeconds": waited, **result}))
    return 0 if result.get("accepted") else 1


if __name__ == "__main__":
    sys.exit(main())
