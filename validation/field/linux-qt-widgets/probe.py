#!/usr/bin/env python3
"""Bounded offline AT-SPI probes for the Linux Qt Widgets field campaign."""

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

from atspi_helpers import (
    application_absent,
    component_extents,
    do_action,
    extents_match,
    find_ancestor,
    find_application,
    find_node,
    find_showing_node,
    node_record,
    set_text,
    text_count,
    wait_until,
    walk,
)

WAIT_SECONDS = 40


def run_command(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=True,
        text=True,
        capture_output=True,
        timeout=WAIT_SECONDS,
        **kwargs,
    )


def active_window_id() -> str:
    result = run_command(["xdotool", "getactivewindow"])
    return result.stdout.strip()


def focused_window_id() -> str:
    result = run_command(["xdotool", "getwindowfocus"])
    return result.stdout.strip()


def process_windows(process_id: int, window_class: str) -> list[dict[str, object]]:
    result = run_command(
        [
            "xdotool",
            "search",
            "--onlyvisible",
            "--pid",
            str(process_id),
            "--class",
            window_class,
        ]
    )
    window_ids = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    windows = []
    for window_id in window_ids:
        geometry = run_command(
            ["xdotool", "getwindowgeometry", "--shell", window_id]
        ).stdout
        fields = dict(re.findall(r"^([A-Z]+)=(.+)$", geometry, re.MULTILINE))
        width = int(fields["WIDTH"])
        height = int(fields["HEIGHT"])
        properties = run_command(
            ["xprop", "-id", window_id, "WM_CLASS", "_NET_WM_PID", "WM_NAME"]
        ).stdout.strip()
        windows.append(
            {
                "id": window_id,
                "width": width,
                "height": height,
                "area": width * height,
                "properties": properties,
            }
        )
    return windows


def x11_window_survey() -> str:
    """Every mapped X11 window, so a failed lookup says what was on screen."""
    result = subprocess.run(
        ["xdotool", "search", "--onlyvisible", "--name", ".*"],
        text=True,
        capture_output=True,
        timeout=WAIT_SECONDS,
    )
    lines = []
    for window_id in result.stdout.split():
        properties = subprocess.run(
            ["xprop", "-id", window_id, "WM_CLASS", "_NET_WM_PID", "WM_NAME"],
            text=True,
            capture_output=True,
            timeout=WAIT_SECONDS,
        )
        lines.append(f"{window_id}: {properties.stdout.strip()!r}")
    return "; ".join(lines) or "no mapped windows"


def process_main_window_id(process_id: int, window_class: str) -> str:
    windows = process_windows(process_id, window_class)
    if not windows:
        raise RuntimeError(
            f"no visible {window_class!r} window found for pid {process_id}"
        )
    windows.sort(key=lambda window: int(window["area"]), reverse=True)
    if len(windows) > 1 and windows[0]["area"] == windows[1]["area"]:
        raise RuntimeError(f"ambiguous largest process-owned windows: {windows}")
    return str(windows[0]["id"])


def focus_window(window_id: str) -> None:
    run_command(["xdotool", "windowactivate", "--sync", window_id])
    run_command(["xdotool", "windowfocus", "--sync", window_id])
    wait_until(
        lambda: (
            active_window_id() == window_id
            and focused_window_id() == window_id
        ),
        f"active and focused X11 window {window_id}",
    )
    active = active_window_id()
    focused = focused_window_id()
    if active != window_id or focused != window_id:
        raise RuntimeError(
            f"active/focused X11 windows {(active, focused)!r} "
            f"did not match {window_id!r}"
        )


def send_foreground_key(window_id: str, key: str) -> None:
    focus_window(window_id)
    run_command(["xdotool", "key", key])


def window_process_id(window_id: str) -> int:
    result = run_command(["xprop", "-id", window_id, "_NET_WM_PID"])
    match = re.search(r"=\s*([0-9]+)", result.stdout)
    if not match:
        raise RuntimeError(f"X11 window {window_id} has no _NET_WM_PID")
    return int(match.group(1))


