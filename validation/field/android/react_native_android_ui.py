#!/usr/bin/env python3
"""Accessibility-tree addressing and gestures shared by both RN applications.

Split out of react_native_android_campaign.py, which reached this repository's
1000 line ceiling once the first executed runs showed what the addressing
actually had to cope with: a React Native touchable that exposes no clickable
attribute, and a navigation bar whose later tabs start off screen.
"""

from __future__ import annotations

import hashlib
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Callable

from nextplayer_permission_loop import (
    ALLOW_BUTTON_IDS,
    AppiumSession,
    BOUNDS,
    Device,
    sha256,
)

ELEMENT_TEXT_KEYS = ("text", "content-desc")
JOPLIN_PACKAGE = "net.cozic.joplin"
MUSIC_PACKAGE = "com.cyanchill.missingcore.music"


def nodes(source: str) -> list[ET.Element]:
    return list(ET.fromstring(source).iter())

def wait_source(
    session: AppiumSession,
    evidence: Path,
    label: str,
    predicate: Callable[[str], bool],
    seconds: int = 90,
) -> str:
    source = ""
    for _ in range(seconds):
        source = session.source()
        if predicate(source):
            return source
        time.sleep(1)
    (evidence / f"{label}-wait-failure.xml").write_text(source, encoding="utf-8")
    raise RuntimeError(f"{label} UI condition did not become true")


def find_node(source: str, text: str, *, contains: bool = False) -> ET.Element:
    root = ET.fromstring(source)
    parents = {child: parent for parent in root.iter() for child in parent}
    matching = None
    for candidate in root.iter():
        values = [candidate.attrib.get(key, "") for key in ELEMENT_TEXT_KEYS]
        if contains and any(text in value for value in values):
            matching = candidate
            break
        if not contains and text in values:
            matching = candidate
            break
    if matching is None:
        raise RuntimeError(f"UI node not found: {text!r}")
    clickable = matching
    while clickable.attrib.get("clickable") != "true" and clickable in parents:
        clickable = parents[clickable]
    if clickable.attrib.get("clickable") == "true" and usable_bounds(clickable):
        return clickable
    # A React Native touchable frequently exposes no clickable attribute at
    # all, so the walk above runs off the top of the tree and lands on the
    # root, which carries no bounds. Address the matching node itself in that
    # case, rising only as far as the nearest ancestor that has bounds a
    # pointer action can actually target.
    addressable = matching
    while not usable_bounds(addressable) and addressable in parents:
        addressable = parents[addressable]
    if not usable_bounds(addressable):
        raise RuntimeError(f"UI node has no usable bounds: {text!r}")
    return addressable


def usable_bounds(node: ET.Element) -> bool:
    return BOUNDS.fullmatch(node.attrib.get("bounds", "")) is not None


def tap_text(
    session: AppiumSession,
    source: str,
    text: str,
    *,
    contains: bool = False,
) -> None:
    node = find_node(source, text, contains=contains)
    session.tap_bounds(node.attrib.get("bounds", ""))


def long_press_text(
    session: AppiumSession,
    source: str,
    text: str,
    *,
    contains: bool = False,
) -> None:
    node = find_node(source, text, contains=contains)
    match = BOUNDS.fullmatch(node.attrib.get("bounds", ""))
    if match is None:
        raise RuntimeError(f"UI node has no usable bounds: {text!r}")
    left, top, right, bottom = map(int, match.groups())
    actions = {
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
                        "x": (left + right) // 2,
                        "y": (top + bottom) // 2,
                    },
                    {"type": "pointerDown", "button": 0},
                    {"type": "pause", "duration": 1400},
                    {"type": "pointerUp", "button": 0},
                ],
            }
        ]
    }
    session._request("POST", f"/session/{session.session_id}/actions", actions)
    session._request("DELETE", f"/session/{session.session_id}/actions")


