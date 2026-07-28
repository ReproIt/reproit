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

    def test_checked_in_contract_is_valid_and_chromium_is_stable(self):
        CHECK.validate_support(self.support, self.gates)
        status = CHECK.status_document(self.support)
        stable = [target["id"] for target in status["targets"]
                  if target["maturity"] == "stable"]
        self.assertEqual(
            stable,
            ["tui", "web-chromium", "web-firefox", "web-webkit"],
        )

    def test_stable_target_cannot_keep_a_promotion_blocker(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["web-chromium"]["promotionBlockers"] = ["field gap"]
        with self.assertRaisesRegex(ValueError, "still has promotion blockers"):
            CHECK.validate_support(candidate, self.gates)

    def test_preview_target_must_name_its_exact_blockers(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["flutter-ios"]["promotionBlockers"] = []
        with self.assertRaisesRegex(ValueError, "must name promotion blockers"):
            CHECK.validate_support(candidate, self.gates)

    def test_release_gate_must_be_owned(self):
        candidate = copy.deepcopy(self.support)
        candidate["targets"]["tui"]["releaseGates"]["web-chromium"] = "linux-hosted"
        with self.assertRaisesRegex(ValueError, "must be owned"):
            CHECK.validate_support(candidate, self.gates)

    def test_render_is_deterministic(self):
        first = CHECK.render()
        second = CHECK.render()
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
