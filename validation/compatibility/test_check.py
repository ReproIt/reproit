import copy
import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("check.py")
SPEC = importlib.util.spec_from_file_location("compatibility_check", MODULE_PATH)
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class CompatibilityContractTests(unittest.TestCase):
    def setUp(self):
        self.support = json.loads(CHECK.SUPPORT_PATH.read_text(encoding="utf-8"))
        self.gates = json.loads(CHECK.GATES_PATH.read_text(encoding="utf-8"))

    def promotion(self, candidate, target_id):
        return candidate["targets"][target_id]["promotion"]

    def production_record(self, root, target_id, level="FixtureQualified"):
        required_stages = [
            "reset",
            "production-signal",
            "cloud-ingestion",
            "local-materialization",
            "exact-local-reproduction",
            "direct-replay",
            "retention-and-deletion",
        ]
        stages = []
        chain_parts = []
        for stage_id in required_stages:
            file_name = f"{stage_id}.json"
            payload = f'{{"stage":"{stage_id}"}}\n'.encode()
            (root / file_name).write_bytes(payload)
            digest = f"sha256:{hashlib.sha256(payload).hexdigest()}"
            stages.append(
                {
                    "id": stage_id,
                    "summary": f"retained {stage_id}",
                    "present": True,
                    "required": True,
                    "file": file_name,
                    "bytes": len(payload),
                    "malformed": False,
                    "rawSha256": digest,
                    "sanitizedSha256": digest,
                }
            )
            chain_parts.append(f"{stage_id}:{digest}")
        commands = [
            {
                "stage": stage_id,
                "command": f"run {stage_id}",
                "assertions": [f"{stage_id} passed"],
            }
            for stage_id in required_stages
        ]
        execution = {
            "commands": commands,
            "reset": {
                "command": "reset test workspace",
                "evidence": ["reset"],
            },
            "cleanup": {
                "command": "delete test project",
                "evidence": ["retention-and-deletion"],
            },
        }
        web_engines = {
            "web-chromium": "chromium",
            "web-firefox": "firefox",
            "web-webkit": "webkit",
        }
        if target_id in web_engines:
            execution["adapter"] = {
                "kind": "playwright",
                "engine": web_engines[target_id],
            }
        record = {
            "schemaVersion": 2,
            "gate": "D5-production-to-local",
            "targetId": target_id,
            "qualification": level,
            "origin": {
                "kind": (
                    "independent-application"
                    if level == "IndependentQualified"
                    else "fixture"
                ),
                "summary": "test-only production occurrence",
            },
            "revisions": {
                "cli": f"git:{'a' * 40}",
                "sdk": {
                    "name": "test-sdk",
                    "revision": f"git:{'b' * 40}",
                },
                "application": f"sha256:{'c' * 64}",
            },
            "cloud": {
                "baseUrl": "https://cloud.example.test",
                "projectId": "app_test",
                "occurrenceId": "run_test",
                "bucketId": "bkt_test",
            },
            "local": {
                "provider": "test-trusted-provider",
                "trusted": True,
            },
            "execution": execution,
            "stages": stages,
            "missingRequiredStages": [],
            "qualificationBlockers": [],
            "chainSha256": (
                "sha256:"
                + hashlib.sha256("\n".join(chain_parts).encode()).hexdigest()
            ),
        }
        record_path = root / "record.json"
        record_path.write_text(f"{json.dumps(record, indent=2)}\n", encoding="utf-8")
        return record_path

    def test_checked_in_contract_is_valid_and_chromium_is_stable(self):
        CHECK.validate_support(self.support, self.gates)
        status = CHECK.status_document(self.support)
        stable = [target["id"] for target in status["targets"]
                  if target["maturity"] == "stable"]
        # All Stable targets have completed the schema-3 ratchet, so the
        # compatibility escape hatch is permanently empty.
        grandfathered = self.support["policy"]["grandfatheredStableTargets"]
        self.assertEqual(grandfathered, [])
        schema_2_stable = sorted(
            target_id for target_id in stable
            if self.support["targets"][target_id]["promotion"]["standard"] == "schema-2"
        )
        self.assertEqual(schema_2_stable, sorted(grandfathered))
        # Anything else that is Stable earned it under schema-3, so it must carry
        # a complete two-application benchmark and no remaining blocker.
        for target_id in stable:
            promotion = self.support["targets"][target_id]["promotion"]
            if promotion["standard"] == "schema-2":
                continue
            self.assertEqual(promotion["blockers"], [], target_id)
            benchmark = json.loads(
                (CHECK.ROOT / promotion["fieldBenchmark"]).read_text(encoding="utf-8"))
            self.assertEqual(benchmark["status"], "complete", target_id)
            self.assertEqual(len(benchmark["applications"]), 2, target_id)
            for slot in CHECK.QUALIFICATION_SLOTS:
                self.assertNotEqual(promotion[slot]["kind"], "missing", f"{target_id}.{slot}")

    def test_electron_linux_is_stable_under_schema_3(self):
        electron = self.support["targets"]["electron-linux"]
        self.assertEqual(electron["maturity"], "stable")
        self.assertEqual(electron["promotion"]["standard"], "schema-3")
        self.assertNotIn("electron-linux", self.support["policy"]["grandfatheredStableTargets"])

    def test_stable_target_cannot_keep_a_promotion_blocker(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "web-chromium")["blockers"] = [
            {
                "code": "incomplete-evidence",
                "detail": "field gap",
                "command": None,
                "evidence": [],
            }
        ]
        with self.assertRaisesRegex(ValueError, "still has promotion blockers"):
            CHECK.validate_support(candidate, self.gates)

    def test_preview_target_must_name_its_exact_blockers(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-android")["blockers"] = []
        with self.assertRaisesRegex(ValueError, "must name typed promotion blockers"):
            CHECK.validate_support(candidate, self.gates)

    def test_blocker_code_must_be_typed(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-android")["blockers"][0]["code"] = "just-because"
        with self.assertRaisesRegex(ValueError, "is untyped"):
            CHECK.validate_support(candidate, self.gates)

    def test_blocker_evidence_must_exist(self):
        candidate = copy.deepcopy(self.support)
        blocker = self.promotion(candidate, "flutter-android")["blockers"][0]
        blocker["evidence"] = ["validation/field/evidence/does-not-exist.json"]
        with self.assertRaisesRegex(ValueError, "evidence is missing"):
            CHECK.validate_support(candidate, self.gates)

    def test_release_gate_must_be_owned(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["releaseGates"]["web-chromium"] = "linux-hosted"
        with self.assertRaisesRegex(ValueError, "must be owned"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_qualification_cannot_cite_an_imaginary_ci_gate(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["packageInstall"] = {
            "kind": "ci-gate",
            "job": "no-such-job",
            "step": "no such step",
        }
        with self.assertRaisesRegex(ValueError, "does not exist"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_qualification_cannot_cite_an_imaginary_step(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["packageInstall"] = {
            "kind": "ci-gate",
            "job": "rust",
            "step": "make it green",
        }
        with self.assertRaisesRegex(ValueError, "absent from CI job"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_qualification_cannot_cite_a_missing_evidence_file(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["cleanCorpus"] = {
            "kind": "evidence",
            "path": "validation/field/corpus/nothing-here.json",
        }
        with self.assertRaisesRegex(ValueError, "evidence is missing"):
            CHECK.validate_support(candidate, self.gates)

    def test_only_grandfathered_targets_may_use_the_schema_2_standard(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["standard"] = "schema-2"
        with self.assertRaisesRegex(ValueError, "only grandfathered targets"):
            CHECK.validate_support(candidate, self.gates)

    def test_the_grandfathered_stable_set_cannot_be_reintroduced(self):
        candidate = copy.deepcopy(self.support)
        candidate["policy"]["grandfatheredStableTargets"] = sorted(
            candidate["policy"]["grandfatheredStableTargets"] + ["flutter-ios"]
        )
        with self.assertRaisesRegex(ValueError, "cannot be reintroduced"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_schema_3_promotion_needs_every_qualification_slot(self):
        candidate = copy.deepcopy(self.support)
        target = candidate["targets"]["electron-linux"]
        target["maturity"] = "stable"
        promotion = target["promotion"]
        promotion["blockers"] = []
        promotion["fieldBenchmark"] = "validation/field/tui.json"
        with self.assertRaisesRegex(ValueError, "names another target"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_schema_3_promotion_rejects_a_missing_qualification_slot(self):
        candidate = copy.deepcopy(self.support)
        target = candidate["targets"]["tui"]
        target["promotion"]["cleanCorpus"] = {"kind": "missing"}
        with self.assertRaisesRegex(ValueError, "has no cleanCorpus qualification"):
            CHECK.validate_support(candidate, self.gates)

    def test_production_to_local_is_independent_of_maturity(self):
        candidate = copy.deepcopy(self.support)
        with tempfile.TemporaryDirectory(
            dir=CHECK.ROOT / "validation/compatibility"
        ) as directory:
            evidence = self.production_record(Path(directory), "flutter-android")
            relative = evidence.relative_to(CHECK.ROOT).as_posix()
            self.promotion(candidate, "flutter-android")["productionToLocal"] = {
                "level": "FixtureQualified",
                "evidence": relative,
            }
            CHECK.validate_support(candidate, self.gates)
            status = CHECK.status_document(candidate)
            entry = next(t for t in status["targets"] if t["id"] == "flutter-android")
            self.assertEqual(entry["maturity"], "preview")
            self.assertEqual(entry["productionToLocal"], "FixtureQualified")
            self.assertEqual(entry["productionToLocalEvidence"], relative)

    def test_a_bare_production_to_local_string_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["productionToLocal"] = "FixtureQualified"
        with self.assertRaisesRegex(ValueError, "evidence binding object"):
            CHECK.validate_support(candidate, self.gates)

    def test_unqualified_target_cannot_cite_production_evidence(self):
        candidate = copy.deepcopy(self.support)
        binding = self.promotion(candidate, "flutter-ios")["productionToLocal"]
        binding["evidence"] = "validation/support-manifest.json"
        with self.assertRaisesRegex(ValueError, "must not cite evidence while Unqualified"):
            CHECK.validate_support(candidate, self.gates)

    def test_production_evidence_must_name_the_same_target(self):
        candidate = copy.deepcopy(self.support)
        with tempfile.TemporaryDirectory(
            dir=CHECK.ROOT / "validation/compatibility"
        ) as directory:
            evidence = self.production_record(Path(directory), "web-chromium")
            self.promotion(candidate, "flutter-ios")["productionToLocal"] = {
                "level": "FixtureQualified",
                "evidence": evidence.relative_to(CHECK.ROOT).as_posix(),
            }
            with self.assertRaisesRegex(ValueError, "names another target"):
                CHECK.validate_support(candidate, self.gates)

    def test_web_production_evidence_must_bind_the_matching_engine(self):
        candidate = copy.deepcopy(self.support)
        with tempfile.TemporaryDirectory(
            dir=CHECK.ROOT / "validation/compatibility"
        ) as directory:
            evidence = self.production_record(Path(directory), "web-firefox")
            record = json.loads(evidence.read_text(encoding="utf-8"))
            record["execution"]["adapter"]["engine"] = "chromium"
            evidence.write_text(f"{json.dumps(record)}\n", encoding="utf-8")
            self.promotion(candidate, "web-firefox")["productionToLocal"] = {
                "level": "FixtureQualified",
                "evidence": evidence.relative_to(CHECK.ROOT).as_posix(),
            }
            with self.assertRaisesRegex(ValueError, "does not match web-firefox"):
                CHECK.validate_support(candidate, self.gates)

    def test_independent_qualification_rejects_fixture_origin(self):
        candidate = copy.deepcopy(self.support)
        with tempfile.TemporaryDirectory(
            dir=CHECK.ROOT / "validation/compatibility"
        ) as directory:
            evidence = self.production_record(Path(directory), "flutter-ios")
            record = json.loads(evidence.read_text(encoding="utf-8"))
            record["qualification"] = "IndependentQualified"
            evidence.write_text(f"{json.dumps(record)}\n", encoding="utf-8")
            self.promotion(candidate, "flutter-ios")["productionToLocal"] = {
                "level": "IndependentQualified",
                "evidence": evidence.relative_to(CHECK.ROOT).as_posix(),
            }
            with self.assertRaisesRegex(ValueError, "origin.kind cannot prove"):
                CHECK.validate_support(candidate, self.gates)

    def test_bounds_must_name_runtime_and_framework(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["bounds"]["runtime"] = []
        with self.assertRaisesRegex(ValueError, "must be a unique non-empty list"):
            CHECK.validate_support(candidate, self.gates)

    def test_generated_surfaces_carry_every_target(self):
        status = CHECK.status_document(self.support)
        table = CHECK.readme_table(status)
        for target in status["targets"]:
            self.assertIn(target["displayName"], table)
        claim = CHECK.support_claim(status)
        self.assertIn("Stable is an atomic compatibility claim", claim)

    def test_stability_plan_covers_every_target_and_real_execution_lane(self):
        status = CHECK.status_document(self.support)
        plan = CHECK.stability_plan(status, self.gates["gates"])
        for target in status["targets"]:
            self.assertIn(f"`{target['id']}`", plan)
        self.assertIn("ssh black@zgx-5a09.local", plan)
        self.assertIn("then `ssh strix`", plan)
        self.assertIn("`IndependentQualified` count: 21", plan)
        self.assertIn("local amd64 emulation failure is", plan)
        self.assertIn("diagnostic only and cannot defer native Linux", plan)

    def test_render_is_deterministic(self):
        first = CHECK.render()
        second = CHECK.render()
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
