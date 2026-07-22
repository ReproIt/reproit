import copy
import importlib.util
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
        "reproductions": [
            {"status": "reproduced", "identity": "crash:example", "cleanLaunch": True}
            for _ in range(3)
        ],
        "minimized": True,
        "controls": ["fixed-revision", "neighboring-legal-behavior"],
        "manualReview": "confirmed-target-bug",
        "metrics": {"setupSeconds": 60, "replaySecondsP95": 3.5, "peakMemoryMiB": 256},
        "evidence": [],
    }


class FieldBenchmarkTest(unittest.TestCase):
    def test_pending_manifest_is_only_allowed_explicitly(self):
        pending = {
            "schemaVersion": 1,
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
            prior = Path.cwd()
            try:
                import os
                os.chdir(root)
                evidence = root / "validation/field/evidence"
                evidence.mkdir(parents=True)
                for name in ("a.json", "a.md", "b.json", "b.md"):
                    (evidence / name).write_text("evidence\n", encoding="utf-8")
                first = application("first-app", "https://github.com/example/first")
                second = application("second-app", "https://github.com/example/second")
                first["evidence"] = [
                    "validation/field/evidence/a.json", "validation/field/evidence/a.md"
                ]
                second["evidence"] = [
                    "validation/field/evidence/b.json", "validation/field/evidence/b.md"
                ]
                document = {
                    "schemaVersion": 1, "target": "web-chromium", "status": "complete",
                    "applications": [first, second],
                }
                CHECK.validate(document)
                duplicate = copy.deepcopy(document)
                duplicate["applications"][1]["repository"] = first["repository"]
                with self.assertRaisesRegex(ValueError, "independent"):
                    CHECK.validate(duplicate)
            finally:
                os.chdir(prior)

    def test_rejects_identity_drift_and_missing_controls(self):
        candidate = application("first-app", "https://github.com/example/first")
        candidate["reproductions"][1]["identity"] = "crash:other"
        with self.assertRaisesRegex(ValueError, "identity drifted"):
            CHECK.validate_application(candidate, 0)
        candidate["reproductions"][1]["identity"] = "crash:example"
        candidate["controls"].pop()
        with self.assertRaisesRegex(ValueError, "controls"):
            CHECK.validate_application(candidate, 0)


if __name__ == "__main__":
    unittest.main()
