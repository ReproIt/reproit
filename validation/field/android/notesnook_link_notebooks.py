#!/usr/bin/env python3
"""Notesnook's single-select relink defect, and the subjects that resemble it.

streetwriters/notesnook issue 7348. In single-select mode the link-notebooks
screen dropped the previously selected notebook out of its selection map
instead of marking it deselected, and `onSave` only walks that map, so the
relink added the new notebook without ever unlinking the old one. The fixed
revision diffs the initial selection against the current one and writes an
explicit deselected state for anything that was selected and no longer is.

The observable needs no navigation: the note row in the list carries its linked
notebooks in its own content-desc, so one dump of the list separates the two
revisions. Everything here is addressed through the accessibility tree, by the
testIDs the application's own Detox suite already uses, and every step is a
bounded wait for its node rather than a single dump.

Split out of react_native_android_campaign.py, which is already close to this
repository's 1000 line ceiling.
"""

from __future__ import annotations

import time
import xml.etree.ElementTree as ET
from pathlib import Path

from nextplayer_permission_loop import (
    AppiumServer,
    AppiumSession,
    BOUNDS,
    Device,
)
from react_native_android_runtime import enforce_offline, record_owned_process
from react_native_android_ui import (
    install,
    long_press_text,
    nodes,
    press_back,
    retain_observation,
    wait_source,
)

NOTESNOOK_PACKAGE = "com.streetwriters.notesnook"
NOTESNOOK_ACTIVITY = ".MainActivity"
NOTESNOOK_REPOSITORY = "https://github.com/streetwriters/notesnook"
NOTESNOOK_ISSUE = "https://github.com/streetwriters/notesnook/issues/7348"
NOTESNOOK_AFFECTED_REVISION = "14f727d6e630f60299f1ceae42e48685e87cba8f"
NOTESNOOK_FIXED_REVISION = "7c3fdab6eec0c083ef4c3b12ff16ba0d2f8aff2c"
NOTESNOOK_IDENTITY = (
    "react-native-state:single-select-relink-keeps-previous-notebook"
)
NOTE_TITLE = "TriggerNote"
NOTEBOOK_NAMES = ("Alpha", "Beta", "Gamma")
NOTE_ROW = "note-item-0"
# The header's restore button is a MaterialCommunityIcons glyph in a Pressable
# that carries no testID, so it is addressed by the one thing it does carry:
# the private-use codepoint of the icon itself. It exists only while the
# selection differs from the one the screen opened with, which is exactly the
# state the adversarial-restored-selection subject needs it in.
RESTORE_GLYPH = "\U000f099b"
# Each observation mode's legal outcomes, read as the set of notebooks the note
# row lists once the trigger has finished. Anything else is a state the
# campaign refuses to interpret rather than folding into one of these.
LEGAL_OUTCOMES: dict[str, tuple[frozenset[str], ...]] = {
    "benchmark": (
        frozenset({"Alpha", "Beta"}),
        frozenset({"Beta"}),
    ),
    "first-link": (frozenset({"Alpha"}),),
    "adversarial-restored-selection": (frozenset({"Alpha"}),),
    "adversarial-multi-select": (frozenset({"Alpha", "Beta", "Gamma"}),),
}
MODE_NOTEBOOKS: dict[str, tuple[str, ...]] = {
    "benchmark": ("Alpha", "Beta"),
    "first-link": ("Alpha", "Beta"),
    "adversarial-restored-selection": ("Alpha", "Beta"),
    "adversarial-multi-select": ("Alpha", "Beta", "Gamma"),
}


def usable(node: ET.Element) -> bool:
    match = BOUNDS.fullmatch(node.attrib.get("bounds", ""))
    if match is None:
        return False
    left, top, right, bottom = map(int, match.groups())
    return right > left and bottom > top


def resource_node(source: str, resource_id: str) -> ET.Element:
    """Address one node by its React Native testID.

    The editor toolbar publishes off-screen buttons with [0,0][0,0] bounds, so
    a node is only addressable when its rectangle has area.
    """
    for node in nodes(source):
        if node.attrib.get("resource-id") == resource_id and usable(node):
            return node
    raise RuntimeError(f"UI node not found by resource id: {resource_id!r}")


def has_resource(source: str, resource_id: str) -> bool:
    try:
        resource_node(source, resource_id)
    except RuntimeError:
        return False
    return True


def tap_resource(session: AppiumSession, source: str, resource_id: str) -> None:
    session.tap_bounds(resource_node(source, resource_id).attrib["bounds"])


def wait_resource(
    session: AppiumSession,
    device: Device,
    label: str,
    resource_id: str,
    seconds: int = 90,
) -> str:
    return wait_source(
        session,
        device.evidence,
        f"{label}-{resource_id}",
        lambda value: has_resource(value, resource_id),
        seconds=seconds,
    )


def tap_after_wait(
    session: AppiumSession,
    device: Device,
    label: str,
    resource_id: str,
    seconds: int = 90,
) -> None:
    source = wait_resource(session, device, label, resource_id, seconds)
    tap_resource(session, source, resource_id)


