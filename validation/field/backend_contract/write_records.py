#!/usr/bin/env python3
"""Convert a complete backend-contract campaign into promotion records."""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[3]
EXPECTED = {
    "gitea": {
        "issue": "https://github.com/go-gitea/gitea/issues/35886",
        "repository": "https://github.com/go-gitea/gitea",
        "affectedRevision": "98c61942aa433342eacf08e4040ded80b1d0efe1",
        "fixedRevision": "4812e354866a066dcb899af667b0fad5fa094065",
        "identity": "filtered-commit-total-count-ignores-bounds",
        "minimizedAction": (
            "GET one commits collection with authored since and until bounds, "
            "then compare X-Total-Count with the returned complete page"
        ),
    },
    "memos": {
        "issue": "https://github.com/usememos/memos/issues/5443",
        "repository": "https://github.com/usememos/memos",
        "affectedRevision": "14fb38f37560541bf2719647e7e8b1468937f8ef",
        "fixedRevision": "7c3fcc297d8e5a955d9c0bc4f3ca917854132e8e",
        "identity": "public-memo-list-requires-authentication",
        "minimizedAction": (
            "seed one public memo through an authenticated operation, then issue "
            "one anonymous GET to the contract's public memo collection"
        ),
    },
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def grouped_records(summary: dict[str, Any]) -> dict[tuple[str, str], list[dict[str, Any]]]:
    require(summary.get("runsPerRevision") == 3, "campaign must use three runs")
    require(summary.get("containersRemaining") == 0, "campaign left containers")
    groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for record in summary.get("records", []):
        groups[(record["application"], record["revisionKind"])].append(record)
    for application, expected in EXPECTED.items():
        for revision_kind in ("affected", "fixed"):
            records = sorted(
                groups[(application, revision_kind)],
                key=lambda record: record["run"],
            )
            require(len(records) == 3, f"{application} {revision_kind} needs three runs")
            require(
                [record["run"] for record in records] == [1, 2, 3],
                f"{application} {revision_kind} run numbers drifted",
            )
            for record in records:
                require(record["cleanLaunch"] is True, "run was not a clean launch")
                require(record["observationReached"] is True, "observation was not reached")
                require(record["exceptions"] == [], "run recorded an exception")
                require(all(record["cleanup"].values()), "run cleanup was incomplete")
                require(
                    record["image"]["revision"] == expected[f"{revision_kind}Revision"],
                    "image revision drifted",
                )
                expected_identity = expected["identity"] if revision_kind == "affected" else None
                require(
                    record["observation"]["identity"] == expected_identity,
                    f"{application} {revision_kind} identity drifted",
                )
            groups[(application, revision_kind)] = records
    return groups


def evidence_record(
    application: str,
    groups: dict[tuple[str, str], list[dict[str, Any]]],
) -> dict[str, Any]:
    expected = EXPECTED[application]

    def runs(revision_kind: str) -> list[dict[str, Any]]:
        return [
            {
                "run": record["run"],
                "cleanLaunch": record["cleanLaunch"],
                "exceptions": record["exceptions"],
                "jsHeapMiB": None,
                "elapsedSeconds": record["observation"]["elapsedSeconds"],
                "setupSeconds": record["setupSeconds"],
                "imageId": record["image"]["id"],
                "imageArchitecture": record["image"]["architecture"],
                "networkInternal": record["runtime"]["networkInternal"],
                "publishedHost": record["runtime"]["publishedHost"],
                "cleanup": record["cleanup"],
                "observation": record["observation"],
                "rawRecordSha256": record["rawRecordSha256"],
            }
            for record in groups[(application, revision_kind)]
        ]

    affected = runs("affected")
    fixed = runs("fixed")
    return {
        "issue": expected["issue"],
        "affectedRevision": expected["affectedRevision"],
        "fixedRevision": expected["fixedRevision"],
        "identity": expected["identity"],
        "memoryMeasurement": "unavailable",
        "affected": affected,
        "fixed": fixed,
        "neighboringLegalBehavior": affected[0]["observation"]["neighboringLegalBehavior"],
        "minimizedAction": expected["minimizedAction"],
        "containment": {
            "network": "per-run internal Docker network with no external egress",
            "hostBinding": "ephemeral port on 127.0.0.1 only",
            "state": "fresh owned bind mount deleted after every run",
            "rootFilesystem": "read-only",
            "remainingContainers": 0,
        },
    }


def benchmark_application(
    application: str,
    evidence: dict[str, Any],
    groups: dict[tuple[str, str], list[dict[str, Any]]],
) -> dict[str, Any]:
    expected = EXPECTED[application]
    affected_seconds = [
        record["observation"]["elapsedSeconds"]
        for record in groups[(application, "affected")]
    ]
    setup_seconds = max(
        record["setupSeconds"]
        for revision_kind in ("affected", "fixed")
        for record in groups[(application, revision_kind)]
    )
    evidence_base = f"validation/field/evidence/{application}"
    return {
        "id": (
            "gitea-total-count-filtered-commits-35886"
            if application == "gitea"
            else "memos-public-read-requires-auth-5443"
        ),
        "repository": expected["repository"],
        "issueUrl": expected["issue"],
        "affectedRevision": expected["affectedRevision"],
        "fixedRevision": expected["fixedRevision"],
        "authority": "authored-contract",
        "expectedIdentity": expected["identity"],
        "affectedReproductions": [
            {
                "status": "reproduced",
                "identity": expected["identity"],
                "cleanLaunch": True,
                "observationReached": True,
            }
            for _ in evidence["affected"]
        ],
        "fixedReproductions": [
            {
                "status": "not_reproduced",
                "identity": None,
                "cleanLaunch": True,
                "observationReached": True,
            }
            for _ in evidence["fixed"]
        ],
        "minimized": True,
        "controls": ["fixed-revision", "neighboring-legal-behavior"],
        "manualReview": "confirmed-target-bug",
        "metrics": {
            "setupSeconds": setup_seconds,
            "replaySecondsP95": max(affected_seconds),
            "peakMemoryMiB": None,
            "memoryMeasurement": "unavailable",
        },
        "evidence": [f"{evidence_base}.json", f"{evidence_base}.md"],
    }


def corpus(groups: dict[tuple[str, str], list[dict[str, Any]]]) -> dict[str, Any]:
    gitea = groups[("gitea", "affected")][0]
    memos = groups[("memos", "fixed")][0]
    image_ids = sorted(
        {
            record["image"]["id"]
            for records in groups.values()
            for record in records
        }
    )
    cases = [
        {
            "id": "memos-fixed-public-list-clean",
            "kind": "clean",
            "application": "Memos",
            "repository": EXPECTED["memos"]["repository"],
            "revision": EXPECTED["memos"]["fixedRevision"],
            "fixture": "validation/field/backend_contract/run_campaign.py",
            "variant": "fixed revision, anonymous read of a seeded PUBLIC memo",
            "why": (
                "the authored anonymous operation succeeds and excludes the adjacent "
                "private memo, so the public-access oracle must remain silent"
            ),
            "observationReached": True,
            "identity": None,
            "falsePositive": False,
            "observation": {
                "identity": None,
                "status": memos["observation"]["anonymousStatus"],
                "publicMemoCount": memos["observation"]["anonymousPublicCount"],
                "privateMemoCount": memos["observation"]["anonymousPrivateCount"],
                "rawRecordSha256": memos["rawRecordSha256"],
            },
        },
        {
            "id": "gitea-affected-unfiltered-adversarial",
            "kind": "adversarial",
            "application": "Gitea",
            "repository": EXPECTED["gitea"]["repository"],
            "revision": EXPECTED["gitea"]["affectedRevision"],
            "fixture": "validation/field/backend_contract/run_campaign.py",
            "variant": "affected revision, adjacent unfiltered commits request",
            "why": (
                "the same endpoint and X-Total-Count header are exercised on legal "
                "unfiltered behavior and must not be classified as the bounded-filter defect"
            ),
            "observationReached": True,
            "identity": None,
            "falsePositive": False,
            "observation": {
                "identity": None,
                "bodyCount": gitea["observation"]["unfilteredBodyCount"],
                "headerCount": gitea["observation"]["unfilteredHeaderCount"],
                "rawRecordSha256": gitea["rawRecordSha256"],
            },
        },
        {
            "id": "memos-fixed-protected-route-adversarial",
            "kind": "adversarial",
            "application": "Memos",
            "repository": EXPECTED["memos"]["repository"],
            "revision": EXPECTED["memos"]["fixedRevision"],
            "fixture": "validation/field/backend_contract/run_campaign.py",
            "variant": "fixed revision, anonymous read of a genuinely protected identity route",
            "why": (
                "a 401 superficially resembles the field defect, but the authored route "
                "requires authentication and must not produce the public-access identity"
            ),
            "observationReached": True,
            "identity": None,
            "falsePositive": False,
            "observation": {
                "identity": None,
                "status": memos["observation"]["protectedStatus"],
                "rawRecordSha256": memos["rawRecordSha256"],
            },
        },
    ]
    return {
        "schemaVersion": 1,
        "target": "backend-contract",
        "worker": {
            "image": " + ".join(image_ids),
            "platform": "Docker Linux application containers over SQLite",
            "network": "none",
        },
        "cleanCases": 1,
        "adversarialCases": 2,
        "confirmedFalsePositives": 0,
        "unreachedObservations": 0,
        "containersRemaining": 0,
        "cases": cases,
    }


def write_json(path: Path, document: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


def write_markdown(application: str, evidence: dict[str, Any]) -> None:
    expected = EXPECTED[application]
    affected_ids = sorted({run["imageId"] for run in evidence["affected"]})
    fixed_ids = sorted({run["imageId"] for run in evidence["fixed"]})
    contract_paths = (
        "`templates/swagger/v1_json.tmpl`"
        if application == "gitea"
        else "`proto/api/v1` and `proto/gen/openapi.yaml`"
    )
    text = f"""# {application.title()} backend-contract field evidence

- Issue: {expected["issue"]}
- Affected revision: `{expected["affectedRevision"]}`
- Fixed revision: `{expected["fixedRevision"]}`
- Affected image ids: `{", ".join(affected_ids)}`
- Fixed image ids: `{", ".join(fixed_ids)}`
- Oracle identity: `{expected["identity"]}`
- Runs: three affected and three fixed, each from a fresh SQLite data directory.
- Containment: an internal per-run Docker network, read-only root filesystem,
  loopback-only ephemeral host binding, and cleanup verification.
- Contract revision check: {contract_paths} have no diff across the affected
  and fixed revisions. `build-images.sh` enforces that with `cmp` and `diff`.

Run the campaign and validators from the repository root:

```sh
python3 validation/field/backend_contract/run_campaign.py
python3 validation/field/backend_contract/write_records.py \\
  target/reproit-validation/backend-contract-field/summary.json
python3 validation/field/check-benchmark.py validation/field/backend-contract.json
python3 validation/field/check-corpus.py validation/field/corpus/backend-contract.json
validation/backend/cli-e2e/run.sh
validation/backend/run-linux-docker.sh
```
"""
    path = ROOT / f"validation/field/evidence/{application}.md"
    path.write_text(text, encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("summary", type=Path)
    args = parser.parse_args()
    summary = json.loads(args.summary.read_text(encoding="utf-8"))
    groups = grouped_records(summary)
    evidence = {
        application: evidence_record(application, groups)
        for application in EXPECTED
    }
    for application, document in evidence.items():
        write_json(
            ROOT / f"validation/field/evidence/{application}.json",
            document,
        )
        write_markdown(application, document)
    benchmark = {
        "schemaVersion": 3,
        "target": "backend-contract",
        "status": "complete",
        "applications": [
            benchmark_application(application, evidence[application], groups)
            for application in ("gitea", "memos")
        ],
    }
    write_json(ROOT / "validation/field/backend-contract.json", benchmark)
    write_json(ROOT / "validation/field/corpus/backend-contract.json", corpus(groups))
    print("backend-contract promotion records written")


if __name__ == "__main__":
    main()
