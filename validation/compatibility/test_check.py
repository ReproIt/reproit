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

    def evidence(self, candidate, target_id):
        return candidate["targets"][target_id]["evidence"]

    def test_checked_in_contract_is_valid(self):
        CHECK.validate_support(self.support, self.gates)
        status = CHECK.status_document(self.support)
        self.assertEqual(
            len(status["targets"]), len(self.support["targets"])
        )
        for target in status["targets"]:
            self.assertTrue(target["nativeGates"], target["id"])
            self.assertTrue(target["bounds"]["platforms"], target["id"])

    def test_qualification_is_derived_from_completed_field_evidence(self):
        status = CHECK.status_document(self.support)
        levels = {target["id"]: target["qualification"] for target in status["targets"]}
        self.assertEqual(levels["backend-contract"], "qualified")
        self.assertEqual(levels["react-native-ios"], "preview")
        self.assertEqual(levels["tauri-linux"], "preview")

    def test_every_owned_gate_is_release_gated(self):
        for target_id, target in self.support["targets"].items():
            self.assertEqual(
                set(target["releaseGates"]), set(target["ownedGates"]), target_id
            )

    def test_an_unreleased_owned_gate_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["releaseGates"] = {}
        with self.assertRaisesRegex(ValueError, "must be release-gated"):
            CHECK.validate_support(candidate, self.gates)

    def test_release_gate_must_be_owned(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["releaseGates"]["web-chromium"] = "linux-hosted"
        with self.assertRaisesRegex(ValueError, "must be release-gated"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_unknown_native_gate_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["ownedGates"] = ["no-such-gate"]
        candidate["targets"]["tui"]["releaseGates"] = {"no-such-gate": "linux-hosted"}
        with self.assertRaisesRegex(ValueError, "unknown native gate"):
            CHECK.validate_support(candidate, self.gates)

    def test_policy_rules_must_fail_closed(self):
        candidate = copy.deepcopy(self.support)
        candidate["policy"]["requireExactIdentity"] = False
        with self.assertRaisesRegex(ValueError, "must fail closed"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_unknown_policy_key_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        candidate["policy"]["stableMinIndependentApplications"] = 2
        with self.assertRaisesRegex(ValueError, "policy keys do not match"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_unknown_target_key_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["maturity"] = "stable"
        with self.assertRaisesRegex(ValueError, "target keys do not match"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_unknown_evidence_key_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "tui")["blockers"] = []
        with self.assertRaisesRegex(ValueError, "evidence keys do not match"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_evidence_slot_cannot_cite_an_imaginary_ci_gate(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "flutter-ios")["packageInstall"] = {
            "kind": "ci-gate",
            "job": "no-such-job",
            "step": "no such step",
        }
        with self.assertRaisesRegex(ValueError, "does not exist"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_evidence_slot_cannot_cite_an_imaginary_step(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "flutter-ios")["packageInstall"] = {
            "kind": "ci-gate",
            "job": "rust",
            "step": "make it green",
        }
        with self.assertRaisesRegex(ValueError, "absent from CI job"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_evidence_slot_cannot_cite_a_missing_file(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "flutter-ios")["cleanCorpus"] = {
            "kind": "evidence",
            "path": "validation/field/corpus/nothing-here.json",
        }
        with self.assertRaisesRegex(ValueError, "evidence is missing"):
            CHECK.validate_support(candidate, self.gates)

    def test_an_evidence_slot_kind_must_be_typed(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "flutter-ios")["cleanCorpus"] = {"kind": "good-enough"}
        with self.assertRaisesRegex(ValueError, "kind is invalid"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_field_benchmark_must_name_its_own_target(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "electron-linux")["fieldBenchmark"] = (
            "validation/field/tui.json"
        )
        with self.assertRaisesRegex(ValueError, "names another target"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_field_benchmark_must_validate(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "tui")["fieldBenchmark"] = (
            "validation/support-manifest.json"
        )
        with self.assertRaises(ValueError):
            CHECK.validate_support(candidate, self.gates)

    def test_bounds_must_name_runtime_and_framework(self):
        candidate = copy.deepcopy(self.support)
        self.evidence(candidate, "flutter-ios")["bounds"]["runtime"] = []
        with self.assertRaisesRegex(ValueError, "must be a unique non-empty list"):
            CHECK.validate_support(candidate, self.gates)

    def test_a_duplicate_display_name_is_rejected(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["displayName"] = "Web Chromium"
        with self.assertRaisesRegex(ValueError, "displayName is duplicated"):
            CHECK.validate_support(candidate, self.gates)

    def test_generated_surfaces_carry_every_target_and_qualification(self):
        status = CHECK.status_document(self.support)
        table = CHECK.readme_platforms(status)
        claim = CHECK.support_claim(status)
        section = CHECK.target_section(status)
        document = CHECK.markdown_status(status)
        for target in status["targets"]:
            # The README is framework-shaped, so it carries each target's
            # framework and platform reach rather than its atomic gate name.
            for framework in target["bounds"]["framework"]:
                self.assertIn(framework, table)
            for platform in target["bounds"]["platforms"]:
                self.assertIn(platform, table)
            self.assertIn(target["displayName"], claim)
            self.assertIn(target["displayName"], section)
            self.assertIn(f"`{target['id']}`", document)
            self.assertIn(target["qualification"], section)
            self.assertIn(target["qualification"], document)
        self.assertNotIn("Backend |", table)
        self.assertIn("qualified", table)
        self.assertIn("preview", table)
        self.assertIn("qualified", claim)
        self.assertIn("Preview", claim)

    def test_render_is_deterministic(self):
        first = CHECK.render()
        second = CHECK.render()
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