def notebook_row(source: str, name: str) -> ET.Element | None:
    """The notebook row, matched against the PARSED tree.

    The row's content-desc is the notebook name, a comma, and the checkbox
    glyph, and the glyph is a private-use codepoint the raw dump escapes as a
    numeric entity, so the raw string is not what to match on. The prefix is
    the name and the comma, which no other node on the screen carries.
    """
    for node in nodes(source):
        if node.attrib.get("content-desc", "").startswith(f"{name},") and usable(node):
            return node
    return None


def wait_notebook_row(
    session: AppiumSession,
    device: Device,
    label: str,
    name: str,
) -> str:
    return wait_source(
        session,
        device.evidence,
        f"{label}-notebook-{name}",
        lambda value: notebook_row(value, name) is not None,
    )


def linked_notebooks(source: str) -> frozenset[str]:
    """The notebooks the note row itself says it is linked to.

    The row draws a notebook glyph before each name, and the accessibility
    label joins every part with a comma, so the linked set is the comma
    separated parts that are notebook names.
    """
    node = resource_node(source, NOTE_ROW)
    parts = [part.strip() for part in node.attrib.get("content-desc", "").split(",")]
    return frozenset(part for part in parts if part in NOTEBOOK_NAMES)


def relink_identity(mode: str, final: frozenset[str]) -> str | None:
    """The verdict, read from the notebooks the note ends up in.

    Only the benchmark subject performs a single-select relink, so only it can
    reach the defect. The affected build leaves the note in both notebooks and
    the fixed build leaves it in the new one alone; the adversarial subjects
    also end with more than one notebook, which is why the mode is part of the
    rule rather than the size of the set.
    """
    if final not in LEGAL_OUTCOMES[mode]:
        raise RuntimeError(
            f"the note ended in {sorted(final)}, which is not an outcome the "
            f"{mode} subject can end in"
        )
    if mode == "benchmark" and final == frozenset({"Alpha", "Beta"}):
        return NOTESNOOK_IDENTITY
    return None


def skip_onboarding(session: AppiumSession, device: Device, label: str) -> None:
    source = wait_source(
        session,
        device.evidence,
        f"{label}-welcome",
        lambda value: "Get started" in value,
        seconds=180,
    )
    node = next(
        (
            item
            for item in nodes(source)
            if item.attrib.get("content-desc") == "Get started" and usable(item)
        ),
        None,
    )
    if node is None:
        raise RuntimeError(f"{label} welcome screen had no Get started button")
    session.tap_bounds(node.attrib["bounds"])
    source = wait_source(
        session,
        device.evidence,
        f"{label}-signup",
        lambda value: "Skip" in value,
    )
    skip = next(
        (item for item in nodes(source) if item.attrib.get("text") == "Skip"),
        None,
    )
    if skip is None:
        raise RuntimeError(f"{label} signup screen had no Skip control")
    session.tap_bounds(skip.attrib["bounds"])
    wait_resource(session, device, f"{label}-home", "buttons.add", seconds=120)


def create_notebooks(
    session: AppiumSession,
    device: Device,
    label: str,
    names: tuple[str, ...],
) -> None:
    tap_after_wait(session, device, label, "left")
    tap_after_wait(session, device, label, "tab-notebooks")
    for name in names:
        tap_after_wait(session, device, f"{label}-{name}", "sidebar-add-button")
        tap_after_wait(session, device, f"{label}-{name}", "title")
        device.adb_run("shell", "input", "text", name)
        tap_after_wait(session, device, f"{label}-{name}", "yes")
        # The "Notebook added" toast draws its own "Add notes" button directly
        # over the sidebar add button, so creating the next notebook without
        # waiting for it to clear taps the toast and lands on another screen.
        wait_source(
            session,
            device.evidence,
            f"{label}-{name}-toast",
            lambda value: not has_resource(value, "toast.button"),
            seconds=60,
        )


def create_note(session: AppiumSession, device: Device, label: str) -> None:
    tap_after_wait(session, device, label, "tab-home")
    tap_after_wait(session, device, label, "Notes")
    tap_after_wait(session, device, label, "buttons.add")
    tap_after_wait(session, device, label, "editor-title", seconds=120)
    device.adb_run("shell", "input", "text", NOTE_TITLE)
    time.sleep(2)
    press_back(session)
    press_back(session)
    wait_source(
        session,
        device.evidence,
        f"{label}-note-list",
        lambda value: NOTE_TITLE in value and has_resource(value, NOTE_ROW),
        seconds=120,
    )


def open_link_screen(session: AppiumSession, device: Device, label: str) -> str:
    tap_after_wait(session, device, label, "listitem.menu")
    tap_after_wait(session, device, label, "icon-notebooks")
    return wait_source(
        session,
        device.evidence,
        f"{label}-link-notebooks",
        lambda value: "Add to notebook" in value
        and notebook_row(value, "Alpha") is not None,
    )


