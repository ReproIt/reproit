#!/usr/bin/env python3
"""Turn one campaign output tree into a target's benchmark, corpus and evidence.

Every number written here is read back out of the retained per-run probe
records, so a record can only claim what the campaign actually observed. The
per-target collectors supply the application and corpus descriptions; none of
the counting, timing or verdict logic lives in them.
"""

from __future__ import annotations

import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / "validation/field/evidence"
RUNS = (1, 2, 3)


def load(path: pathlib.Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def run_records(output: pathlib.Path, application: str, revision: str) -> list[dict]:
    records = []
    for run in RUNS:
        record = load(output / f"{application}-{revision}-{run}.json")
        if record["run"] != run or record["revision"] != revision:
            raise SystemExit(f"{application} {revision} run {run} is mislabelled")
        if record["memoryMeasurement"] != "unavailable":
            raise SystemExit(f"{application} {revision} run {run} claims memory")
        if record["jsHeapMiB"] is not None:
            raise SystemExit(f"{application} {revision} run {run} invented a heap")
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


def markdown(
    target: str,
    application: dict,
    affected: list[dict],
    fixed: list[dict],
    image: str,
    setup_seconds: int,
) -> str:
    describe = application["observed"]
    lines = [
        f"# {target} field campaign: {application['id']}",
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
        "for the whole image, every application and both revisions. The worker "
        "reuses any layer it already holds, so this is not a cold-build cost",
        "- Worker: linux/amd64 container on the native x86_64 host, --network none",
        "- Seconds below are the probe's own trigger-to-observation time inside an"
        " already-running container, not the container lifetime",
        "",
        "Observed difference, affected run 1 versus fixed run 1:",
        "",
        f"- affected: {describe(affected[0])}",
        f"- fixed: {describe(fixed[0])}",
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


def build_application(
    target: str,
    output: pathlib.Path,
    application: dict,
    setup: int,
    image: str,
) -> dict:
    name = application["probeName"]
    affected = run_records(output, name, "affected")
    fixed = run_records(output, name, "fixed")
    identifier = application["id"]
    (EVIDENCE / f"{identifier}-runs.jsonl").write_text(
        "".join(json.dumps(item, sort_keys=True) + "\n" for item in affected + fixed),
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
    (EVIDENCE / f"{identifier}.json").write_text(
        json.dumps(record, indent=2) + "\n", encoding="utf-8"
    )
    (EVIDENCE / f"{identifier}.md").write_text(
        markdown(target, application, affected, fixed, image, setup),
        encoding="utf-8",
    )
    return {
        "id": identifier,
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
            f"validation/field/evidence/{identifier}.json",
            f"validation/field/evidence/{identifier}.md",
            f"validation/field/evidence/{identifier}-runs.jsonl",
        ],
    }


def build_corpus(
    target: str,
    output: pathlib.Path,
    applications: dict,
    cases: list[dict],
    image: str,
    containers_remaining: int,
) -> dict:
    built = []
    for case in cases:
        application = applications[case["application"]]
        record = load(output / f"corpus-{case['application']}-{case['variant']}.json")
        if record["revision"] != "fixed" or record["variant"] != case["variant"]:
            raise SystemExit(f"corpus case {case['id']} ran the wrong subject")
        built.append(
            {
                "id": case["id"],
                "kind": case["kind"],
                "application": application["id"],
                "repository": application["repository"],
                "revision": application["fixedRevision"],
                "fixture": case.get("fixture"),
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
        "target": target,
        "worker": {"image": image, "platform": "linux/amd64", "network": "none"},
        "cleanCases": sum(1 for case in built if case["kind"] == "clean"),
        "adversarialCases": sum(1 for case in built if case["kind"] == "adversarial"),
        "confirmedFalsePositives": sum(1 for case in built if case["falsePositive"]),
        "unreachedObservations": sum(
            1 for case in built if not case["observationReached"]
        ),
        "containersRemaining": containers_remaining,
        "cases": built,
    }


def collect(
    target: str,
    output: pathlib.Path,
    applications: dict,
    corpus_cases: list[dict],
) -> dict:
    start = int((output / "build-start.epoch").read_text().strip())
    finish = int((output / "build-finish.epoch").read_text().strip())
    setup = finish - start
    inspect = load(output / "image-inspect.json")
    image = inspect[0]["Id"]
    cleanup = load(output / "cleanup.json")
    benchmark = {
        "schemaVersion": 3,
        "target": target,
        "status": "complete",
        "applications": [
            build_application(target, output, application, setup, image)
            for application in applications.values()
        ],
    }
    (ROOT / f"validation/field/{target}.json").write_text(
        json.dumps(benchmark, indent=2) + "\n", encoding="utf-8"
    )
    corpus = build_corpus(
        target,
        output,
        applications,
        corpus_cases,
        image,
        cleanup["containersRemaining"],
    )
    (ROOT / f"validation/field/corpus/{target}.json").write_text(
        json.dumps(corpus, indent=2) + "\n", encoding="utf-8"
    )
    return {
        "setupSeconds": setup,
        "image": image,
        "cleanCases": corpus["cleanCases"],
        "adversarialCases": corpus["adversarialCases"],
        "confirmedFalsePositives": corpus["confirmedFalsePositives"],
    }
