#!/usr/bin/env python3
"""Derive the Linux Qt Quick field records from one campaign output tree."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from campaign_records import collect  # noqa: E402

TARGET = "linux-qt-quick"

APPLICATIONS = {
    "kalk": {
        "probeName": "kalk",
        "id": "kalk-second-equals-clears-input-475907",
        "repository": "https://invent.kde.org/utilities/kalk",
        "issueUrl": "https://bugs.kde.org/show_bug.cgi?id=475907",
        "affectedRevision": "67e5d3dd76425bf946687586288463c6df9508fe",
        "fixedRevision": "662aa91d58160fca8538f01d7a35253d351214c4",
        "authority": "standard",
        "expectedIdentity": "input-state:second-equals-clears-the-result",
        "minimizedAction": (
            "type 1+1, press equals, then press equals a second time, and read "
            "the display back through the AT-SPI text interface"
        ),
        "neighboringLegalBehavior": (
            "the first equals yields 2 on both revisions, so the equals path "
            "and the display read are both sound and only the second press "
            "differs"
        ),
        "observed": lambda run: (
            f"after typing {run['afterTyping']}, after the first equals "
            f"{run['afterFirstEquals']}, after {run['secondAction']} "
            f"{run['afterSecondAction']}"
        ),
    },
    "elisa": {
        "probeName": "elisa",
        "id": "elisa-progress-indicator-padding-497592",
        "repository": "https://invent.kde.org/multimedia/elisa",
        "issueUrl": "https://bugs.kde.org/show_bug.cgi?id=497592",
        "affectedRevision": "8286818ff1c55e9f45c0f64d4600e11655898a90",
        "fixedRevision": "cf0f8b41917ec2de61fe6fc89335cf0939568600",
        "authority": "standard",
        "expectedIdentity": (
            "progress-indicator:elapsed-time-minutes-not-zero-padded"
        ),
        "minimizedAction": (
            "open a short silent track and read the elapsed-position heading "
            "immediately before the Duration slider"
        ),
        "neighboringLegalBehavior": (
            "the track title heading in the same window reads identically on "
            "both revisions, so the same tree read returns unchanged strings "
            "for labels the formatter does not produce"
        ),
        "observed": lambda run: (
            f"elapsed heading {run['elapsedReading']!r}, total heading "
            f"{run['totalReading']!r}, zero padded="
            f"{run['elapsedIsZeroPadded']}"
        ),
    },
}

CORPUS_CASES = [
    {
        "id": "kalk-clean-base",
        "kind": "clean",
        "application": "kalk",
        "variant": "default",
        "why": (
            "the ordinary equals flow on the fixed build: pressing equals a "
            "second time leaves the result on the display"
        ),
    },
    {
        "id": "kalk-adversarial-explicit-backspace",
        "kind": "adversarial",
        "application": "kalk",
        "variant": "explicit-backspace",
        "why": (
            "backspacing the input away empties the display exactly as the "
            "defect does, and is entirely legal, so an oracle that reported an "
            "empty display rather than the equals path would call this a defect"
        ),
    },
    {
        "id": "elisa-clean-base",
        "kind": "clean",
        "application": "elisa",
        "variant": "default",
        "why": (
            "the ordinary playback view on the fixed build: the elapsed "
            "heading is zero padded"
        ),
    },
    {
        "id": "elisa-adversarial-track-title",
        "kind": "adversarial",
        "application": "elisa",
        "variant": "track-title",
        "why": (
            "the track title is a heading in the same window that legitimately "
            "carries no zero-padded time, so an oracle that demanded every "
            "heading match the padded form would report it"
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