def save_selection(
    session: AppiumSession,
    device: Device,
    label: str,
    before: frozenset[str],
) -> str:
    """Save, then wait for the note row to stop saying what it said before.

    Waiting for one specific set would decide the outcome in advance. The two
    revisions disagree about WHICH set follows the relink but agree that it is
    not the one the screen opened with, so the wait is neutral between them.
    """
    tap_after_wait(session, device, label, "floating-save-button")
    source = wait_source(
        session,
        device.evidence,
        f"{label}-saved",
        lambda value: has_resource(value, NOTE_ROW)
        and linked_notebooks(value) != before,
        seconds=90,
    )
    time.sleep(2)
    return session.source()


def restore_selection(session: AppiumSession, device: Device, label: str) -> str:
    """Revert the selection with the header restore button and leave.

    Restoring makes the selection equal the initial one again, which is the
    same condition that draws the save button, so the save button goes away
    with it and the only way off the screen is back.
    """
    source = wait_source(
        session,
        device.evidence,
        f"{label}-restore-button",
        lambda value: any(
            node.attrib.get("content-desc") == RESTORE_GLYPH and usable(node)
            for node in nodes(value)
        ),
    )
    node = next(
        item for item in nodes(source) if item.attrib.get("content-desc") == RESTORE_GLYPH
    )
    session.tap_bounds(node.attrib["bounds"])
    wait_source(
        session,
        device.evidence,
        f"{label}-restored",
        lambda value: not has_resource(value, "floating-save-button"),
    )
    press_back(session)
    source = wait_source(
        session,
        device.evidence,
        f"{label}-note-list",
        lambda value: has_resource(value, NOTE_ROW),
    )
    time.sleep(2)
    return source


def tap_notebook(
    session: AppiumSession,
    device: Device,
    label: str,
    name: str,
) -> None:
    source = wait_notebook_row(session, device, label, name)
    row = notebook_row(source, name)
    assert row is not None
    session.tap_bounds(row.attrib["bounds"])


def trigger(
    session: AppiumSession,
    device: Device,
    label: str,
    mode: str,
) -> tuple[frozenset[str], frozenset[str]]:
    """Drive one subject and return the linked sets before and after it."""
    skip_onboarding(session, device, label)
    create_notebooks(session, device, label, MODE_NOTEBOOKS[mode])
    create_note(session, device, label)
    open_link_screen(session, device, label)
    if mode == "adversarial-multi-select":
        # Two relations make the screen open in multi-select, where keeping the
        # notebooks already linked is the documented behaviour rather than the
        # defect. Long-pressing is how the application itself enables it.
        source = wait_notebook_row(session, device, label, "Alpha")
        long_press_text(session, source, "Alpha,", contains=True)
        tap_notebook(session, device, label, "Beta")
    else:
        tap_notebook(session, device, label, "Alpha")
    source = save_selection(session, device, label, frozenset())
    first = linked_notebooks(source)
    if mode == "first-link":
        return first, first
    open_link_screen(session, device, f"{label}-second")
    if mode == "adversarial-multi-select":
        tap_notebook(session, device, f"{label}-second", "Gamma")
    else:
        tap_notebook(session, device, f"{label}-second", "Beta")
    if mode == "adversarial-restored-selection":
        return first, linked_notebooks(restore_selection(session, device, label))
    return first, linked_notebooks(
        save_selection(session, device, f"{label}-second", first)
    )


def observe_notesnook(
    device: Device,
    apk: Path,
    label: str,
    expected_identity: str | None,
    mode: str = "benchmark",
) -> dict:
    started = time.monotonic()
    if mode not in LEGAL_OUTCOMES:
        raise RuntimeError(f"unexpected Notesnook observation mode: {mode}")
    network = enforce_offline(device)
    if device.process is None:
        raise RuntimeError("Notesnook observation has no owned emulator process")
    record_owned_process("emulator", label, device.process.pid)
    install(device, NOTESNOOK_PACKAGE, apk)
    server = AppiumServer(device.evidence, label)
    session = None
    try:
        appium_url = server.start()
        if server.process is None:
            raise RuntimeError("Notesnook observation has no owned Appium process")
        record_owned_process("appium", label, server.process.pid)
        session = AppiumSession(
            appium_url,
            device.udid,
            NOTESNOOK_PACKAGE,
            NOTESNOOK_ACTIVITY,
        )
        appium = session.evidence()
        first, final = trigger(session, device, label, mode)
        source = session.source()
        retained = retain_observation(device, session, label, source)
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()
    identity = relink_identity(mode, final)
    if identity != expected_identity:
        raise RuntimeError(
            f"{label} identity was {identity!r}, expected {expected_identity!r}"
        )
    return {
        "status": "reproduced" if identity else "not_reproduced",
        "identity": identity,
        "cleanLaunch": True,
        "observationReached": True,
        "exceptions": [],
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "observationMode": mode,
        "notebooksAfterFirstLink": sorted(first),
        "notebooksAfterTrigger": sorted(final),
        "networkContainment": network,
        "appium": appium,
        **retained,
    }
