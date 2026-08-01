#!/usr/bin/env python3
"""Shared Appium XCUITest client and trigger steps for the swiftui-ios field work.

Two independent SwiftUI applications carry this target's field benchmark, and
both are driven from here so the benchmark and the corpus read the same
observables through the same code path:

  dimeApp    rafsoh/dimeApp issue 72. Settings, Number Entry hosts a live
             NumberPadTextView, the exact view pull request 77 touches. Tapping
             the decimal key at zero leaves price == 0 with a decimal pending;
             the affected delete key carries .disabled(price == 0) and is inert.

  kiwix-apple  kiwix/kiwix-apple issue 1607. With no ZIM archive present the
             affected SplitViewForiPad recomputes columnVisibility on every
             navigation.currentItem change and forces .detailOnly, so opening
             the sidebar and then creating a tab collapses it again.
"""

from __future__ import annotations

import base64
import json
import subprocess
import time
from pathlib import Path
from urllib import error, request

ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"
DIME_BUNDLE_ID = "com.rafaelsoh.dime"
KIWIX_BUNDLE_ID = "self.Kiwix"
DIME_IDENTITY = "delete-key-inert-while-decimal-pending-at-zero"
KIWIX_IDENTITY = "ipad-sidebar-collapses-on-selection-without-zim-archive"
SIDEBAR_SHOWN = "type == 'XCUIElementTypeButton' AND label == 'Hide Sidebar'"
SIDEBAR_HIDDEN = "type == 'XCUIElementTypeButton' AND label == 'Show Sidebar'"
AMOUNT_LABEL = "type == 'XCUIElementTypeStaticText' AND name BEGINSWITH '$'"
DELETE_KEY = "delete.left.fill"
MAX_SETUP_SECONDS = 300