def click_visible_node(
    process_id: int,
    node: object,
) -> tuple[int, int, int, int]:
    active = active_window_id()
    active_process_id = window_process_id(active)
    if active_process_id != process_id:
        raise RuntimeError(
            f"active X11 pid {active_process_id} did not match qView pid {process_id}"
        )
    extents = component_extents(node)
    x, y, width, height = extents
    if width <= 0 or height <= 0:
        raise RuntimeError(f"cannot click zero-sized AT-SPI node {node_record(node)}")
    run_command(
        [
            "xdotool",
            "mousemove",
            "--sync",
            str(x + width // 2),
            str(y + height // 2),
        ]
    )
    run_command(["xdotool", "click", "1"])
    return extents


def maximize_with_titlebar(
    process_id: int,
    window_id: str,
    window: object,
) -> tuple[int, int]:
    focus_window(window_id)
    if window_process_id(active_window_id()) != process_id:
        raise RuntimeError("qView was not foreground before title-bar maximize")
    x, y, width, _ = component_extents(window)
    if width <= 0 or y < 12:
        raise RuntimeError(
            f"invalid qView extents for title-bar maximize: {(x, y, width)}"
        )
    click_point = (x + width // 2, y - 10)
    run_command(
        [
            "xdotool",
            "mousemove",
            "--sync",
            str(click_point[0]),
            str(click_point[1]),
        ]
    )
    run_command(["xdotool", "click", "--repeat", "2", "--delay", "100", "1"])
    return click_point


def window_states(window_id: str) -> set[str]:
    result = run_command(["xprop", "-id", window_id, "_NET_WM_STATE"])
    return set(re.findall(r"_NET_WM_STATE_[A-Z_]+", result.stdout))


def wait_window_state(window_id: str, state: str, expected: bool) -> None:
    wait_until(
        lambda: (state in window_states(window_id)) == expected,
        f"window state {state} expected={expected}",
    )


def launch(binary: str, arguments: list[str], home: pathlib.Path) -> subprocess.Popen[str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local/share"),
            "XDG_STATE_HOME": str(home / ".local/state"),
            "QT_ACCESSIBILITY": "1",
            "QT_LINUX_ACCESSIBILITY_ALWAYS_ON": "1",
            "QT_QPA_PLATFORM": "xcb",
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
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
    stdout, stderr = process.communicate(timeout=5)
    return {
        "exitCode": process.returncode,
        "stdoutTail": stdout[-4_096:],
        "stderrTail": stderr[-4_096:],
    }


def write_qview_image(path: pathlib.Path) -> None:
    width = 320
    height = 200
    header = f"P6\n{width} {height}\n255\n".encode()
    pixel = bytes((32, 96, 192))
    path.write_bytes(header + pixel * width * height)


def write_qview_config(home: pathlib.Path) -> pathlib.Path:
    path = home / ".config/qView/qView.conf"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            [
                "[General]",
                "firstlaunch=true",
                "",
                "[options]",
                "windowresizemode=2",
                "",
            ]
        ),
        encoding="utf-8",
    )
    return path


def qview_fullscreen_roundtrip(
    application: object,
    process_id: int,
    window: object,
    window_id: str,
    label: str,
) -> dict[str, object]:
    enter_node = find_node(application, {"menu item"}, r"^enter full screen$")
    view_menu = find_ancestor(enter_node, r"^view$")
    focus_window(window_id)
    view_menu_action = do_action(view_menu)
    wait_until(
        lambda: "showing" in node_record(enter_node)["states"],
        f"visible {label} qView full screen menu item",
    )
    enter_extents = click_visible_node(process_id, enter_node)
    wait_window_state(window_id, "_NET_WM_STATE_FULLSCREEN", True)
    fullscreen_extents = component_extents(window)
    exit_node = find_node(application, {"menu item"}, r"^exit full screen$")
    exit_record = node_record(exit_node)
    exit_action = do_action(exit_node)
    wait_window_state(window_id, "_NET_WM_STATE_FULLSCREEN", False)
    return {
        "trigger": {
            "channel": "AT-SPI discovery plus foreground XTEST click",
            "node": node_record(enter_node),
            "parentMenu": node_record(view_menu),
            "parentMenuAction": view_menu_action,
            "enterNodeExtents": enter_extents,
            "enterAction": "foreground XTEST left click",
            "exitNode": exit_record,
            "exitAction": exit_action,
        },
        "fullscreenExtents": fullscreen_extents,
        "postExtents": component_extents(window),
        "postStates": sorted(window_states(window_id)),
    }


def probe_qview_nonmax_target(
    binary: str,
    run_root: pathlib.Path,
    image: pathlib.Path,
) -> dict[str, object]:
    home = run_root / "neighbor-home"
    home.mkdir(parents=True)
    write_qview_config(home)
    process = launch(binary, [str(image)], home)
    try:
        application = find_application(r"qview")
        window = find_node(application, {"frame"}, r".+")
        window_id = wait_until(
            lambda: process_main_window_id(process.pid, "qview"),
            "visible neighboring qView X11 window",
        )
        run_command(
            [
                "wmctrl",
                "-i",
                "-r",
                str(window_id),
                "-b",
                "remove,maximized_vert,maximized_horz",
            ]
        )
        wait_window_state(str(window_id), "_NET_WM_STATE_MAXIMIZED_VERT", False)
        before_extents = component_extents(window)
        roundtrip = qview_fullscreen_roundtrip(
            application,
            process.pid,
            window,
            str(window_id),
            "target",
        )
        after_extents = roundtrip["postExtents"]
        states = window_states(str(window_id))
        return {
            "atspiWindow": node_record(window),
            "beforeExtents": before_extents,
            "afterExtents": after_extents,
            "states": sorted(states),
            "fullscreenTrigger": roundtrip["trigger"],
            "geometryPreserved": extents_match(before_extents, after_extents, 2),
            "nonMaximizedPreserved": (
                "_NET_WM_STATE_MAXIMIZED_VERT" not in states
                and "_NET_WM_STATE_MAXIMIZED_HORZ" not in states
            ),
        }
    finally:
        stop(process)


def probe_qview_maximized(
    binary: str,
    run_root: pathlib.Path,
    image: pathlib.Path,
) -> dict[str, object]:
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    write_qview_config(home)
    process = launch(binary, [str(image)], home)
    try:
        application = find_application(r"qview")
        window = find_node(application, {"frame"}, r".+")
        try:
            window_id = wait_until(
                lambda: process_main_window_id(process.pid, "qview"),
                "visible process-owned qView X11 window",
            )
        except RuntimeError as error:
            raise RuntimeError(
                f"{error}; pid={process.pid} poll={process.poll()} "
                f"windows={x11_window_survey()}"
            ) from error
        maximize_click_point = maximize_with_titlebar(
            process.pid,
            str(window_id),
            window,
        )
        wait_window_state(str(window_id), "_NET_WM_STATE_MAXIMIZED_VERT", True)
        maximized_extents = component_extents(window)
        roundtrip = qview_fullscreen_roundtrip(
            application,
            process.pid,
            window,
            str(window_id),
            "maximized diagnostic",
        )
        return {
            "atspiApplication": application.name,
            "atspiWindow": node_record(window),
            "x11WindowId": window_id,
            "x11ProcessOwnedWindows": process_windows(process.pid, "qview"),
            "maximizeClickPoint": maximize_click_point,
            "maximizedExtents": maximized_extents,
            "roundtrip": roundtrip,
        }
    finally:
        process_record = stop(process)
        if process_record["exitCode"] not in {0, -signal.SIGTERM}:
            print(json.dumps({"process": process_record}), file=sys.stderr)


def qview_result(
    maximized: dict[str, object],
    target: dict[str, object],
    config_path: pathlib.Path,
    elapsed_seconds: float,
    variant: str,
) -> dict[str, object]:
    roundtrip = maximized["roundtrip"]
    post_states = set(roundtrip["postStates"])
    atspi_maximized_restored = extents_match(
        maximized["maximizedExtents"],
        roundtrip["postExtents"],
        2,
    )
    x11_maximized_restored = {
        "_NET_WM_STATE_MAXIMIZED_VERT",
        "_NET_WM_STATE_MAXIMIZED_HORZ",
    }.issubset(post_states)
    # The default observable is the non-maximized round trip, which is the one
    # PR #623 changes. The maximized round trip is the scenario the issue title
    # names, so it is carried as its own corpus variant.
    if variant == "maximized-roundtrip":
        preserved = atspi_maximized_restored and x11_maximized_restored
    else:
        preserved = bool(target["geometryPreserved"])
    return {
        "identity": (
            None
            if preserved
            else "window-state:resized-after-fullscreen-round-trip"
        ),
        "variant": variant,
        "observationReached": True,
        "cleanLaunch": True,
        "exceptions": [],
        "memoryMeasurement": "unavailable",
        "jsHeapMiB": None,
        "elapsedSeconds": round(elapsed_seconds, 3),
        "atspiApplication": maximized["atspiApplication"],
        "atspiWindow": maximized["atspiWindow"],
        "x11WindowId": maximized["x11WindowId"],
        "x11ProcessOwnedWindows": maximized["x11ProcessOwnedWindows"],
        "maximizeTrigger": {
            "channel": "foreground XTEST title-bar double-click",
            "clickPoint": maximized["maximizeClickPoint"],
        },
        "windowResizeMode": {
            "source": str(config_path),
            "value": 2,
            "meaning": "when opening images",
        },
        "firstLaunchDialogSuppressed": {
            "source": str(config_path),
            "key": "firstlaunch",
            "value": True,
        },
        "fullscreenTrigger": roundtrip["trigger"],
        "maximizedExtents": maximized["maximizedExtents"],
        "fullscreenExtents": roundtrip["fullscreenExtents"],
        "postRoundTripExtents": roundtrip["postExtents"],
        "atspiExtentsTolerancePixels": 2,
        "atspiMaximizedGeometryRestored": atspi_maximized_restored,
        "postRoundTripStates": roundtrip["postStates"],
        "x11MaximizedStateRestored": x11_maximized_restored,
        "targetObservation": target,
        "maximizedPathDiagnostic": {
            "result": "non-separating on Debian Bookworm with Openbox",
            "atspiGeometryRestored": atspi_maximized_restored,
            "x11StateRestored": x11_maximized_restored,
        },
        "neighboringLegalBehavior": {
            "maximizedGeometryPreserved": atspi_maximized_restored,
            "maximizedStatePreserved": x11_maximized_restored,
        },
    }


def probe_qview(
    binary: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    image = run_root / "fixture.ppm"
    write_qview_image(image)
    config_path = write_qview_config(run_root / "home")
    started = time.monotonic()
    maximized = probe_qview_maximized(binary, run_root, image)
    wait_until(lambda: application_absent(r"qview"), "maximized qView process exit")
    # PR #623 removes only setWindowSize(), whose direct side effect is the
    # target resize. Run it independently from the maximized-state control.
    target = probe_qview_nonmax_target(binary, run_root, image)
    return qview_result(
        maximized,
        target,
        config_path,
        time.monotonic() - started,
        variant,
    )


def create_database(cli: str, database: pathlib.Path, home: pathlib.Path) -> None:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    result = subprocess.run(
        [cli, "db-create", str(database), "--set-password"],
        input="campaign-password\ncampaign-password\n",
        env=environment,
        check=True,
        text=True,
        capture_output=True,
        timeout=WAIT_SECONDS,
    )
    if "Successfully created new database." not in result.stdout:
        raise RuntimeError(f"database creation did not confirm success: {result.stdout!r}")


def write_keepass_config(path: pathlib.Path, length: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            [
                "[General]",
                "AutoGeneratePasswordForNewEntries=true",
                "",
                "[PasswordGenerator]",
                "AdvancedMode=false",
                "EnsureEvery=true",
                f"Length={length}",
                "LowerCase=false",
                "Numbers=true",
                "SpecialChars=false",
                "UpperCase=false",
                "",
            ]
        ),
        encoding="utf-8",
    )


def require_one_visible_password_field(
    root: object,
    label: str,
) -> tuple[int, dict[str, object]]:
    def candidates() -> list[tuple[int, dict[str, object]]]:
        fields = []
        for node in walk(root):
            record = node_record(node)
            if record["role"] != "password text":
                continue
            if not {"editable", "showing", "visible"}.issubset(record["states"]):
                continue
            count = text_count(node)
            if count:
                fields.append((count, record))
        return fields

    fields = wait_until(candidates, f"visible password field in {label}")
    if len(fields) != 1:
        raise RuntimeError(
            f"expected one visible password field in {label}, found {fields}"
        )
    return fields[0]


def require_one_visible_editable_field(
    root: object,
    label: str,
) -> tuple[int, dict[str, object]]:
    fields = []
    for node in walk(root):
        record = node_record(node)
        if record["role"] not in {"password text", "text"}:
            continue
        if not {"editable", "showing", "visible"}.issubset(record["states"]):
            continue
        count = text_count(node)
        if count:
            fields.append((count, record))
    if len(fields) != 1:
        raise RuntimeError(
            f"expected one visible editable field in {label}, found {fields}"
        )
    return fields[0]


def require_one_visible_spin_button(root: object) -> object:
    nodes = [
        node
        for node in walk(root)
        if node_record(node)["role"] == "spin button"
        and {"showing", "visible"}.issubset(node_record(node)["states"])
    ]
    if len(nodes) != 1:
        raise RuntimeError(
            "expected one visible spin button in password-generator dialog, "
            f"found {[node_record(node) for node in nodes]}"
        )
    return nodes[0]


def open_keepass_database(application: object) -> dict[str, str]:
    notice_close = find_showing_node(application, {"push button"}, r"^close$")
    notice_action = do_action(notice_close)
    password_input = find_showing_node(application, {"password text"}, r".*")
    set_text(password_input, "campaign-password")
    unlock = find_showing_node(
        application,
        {"push button"},
        r"^(unlock|open)$",
    )
    unlock_action = do_action(unlock)
    wait_until(
        lambda: any(
            node_record(node)["role"] == "frame"
            and re.search(
                r"campaign\.kdbx",
                str(node_record(node)["name"]),
                re.IGNORECASE,
            )
            and "[Locked]" not in str(node_record(node)["name"])
            for node in walk(application)
        ),
        "unlocked database window",
    )
    return {
        "developmentNoticeAction": notice_action,
        "unlockAction": unlock_action,
    }


def open_keepass_new_entry(
    application: object,
    process_id: int,
) -> dict[str, object]:
    new_entry = find_node(application, {"menu item"}, r"^(new|add).+entry")
    entries_menu = find_ancestor(new_entry, r"^entries?$")
    main_window_id = wait_until(
        lambda: process_main_window_id(process_id, "keepassxc"),
        "visible process-owned KeePassXC X11 window",
    )
    focus_window(str(main_window_id))
    menu_action = do_action(entries_menu)
    wait_until(
        lambda: "showing" in node_record(new_entry)["states"],
        "visible KeePassXC new-entry menu item",
    )
    entry_action = do_action(new_entry)
    generated_count, generated_record = require_one_visible_password_field(
        application,
        "new-entry view",
    )
    return {
        "generatedCount": generated_count,
        "generatedRecord": generated_record,
        "newEntryAction": entry_action,
        "newEntryMenuAction": menu_action,
    }


def probe_keepass_generator_control(
    application: object,
    process_id: int,
    expected_length: int,
) -> dict[str, object]:
    cancel_action = do_action(
        find_node(application, {"push button"}, r"^cancel$")
    )
    generator_item = find_node(
        application,
        {"menu item"},
        r"^password generator$",
    )
    tools_menu = find_ancestor(generator_item, r"^tools$")
    main_window_id = wait_until(
        lambda: process_main_window_id(process_id, "keepassxc"),
        "visible process-owned KeePassXC X11 window",
    )
    focus_window(str(main_window_id))
    tools_action = do_action(tools_menu)
    wait_until(
        lambda: "showing" in node_record(generator_item)["states"],
        "visible KeePassXC password-generator menu item",
    )
    generator_action = do_action(generator_item)
    generator = find_showing_node(
        application,
        {"dialog", "frame"},
        r"password generator",
    )
    length = require_one_visible_spin_button(generator)
    configured_length = int(length.queryValue().currentValue)
    generated_count, generated_record = require_one_visible_editable_field(
        generator,
        "password-generator dialog",
    )
    if configured_length != expected_length or generated_count != expected_length:
        raise RuntimeError(
            "explicit generator did not honor configured length "
            f"{expected_length}: length={configured_length}, "
            f"generated={generated_count}"
        )
    return {
        "explicitGeneratorDialog": node_record(generator),
        "lengthField": node_record(length),
        "configuredLength": configured_length,
        "generatedPasswordField": generated_record,
        "generatedPasswordCharacterCount": generated_count,
        "generateAction": "initial generation on dialog open",
        "cancelEntryAction": cancel_action,
        "toolsMenuAction": tools_action,
        "passwordGeneratorAction": generator_action,
        "sameSavedConfigurationHonored": True,
    }


def keepass_result(
    application: object,
    launch_actions: dict[str, str],
    new_entry: dict[str, object],
    generator_control: dict[str, object],
    elapsed_seconds: float,
    length: int,
    variant: str,
) -> dict[str, object]:
    generated_count = new_entry["generatedCount"]
    return {
        "identity": (
            None
            if generated_count == length
            else "generator-settings:new-entry-password-ignores-saved-length"
        ),
        "variant": variant,
        "observationReached": True,
        "cleanLaunch": True,
        "exceptions": [],
        "memoryMeasurement": "unavailable",
        "jsHeapMiB": None,
        "elapsedSeconds": round(elapsed_seconds, 3),
        "atspiApplication": application.name,
        "generatedPasswordField": new_entry["generatedRecord"],
        "generatedPasswordCharacterCount": generated_count,
        "configuredPasswordLength": length,
        **launch_actions,
        "newEntryAction": new_entry["newEntryAction"],
        "newEntryMenuAction": new_entry["newEntryMenuAction"],
        "neighboringLegalBehavior": generator_control,
    }


def probe_keepassxc(
    binary: str,
    cli: str,
    run_root: pathlib.Path,
    variant: str,
) -> dict[str, object]:
    length = 32 if variant == "configured-length-32" else 7
    home = run_root / "home"
    home.mkdir(parents=True, exist_ok=True)
    database = run_root / "campaign.kdbx"
    config = run_root / "keepassxc.ini"
    write_keepass_config(config, length)
    create_database(cli, database, home)
    old_config = os.environ.get("KPXC_CONFIG")
    os.environ["KPXC_CONFIG"] = str(config)
    process = launch(binary, [str(database)], home)
    started = time.monotonic()
    try:
        application = find_application(r"keepassxc")
        launch_actions = open_keepass_database(application)
        new_entry = open_keepass_new_entry(application, process.pid)
        generator_control = probe_keepass_generator_control(
            application,
            process.pid,
            length,
        )
        return keepass_result(
            application,
            launch_actions,
            new_entry,
            generator_control,
            time.monotonic() - started,
            length,
            variant,
        )
    finally:
        stop(process)
        if old_config is None:
            os.environ.pop("KPXC_CONFIG", None)
        else:
            os.environ["KPXC_CONFIG"] = old_config


def start_desktop() -> tuple[subprocess.Popen[str], subprocess.Popen[str]]:
    xvfb = subprocess.Popen(
        ["Xvfb", ":99", "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
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
    wait_until(lambda: run_command(["wmctrl", "-m"]).returncode == 0, "Openbox")
    return xvfb, openbox


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", choices=("qview", "keepassxc"), required=True)
    parser.add_argument("--revision", choices=("affected", "fixed"), required=True)
    parser.add_argument("--run", type=int, choices=range(1, 4), required=True)
    parser.add_argument(
        "--variant",
        choices=("default", "maximized-roundtrip", "configured-length-32"),
        default="default",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    allowed = {
        "qview": {"default", "maximized-roundtrip"},
        "keepassxc": {"default", "configured-length-32"},
    }
    if arguments.variant not in allowed[arguments.application]:
        parser.error(
            f"{arguments.variant!r} is not a {arguments.application} variant"
        )
    return arguments


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
        if args.application == "qview":
            result = probe_qview(
                f"/opt/reproit/qview-{args.revision}",
                run_root,
                args.variant,
            )
        else:
            result = probe_keepassxc(
                f"/opt/reproit/keepassxc-{args.revision}",
                f"/opt/reproit/keepassxc-cli-{args.revision}",
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
