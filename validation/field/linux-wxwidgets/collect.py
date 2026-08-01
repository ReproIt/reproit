#!/usr/bin/env python3
"""Derive the Linux wxWidgets field records from one campaign output tree."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from campaign_records import collect  # noqa: E402

TARGET = "linux-wxwidgets"

APPLICATIONS = {
    "wxmaxima": {
        "probeName": "wxmaxima",
        "id": "wxmaxima-greek-pane-1276",
        "repository": "https://github.com/wxMaxima-developers/wxmaxima",
        "issueUrl": "https://github.com/wxMaxima-developers/wxmaxima/issues/1276",
        "affectedRevision": "e5c410e884c3f1b24c54ac1179c16c3cf0283247",
        "fixedRevision": "684d2ed4e106fc0fc174f5537129bfd13ba68b93",
        "authority": "standard",
        "expectedIdentity": "aui-perspective:greek-pane-forced-open-on-launch",
        "minimizedAction": (
            "launch against a clean profile and read the checked state of the "
            "Greek Letters item on the View menu, which wxMaxima derives from "
            "the pane's own IsShown()"
        ),
        "neighboringLegalBehavior": (
            "two neighbouring panes keep their own defaults at both revisions, "
            "Main Toolbar shown and Statistics hidden, and the Greek pane still "
            "responds to its own View toggle, so the reader is neither blind nor "
            "reporting a stale snapshot"
        ),
        "observed": lambda run: (
            "Greek Letters view item checked="
            f"{run['greekPane']['shown']} on a clean profile; after its own "
            f"toggle checked={run['greekPaneAfterOwnToggle']['shown']}"
        ),
    },
    "poedit": {
        "probeName": "poedit",
        "id": "poedit-source-view-no-line-900",
        "repository": "https://github.com/vslavik/poedit",
        "issueUrl": "https://github.com/vslavik/poedit/issues/900",
        "affectedRevision": "c4fe890ef72c8c8cbc6b9b8cc1784dba10447798",
        "fixedRevision": "f261986fe726a5c2a0ca0717179740c31875de57",
        "authority": "standard",
        "expectedIdentity": "source-reference:line-less-reference-not-resolved",
        "minimizedAction": (
            "open a catalog whose first entry references a source file with no "
            "line number, activate Show Code Occurrences, and read the code "
            "occurrence viewer back"
        ),
        "neighboringLegalBehavior": (
            "the second entry references the same file with a line number and "
            "resolves on the affected build, so the viewer, the file and the "
            "base path are all sound and only the missing line number breaks it"
        ),
        "observed": lambda run: (
            "line-less reference errors="
            f"{run['linelessReference']['errors']} source shown="
            f"{run['linelessReference']['sourceShown']}; line-numbered "
            f"reference errors={run['lineNumberedReference']['errors']} source "
            f"shown={run['lineNumberedReference']['sourceShown']}"
        ),
    },
}

CORPUS_CASES = [
    {
        "id": "wxmaxima-clean-base",
        "kind": "clean",
        "application": "wxmaxima",
        "variant": "default",
        "why": (
            "the ordinary launch on the fixed build: the Greek pane stays closed "
            "because nothing forces it open"
        ),
    },
    {
        "id": "wxmaxima-adversarial-neighboring-panes",
        "kind": "adversarial",
        "application": "wxmaxima",
        "variant": "neighboring-panes",
        "why": (
            "the Main Toolbar pane is open at launch on both revisions and that "
            "is correct, so an oracle that reported any pane being open at "
            "launch would call this the defect"
        ),
    },
    {
        "id": "poedit-clean-base",
        "kind": "clean",
        "application": "poedit",
        "variant": "default",
        "why": (
            "the ordinary code occurrence flow on the fixed build: both the "
            "line-less and the line-numbered reference open the source file"
        ),
    },
    {
        "id": "poedit-adversarial-missing-source",
        "kind": "adversarial",
        "application": "poedit",
        "variant": "missing-source",
        "why": (
            "the referenced source file genuinely does not exist, so the viewer "
            "shows the same Source code not found heading the defect produces; "
            "only the line-numbered reference failing too separates the two, and "
            "an oracle keyed on the heading alone would report a false positive"
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
