#!/usr/bin/env python3
"""Derive the Linux GTK field records from one campaign output tree."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from campaign_records import collect  # noqa: E402

TARGET = "linux-gtk"

APPLICATIONS = {
    "gnome-text-editor": {
        "probeName": "gnome-text-editor",
        "id": "gnome-text-editor-search-entry-a11y-771",
        "repository": "https://gitlab.gnome.org/GNOME/gnome-text-editor",
        "issueUrl": "https://gitlab.gnome.org/GNOME/gnome-text-editor/-/issues/771",
        "affectedRevision": "8732544897aada0500e32df6dba1a7259f9ddc7b",
        "fixedRevision": "bf3a1414dc8ab39349c1d24beec89ea417a058b0",
        "authority": "platform",
        "expectedIdentity": "accessibility:search-and-replace-entries-unlabeled",
        "minimizedAction": (
            "open a document, press Ctrl+H to open the search bar in replace "
            "mode, and read the accessible names of the entries it realises"
        ),
        "neighboringLegalBehavior": (
            "every button in the same window reports its own accessible name on "
            "both revisions, so the reader is not simply blind to the search bar"
        ),
        "observed": lambda run: (
            "search entry labeled="
            f"{run['searchEntryLabeled']}, replace entry labeled="
            f"{run['replaceEntryLabeled']}, entries="
            f"{[(r['role'], r['name']) for r in run['entries']]}"
        ),
    },
    "gnome-clocks": {
        "probeName": "gnome-clocks",
        "id": "gnome-clocks-world-dialog-default-focus-393",
        "repository": "https://gitlab.gnome.org/GNOME/gnome-clocks",
        "issueUrl": "https://gitlab.gnome.org/GNOME/gnome-clocks/-/issues/393",
        "affectedRevision": "1283eb4668d83fd710e9b272abca1443f96ff21f",
        "fixedRevision": "6055f282826d3ac817697e33697142899989c269",
        "authority": "platform",
        "expectedIdentity": "dialog-focus:initial-focus-on-cancel-instead-of-entry",
        "minimizedAction": (
            "activate Add World Clock and read which object holds the AT-SPI "
            "focused state once the dialog has mapped, before any input"
        ),
        "neighboringLegalBehavior": (
            "one Tab moves focus to the location entry on the affected build, so "
            "the entry is focusable and the dialog's focus chain is sound; only "
            "the initial assignment is wrong"
        ),
        "observed": lambda run: (
            "focused on dialog map="
            f"{[(r['role'], r['name']) for r in run['dialogInitialFocus']]}, "
            f"entry reachable by Tab="
            f"{run['neighboringLegalBehavior']['entryReachableByTab']}"
        ),
    },
}

CORPUS_CASES = [
    {
        "id": "text-editor-clean-base",
        "kind": "clean",
        "application": "gnome-text-editor",
        "variant": "default",
        "why": (
            "the ordinary search bar on the fixed build: both entries carry the "
            "accessible names the fix gives them"
        ),
    },
    {
        "id": "text-editor-adversarial-document-body",
        "kind": "adversarial",
        "application": "gnome-text-editor",
        "variant": "document-body",
        "why": (
            "the document body is a text node with no accessible name on both "
            "revisions and that is correct, so an oracle that reported any "
            "unnamed text node would call the fixed build defective"
        ),
    },
    {
        "id": "clocks-clean-base",
        "kind": "clean",
        "application": "gnome-clocks",
        "variant": "default",
        "why": (
            "the ordinary add-clock dialog on the fixed build: the location "
            "entry holds focus the moment the dialog maps"
        ),
    },
    {
        "id": "clocks-adversarial-main-window-focus",
        "kind": "adversarial",
        "application": "gnome-clocks",
        "variant": "main-window-focus",
        "why": (
            "a button legitimately holds focus in the main window before the "
            "dialog opens, which is the same condition the defect shows inside "
            "the dialog; only scoping the judgement to the dialog separates them"
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
