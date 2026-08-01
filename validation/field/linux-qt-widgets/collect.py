#!/usr/bin/env python3
"""Derive the Linux Qt Widgets field records from one campaign output tree."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from campaign_records import collect  # noqa: E402

TARGET = "linux-qt-widgets"

APPLICATIONS = {
    "qview": {
        "probeName": "qview",
        "id": "qview-fullscreen-unmaximize-453",
        "repository": "https://github.com/jurplel/qView",
        "issueUrl": "https://github.com/jurplel/qView/issues/453",
        "affectedRevision": "9f6c225451bb060af8fafd948839432a6de32f4a",
        "fixedRevision": "e28cbe7b8521959777f40ad6a43b62b4ee243b28",
        "authority": "standard",
        "expectedIdentity": "window-state:resized-after-fullscreen-round-trip",
        "minimizedAction": (
            "open the View menu, click Enter Full Screen, then activate Exit Full "
            "Screen, and read the frame extents back before and after the round trip"
        ),
        "neighboringLegalBehavior": (
            "the same full-screen round trip performed from a maximized window "
            "restores both the maximized geometry and the X11 maximized state on "
            "the affected build, so only the window-size restore path is wrong"
        ),
        "observed": lambda run: (
            "frame extents before the round trip "
            f"{run['targetObservation']['beforeExtents']} and after "
            f"{run['targetObservation']['afterExtents']}. The fixture image already "
            "matches the window size, so setWindowSize() shows up here as the window "
            "being moved rather than scaled"
        ),
    },
    "keepassxc": {
        "probeName": "keepassxc",
        "id": "keepassxc-autogenerate-charset-13073",
        "repository": "https://github.com/keepassxreboot/keepassxc",
        "issueUrl": "https://github.com/keepassxreboot/keepassxc/issues/13073",
        "affectedRevision": "caa7d1476134d86c1cf769081d8460933f4cd11c",
        "fixedRevision": "58a2919650f814e042daf0f51fe7c76705f0288c",
        "authority": "authored-contract",
        "expectedIdentity": (
            "generator-settings:new-entry-password-ignores-saved-length"
        ),
        "minimizedAction": (
            "store a password-generator configuration with a distinctive length, "
            "open Entries then New Entry, and read the character count of the "
            "auto-generated password field"
        ),
        "neighboringLegalBehavior": (
            "the same stored configuration used through the explicit Tools then "
            "Password Generator dialog is honoured on the affected build, so the "
            "settings write itself is not what fails"
        ),
        "observed": lambda run: (
            f"stored generator length {run['configuredPasswordLength']}, new-entry "
            f"password character count {run['generatedPasswordCharacterCount']}"
        ),
    },
}

CORPUS_CASES = [
    {
        "id": "qview-clean-base",
        "kind": "clean",
        "application": "qview",
        "variant": "default",
        "why": (
            "the ordinary full-screen round trip on the fixed build: the window "
            "keeps the geometry it had before the round trip"
        ),
    },
    {
        "id": "qview-adversarial-maximized-roundtrip",
        "kind": "adversarial",
        "application": "qview",
        "variant": "maximized-roundtrip",
        "why": (
            "the maximized round trip is the scenario the issue title names and it "
            "changes the window geometry twice on the way through full screen, so "
            "an oracle that merely watches for a resize would report it"
        ),
    },
    {
        "id": "keepassxc-clean-base",
        "kind": "clean",
        "application": "keepassxc",
        "variant": "default",
        "why": (
            "the ordinary new-entry flow on the fixed build: the generated password "
            "is exactly as long as the stored configuration says"
        ),
    },
    {
        "id": "keepassxc-adversarial-configured-length-32",
        "kind": "adversarial",
        "application": "keepassxc",
        "variant": "configured-length-32",
        "why": (
            "the stored configuration asks for 32 characters, so the new entry gets "
            "the same 32-character password the affected build produced by ignoring "
            "the configuration; only the stored length distinguishes the two"
        ),
    },
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=pathlib.Path)
    arguments = parser.parse_args()
    summary = collect(TARGET, arguments.output, APPLICATIONS, CORPUS_CASES)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