class Session:
    """The subset of the WebDriver protocol these campaigns actually use."""

    def __init__(self, url: str, udid: str, bundle_id: str, **extra: object) -> None:
        self.url = url.rstrip("/")
        capabilities = {
            "platformName": "iOS",
            "appium:automationName": "XCUITest",
            "appium:udid": udid,
            "appium:bundleId": bundle_id,
            "appium:noReset": True,
            "appium:newCommandTimeout": 600,
            "appium:useNewWDA": True,
            "appium:shouldUseSingletonTestManager": True,
            "appium:wdaStartupRetries": 2,
            "appium:wdaStartupRetryInterval": 2000,
            "appium:wdaLaunchTimeout": 300000,
            "appium:simulatorStartupTimeout": 300000,
            "appium:autoDismissAlerts": True,
        }
        capabilities.update(extra)
        self.capabilities = capabilities
        result = self.request(
            "POST", "/session", {"capabilities": {"alwaysMatch": capabilities}}, timeout=600
        )
        value = result.get("value", {})
        self.session_id = value.get("sessionId") or result.get("sessionId")
        if not self.session_id:
            raise RuntimeError("Appium did not return a session id")

    def request(self, method: str, path: str, payload=None, timeout: int = 180) -> dict:
        body = None if payload is None else json.dumps(payload).encode()
        operation = request.Request(
            f"{self.url}{path}",
            data=body,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        try:
            with request.urlopen(operation, timeout=timeout) as response:
                result = json.loads(response.read().decode())
        except error.HTTPError as failure:
            detail = failure.read().decode(errors="replace")
            raise RuntimeError(f"{method} {path} -> {failure.code}: {detail[:800]}") from None
        value = result.get("value")
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(f"{method} {path} failed: {json.dumps(value)[:800]}")
        return result

    def call(self, method: str, path: str, payload=None, timeout: int = 180) -> dict:
        return self.request(method, f"/session/{self.session_id}{path}", payload, timeout)

    def source(self) -> str:
        return self.call("GET", "/source")["value"]

    def screenshot(self, out: Path) -> None:
        Path(out).write_bytes(base64.b64decode(self.call("GET", "/screenshot")["value"]))

    def find(self, using: str, value: str) -> str | None:
        try:
            return self.call("POST", "/element", {"using": using, "value": value})["value"][
                ELEMENT_KEY
            ]
        except RuntimeError:
            return None

    def find_all(self, using: str, value: str) -> list[str]:
        try:
            items = self.call("POST", "/elements", {"using": using, "value": value})["value"]
        except RuntimeError:
            return []
        return [item[ELEMENT_KEY] for item in items]

    def require(self, using: str, value: str) -> str:
        element = self.find(using, value)
        if element is None:
            raise RuntimeError(f"element not found: {using} {value}")
        return element

    def click(self, element: str) -> None:
        self.call("POST", f"/element/{element}/click", {})

    def text(self, element: str) -> str:
        return self.call("GET", f"/element/{element}/text")["value"] or ""

    def rect(self, element: str) -> dict:
        return self.call("GET", f"/element/{element}/rect")["value"]

    def attribute(self, element: str, name: str) -> str:
        return self.call("GET", f"/element/{element}/attribute/{name}")["value"]

    def close(self) -> None:
        if getattr(self, "session_id", ""):
            try:
                self.call("DELETE", "")
            finally:
                self.session_id = ""


def wait_for_server(url: str, seconds: int = 120) -> None:
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            with request.urlopen(f"{url}/status", timeout=5) as response:
                if response.status == 200:
                    return
        except Exception:  # noqa: BLE001 - the server is simply not up yet
            time.sleep(1)
    raise RuntimeError("Appium server did not become ready within its bound")


def start_server(log_path: Path, port: int) -> subprocess.Popen:
    handle = Path(log_path).open("w", encoding="utf-8")
    process = subprocess.Popen(
        ["appium", "--port", str(port), "--log-level", "info", "--relaxed-security"],
        stdout=handle,
        stderr=subprocess.STDOUT,
    )
    wait_for_server(f"http://127.0.0.1:{port}")
    return process


def reset_application(udid: str, bundle_id: str, application: Path) -> None:
    """Every run starts from a first-launch container."""
    for command in (
        ["xcrun", "simctl", "terminate", udid, bundle_id],
        ["xcrun", "simctl", "uninstall", udid, bundle_id],
    ):
        subprocess.run(command, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["xcrun", "simctl", "install", udid, str(application)], check=True)


def _add_first_suggested_category(session: Session) -> None:
    """Dime's first-run sheet refuses to close until one category exists."""
    food = session.require(
        "-ios predicate string", "type == 'XCUIElementTypeStaticText' AND name == 'Food'"
    )
    row = session.rect(food)
    centre = row["y"] + row["height"] / 2
    best, distance = None, float("inf")
    for element in session.find_all(
        "-ios predicate string",
        "type == 'XCUIElementTypeImage' AND name == 'plus' AND label == 'Add'",
    ):
        box = session.rect(element)
        gap = abs(box["y"] + box["height"] / 2 - centre)
        if gap < distance:
            best, distance = element, gap
    if best is None:
        raise RuntimeError("the first-run category sheet exposed no suggested category")
    session.click(best)


def dime_setup(session: Session) -> None:
    """Setup only: reach Settings, Number Entry with entry method Type 2."""
    session.click(
        session.require(
            "-ios predicate string",
            "type == 'XCUIElementTypeButton' AND label CONTAINS 'Get Started'",
        )
    )
    time.sleep(3)
    _add_first_suggested_category(session)
    time.sleep(2)
    session.click(session.require("accessibility id", "arrow.right"))
    time.sleep(3)
    session.click(session.require("accessibility id", "Settings tab"))
    time.sleep(2)
    session.click(
        session.require(
            "-ios predicate string",
            "name BEGINSWITH 'Number Entry' AND type != 'XCUIElementTypeStaticText'",
        )
    )
    time.sleep(2)
    session.click(session.require("accessibility id", "Type 2"))
    time.sleep(1)


def dime_amount(session: Session) -> str:
    return session.text(session.require("-ios predicate string", AMOUNT_LABEL))


DIME_MODES = ("exact", "nonzero", "decimal-digit")


def dime_observe(session: Session, *, mode: str = "exact") -> dict:
    """Trigger and observation on the settings number pad.

    exact          tap the decimal key, which is the defect trigger.
    nonzero        type 5, so the key is legally live because price != 0.
    decimal-digit  type a decimal and then a digit, so a decimal is pending
                   AND price != 0, which is the legal case closest to the
                   defect and therefore the sharpest adversarial subject.
    """
    if mode not in DIME_MODES:
        raise ValueError(f"unknown dime trigger mode: {mode}")
    if mode == "nonzero":
        session.click(session.require("accessibility id", "5"))
    elif mode == "decimal-digit":
        session.click(session.require("accessibility id", "."))
        time.sleep(1)
        session.click(session.require("accessibility id", "5"))
    else:
        session.click(session.require("accessibility id", "."))
    time.sleep(1)
    before = dime_amount(session)
    enabled = session.attribute(session.require("accessibility id", DELETE_KEY), "enabled")
    session.click(session.require("accessibility id", DELETE_KEY))
    time.sleep(1)
    after = dime_amount(session)
    inert = enabled in ("false", False) and after == before
    return {
        "observable": "the enabled state of the delete key and the amount label",
        "mode": mode,
        "amountBeforeDelete": before,
        "deleteEnabledBeforeTap": enabled,
        "amountAfterDelete": after,
        "identity": DIME_IDENTITY if inert else None,
        "observationReached": True,
        "cleanLaunch": True,
    }


def kiwix_state(session: Session) -> str:
    if session.find("-ios predicate string", SIDEBAR_SHOWN):
        return "shown"
    if session.find("-ios predicate string", SIDEBAR_HIDDEN):
        return "hidden"
    return "absent"


def kiwix_observe(session: Session, *, legal_neighbor: bool) -> dict:
    """Trigger and observation.

    legal_neighbor selects a sidebar entry, which changes only the list
    selection, instead of creating a tab, which changes currentItem.
    """
    at_launch = kiwix_state(session)
    session.click(session.require("-ios predicate string", SIDEBAR_HIDDEN))
    time.sleep(2)
    after_toggle = kiwix_state(session)
    if legal_neighbor:
        session.click(session.require("accessibility id", "Bookmarks"))
    else:
        tabs = session.find_all("accessibility id", "New Tab")
        if not tabs:
            raise RuntimeError("the split view exposed no New Tab control")
        session.click(tabs[0])
    time.sleep(3)
    after_action = kiwix_state(session)
    collapsed = not legal_neighbor and after_toggle == "shown" and after_action == "hidden"
    return {
        "observable": "whether the split view still exposes the Hide Sidebar control",
        "stateAtLaunch": at_launch,
        "stateAfterToggle": after_toggle,
        "stateAfterAction": after_action,
        "identity": KIWIX_IDENTITY if collapsed else None,
        "observationReached": after_toggle == "shown",
        "cleanLaunch": at_launch == "hidden",
    }
