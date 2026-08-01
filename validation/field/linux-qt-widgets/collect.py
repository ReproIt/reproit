#!/usr/bin/env python3
"""Derive the Linux Qt Widgets field records from one campaign output tree.

Every number this writes is read back out of the retained per-run probe
records. Nothing here is typed by hand, so a record can only claim what the
campaign actually observed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[3]
TARGET = "linux-qt-widgets"
EVIDENCE = ROOT / "validation/field/evidence"

APPLICATIONS = {
    "qview": {
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
    },
    "keepassxc": {
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


def load(path: pathlib.Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_records(output: pathlib.Path, application: str, revision: str) -> list[dict]:
    records = []
    for run in (1, 2, 3):
        record = load(output / f"{application}-{revision}-{run}.json")
        if record["run"] != run or record["revision"] != revision:
            raise SystemExit(f"{application} {revision} run {run} is mislabelled")
        if record["memoryMeasurement"] != "unavailable" or record["jsHeapMiB"] is not None:
            raise SystemExit(f"{application} {revision} run {run} invented memory")
        records.append(record)
    return records


def reproduction(record: dict, expected: str | None) -> dict:
    if record["identity"] != expected:
        raise SystemExit(
            f"identity {record['identity']!r} is not the expected {expected!r}"
        )
    return {
        "status": "reproduced" if expected else "not_reproduced",
        "identity": record["identity"],
        "cleanLaunch": record["cleanLaunch"],
        "observationReached": record["observationReached"],
    }


def evidence_runs(records: list[dict]) -> list[dict]:
    return [
        {
            "run": record["run"],
            "cleanLaunch": record["cleanLaunch"],
            "exceptions": record["exceptions"],
            "jsHeapMiB": record["jsHeapMiB"],
            "elapsedSeconds": record["elapsedSeconds"],
        }
        for record in records
    ]


OBSERVED = {
    "qview-fullscreen-unmaximize-453": lambda run: (
        "frame extents before the round trip "
        f"{run['targetObservation']['beforeExtents']} and after "
        f"{run['targetObservation']['afterExtents']}. The fixture image already "
        "matches the window size, so setWindowSize() shows up here as the window "
        "being moved rather than scaled"
    ),
    "keepassxc-autogenerate-charset-13073": lambda run: (
        f"stored generator length {run['configuredPasswordLength']}, new-entry "
        f"password character count {run['generatedPasswordCharacterCount']}"
    ),
}


def evidence_markdown(
    application: dict,
    affected: list[dict],
    fixed: list[dict],
    image: str,
    setup_seconds: int,
) -> str:
    describe = OBSERVED[application["id"]]
    observed = (describe(affected[0]), describe(fixed[0]))
    lines = [
        f"# {TARGET} field campaign: {application['id']}",
        "",
        f"- Repository: {application['repository']}",
        f"- Issue: {application['issueUrl']}",
        f"- Affected revision: {application['affectedRevision']}",
        f"- Fixed revision: {application['fixedRevision']}",
        f"- Expected identity: {application['expectedIdentity']}",
        f"- Minimized action: {application['minimizedAction']}",
        f"- Neighboring legal behavior: {application['neighboringLegalBehavior']}",
        f"- Worker image digest: {image}",
        f"- Worker image assembly: {setup_seconds} s of wall time on the worker "
        "for the whole image, both applications and both revisions. The worker "
        "reuses any layer it already holds, so this is not a cold-build cost",
        "- Worker: linux/amd64 container on the native x86_64 host, --network none",
        "- Seconds below are the probe's own trigger-to-observation time inside an"
        " already-running container, not the container lifetime",
        "",
        "Observed difference, affected run 1 versus fixed run 1:",
        "",
        f"- affected: {observed[0]}",
        f"- fixed: {observed[1]}",
        "",
        "| Revision | Run | Identity | Observation reached | Clean launch | Seconds |",
        "|---|---|---|---|---|---|",
    ]
    for revision, records in (("affected", affected), ("fixed", fixed)):
        for record in records:
            lines.append(
                f"| {revision} | {record['run']} | {record['identity'] or 'none'} "
                f"| {str(record['observationReached']).lower()} "
                f"| {str(record['cleanLaunch']).lower()} "
                f"| {record['elapsedSeconds']} |"
            )
    lines.append("")
    return "\n".join(lines)


def build_application(output: pathlib.Path, name: str, setup: int, image: str) -> dict:
    application = APPLICATIONS[name]
    affected = run_records(output, name, "affected")
    fixed = run_records(output, name, "fixed")
    # One retained probe record per line. The suffix is deliberate: the
    # benchmark validator wants exactly one .json structured record.
    raw_path = EVIDENCE / f"{application['id']}-runs.jsonl"
    raw_path.write_text(
        "".join(
            json.dumps(item, sort_keys=True) + "\n" for item in affected + fixed
        ),
        encoding="utf-8",
    )
    record = {
        "issue": application["issueUrl"],
        "affectedRevision": application["affectedRevision"],
        "fixedRevision": application["fixedRevision"],
        "identity": application["expectedIdentity"],
        "memoryMeasurement": "unavailable",
        "affected": evidence_runs(affected),
        "fixed": evidence_runs(fixed),
        "neighboringLegalBehavior": application["neighboringLegalBehavior"],
        "minimizedAction": application["minimizedAction"],
    }
    (EVIDENCE / f"{application['id']}.json").write_text(
        json.dumps(record, indent=2) + "\n", encoding="utf-8"
    )
    (EVIDENCE / f"{application['id']}.md").write_text(
        evidence_markdown(application, affected, fixed, image, setup),
        encoding="utf-8",
    )
    return {
        "id": application["id"],
        "repository": application["repository"],
        "issueUrl": application["issueUrl"],
        "affectedRevision": application["affectedRevision"],
        "fixedRevision": application["fixedRevision"],
        "authority": application["authority"],
        "expectedIdentity": application["expectedIdentity"],
        "affectedReproductions": [
            reproduction(item, application["expectedIdentity"]) for item in affected
        ],
        "fixedReproductions": [reproduction(item, None) for item in fixed],
        "minimized": True,
        "controls": ["fixed-revision", "neighboring-legal-behavior"],
        "manualReview": "confirmed-target-bug",
        "metrics": {
            "setupSeconds": setup,
            "replaySecondsP95": max(item["elapsedSeconds"] for item in affected),
            "peakMemoryMiB": None,
            "memoryMeasurement": "unavailable",
        },
        "evidence": [
            f"validation/field/evidence/{application['id']}.json",
            f"validation/field/evidence/{application['id']}.md",
            f"validation/field/evidence/{application['id']}-runs.jsonl",
        ],
    }


def build_corpus(output: pathlib.Path, image: str, containers_remaining: int) -> dict:
    cases = []
    for case in CORPUS_CASES:
        record = load(output / f"corpus-{case['application']}-{case['variant']}.json")
        if record["revision"] != "fixed" or record["variant"] != case["variant"]:
            raise SystemExit(f"corpus case {case['id']} ran the wrong subject")
        application = APPLICATIONS[case["application"]]
        cases.append(
            {
                "id": case["id"],
                "kind": case["kind"],
                "application": application["id"],
                "repository": application["repository"],
                "revision": application["fixedRevision"],
                "fixture": None,
                "variant": case["variant"],
                "why": case["why"],
                "observationReached": record["observationReached"],
                "identity": record["identity"],
                "falsePositive": record["identity"] is not None,
                "observation": record,
            }
        )
    return {
        "schemaVersion": 1,
        "target": TARGET,
        "worker": {
            "image": image,
            "platform": "linux/amd64",
            "network": "none",
        },
        "cleanCases": sum(1 for case in cases if case["kind"] == "clean"),
        "adversarialCases": sum(1 for case in cases if case["kind"] == "adversarial"),
        "confirmedFalsePositives": sum(1 for case in cases if case["falsePositive"]),
        "unreachedObservations": sum(
            1 for case in cases if not case["observationReached"]
        ),
        "containersRemaining": containers_remaining,
        "cases": cases,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=pathlib.Path)
    arguments = parser.parse_args()
    output = arguments.output
    start = int((output / "build-start.epoch").read_text().strip())
    finish = int((output / "build-finish.epoch").read_text().strip())
    setup = finish - start
    inspect = json.loads((output / "image-inspect.json").read_text(encoding="utf-8"))
    image = inspect[0]["Id"]
    cleanup = load(output / "cleanup.json")
    benchmark = {
        "schemaVersion": 3,
        "target": TARGET,
        "status": "complete",
        "applications": [
            build_application(output, name, setup, image) for name in APPLICATIONS
        ],
    }
    (ROOT / f"validation/field/{TARGET}.json").write_text(
        json.dumps(benchmark, indent=2) + "\n", encoding="utf-8"
    )
    corpus = build_corpus(output, image, cleanup["containersRemaining"])
    (ROOT / f"validation/field/corpus/{TARGET}.json").write_text(
        json.dumps(corpus, indent=2) + "\n", encoding="utf-8"
    )
    print(
        json.dumps(
            {
                "setupSeconds": setup,
                "image": image,
                "cleanCases": corpus["cleanCases"],
                "adversarialCases": corpus["adversarialCases"],
                "confirmedFalsePositives": corpus["confirmedFalsePositives"],
                "packagesDigest": digest(output / "build-packages.tsv"),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
