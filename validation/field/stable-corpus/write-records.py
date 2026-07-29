#!/usr/bin/env python3
"""Convert the native probes into strict retained corpus records."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

APPLICATIONS = {
    "vert": {
        "application": "vert-route-57",
        "repository": "https://github.com/VERT-sh/VERT",
        "revision": "a8386ee3f1efc40c37828f780e75cb3a8df4b12b",
    },
    "slidev-chromium": {
        "application": "slidev-monaco-2297",
        "repository": "https://github.com/slidevjs/slidev",
        "revision": "7d7aad8d2e0c3117227ed8e8840439723568c1ae",
    },
    "slidev-hash": {
        "application": "slidev-hash-1637",
        "repository": "https://github.com/slidevjs/slidev",
        "revision": "8b7ccf13358b904636d476072a0b67a857115a10",
    },
    "fx": {
        "application": "fx-empty-343",
        "repository": "https://github.com/antonmedv/fx",
        "revision": "14b2139b55627a823201aac8972699daf90076ce",
    },
    "nnn": {
        "application": "nnn-filter-2120",
        "repository": "https://github.com/jarun/nnn",
        "revision": "c73600a0da993b4675a6e6c7357546d5de22b4d1",
    },
}


def corpus_case(
    identifier: str,
    kind: str,
    application_key: str,
    fixture: str | None,
    variant: str,
    why: str,
    observation: dict,
) -> dict:
    application = APPLICATIONS[application_key]
    identity = observation["identity"]
    return {
        "id": identifier,
        "kind": kind,
        **application,
        "fixture": fixture,
        "variant": variant,
        "why": why,
        "observationReached": observation["observationReached"],
        "identity": identity,
        "falsePositive": identity is not None,
        "observation": observation,
    }


def web_cases(target: str, probe: dict) -> list[dict]:
    engine = probe["engine"]
    slidev_key = "slidev-chromium" if engine == "chromium" else "slidev-hash"
    return [
        corpus_case(
            f"vert-about-fixed-{engine}",
            "clean",
            "vert",
            None,
            "fixed-about-route",
            "the fixed direct route renders the exact About content",
            probe["cases"]["vertAbout"],
        ),
        corpus_case(
            f"vert-root-neighbor-{engine}",
            "adversarial",
            "vert",
            None,
            "legal-root-route",
            "the neighboring root route legally renders home content without About content",
            probe["cases"]["vertRootRoute"],
        ),
        corpus_case(
            f"slidev-neighbor-{engine}",
            "adversarial",
            slidev_key,
            "slidev-monaco.md" if engine == "chromium" else "slidev-hash.md",
            "legal-neighboring-navigation",
            "the retained neighboring navigation behavior remains legal on the fixed build",
            probe["cases"]["slidevAction"],
        ),
    ]


def tui_cases(probe: dict) -> list[dict]:
    observations = probe["cases"]
    return [
        corpus_case(
            "fx-valid-json",
            "clean",
            "fx",
            "generated-valid-json",
            "nonempty-json",
            "a nonempty JSON document renders normally in a fresh PTY",
            observations[0],
        ),
        corpus_case(
            "fx-fixed-empty-file",
            "adversarial",
            "fx",
            "generated-empty-json",
            "fixed-empty-file",
            "the repaired empty-file behavior resembles the old stall but completes legally",
            observations[1],
        ),
        corpus_case(
            "nnn-all-rows-match",
            "adversarial",
            "nnn",
            "generated-three-file-directory",
            "all-rows-legally-match",
            "a broad filter legally retains every row because every filename matches",
            observations[2],
        ),
    ]


def record(target: str, image: str, cases: list[dict]) -> dict:
    return {
        "schemaVersion": 1,
        "target": target,
        "worker": {
            "image": image,
            "platform": "linux/amd64",
            "network": "none",
        },
        "cleanCases": sum(case["kind"] == "clean" for case in cases),
        "adversarialCases": sum(case["kind"] == "adversarial" for case in cases),
        "confirmedFalsePositives": sum(case["falsePositive"] for case in cases),
        "unreachedObservations": sum(
            not case["observationReached"] for case in cases
        ),
        "containersRemaining": 0,
        "cases": cases,
    }


def write(path: Path, document: dict) -> None:
    path.write_text(f"{json.dumps(document, indent=2)}\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", required=True)
    parser.add_argument("--web", type=Path, action="append", default=[])
    parser.add_argument("--tui", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    for path in args.web:
        probe = json.loads(path.read_text(encoding="utf-8"))
        engine = probe["engine"]
        target = f"web-{engine}"
        write(args.output / f"{target}.json", record(target, args.image, web_cases(target, probe)))

    if not args.web and args.tui is None:
        parser.error("at least one --web or --tui probe is required")
    if args.tui is not None:
        tui_probe = json.loads(args.tui.read_text(encoding="utf-8"))
        write(args.output / "tui.json", record("tui", args.image, tui_cases(tui_probe)))


if __name__ == "__main__":
    main()