def tab_bar_row(source: str) -> tuple[int, int]:
    """Return the horizontal band the Music navigation FlatList occupies."""
    node = find_node(source, "HOME")
    match = BOUNDS.fullmatch(node.attrib.get("bounds", ""))
    if match is None:
        raise RuntimeError("the Music navigation bar has no usable bounds")
    _, top, _, bottom = map(int, match.groups())
    return (top + bottom) // 2, bottom - top


def reveal_music_tab(
    session: AppiumSession,
    device: Device,
    label: str,
    tab: str,
) -> str:
    """Scroll the navigation bar until one of its later tabs is addressable.

    The bar is a horizontal FlatList over index plus every displayed tab, and
    the default order is folder, playlist, track, album, artist. Only the first
    four fit a 1080 wide screen, so ALBUMS and ARTISTS exist but start off the
    right edge and never appear in a UiAutomator2 dump until the bar is
    scrolled. Waiting for them without scrolling can only ever time out.
    """
    source = session.source()
    for _ in range(8):
        if tab in source:
            return source
        centre, _ = tab_bar_row(source)
        swipe_left(session, y=centre, start=900, end=200, duration=500)
        time.sleep(1)
        source = session.source()
    (device.evidence / f"{label}-tab-reveal-failure.xml").write_text(
        source,
        encoding="utf-8",
    )
    raise RuntimeError(f"the Music navigation bar never revealed {tab!r}")


def swipe_left(
    session: AppiumSession,
    y: int = 720,
    start: int = 850,
    end: int = 180,
    duration: int = 700,
) -> None:
    actions = {
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
                        "x": start,
                        "y": y,
                    },
                    {"type": "pointerDown", "button": 0},
                    {
                        "type": "pointerMove",
                        "duration": duration,
                        "origin": "viewport",
                        "x": end,
                        "y": y,
                    },
                    {"type": "pointerUp", "button": 0},
                ],
            }
        ]
    }
    session._request("POST", f"/session/{session.session_id}/actions", actions)
    session._request("DELETE", f"/session/{session.session_id}/actions")


def press_back(session: AppiumSession) -> None:
    session._request(
        "POST",
        f"/session/{session.session_id}/execute/sync",
        {"script": "mobile: pressKey", "args": [{"keycode": 4}]},
    )


def grant_permission(session: AppiumSession, evidence: Path, label: str) -> None:
    source = ""
    for _ in range(45):
        source = session.source()
        for node in nodes(source):
            resource_id = node.attrib.get("resource-id")
            text = node.attrib.get("text", "")
            if resource_id not in ALLOW_BUTTON_IDS and text not in {
                "Allow",
                "Allow all",
                "While using the app",
            }:
                continue
            session.tap_bounds(node.attrib.get("bounds", ""))
            return
        if MUSIC_PACKAGE in source or JOPLIN_PACKAGE in source:
            time.sleep(1)
    (evidence / f"{label}-permission-failure.xml").write_text(
        source,
        encoding="utf-8",
    )
    raise RuntimeError(f"{label} permission dialog did not expose Allow")


def retain_observation(
    device: Device,
    session: AppiumSession,
    label: str,
    source: str,
) -> dict:
    source_path = device.evidence / f"{label}-source.xml"
    source_path.write_text(source, encoding="utf-8")
    screenshot = device.evidence / f"{label}-screen.png"
    session.screenshot(screenshot)
    logcat = device.adb_run(
        "logcat",
        "-d",
        "-t",
        "3000",
        capture=True,
        check=False,
        timeout=60,
    )
    (device.evidence / f"{label}-logcat.log").write_text(
        logcat,
        encoding="utf-8",
    )
    return {
        "sourceSha256": f"sha256:{hashlib.sha256(source.encode()).hexdigest()}",
        "screenshotSha256": f"sha256:{sha256(screenshot)}",
    }


def install(device: Device, package: str, apk: Path) -> None:
    device.adb_run("uninstall", package, capture=True, check=False)
    device.adb_run("install", str(apk), timeout=300)
    device.adb_run("logcat", "-c")


