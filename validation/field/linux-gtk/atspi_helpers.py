"""Bounded AT-SPI tree discovery and interaction helpers."""

from __future__ import annotations

import json
import re
import time
from typing import Callable

import pyatspi

WAIT_SECONDS = 40
MAX_TREE_NODES = 4_096
POLL_SECONDS = 0.1


def wait_until(predicate: Callable[[], object], label: str) -> object:
    deadline = time.monotonic() + WAIT_SECONDS
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except Exception as error:
            last_error = error
        time.sleep(POLL_SECONDS)
    detail = f": {last_error}" if last_error else ""
    raise RuntimeError(f"timed out waiting for {label}{detail}")


def walk(root: object) -> list[object]:
    nodes: list[object] = []
    pending = [root]
    while pending and len(nodes) < MAX_TREE_NODES:
        node = pending.pop()
        nodes.append(node)
        try:
            children = [node[index] for index in range(node.childCount)]
        except Exception:
            children = []
        pending.extend(reversed(children))
    if pending:
        raise RuntimeError(f"AT-SPI tree exceeded {MAX_TREE_NODES} nodes")
    return nodes


def node_record(node: object) -> dict[str, object]:
    try:
        role = node.getRoleName()
    except Exception:
        role = ""
    try:
        name = node.name or ""
    except Exception:
        name = ""
    try:
        states = [
            pyatspi.stateToString(state)
            for state in node.getState().getStates()
        ]
    except Exception:
        states = []
    return {"role": role, "name": name, "states": states}


def find_application(pattern: str) -> object:
    regex = re.compile(pattern, re.IGNORECASE)

    def candidate() -> object | None:
        desktop = pyatspi.Registry.getDesktop(0)
        for application in [desktop[index] for index in range(desktop.childCount)]:
            try:
                if regex.search(application.name or ""):
                    return application
            except Exception:
                continue
        return None

    return wait_until(candidate, f"AT-SPI application matching {pattern!r}")


def application_absent(pattern: str) -> bool:
    regex = re.compile(pattern, re.IGNORECASE)
    desktop = pyatspi.Registry.getDesktop(0)
    for application in [desktop[index] for index in range(desktop.childCount)]:
        try:
            if regex.search(application.name or ""):
                return False
        except Exception:
            continue
    return True


def find_node(root: object, roles: set[str], name_pattern: str) -> object:
    regex = re.compile(name_pattern, re.IGNORECASE)

    def candidate() -> object | None:
        for node in walk(root):
            record = node_record(node)
            if record["role"] in roles and regex.search(str(record["name"])):
                return node
        return None

    try:
        return wait_until(candidate, f"{sorted(roles)} named {name_pattern!r}")
    except RuntimeError as error:
        snapshot = [node_record(node) for node in walk(root)[:128]]
        raise RuntimeError(
            f"{error}; AT-SPI snapshot={json.dumps(snapshot, sort_keys=True)}"
        ) from error


def find_showing_node(root: object, roles: set[str], name_pattern: str) -> object:
    regex = re.compile(name_pattern, re.IGNORECASE)

    def candidate() -> object | None:
        for node in walk(root):
            record = node_record(node)
            states = record["states"]
            if record["role"] not in roles:
                continue
            if not regex.search(str(record["name"])):
                continue
            if "showing" in states and "visible" in states:
                return node
        return None

    return wait_until(
        candidate,
        f"visible {sorted(roles)} named {name_pattern!r}",
    )


def find_ancestor(node: object, name_pattern: str) -> object:
    regex = re.compile(name_pattern, re.IGNORECASE)
    current = node
    for _ in range(16):
        current = current.parent
        record = node_record(current)
        if regex.search(str(record["name"])):
            return current
    raise RuntimeError(
        f"no ancestor named {name_pattern!r} for {node_record(node)}"
    )


def do_action(node: object) -> str:
    action = node.queryAction()
    if action.nActions < 1:
        raise RuntimeError(f"{node_record(node)} exposes no AT-SPI action")
    action_name = action.getName(0)
    if not action.doAction(0):
        raise RuntimeError(f"AT-SPI action {action_name!r} failed for {node_record(node)}")
    return action_name


def set_text(node: object, value: str) -> None:
    editable = node.queryEditableText()
    if not editable.setTextContents(value):
        raise RuntimeError(f"AT-SPI text replacement failed for {node_record(node)}")


def text_count(node: object) -> int:
    return int(node.queryText().characterCount)


def component_extents(node: object) -> tuple[int, int, int, int]:
    extents = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
    return extents.x, extents.y, extents.width, extents.height


def extents_match(
    left: tuple[int, int, int, int],
    right: tuple[int, int, int, int],
    tolerance_pixels: int,
) -> bool:
    return all(
        abs(left_value - right_value) <= tolerance_pixels
        for left_value, right_value in zip(left, right)
    )
