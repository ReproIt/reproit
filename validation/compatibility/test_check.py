import copy
import importlib.util
import json
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

    def test_checked_in_contract_is_valid_and_chromium_is_stable(self):
        CHECK.validate_support(self.support, self.gates)
        status = CHECK.status_document(self.support)
        stable = [target["id"] for target in status["targets"]
                  if target["maturity"] == "stable"]
        # The grandfathered schema-2 set is frozen: nothing may join it, because
        # it predates the schema-3 evidence standard.
        grandfathered = self.support["policy"]["grandfatheredStableTargets"]
        self.assertEqual(sorted(grandfathered), ["tui", "web-chromium", "web-firefox", "web-webkit"])
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
        self.promotion(candidate, "flutter-ios")["blockers"] = []
        with self.assertRaisesRegex(ValueError, "must name typed promotion blockers"):
            CHECK.validate_support(candidate, self.gates)

    def test_blocker_code_must_be_typed(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["blockers"][0]["code"] = "just-because"
        with self.assertRaisesRegex(ValueError, "is untyped"):
            CHECK.validate_support(candidate, self.gates)

    def test_blocker_evidence_must_exist(self):
        candidate = copy.deepcopy(self.support)
        blocker = self.promotion(candidate, "flutter-ios")["blockers"][0]
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

    def test_the_grandfathered_stable_set_cannot_grow(self):
        candidate = copy.deepcopy(self.support)
        candidate["policy"]["grandfatheredStableTargets"] = sorted(
            candidate["policy"]["grandfatheredStableTargets"] + ["flutter-ios"]
        )
        with self.assertRaisesRegex(ValueError, "cannot grow"):
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
        target["promotion"]["standard"] = "schema-3"
        candidate["policy"]["grandfatheredStableTargets"] = [
            name
            for name in candidate["policy"]["grandfatheredStableTargets"]
            if name != "tui"
        ]
        with self.assertRaisesRegex(ValueError, "has no cleanCorpus qualification"):
            CHECK.validate_support(candidate, self.gates)

    def test_production_to_local_is_independent_of_maturity(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["productionToLocal"] = "FixtureQualified"
        CHECK.validate_support(candidate, self.gates)
        status = CHECK.status_document(candidate)
        entry = next(t for t in status["targets"] if t["id"] == "flutter-ios")
        self.assertEqual(entry["maturity"], "preview")
        self.assertEqual(entry["productionToLocal"], "FixtureQualified")

    def test_an_invalid_production_to_local_value_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        self.promotion(candidate, "flutter-ios")["productionToLocal"] = "Totally"
        with self.assertRaisesRegex(ValueError, "productionToLocal value is invalid"):
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

    def test_render_is_deterministic(self):
        first = CHECK.render()
        second = CHECK.render()
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
