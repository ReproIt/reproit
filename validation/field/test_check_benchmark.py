import copy
import importlib.util
import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory


MODULE_PATH = Path(__file__).with_name("check-benchmark.py")
SPEC = importlib.util.spec_from_file_location("check_benchmark", MODULE_PATH)
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def application(identifier, repository):
    return {
        "id": identifier,
        "repository": repository,
        "issueUrl": f"{repository}/issues/1",
        "affectedRevision": "a" * 40,
        "fixedRevision": "b" * 40,
        "authority": "platform",
        "expectedIdentity": "crash:example",
        "affectedReproductions": [
            {
                "status": "reproduced",
                "identity": "crash:example",
                "cleanLaunch": True,
                "observationReached": True,
            }
            for _ in range(3)
        ],
        "fixedReproductions": [
            {
                "status": "not_reproduced",
                "identity": None,
                "cleanLaunch": True,
                "observationReached": True,
            }
            for _ in range(3)
        ],
        "minimized": True,
        "controls": ["fixed-revision", "neighboring-legal-behavior"],
        "manualReview": "confirmed-target-bug",
        "metrics": {"setupSeconds": 60, "replaySecondsP95": 3.5, "peakMemoryMiB": 256},
        "evidence": [],
    }


def write_evidence(root, candidate, stem):
    evidence = root / "validation/field/evidence"
    evidence.mkdir(parents=True, exist_ok=True)
    record_path = evidence / f"{stem}.json"
    record_path.write_text(
        json.dumps(
            {
                "issue": candidate["issueUrl"],
                "affectedRevision": candidate["affectedRevision"],
                "fixedRevision": candidate["fixedRevision"],
                "identity": candidate["expectedIdentity"],
                "affected": [
                    {
                        "run": run,
                        "cleanLaunch": True,
                        "exceptions": [],
                        "jsHeapMiB": 256,
                        "elapsedSeconds": elapsed_seconds,
                    }
                    for run, elapsed_seconds in enumerate((1.0, 2.0, 3.5), start=1)
                ],
                "fixed": [
                    {
                        "run": run,
                        "cleanLaunch": True,
                        "exceptions": [],
                        "jsHeapMiB": 256,
                        "elapsedSeconds": elapsed_seconds,
                    }
                    for run, elapsed_seconds in enumerate((1.0, 2.0, 3.0), start=1)
                ],
                "neighboringLegalBehavior": "control passed",
                "minimizedAction": "one action",
            }
        ),
        encoding="utf-8",
    )
    markdown_path = evidence / f"{stem}.md"
    markdown_path.write_text("evidence\n", encoding="utf-8")
    candidate["evidence"] = [
        f"validation/field/evidence/{stem}.json",
        f"validation/field/evidence/{stem}.md",
    ]


class FieldBenchmarkTest(unittest.TestCase):
    def test_pending_manifest_is_only_allowed_explicitly(self):
        pending = {
            "schemaVersion": 2,
            "target": "web-chromium",
            "status": "pending",
            "applications": [],
        }
        CHECK.validate(pending, allow_pending=True)
        with self.assertRaisesRegex(ValueError, "pending"):
            CHECK.validate(pending)

    def test_complete_manifest_requires_independent_apps_and_evidence(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            first = application("first-app", "https://github.com/example/first")
            second = application("second-app", "https://github.com/example/second")
            write_evidence(root, first, "a")
            write_evidence(root, second, "b")
            document = {
                "schemaVersion": 2,
                "target": "web-chromium",
                "status": "complete",
                "applications": [first, second],
            }
            CHECK.validate(document, root=root)
            duplicate = copy.deepcopy(document)
            duplicate["applications"][1]["repository"] = first["repository"]
            with self.assertRaisesRegex(ValueError, "independent"):
                CHECK.validate(duplicate, root=root)

    def test_evidence_revision_and_metric_drift_are_rejected(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            first = application("first-app", "https://github.com/example/first")
            second = application("second-app", "https://github.com/example/second")
            write_evidence(root, first, "a")
            write_evidence(root, second, "b")
            document = {
                "schemaVersion": 2,
                "target": "web-chromium",
                "status": "complete",
                "applications": [first, second],
            }
            evidence_path = root / first["evidence"][0]
            record = json.loads(evidence_path.read_text(encoding="utf-8"))
            record["affectedRevision"] = "c" * 40
            evidence_path.write_text(json.dumps(record), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "affectedRevision"):
                CHECK.validate(document, root=root)
            record["affectedRevision"] = first["affectedRevision"]
            evidence_path.write_text(json.dumps(record), encoding="utf-8")
            first["metrics"]["peakMemoryMiB"] = 257
            with self.assertRaisesRegex(ValueError, "peak memory"):
                CHECK.validate(document, root=root)

    def test_rejects_identity_drift_and_missing_controls(self):
        candidate = application("first-app", "https://github.com/example/first")
        candidate["affectedReproductions"][1]["identity"] = "crash:other"
        with self.assertRaisesRegex(ValueError, "identity drifted"):
            CHECK.validate_application(candidate, 0)
        candidate["affectedReproductions"][1]["identity"] = "crash:example"
        candidate["controls"].pop()
        with self.assertRaisesRegex(ValueError, "controls"):
            CHECK.validate_application(candidate, 0)

    def test_rejects_a_fixed_control_that_did_not_reach_observation(self):
        candidate = application("first-app", "https://github.com/example/first")
        candidate["fixedReproductions"][0]["observationReached"] = False
        with self.assertRaisesRegex(ValueError, "observation point"):
            CHECK.validate_application(candidate, 0)


if __name__ == "__main__":
    unittest.main()
